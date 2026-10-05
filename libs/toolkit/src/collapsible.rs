// SPDX-License-Identifier: MIT OR Apache-2.0
//! A collapsible section: [`Collapsible`], a header button with a ▸/▾
//! glyph that shows and hides a body. The expanded state is the
//! caller's, like every toolkit widget; toggling is a click, Enter or
//! Space on the header (the header is a stock button, so it is focusable
//! and keyboard-operable for free).

use iced_core::{Element, Length, Padding};
use iced_widget::{button, column, row, text};

/// The default title text size.
const DEFAULT_TEXT_SIZE: f32 = 14.0;

/// A collapsible section: a header that shows and hides a body.
///
/// ```no_run
/// # use toolkit::collapsible::Collapsible;
/// #[derive(Clone)]
/// enum Message { Toggled(bool) }
///
/// fn view<'a, Theme, Renderer>(
///     expanded: bool,
/// ) -> iced_core::Element<'a, Message, Theme, Renderer>
/// where
///     Theme: iced_widget::button::Catalog + iced_core::widget::text::Catalog + 'a,
///     Renderer: iced_core::text::Renderer + 'static,
/// {
///     Collapsible::new("Section", expanded, Message::Toggled)
///         .body(iced_widget::text("The section body."))
///         .into()
/// }
/// ```
pub struct Collapsible<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::text::Renderer,
{
    title: String,
    expanded: bool,
    on_toggle: Box<dyn Fn(bool) -> Message + 'a>,
    body: Option<Element<'a, Message, Theme, Renderer>>,
    width: Length,
    padding: Padding,
    text_size: f32,
    style: Option<fn(&Theme, iced_widget::button::Status) -> iced_widget::button::Style>,
}

impl<'a, Message, Theme, Renderer> Collapsible<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::text::Renderer,
{
    /// Creates a new [`Collapsible`] with a title, the expanded state and
    /// the message produced on toggle (carrying the new state).
    pub fn new(
        title: impl Into<String>,
        expanded: bool,
        on_toggle: impl Fn(bool) -> Message + 'a,
    ) -> Self {
        Self {
            title: title.into(),
            expanded,
            on_toggle: Box::new(on_toggle),
            body: None,
            width: Length::Fill,
            padding: Padding::new(8.0),
            text_size: DEFAULT_TEXT_SIZE,
            style: None,
        }
    }

    /// Sets the body shown while expanded.
    pub fn body(mut self, body: impl Into<Element<'a, Message, Theme, Renderer>>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Sets the width.
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Sets the padding inside the header and around the body.
    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.padding = padding.into();
        self
    }

    /// Sets the title text size.
    pub fn text_size(mut self, size: f32) -> Self {
        self.text_size = size;
        self
    }

    /// Sets the header button style function (a plain
    /// [`button::secondary`](iced_widget::theme::button) look otherwise,
    /// whatever the host theme's default class is).
    #[must_use]
    pub fn header_style(
        mut self,
        style: fn(&Theme, iced_widget::button::Status) -> iced_widget::button::Style,
    ) -> Self {
        self.style = Some(style);
        self
    }
}

impl<'a, Message, Theme, Renderer> From<Collapsible<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::text::Renderer + 'a,
    Theme: iced_widget::button::Catalog + iced_core::widget::text::Catalog + 'a,
    <Theme as iced_widget::button::Catalog>::Class<'a>:
        From<iced_widget::button::StyleFn<'a, Theme>>,
    Message: 'a + Clone,
{
    fn from(collapsible: Collapsible<'a, Message, Theme, Renderer>) -> Self {
        let glyph = if collapsible.expanded { "▾" } else { "▸" };

        let header_row = row![
            text(glyph).size(collapsible.text_size),
            text(collapsible.title).size(collapsible.text_size)
        ]
        .spacing(6)
        .align_y(iced_core::alignment::Vertical::Center);

        let mut header = button(header_row)
            .padding(collapsible.padding)
            .width(collapsible.width)
            .on_press((collapsible.on_toggle)(!collapsible.expanded));

        if let Some(style) = collapsible.style {
            header = header.style(style);
        }

        let section = column![header.padding(collapsible.padding)]
            .width(collapsible.width)
            .spacing(0);

        match collapsible.body.filter(|_| collapsible.expanded) {
            Some(body) => column![section, body]
                .width(collapsible.width)
                .spacing(0)
                .into(),
            None => section.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_renderer::LayoutRenderer;
    use iced_core::layout::{Layout, Limits};
    use iced_core::Size;

    #[test]
    fn the_body_takes_height_only_when_expanded() {
        let renderer = LayoutRenderer::new();
        let limits = Limits::new(Size::ZERO, Size::new(300.0, f32::INFINITY));

        let collapsed: Element<'_, bool, iced_core::Theme, LayoutRenderer> = Collapsible::new(
            "Section",
            false,
            |expanded| expanded,
        )
        .body(iced_widget::text("Body text that occupies a line"))
        .into();
        let expanded: Element<'_, bool, iced_core::Theme, LayoutRenderer> = Collapsible::new(
            "Section",
            true,
            |expanded| expanded,
        )
        .body(iced_widget::text("Body text that occupies a line"))
        .into();

        let mut collapsed = collapsed;
        let mut tree = iced_core::widget::Tree::new(&collapsed);
        collapsed.as_widget_mut().diff(&mut tree);
        let collapsed_node = collapsed.as_widget_mut().layout(&mut tree, &renderer, &limits);
        let mut expanded = expanded;
        let mut tree = iced_core::widget::Tree::new(&expanded);
        expanded.as_widget_mut().diff(&mut tree);
        let expanded_node = expanded.as_widget_mut().layout(&mut tree, &renderer, &limits);

        assert!(
            expanded_node.bounds().height > collapsed_node.bounds().height,
            "the expanded section is taller: {} vs {}",
            expanded_node.bounds().height,
            collapsed_node.bounds().height
        );
        let _ = Layout::new(&expanded_node);
    }
}
