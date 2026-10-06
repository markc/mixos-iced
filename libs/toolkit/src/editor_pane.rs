// SPDX-License-Identifier: MIT OR Apache-2.0
//! A reusable cell-grid editor over a borrowed document provider. Editing
//! commands are intents: the caller's engine applies them and echoes selection,
//! scroll and composition state. Only visible rows are measured and drawn.
//!
//! Offsets are UTF-8 byte offsets, lines are one-based and cells zero-based.
//! Providers must return valid grapheme boundaries and a new revision whenever
//! content changes. `identity` distinguishes documents with equal revisions.

mod draw;
mod ime;
mod input;
pub mod layout;
pub mod lines;
mod widget;

use iced_core::{Color, Font};
use std::ops::Range;
pub use widget::EditorPane;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scroll {
    pub first_line: usize,
    pub x_cells: usize,
}
impl Default for Scroll {
    fn default() -> Self {
        Self {
            first_line: 1,
            x_cells: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ViewState {
    pub sel: Selection,
    pub scroll: Scroll,
    pub overwrite: bool,
    pub composition: Option<Range<usize>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    WordLeft,
    WordRight,
    Up,
    Down,
    Home,
    End,
    PageUp(usize),
    PageDown(usize),
    DocStart,
    DocEnd,
    To(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Insert(String),
    Newline,
    Backspace,
    Delete,
    DeleteWordLeft,
    DeleteWordRight,
    Tab,
    Outdent,
    DuplicateLine,
    DeleteLine,
    MoveLineUp,
    MoveLineDown,
    ToggleComment,
    Move { to: Motion, extend: bool },
    SelectAll,
    SelectWord(usize),
    SelectLine(usize),
    SetSelection(Selection),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Command(Command),
    Scrolled(Scroll),
    Copy,
    Cut,
    Paste { primary: bool },
    /// Publish the bounded selected text; the host owns primary-selection support.
    PrimarySelection(String),
    Preedit(String),
    ImeCommit(String),
    Focus(bool),
    Layout(LayoutReport),
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LayoutReport {
    pub editor: [f32; 4],
    pub gutter_w: f32,
    pub line_height: f32,
    pub cell_w: f32,
    pub first_line: usize,
    pub visible_rows: usize,
    pub caret: [f32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeasureCfg {
    pub tab_size: u8,
    pub ambiguous_wide: bool,
}
impl Default for MeasureCfg {
    fn default() -> Self {
        Self {
            tab_size: 4,
            ambiguous_wide: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cluster {
    pub range: Range<usize>,
    pub cells: u8,
    pub is_tab: bool,
    pub ascii: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Plain,
    Comment,
    Keyword,
    String,
    Number,
    Constant,
    Type,
    Function,
    Variable,
    Operator,
    Punctuation,
    Meta,
    Inserted,
    Deleted,
    Heading,
    Link,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Note,
}
pub struct Diagnostic {
    pub range: Range<usize>,
    pub severity: Severity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginKind {
    Human,
    Agent,
    Tool,
}
#[derive(Debug, Clone, Copy)]
pub struct Origin<'a> {
    pub kind: OriginKind,
    pub label: &'a str,
}
impl std::fmt::Display for Origin<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.kind {
            OriginKind::Human => "human",
            OriginKind::Agent => "agent",
            OriginKind::Tool => "tool",
        };
        write!(f, "{kind}:{}", self.label)
    }
}
pub type Remote<'a> = (Origin<'a>, Box<dyn Iterator<Item = Selection> + 'a>);
pub type Marker<'a> = (Range<usize>, Origin<'a>, u64);

/// Shared budget for bounded syntax work in one draw. Providers decrement it
/// as they seek; a row beyond the budget may return no spans and render plain.
pub struct SliceBudget {
    pub max_lines: usize,
}
impl Default for SliceBudget {
    fn default() -> Self {
        Self { max_lines: 2000 }
    }
}

/// Presentation access over the caller's document and engine. Line access and
/// cluster iteration must avoid flattening the whole document. Checkpoints are
/// grapheme boundaries approximately every 4 KiB, with absolute cell positions.
pub trait Source {
    fn identity(&self) -> u64;
    /// Changes whenever visible document content changes, including undo.
    fn revision(&self) -> u64;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn line_count(&self) -> usize;
    fn line_start(&self, line: usize) -> Option<usize>;
    fn line_range(&self, line: usize) -> Option<Range<usize>>;
    fn content_end(&self, line: usize) -> usize;
    fn line_of(&self, offset: usize) -> usize;
    fn clamp_offset(&self, offset: usize) -> usize;
    fn read(&self, range: Range<usize>, output: &mut String);
    fn clusters(
        &self,
        cfg: &MeasureCfg,
        range: Range<usize>,
        cells: usize,
    ) -> Box<dyn Iterator<Item = Cluster> + '_>;
    fn line_checkpoints(&self, cfg: &MeasureCfg, line: usize) -> Vec<(usize, usize)>;
    fn state(&self) -> ViewState {
        ViewState::default()
    }
    fn remote(&self) -> Box<dyn Iterator<Item = Remote<'_>> + '_> {
        Box::new(std::iter::empty())
    }
    fn markers(&self) -> Box<dyn Iterator<Item = Marker<'_>> + '_> {
        Box::new(std::iter::empty())
    }
    fn diagnostics(&self) -> Box<dyn Iterator<Item = Diagnostic> + '_> {
        Box::new(std::iter::empty())
    }
    fn highlight_spans(
        &self,
        _line: usize,
        _budget: &mut SliceBudget,
    ) -> Vec<(Range<usize>, Class)> {
        Vec::new()
    }
}

pub(crate) fn line_of(text: &dyn Source, offset: usize) -> usize {
    text.line_of(offset)
}
pub(crate) fn clamp_offset(text: &dyn Source, offset: usize) -> usize {
    text.clamp_offset(offset)
}
pub(crate) fn content_end(text: &dyn Source, line: usize) -> usize {
    text.content_end(line)
}

pub const HL_CLASSES: usize = 17;
const _: () = assert!(Class::Invalid as usize + 1 == HL_CLASSES);

#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub background: Color,
    pub text: Color,
    pub gutter_background: Color,
    pub gutter_text: Color,
    pub current_line: Color,
    pub selection: Color,
    pub caret: Color,
    pub human_other: Color,
    pub agent: Color,
    pub error: Color,
    pub warning: Color,
    pub note: Color,
    pub highlight: [Color; HL_CLASSES],
}
impl Palette {
    pub fn hl(&self, class: Class) -> Color {
        self.highlight[class as usize]
    }
}

impl From<crate::Tokens> for Palette {
    fn from(tokens: crate::Tokens) -> Self {
        let colours = tokens.palette;
        Self {
            background: colours.surface,
            text: colours.text,
            gutter_background: colours.muted_surface,
            gutter_text: colours.muted_text,
            current_line: colours.muted_surface,
            selection: colours.selection,
            caret: colours.text,
            human_other: colours.ring,
            agent: colours.ring,
            error: colours.destructive,
            warning: colours.primary,
            note: colours.ring,
            highlight: [colours.text; HL_CLASSES],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub font: Font,
    pub px: f32,
    pub line_height: f32,
    pub measure: MeasureCfg,
    pub whitespace: bool,
    pub line_numbers: bool,
    pub remote_carets: bool,
    pub focused: bool,
    pub matches: Vec<Range<usize>>,
}
impl Default for View {
    fn default() -> Self {
        Self {
            font: Font::MONOSPACE,
            px: 16.0,
            line_height: 1.3,
            measure: MeasureCfg::default(),
            whitespace: false,
            line_numbers: true,
            remote_carets: true,
            focused: true,
            matches: Vec::new(),
        }
    }
}

#[cfg(test)]
mod fixture;
