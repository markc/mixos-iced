// SPDX-License-Identifier: MIT OR Apache-2.0
//! Line measurement for drawing and hit-testing (plan §4.2, D8, D9): walks
//! `view::clusters` over the visible window of one line, seeking long lines
//! through `view::line_checkpoints` (cached per text version) so a caret deep
//! in a 5 MiB line costs O(4 KiB), not O(line).

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use super::Source;
use super::{Cluster, MeasureCfg};
use super::{content_end, line_of};

/// Lines longer than this seek through checkpoints.
pub const LONG_LINE: usize = 4096;

/// Checkpoints per line start, valid for one text version and measure.
#[derive(Default)]
pub struct Checkpoints {
    key: Option<(u64, u64, usize, MeasureCfg)>,
    lines: HashMap<usize, Vec<(usize, usize)>>,
}

impl Checkpoints {
    /// Document identity, content revision and measurement configuration each
    /// invalidate checkpoints, including equal-length replacements.
    fn sync(&mut self, text: &dyn Source, cfg: &MeasureCfg) {
        let key = Some((text.identity(), text.revision(), text.len(), *cfg));
        if self.key != key {
            self.key = key;
            self.lines.clear();
        }
    }

    /// The best `(offset, cells)` start at or before `offset` / `cells` on
    /// `line` (the line start for short lines).
    fn start(
        &mut self,
        text: &dyn Source,
        cfg: &MeasureCfg,
        line: usize,
        before: impl Fn(&(usize, usize)) -> bool,
    ) -> (usize, usize) {
        let Some(r) = text.line_range(line) else {
            return (text.len(), 0);
        };
        if r.len() <= LONG_LINE {
            return (r.start, 0);
        }
        self.sync(text, cfg);
        if self.lines.len() > 64 {
            self.lines.clear();
        }
        let ck = self
            .lines
            .entry(r.start)
            .or_insert_with(|| text.line_checkpoints(cfg, line));
        ck.get(ck.partition_point(before).saturating_sub(1))
            .copied()
            .unwrap_or((r.start, 0))
    }
}

/// One cluster placed on the grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    pub range: Range<usize>,
    /// Starting cell (absolute, from the line start).
    pub cell: usize,
    pub cells: u8,
    pub is_tab: bool,
    pub ascii: bool,
}

/// The clusters of one line that intersect the cell window `[x0, x1)`.
#[derive(Debug, Clone, Default)]
pub struct LineCells {
    /// Line content range (no `\r\n` / `\n`).
    pub content: Range<usize>,
    pub placed: Vec<Placed>,
    /// Cells at the content end, when the walk reached it.
    pub end_cells: Option<usize>,
}

impl LineCells {
    /// Absolute cell of `offset` on this line, clipped to the walked window:
    /// before the window → its left edge, beyond it → `x1`.
    pub fn cell_of(&self, offset: usize, x1: usize) -> usize {
        if offset >= self.content.end {
            return self.end_cells.unwrap_or(x1);
        }
        // The first placed cluster ending after `offset` contains it or, when
        // `offset` lies left of the window, starts the window.
        self.placed
            .get(self.placed.partition_point(|p| p.range.end <= offset))
            .map_or(self.end_cells.unwrap_or(x1), |p| p.cell)
    }
}

/// One measured visible line. Syntax and decorations deliberately stay out of
/// this cache: they may change without a document content revision.
pub(super) struct CachedLine {
    pub cells: LineCells,
    pub text: Option<String>,
}

/// Reuse unchanged rows between redraws and keep the overlap on vertical
/// scrolls. Storage is confined to the viewport; long graphemes are measured
/// but their unbounded bytes are not retained between frames.
#[derive(Default)]
pub(super) struct ViewportCache {
    key: Option<(u64, u64, usize, MeasureCfg, usize, usize)>,
    rows: BTreeMap<usize, CachedLine>,
}

const MAX_ROW_TEXT: usize = 64 * 1024;

impl ViewportCache {
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        text: &dyn Source,
        cfg: &MeasureCfg,
        ck: &mut Checkpoints,
        first: usize,
        last: usize,
        x0: usize,
        x1: usize,
    ) {
        let key = Some((text.identity(), text.revision(), text.len(), *cfg, x0, x1));
        if self.key != key {
            self.key = key;
            self.rows.clear();
        }
        self.rows.retain(|line, _| *line >= first && *line <= last);
        for line in first..=last {
            self.rows.entry(line).or_insert_with(|| {
                let cells = walk(text, cfg, ck, line, x0, x1);
                let text_bytes = match (cells.placed.first(), cells.placed.last()) {
                    (Some(a), Some(b)) if b.range.end - a.range.start <= MAX_ROW_TEXT => {
                        let mut bytes = String::with_capacity(b.range.end - a.range.start);
                        text.read(a.range.start..b.range.end, &mut bytes);
                        Some(bytes)
                    }
                    (None, None) => Some(String::new()),
                    _ => None,
                };
                CachedLine {
                    cells,
                    text: text_bytes,
                }
            });
        }
    }

    pub fn rows(&self) -> impl Iterator<Item = (usize, &CachedLine)> {
        self.rows.iter().map(|(&line, row)| (line, row))
    }
}

/// Walk `line`'s clusters covering cells `[x0, x1)`.
pub fn walk(
    text: &dyn Source,
    cfg: &MeasureCfg,
    ck: &mut Checkpoints,
    line: usize,
    x0: usize,
    x1: usize,
) -> LineCells {
    let Some(r) = text.line_range(line) else {
        return LineCells::default();
    };
    let end = content_end(text, line);
    let (from, from_cells) = ck.start(text, cfg, line, |&(_, c)| c <= x0);
    let mut out = LineCells {
        content: r.start..end,
        placed: Vec::new(),
        end_cells: None,
    };
    if from >= end {
        out.end_cells = Some(from_cells);
        return out;
    }
    let mut cell = from_cells;
    let mut reached_end = true;
    for Cluster {
        range,
        cells,
        is_tab,
        ascii,
    } in text.clusters(cfg, from..end, from_cells)
    {
        if cell >= x1 {
            reached_end = false;
            break;
        }
        if range.start >= end {
            break;
        }
        let next = cell + cells as usize;
        if next > x0 || (cells == 0 && cell >= x0) {
            out.placed.push(Placed {
                range: range.clone(),
                cell,
                cells,
                is_tab,
                ascii,
            });
        }
        cell = next;
    }
    if reached_end {
        out.end_cells = Some(cell);
    }
    out
}

/// The absolute cell of `offset` (exact, any line length).
pub fn cells_of(
    text: &dyn Source,
    cfg: &MeasureCfg,
    ck: &mut Checkpoints,
    offset: usize,
) -> (usize, usize) {
    let line = line_of(text, offset);
    let end = content_end(text, line);
    let offset = offset.min(end);
    let (from, from_cells) = ck.start(text, cfg, line, |&(o, _)| o <= offset);
    let mut cell = from_cells;
    for c in text.clusters(cfg, from..end, from_cells) {
        if c.range.end > offset {
            break;
        }
        cell += c.cells as usize;
    }
    (line, cell)
}

/// The offset nearest to fractional cell `target` on `line` (mouse hits):
/// the boundary before a cluster when the target is in its first half.
pub fn offset_at(
    text: &dyn Source,
    cfg: &MeasureCfg,
    ck: &mut Checkpoints,
    line: usize,
    target: f32,
) -> usize {
    let line = line.clamp(1, text.line_count().max(1));
    let end = content_end(text, line);
    let t = target.max(0.0) as usize;
    let (from, from_cells) = ck.start(text, cfg, line, |&(_, c)| c <= t);
    let mut cell = from_cells as f32;
    for c in text.clusters(cfg, from..end, from_cells) {
        if c.range.start >= end {
            break;
        }
        let w = c.cells as f32;
        if target < cell + w / 2.0 {
            return c.range.start;
        }
        cell += w;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::super::fixture::Text;
    use super::*;
    use std::cell::Cell;

    struct Counted {
        text: Text,
        identity: u64,
        revision: u64,
        walks: Cell<usize>,
        reads: Cell<usize>,
    }

    impl Counted {
        fn new(body: &str) -> Self {
            Self {
                text: Text::from_text(body).unwrap(),
                identity: 1,
                revision: 0,
                walks: Cell::new(0),
                reads: Cell::new(0),
            }
        }
    }

    impl Source for Counted {
        fn identity(&self) -> u64 {
            self.identity
        }
        fn revision(&self) -> u64 {
            self.revision
        }
        fn len(&self) -> usize {
            self.text.len()
        }
        fn line_count(&self) -> usize {
            self.text.line_count()
        }
        fn line_start(&self, line: usize) -> Option<usize> {
            self.text.line_start(line)
        }
        fn line_range(&self, line: usize) -> Option<Range<usize>> {
            self.text.line_range(line)
        }
        fn content_end(&self, line: usize) -> usize {
            self.text.content_end(line)
        }
        fn line_of(&self, offset: usize) -> usize {
            self.text.line_of(offset)
        }
        fn clamp_offset(&self, offset: usize) -> usize {
            self.text.clamp_offset(offset)
        }
        fn read(&self, range: Range<usize>, output: &mut String) {
            self.reads.set(self.reads.get() + 1);
            self.text.read(range, output);
        }
        fn clusters(
            &self,
            cfg: &MeasureCfg,
            range: Range<usize>,
            cells: usize,
        ) -> Box<dyn Iterator<Item = Cluster> + '_> {
            self.walks.set(self.walks.get() + 1);
            self.text.clusters(cfg, range, cells)
        }
        fn line_checkpoints(&self, cfg: &MeasureCfg, line: usize) -> Vec<(usize, usize)> {
            self.text.line_checkpoints(cfg, line)
        }
    }

    fn cfg() -> MeasureCfg {
        MeasureCfg {
            tab_size: 4,
            ambiguous_wide: false,
        }
    }

    #[test]
    fn tabs_and_wide_characters_on_the_grid() {
        let text = Text::from_text("a\tb中c\r\nnext").unwrap();
        let mut ck = Checkpoints::default();
        let lc = walk(&text, &cfg(), &mut ck, 1, 0, 80);
        let cells: Vec<(usize, u8)> = lc.placed.iter().map(|p| (p.cell, p.cells)).collect();
        assert_eq!(cells, [(0, 1), (1, 3), (4, 1), (5, 2), (7, 1)]);
        assert_eq!(lc.end_cells, Some(8), "the CR is not part of the content");
        assert_eq!(
            cells_of(&text, &cfg(), &mut ck, 3),
            (1, 5),
            "the wide char starts at cell 5"
        );
        assert_eq!(cells_of(&text, &cfg(), &mut ck, 6), (1, 7));
        assert_eq!(
            cells_of(&text, &cfg(), &mut ck, 8),
            (1, 8),
            "inside the CRLF clamps to the content end"
        );
        // Round trip offset ↔ cell across the tab and the wide char.
        for (o, _) in "a\tb中c".char_indices() {
            let (_, c) = cells_of(&text, &cfg(), &mut ck, o);
            assert_eq!(
                offset_at(&text, &cfg(), &mut ck, 1, c as f32),
                o,
                "offset {o} at cell {c}"
            );
        }
        assert_eq!(
            offset_at(&text, &cfg(), &mut ck, 1, 6.2),
            6,
            "past the middle of 中 → after it"
        );
        assert_eq!(offset_at(&text, &cfg(), &mut ck, 1, 5.9), 3);
        assert_eq!(
            offset_at(&text, &cfg(), &mut ck, 1, 999.0),
            7,
            "clamped before the CRLF"
        );
    }

    #[test]
    fn a_window_clips_and_reports_offscreen_offsets_at_its_edges() {
        let s = "0123456789".repeat(3);
        let text = Text::from_text(&s).unwrap();
        let mut ck = Checkpoints::default();
        let lc = walk(&text, &cfg(), &mut ck, 1, 10, 20);
        assert_eq!(lc.placed.first().map(|p| p.cell), Some(10));
        assert_eq!(lc.placed.len(), 10);
        assert_eq!(lc.end_cells, None);
        assert_eq!(lc.cell_of(15, 20), 15);
        assert_eq!(lc.cell_of(25, 20), 20);
        assert_eq!(lc.cell_of(2, 20), 10, "left of the window → its left edge");
    }

    #[test]
    fn long_lines_seek_through_checkpoints() {
        let s = format!("{}\nend", "x".repeat(5 * 1024 * 1024));
        let text = Text::from_text(&s).unwrap();
        let mut ck = Checkpoints::default();
        let far = 5 * 1024 * 1024 - 3;
        assert_eq!(cells_of(&text, &cfg(), &mut ck, far), (1, far));
        let lc = walk(&text, &cfg(), &mut ck, 1, far, far + 100);
        assert_eq!(lc.placed.first().map(|p| p.range.start), Some(far));
        assert_eq!(lc.end_cells, Some(5 * 1024 * 1024));
        assert_eq!(
            offset_at(&text, &cfg(), &mut ck, 1, (far + 1) as f32),
            far + 1
        );
    }

    #[test]
    fn viewport_reuses_redraws_and_measures_only_incoming_scroll_rows() {
        let text = Counted::new(&"hello 中\tworld\n".repeat(200));
        let mut cache = ViewportCache::default();
        let mut ck = Checkpoints::default();
        cache.prepare(&text, &cfg(), &mut ck, 1, 40, 0, 80);
        assert_eq!((text.walks.get(), text.reads.get()), (40, 40));
        // Selection, mouse movement and live syntax changes do not remeasure.
        for _ in 0..10 {
            cache.prepare(&text, &cfg(), &mut ck, 1, 40, 0, 80);
        }
        assert_eq!((text.walks.get(), text.reads.get()), (40, 40));
        for first in 2..=101 {
            cache.prepare(&text, &cfg(), &mut ck, first, first + 39, 0, 80);
            assert_eq!(
                cache.rows.len(),
                40,
                "only the current viewport is retained"
            );
            for (line, row) in cache.rows() {
                let expected = walk(&text.text, &cfg(), &mut ck, line, 0, 80);
                assert_eq!(row.cells.placed, expected.placed);
                assert_eq!(row.text.as_deref(), Some("hello 中\tworld"));
            }
        }
        assert_eq!((text.walks.get(), text.reads.get()), (140, 140));
        // A reverse notch adds exactly the previously evicted top row.
        cache.prepare(&text, &cfg(), &mut ck, 100, 139, 0, 80);
        assert_eq!((text.walks.get(), text.reads.get()), (141, 141));
        cache.prepare(&text, &cfg(), &mut ck, 1, 0, 0, 80);
        assert_eq!(cache.rows.len(), 0);
    }

    #[test]
    fn viewport_invalidates_document_measurement_and_horizontal_window_changes() {
        let mut text = Counted::new("a\t中\nb\t中\nc\t中");
        let mut cache = ViewportCache::default();
        let mut ck = Checkpoints::default();
        let mut measure = cfg();
        cache.prepare(&text, &measure, &mut ck, 1, 3, 0, 80);
        text.text = Text::from_text("z\t文\ny\t文\nx\t文").unwrap();
        text.revision += 1; // equal-length replacement must still invalidate
        cache.prepare(&text, &measure, &mut ck, 1, 3, 0, 80);
        assert_eq!(text.walks.get(), 6);
        assert_eq!(cache.rows[&1].text.as_deref(), Some("z\t文"));
        text.identity += 1;
        cache.prepare(&text, &measure, &mut ck, 1, 3, 0, 80);
        assert_eq!(text.walks.get(), 9);
        measure.tab_size = 8;
        cache.prepare(&text, &measure, &mut ck, 1, 3, 0, 80);
        assert_eq!(text.walks.get(), 12);
        assert_eq!(cache.rows[&1].cells.end_cells, Some(10));
        measure.ambiguous_wide = true;
        cache.prepare(&text, &measure, &mut ck, 1, 3, 0, 80);
        cache.prepare(&text, &measure, &mut ck, 1, 3, 3, 83);
        cache.prepare(&text, &measure, &mut ck, 1, 3, 3, 90);
        assert_eq!(text.walks.get(), 21);
    }

    #[test]
    fn viewport_does_not_retain_unbounded_grapheme_bytes() {
        let text = Counted::new(&format!("a{}", "\u{301}".repeat(MAX_ROW_TEXT)));
        let mut cache = ViewportCache::default();
        let mut ck = Checkpoints::default();
        cache.prepare(&text, &cfg(), &mut ck, 1, 1, 0, 80);
        assert_eq!(cache.rows[&1].cells.placed.len(), 1);
        assert!(cache.rows[&1].text.is_none());
        assert_eq!(text.reads.get(), 0);
    }
}
