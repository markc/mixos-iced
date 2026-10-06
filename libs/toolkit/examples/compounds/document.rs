// SPDX-License-Identifier: MIT OR Apache-2.0
//! Small neutral string backend for presentation tests.

use std::ops::Range;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use toolkit::editor_pane::*;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone)]
pub struct Text {
    body: Arc<str>,
    starts: Arc<[usize]>,
    identity: u64,
    revision: u64,
    pub state: ViewState,
}

impl Text {
    pub fn from_text(body: &str) -> Result<Self, std::convert::Infallible> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let mut starts = vec![0];
        starts.extend(body.match_indices('\n').map(|(offset, _)| offset + 1));
        Ok(Self {
            body: Arc::from(body),
            starts: Arc::from(starts),
            identity: NEXT.fetch_add(1, Ordering::Relaxed),
            revision: 0,
            state: ViewState::default(),
        })
    }
}

impl Source for Text {
    fn state(&self) -> ViewState {
        self.state.clone()
    }
    fn identity(&self) -> u64 {
        self.identity
    }
    fn revision(&self) -> u64 {
        self.revision
    }
    fn len(&self) -> usize {
        self.body.len()
    }
    fn line_count(&self) -> usize {
        self.starts.len()
    }
    fn line_start(&self, line: usize) -> Option<usize> {
        self.starts.get(line.checked_sub(1)?).copied()
    }
    fn line_range(&self, line: usize) -> Option<Range<usize>> {
        let start = self.line_start(line)?;
        let end = self
            .line_start(line + 1)
            .map_or(self.body.len(), |next| next - 1);
        Some(start..end)
    }
    fn content_end(&self, line: usize) -> usize {
        let Some(range) = self.line_range(line) else {
            return self.len();
        };
        if self.body.as_bytes().get(range.end) == Some(&b'\n')
            && range.end > range.start
            && self.body.as_bytes()[range.end - 1] == b'\r'
        {
            range.end - 1
        } else {
            range.end
        }
    }
    fn line_of(&self, offset: usize) -> usize {
        self.starts.partition_point(|start| *start <= offset).max(1)
    }
    fn clamp_offset(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.len());
        while !self.body.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }
    fn read(&self, range: Range<usize>, output: &mut String) {
        output.push_str(&self.body[range]);
    }
    fn clusters(
        &self,
        cfg: &MeasureCfg,
        range: Range<usize>,
        start_cells: usize,
    ) -> Box<dyn Iterator<Item = Cluster> + '_> {
        let cfg = *cfg;
        let base = range.start;
        let mut column = start_cells;
        Box::new(
            self.body[range]
                .grapheme_indices(true)
                .take_while(|(_, text)| !text.contains('\n'))
                .map(move |(offset, text)| {
                    let is_tab = text == "\t";
                    let width = if is_tab {
                        let tab = usize::from(cfg.tab_size.clamp(1, 16));
                        tab - column % tab
                    } else if cfg.ambiguous_wide {
                        text.width_cjk()
                    } else {
                        text.width()
                    };
                    column += width;
                    Cluster {
                        range: base + offset..base + offset + text.len(),
                        cells: width as u8,
                        is_tab,
                        ascii: text.is_ascii(),
                    }
                }),
        )
    }
    fn line_checkpoints(&self, cfg: &MeasureCfg, line: usize) -> Vec<(usize, usize)> {
        let Some(start) = self.line_start(line) else {
            return Vec::new();
        };
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

impl Text {
    fn replace_selection(&mut self, text: &str) {
        let a = self.clamp_offset(self.state.sel.anchor.min(self.state.sel.head));
        let b = self.clamp_offset(self.state.sel.anchor.max(self.state.sel.head));
        let mut body = self.body.to_string();
        body.replace_range(a..b, text);
        self.body = Arc::from(body);
        let mut starts = vec![0];
        starts.extend(self.body.match_indices('\n').map(|(offset, _)| offset + 1));
        self.starts = Arc::from(starts);
        self.revision += 1;
        self.state.sel = Selection {
            anchor: a + text.len(),
            head: a + text.len(),
        };
    }
    pub fn apply(&mut self, message: Message) {
        match message {
            Message::Scrolled(scroll) => self.state.scroll = scroll,
            Message::Preedit(_) => {
                self.state.composition = Some(self.state.sel.head..self.state.sel.head);
            }
            Message::ImeCommit(text) => {
                self.replace_selection(&text);
                self.state.composition = None;
            }
            Message::Command(command) => match command {
                Command::Insert(text) => self.replace_selection(&text),
                Command::Newline => self.replace_selection("\n"),
                Command::Tab => self.replace_selection("\t"),
                Command::Backspace | Command::Delete => {
                    if self.state.sel.anchor == self.state.sel.head {
                        let head = self.state.sel.head;
                        let other = if command == Command::Backspace {
                            self.body[..head]
                                .grapheme_indices(true)
                                .next_back()
                                .map_or(0, |(offset, _)| offset)
                        } else {
                            self.body[head..]
                                .graphemes(true)
                                .next()
                                .map_or(head, |text| head + text.len())
                        };
                        self.state.sel.anchor = other;
                    }
                    self.replace_selection("");
                }
                Command::SetSelection(selection) => self.state.sel = selection,
                Command::SelectAll => {
                    self.state.sel = Selection {
                        anchor: 0,
                        head: self.len(),
                    }
                }
                Command::SelectLine(at) => {
                    let line = self.line_of(at);
                    self.state.sel = Selection {
                        anchor: self.line_start(line).unwrap_or(0),
                        head: self.content_end(line),
                    };
                }
                Command::SelectWord(at) => {
                    if let Some((start, word)) = self
                        .body
                        .unicode_word_indices()
                        .find(|(start, word)| at >= *start && at <= start + word.len())
                    {
                        self.state.sel = Selection {
                            anchor: start,
                            head: start + word.len(),
                        };
                    }
                }
                Command::Move { to, extend } => {
                    let head = self.clamp_offset(self.state.sel.head);
                    let next = match to {
                        Motion::To(offset) => self.clamp_offset(offset),
                        Motion::DocStart => 0,
                        Motion::DocEnd => self.len(),
                        Motion::Home => self.line_start(self.line_of(head)).unwrap_or(0),
                        Motion::End => self.content_end(self.line_of(head)),
                        Motion::Left => self.body[..head]
                            .grapheme_indices(true)
                            .next_back()
                            .map_or(0, |(offset, _)| offset),
                        Motion::Right => self.body[head..]
                            .graphemes(true)
                            .next()
                            .map_or(head, |text| head + text.len()),
                        Motion::WordLeft => self.body[..head]
                            .unicode_word_indices()
                            .next_back()
                            .map_or(0, |(offset, _)| offset),
                        Motion::WordRight => self.body[head..]
                            .unicode_word_indices()
                            .next()
                            .map_or(self.len(), |(offset, word)| head + offset + word.len()),
                        // A small example model; full engine-specific motions remain intents.
                        _ => head,
                    };
                    self.state.sel.head = next;
                    if !extend {
                        self.state.sel.anchor = next;
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}
