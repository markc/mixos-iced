// SPDX-License-Identifier: MIT OR Apache-2.0
//! The editor widget and its messages (ced E1 plan §4.2). Stage S freezes the
//! message type, the palette, the layout report and the widget's constructor
//! signature (a compiling stub, so E1f's `app.rs` builds before E1e lands);
//! Stage E1e replaces the widget's body WITHOUT changing the signature (a
//! change needs the lead's sign-off — E1f composes it).

mod source;
pub mod widget;
pub use toolkit::editor_pane::{HL_CLASSES, LayoutReport, Palette};

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

const _: () = assert!(editor_model::highlight::HlClass::Invalid as usize + 1 == HL_CLASSES);
