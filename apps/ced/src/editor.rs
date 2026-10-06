// SPDX-License-Identifier: MIT OR Apache-2.0
//! The editor widget and its messages (ced E1 plan §4.2). Stage S freezes the
//! message type, the palette, the layout report and the widget's constructor
//! signature (a compiling stub, so E1f's `app.rs` builds before E1e lands);
//! Stage E1e replaces the widget's body WITHOUT changing the signature (a
//! change needs the lead's sign-off — E1f composes it).

mod draw;
mod ime;
mod input;
pub mod layout;
pub mod lines;
pub mod widget;

use editor_model::highlight::HlClass;
use editor_model::model::{EditCommand, Scroll};

/// What the editor widget reports to the app.
#[derive(Debug, Clone, PartialEq)]
pub enum EditorMsg {
    /// A keyboard- or mouse-derived editing / motion command.
    Command(EditCommand),
    /// The view scrolled.
    Scrolled(Scroll),
    Copy,
    Cut,
    /// Paste from the clipboard (`primary`: the middle-click selection).
    Paste {
        primary: bool,
    },
    /// IME composition changed (preedit text; empty = cancelled).
    Preedit(String),
    /// IME committed text.
    ImeCommit(String),
    /// The widget gained / lost keyboard focus.
    Focus(bool),
    /// Geometry of the frame just laid out (feeds `ced.layout`).
    Layout(LayoutReport),
}

/// Engine geometry of the last frame, logical px (plan §4.8 `ced.layout`).
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

/// Colours the widget draws with, built by `theme.rs` from mixos-design
/// tokens (D17) — never literal colours in the widget.
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub background: application::iced::Color,
    pub text: application::iced::Color,
    pub gutter_background: application::iced::Color,
    pub gutter_text: application::iced::Color,
    pub current_line: application::iced::Color,
    pub selection: application::iced::Color,
    pub caret: application::iced::Color,
    /// Origin colours: other `human:*` origins / `agent:*` origins.
    pub human_other: application::iced::Color,
    pub agent: application::iced::Color,
    pub error: application::iced::Color,
    pub warning: application::iced::Color,
    pub note: application::iced::Color,
    /// Indexed by `HlClass as usize`.
    pub highlight: [application::iced::Color; HL_CLASSES],
}

/// How the view is drawn beyond the text and colours: the Mono font at the
/// current zoom, the measurement settings and the View toggles. Built by the
/// app from the theme and `ced.conf.mix` every frame (E1f addition, additive
/// to the frozen `EditorWidget::new`).
#[derive(Debug, Clone, PartialEq)]
pub struct EditorView {
    pub font: application::iced::Font,
    /// Text size, logical px (the Mono role, config `font_px`, zoom).
    pub px: f32,
    /// Line height as a multiple of `px`.
    pub line_height: f32,
    pub measure: edit::view::MeasureCfg,
    pub whitespace: bool,
    pub line_numbers: bool,
    pub remote_carets: bool,
    /// The window has focus and no chrome field holds the keyboard.
    pub focused: bool,
    /// Find highlight-all: view byte ranges, ascending (≤ 1000; the
    /// controller's `find_matches`). Drawn as a tint under the selection.
    pub matches: Vec<std::ops::Range<usize>>,
}

impl Default for EditorView {
    fn default() -> Self {
        Self {
            font: application::iced::Font::MONOSPACE,
            px: 16.0,
            line_height: 1.3,
            measure: edit::view::MeasureCfg {
                tab_size: 4,
                ambiguous_wide: false,
            },
            whitespace: false,
            line_numbers: true,
            remote_carets: true,
            focused: true,
            matches: Vec::new(),
        }
    }
}

/// Number of [`HlClass`] variants.
pub const HL_CLASSES: usize = 17;

impl Palette {
    pub fn hl(&self, class: HlClass) -> application::iced::Color {
        self.highlight[class as usize]
    }
}

const _: () = assert!(
    HlClass::Invalid as usize + 1 == HL_CLASSES,
    "HL_CLASSES must match HlClass"
);
