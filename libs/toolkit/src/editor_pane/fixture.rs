// SPDX-License-Identifier: MIT OR Apache-2.0
//! Small neutral string backend for presentation tests.

use super::*;
use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone)]
pub struct Text { body: Arc<str>, starts: Arc<[usize]>, identity: u64 }

impl Text {
    pub fn from_text(body: &str) -> Result<Self, std::convert::Infallible> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let mut starts = vec![0];
        starts.extend(body.match_indices('\n').map(|(offset, _)| offset + 1));
        Ok(Self { body: Arc::from(body), starts: Arc::from(starts), identity: NEXT.fetch_add(1, Ordering::Relaxed) })
    }
}

impl Source for Text {
    fn identity(&self) -> u64 { self.identity }
    fn commit_calls(&self) -> u64 { 0 }
    fn len(&self) -> usize { self.body.len() }
    fn line_count(&self) -> usize { self.starts.len() }
    fn line_start(&self, line: usize) -> Option<usize> { self.starts.get(line.checked_sub(1)?).copied() }
    fn line_range(&self, line: usize) -> Option<Range<usize>> {
        let start = self.line_start(line)?;
        let end = self.line_start(line + 1).map_or(self.body.len(), |next| next - 1);
        Some(start..end)
    }
    fn content_end(&self, line: usize) -> usize {
        let Some(range) = self.line_range(line) else { return self.len(); };
        if self.body.as_bytes().get(range.end) == Some(&b'\n')
            && range.end > range.start && self.body.as_bytes()[range.end - 1] == b'\r'
        { range.end - 1 } else { range.end }
    }
    fn line_of(&self, offset: usize) -> usize { self.starts.partition_point(|start| *start <= offset).max(1) }
    fn clamp_offset(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.len());
        while !self.body.is_char_boundary(offset) { offset -= 1; }
        offset
    }
    fn read(&self, range: Range<usize>, output: &mut String) { output.push_str(&self.body[range]); }
    fn clusters(&self, cfg: &MeasureCfg, range: Range<usize>, start_cells: usize) -> Box<dyn Iterator<Item = Cluster> + '_> {
        let cfg = *cfg;
        let base = range.start;
        let mut column = start_cells;
        Box::new(self.body[range].grapheme_indices(true).take_while(|(_, text)| !text.contains('\n')).map(move |(offset, text)| {
            let is_tab = text == "\t";
            let width = if is_tab {
                let tab = usize::from(cfg.tab_size.clamp(1, 16));
                tab - column % tab
            } else if cfg.ambiguous_wide { text.width_cjk() } else { text.width() };
            column += width;
            Cluster { range: base + offset..base + offset + text.len(), cells: width as u8, is_tab, ascii: text.is_ascii() }
        }))
    }
    fn line_checkpoints(&self, cfg: &MeasureCfg, line: usize) -> Vec<(usize, usize)> {
        let Some(start) = self.line_start(line) else { return Vec::new(); };
        let mut checkpoints = vec![(start, 0)];
        let mut next = start + 4096;
        let mut column = 0;
        for cluster in self.clusters(cfg, start..self.content_end(line), 0) {
            if cluster.range.start >= next {
                checkpoints.push((cluster.range.start, column));
                next = cluster.range.start + 4096;
            }
            column += usize::from(cluster.cells);
        }
        checkpoints
    }
}
