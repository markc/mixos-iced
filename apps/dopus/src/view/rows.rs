// SPDX-License-Identifier: MIT OR Apache-2.0
//! Adapters from the file-management engine to shared file presentation.
use crate::icons::{self, Icons};
use crate::view::Look;
use application::cpu::Renderer;
use application::iced::advanced::text::Paragraph as _;
use application::iced::{Point, Rectangle};
use dopus_core::{FileEntry, VisibleRow};
pub use pane::{Columns, Message as RowsMsg, listing_size_width};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use toolkit::file_pane as pane;
pub type FileList<'a> = pane::FilePane<'a, application::iced::Theme, Renderer>;

pub fn presentation(look: Look) -> pane::Presentation {
    pane::Presentation {
        ui_font: look.ui_font,
        mono_font: look.mono_font,
        px: look.px,
        small_px: look.small_px,
        tokens: look.tokens,
        chrome: pane::Metrics {
            icon: look.chrome.icon,
            small: look.chrome.small,
            pad: look.chrome.pad,
            gap: look.chrome.gap,
            edge: look.chrome.edge,
        },
    }
}

fn columns_new(look: Look, rows: &[VisibleRow]) -> Columns {
    let measure = |s: &str| {
        FileList::shape_with_line_height(s, look.mono_font, look.small_px, look.small_line_height)
            .min_bounds()
            .width
    };
    Columns {
        name_min: look.chrome.icon * 2.0
            + look.chrome.small
            + FileList::shape_with_line_height("MMMM", look.ui_font, look.px, look.ui_line_height)
                .min_bounds()
                .width,
        size: listing_size_width(
            rows.iter().map(|row| size_text(&row.entry)),
            look.chrome.small,
            measure,
        ),
        modified: measure("88/88/88 88:88"),
        gap: look.chrome.gap,
        pad: look.chrome.pad,
    }
}

struct Listing<'a> {
    rows: &'a [VisibleRow],
    root: &'a Path,
    selected: Option<&'a Path>,
    expanded: &'a HashSet<PathBuf>,
}
impl pane::Source for Listing<'_> {
    fn root(&self) -> &Path {
        self.root
    }
    fn len(&self) -> usize {
        self.rows.len()
    }
    fn row(&self, index: usize) -> Option<pane::Row<'_>> {
        self.rows.get(index).map(|row| pane::Row {
            path: &row.entry.path,
            name: &row.entry.name,
            depth: row.depth,
            is_dir: row.entry.is_dir,
        })
    }
    fn selected(&self) -> Option<&Path> {
        self.selected
    }
    fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(path)
    }
    fn size_text(&self, index: usize) -> String {
        size_text(&self.rows[index].entry)
    }
    fn modified_text(&self, index: usize) -> String {
        self.rows[index]
            .entry
            .modified
            .map(dopus_core::format_modified_at)
            .unwrap_or_else(|| "—".into())
    }
}

struct Transfer {
    shared: super::drag::Shared,
    pane: dopus_core::PaneId,
}
impl pane::Transfer for Transfer {
    fn cancel_epoch(&self) -> u64 {
        super::drag::lock(&self.shared).cancel_epoch
    }
    fn active(&self) -> bool {
        super::drag::lock(&self.shared).active.is_some()
    }
    fn highlight(&self) -> Option<Rectangle> {
        let state = super::drag::lock(&self.shared);
        state
            .active
            .as_ref()
            .or(state.pending.as_ref())
            .and_then(|gesture| gesture.target.as_ref())
            .map(|target| target.highlight)
    }
    fn hover(
        &self,
        directory: Option<&Path>,
        root: &Path,
        bounds: Rectangle,
        highlight: Rectangle,
        _pointer: Point,
        busy: bool,
    ) {
        let mut state = super::drag::lock(&self.shared);
        if let Some(active) = state.active.as_mut()
            && self.pane != active.pane
        {
            let destination = directory.unwrap_or(root);
            if dopus_core::model::file_drop_actions(&active.source, destination, busy)
                .contains(dopus_core::DropAction::Ask)
            {
                active.target = Some(super::drag::Target {
                    path: destination.to_path_buf(),
                    root: root.to_path_buf(),
                    bounds,
                    highlight,
                });
            }
        }
    }
    fn start(&self, path: &Path, is_dir: bool, root: &Path, pointer: Point) -> bool {
        let mut state = super::drag::lock(&self.shared);
        if state.active.is_some() || state.pending.is_some() {
            return false;
        }
        state.active = Some(super::drag::Gesture {
            pane: self.pane,
            source_root: root.to_path_buf(),
            source: path.to_path_buf(),
            is_dir,
            pointer,
            target: None,
        });
        true
    }
}

#[allow(clippy::too_many_arguments)]
pub fn file_list<'a>(
    rows: &'a [VisibleRow],
    selected: Option<&'a Path>,
    root: &'a Path,
    expanded: &'a HashSet<PathBuf>,
    icons: &'a Icons,
    tint: &'a str,
    look: Look,
    actions: &[crate::verbs::ActionRow],
    columns: Columns,
    id: dopus_core::PaneId,
    drag: super::drag::Shared,
    busy: bool,
) -> FileList<'a> {
    FileList::new(
        Listing {
            rows,
            root,
            selected,
            expanded,
        },
        presentation(look),
        columns,
    )
    // Numeric columns deliberately retain Mono at the Small role's size.
    .line_heights(look.ui_line_height, look.small_line_height)
    .tint(tint)
    .busy(busy)
    .open_label(super::tips::action_label(
        actions,
        actions::filemgr::FILE_OPEN,
        "Open",
    ))
    .transfer(Transfer {
        shared: drag,
        pane: id,
    })
    .tooltip(move |label, size| {
        super::tips::tip(
            look,
            application::iced::widget::Space::new()
                .width(size.width)
                .height(size.height),
            label,
        )
    })
    .decoration(move |renderer, index, decoration, bounds, clip| {
        let row = &rows[index];
        let icon = match decoration {
            pane::Decoration::ChevronDown => icons::Icon::ChevronDown,
            pane::Decoration::ChevronRight => icons::Icon::ChevronRight,
            pane::Decoration::Entry => icons::file_icon(
                &row.entry.path,
                row.entry.is_dir,
                expanded.contains(&row.entry.path),
            ),
        };
        icons.draw(renderer, icon, tint, bounds, clip);
    })
}

fn size_text(entry: &FileEntry) -> String {
    if entry.is_dir {
        dopus_core::format_child_count(entry.child_count)
    } else {
        entry
            .size
            .map(dopus_core::format_size)
            .unwrap_or_else(|| "—".into())
    }
}

type ColumnMetrics = (
    application::iced::Font,
    u32,
    application::iced::Font,
    u32,
    crate::theme::Chrome,
);

/// Shared header/row measurements. Unchanged signatures do no formatting or
/// shaping. Changed listings shape the four longest Size candidates and all
/// cutoff ties (deduplicated), plus four fixed layout samples.
#[derive(Default)]
pub struct ColumnCache {
    root: Option<PathBuf>,
    signature: Option<(u64, u64, usize)>,
    metrics: Option<ColumnMetrics>,
    columns: Option<Columns>,
}
impl ColumnCache {
    pub fn refresh(&mut self, look: Look, pane: &dopus_core::PaneModel, rows: &[VisibleRow]) {
        let metrics = (
            look.ui_font,
            look.px.to_bits(),
            look.mono_font,
            look.small_px.to_bits(),
            look.chrome,
        );
        if self.metrics.as_ref() != Some(&metrics) {
            self.reset_measurements();
        }
        self.refresh_columns(
            &pane.path,
            (pane.generation, pane.listing_revision, rows.len()),
            || columns_new(look, rows),
        );
        self.metrics = Some(metrics);
    }

    fn reset_measurements(&mut self) {
        self.signature = None;
        self.root = None;
        self.columns = None;
    }

    fn refresh_columns(
        &mut self,
        root: &Path,
        signature: (u64, u64, usize),
        build: impl FnOnce() -> Columns,
    ) {
        let same_root = self.root.as_deref() == Some(root);
        if same_root && self.signature == Some(signature) {
            return;
        }
        let mut columns = build();
        if same_root && let Some(previous) = self.columns {
            columns.size = columns.size.max(previous.size);
        }
        self.columns = Some(columns);
        if !same_root {
            self.root = Some(root.to_path_buf());
        }
        self.signature = Some(signature);
    }

    pub fn get(&self, look: Look) -> Columns {
        self.columns.unwrap_or_else(|| columns_new(look, &[]))
    }
}

#[cfg(test)]
mod column_tests {
    use super::*;
    use dopus_core::format_size;

    fn columns() -> Columns {
        Columns {
            name_min: 90.0,
            size: 90.0,
            modified: 180.0,
            gap: 12.0,
            pad: 8.0,
        }
    }

    #[test]
    fn five_thousand_values_without_cutoff_ties_shape_eight_paragraphs() {
        use std::cell::Cell;
        let calls = Cell::new(0);
        let measure = |s: &str| {
            calls.set(calls.get() + 1);
            FileList::shape(s, application::iced::Font::MONOSPACE, 11.0)
                .min_bounds()
                .width
        };
        let mut cache = ColumnCache::default();
        let root = Path::new("/listing");
        let signature = (1, 1, 5000);
        cache.refresh_columns(root, signature, || Columns {
            size: listing_size_width(
                (0..4996).map(|_| "1 B").chain([
                    "1.0 KiB",
                    "99.9 KiB",
                    "1023.9 KiB",
                    "999999 items",
                ]),
                4.0,
                measure,
            ),
            // The production constructor also shapes these two samples.
            name_min: measure("MMMM"),
            modified: measure("88/88/88 88:88"),
            ..columns()
        });
        assert_eq!(calls.get(), 4 + 4);
        for _ in 0..100 {
            cache.refresh_columns(root, signature, || {
                panic!("unchanged listing was formatted")
            });
        }
        assert_eq!(calls.get(), 4 + 4);
    }

    #[test]
    fn proportional_widths_keep_every_cutoff_tie() {
        let values = ["11111111", "22222222", "33333333", "44444444", "88888888"];
        let width = listing_size_width(values.into_iter(), 0.0, |s: &str| match s {
            "99.9 MiB" => 5.0,
            "999999 items" => 20.0,
            "88888888" => 15.0,
            _ => 8.0,
        });
        assert_eq!(width, 15.0, "the fifth equal-length candidate is widest");
    }

    #[test]
    fn size_only_grows_until_the_pane_root_changes() {
        let mut cache = ColumnCache::default();
        let root = Path::new("/listing");
        let with_size = |size| Columns { size, ..columns() };
        cache.refresh_columns(root, (1, 1, 10), || with_size(80.0));
        // A count reply grows Size even though the row count is unchanged.
        cache.refresh_columns(root, (1, 2, 10), || with_size(110.0));
        assert_eq!(cache.columns.unwrap().size, 110.0);
        // Collapse, expand and a new generation (refresh/hidden toggle)
        // must not shrink it while the root identity stays the same.
        for signature in [(1, 2, 5), (1, 2, 10), (2, 3, 8)] {
            cache.refresh_columns(root, signature, || with_size(70.0));
            assert_eq!(cache.columns.unwrap().size, 110.0);
        }
        cache.refresh_columns(Path::new("/other"), (3, 4, 8), || with_size(70.0));
        assert_eq!(cache.columns.unwrap().size, 70.0);
        // A typography change resets hysteresis even with identical root/signature.
        cache.reset_measurements();
        cache.refresh_columns(Path::new("/other"), (3, 4, 8), || with_size(40.0));
        assert_eq!(cache.columns.unwrap().size, 40.0);
    }

    #[test]
    fn small_file_listings_leave_most_width_for_names() {
        let measure = |s: &str| {
            FileList::shape(s, application::iced::Font::MONOSPACE, 11.0)
                .min_bounds()
                .width
        };
        let small = [format_size(12), format_size(512), format_size(2048)];
        let width = listing_size_width(small.iter().map(String::as_str), 4.0, measure);
        assert_eq!(width, measure("99.9 MiB") + 8.0);
        let crowded = listing_size_width(["999999 items"].into_iter(), 4.0, measure);
        assert!(width < crowded);
        let columns = Columns {
            size: width,
            modified: measure("88/88/88 88:88"),
            ..columns()
        };
        let cells = columns.cells(500.0);
        assert!(cells[0].1 > 250.0);
        assert_eq!(cells[1].1, width);
        assert!(cells[2].1 > 0.0);
        assert_eq!(
            listing_size_width(["999999999999 items"].into_iter(), 4.0, measure),
            crowded
        );
    }

    #[test]
    fn a_shaped_name_that_fits_the_actual_name_cell_is_not_elided() {
        let name = "ardour-session-Walthius_2009_Theme";
        let measure = |s: &str| {
            FileList::shape(s, application::iced::Font::DEFAULT, 14.0)
                .min_bounds()
                .width
        };
        let columns = columns();
        let (icon, padding, depth) = (16.0, 4.0, 2);
        assert!(measure(name) > 0.0);
        let decoration = (depth as f32 + 2.0) * icon + padding;
        let width = columns.pad * 2.0
            + columns.size
            + columns.modified
            + columns.gap * 2.0
            + decoration
            + measure(name)
            + 1.0;
        let (x, budget) = columns.name_text(width, depth, icon, padding);
        let cell = columns.cells(width)[0];
        assert!((x + budget - (cell.0 + cell.1)).abs() < 0.01);
        assert!(budget >= measure(name));
        assert_eq!(super::super::elide::middle(name, budget, measure), name);
    }

    #[test]
    fn header_and_rows_share_reserved_right_edges() {
        let columns = columns();
        for width in [400.0, 617.0, 920.0] {
            let [name, size, modified] = columns.cells(width);
            assert_eq!(name.0, columns.pad);
            assert_eq!(name.0 + name.1 + columns.gap, size.0);
            assert_eq!(size.1, columns.size);
            assert_eq!(size.0 + size.1 + columns.gap, modified.0);
            assert_eq!(modified.1, columns.modified);
            assert_eq!(modified.0 + modified.1 + columns.pad, width);
        }
    }

    #[test]
    fn capped_sidebars_preserve_names_without_eliding_numeric_size_at_190_pixels() {
        let columns = columns();
        let [name, size, modified] = columns.cells(190.0);
        assert!(name.1 >= columns.name_min);
        assert_eq!(size.1, 0.0);
        assert_eq!(modified.1, 0.0);
        assert_eq!(name.0 + name.1 + columns.pad, 190.0);
        // Deep tree rows keep the text budget; draw and hit-testing share this.
        assert_eq!(
            columns.indentation(190.0, 20, 16.0),
            name.1 - columns.name_min
        );
    }

    #[test]
    fn responsive_columns_hide_modified_before_size_and_never_overflow() {
        let columns = columns();
        assert_eq!(columns.cells(300.0)[1].1, columns.size);
        assert_eq!(columns.cells(300.0)[2].1, 0.0);
        assert_eq!(columns.cells(150.0)[1].1, 0.0);
        for width in 0..1000 {
            let width = width as f32;
            let cells = columns.cells(width);
            for (x, w) in cells {
                assert!(x >= 0.0 && w >= 0.0 && x + w <= width);
            }
            assert!(cells[0].1 >= columns.name_min.min((width - 2.0 * columns.pad).max(0.0)));
        }
    }
}
