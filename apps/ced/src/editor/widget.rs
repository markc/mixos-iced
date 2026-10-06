// SPDX-License-Identifier: MIT OR Apache-2.0
//! App adapter for the reusable editor surface. No drawing or input mechanics
//! live here; the controller still consumes its established engine commands.

use super::{EditorMsg, EditorView, Palette, source};
use application::iced::Element;
use editor_model::{diag::Diagnostics, highlight::Highlight, mirror::Mirror, model::EditorModel};

pub struct EditorWidget;
impl EditorWidget {
    pub fn with<'a>(
        document: u64,
        mirror: &'a Mirror,
        model: &'a EditorModel,
        highlight: &'a Highlight,
        palette: &'a Palette,
        diagnostics: &'a Diagnostics,
        view: &EditorView,
    ) -> Element<'a, EditorMsg> {
        let provider = source::Source::new(document, mirror, model, highlight, diagnostics);
        let view = toolkit::editor_pane::View {
            font: view.font,
            px: view.px,
            line_height: view.line_height,
            measure: toolkit::editor_pane::MeasureCfg {
                tab_size: view.measure.tab_size,
                ambiguous_wide: view.measure.ambiguous_wide,
            },
            whitespace: view.whitespace,
            line_numbers: view.line_numbers,
            remote_carets: view.remote_carets,
            focused: view.focused,
            matches: view.matches.clone(),
        };
        let pane: Element<'a, toolkit::editor_pane::Message> =
            toolkit::EditorPane::new(provider, palette, &view).into();
        pane.map(source::event)
    }
}
