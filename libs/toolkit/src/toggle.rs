// SPDX-License-Identifier: MIT OR Apache-2.0
//! Latching mute/solo-style button.
use iced_core::text::Paragraph as _;
use iced_core::{Element, Event, Length, Padding, Point, Rectangle, Size};
use iced_core::{
    Layout, Shell, Widget, layout, mouse, renderer, text,
    widget::{Tree, tree},
};

use crate::AudioStyle;
use crate::audio_style::quad;
use crate::theme::Catalog;
use crate::typography::TextStyle;

/// A controlled on/off button that flips on press, as mixer mute and solo
/// buttons do. `alert(true)` uses the alert colour (mute style); otherwise the
/// active colour (solo style).
pub struct Toggle<'a, Message> {
    label: String,
    on: bool,
    alert: bool,
    on_toggle: Option<Box<dyn Fn(bool) -> Message + 'a>>,
    width: f32,
    height: f32,
    style: Option<AudioStyle>,
}

impl<'a, Message> Toggle<'a, Message> {
    /// A toggle with a short `label` (for example "M" or "S").
    pub fn new(label: impl Into<String>, on: bool) -> Self {
        Self {
            label: label.into(),
            on,
            alert: false,
            on_toggle: None,
            width: 24.0,
            height: 20.0,
            style: None,
        }
    }

    /// Enables the toggle; a press publishes the new state.
    pub fn on_toggle(mut self, callback: impl Fn(bool) -> Message + 'a) -> Self {
        self.on_toggle = Some(Box::new(callback));
        self
    }

    /// Use the alert colour when on.
    pub fn alert(mut self, alert: bool) -> Self {
        self.alert = alert;
        self
    }

    /// Size in logical pixels (default 24 x 20).
    pub fn size(mut self, width: f32, height: f32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Colours; the theme's `audio_style` unless set (`theme::Catalog`).
    pub fn style(mut self, style: AudioStyle) -> Self {
        self.style = Some(style);
        self
    }

    fn colours(&self, style: AudioStyle) -> (iced_core::Color, iced_core::Color) {
        match (self.on, self.alert) {
            (false, _) => (style.track, style.muted_text),
            (true, false) => (style.active, style.active_text),
            (true, true) => (style.alert, style.alert_text),
        }
    }
}

impl<'a, Message> Toggle<'a, Message> {
    /// The same toggle with a prepared label style: its font, size and line
    /// height drive the intrinsic text bounds and the drawing. The existing
    /// [`Toggle`] stays renderer-agnostic; the wrapper is for
    /// `Renderer<Font = F>` hosts. `.size` remains a minimum on this path.
    pub fn text_style<F>(self, text: TextStyle<F>) -> StyledToggle<'a, Message, F> {
        StyledToggle {
            toggle: self,
            text,
            padding: Padding::ZERO,
        }
    }
}

/// A [`Toggle`] with a prepared label style (see [`Toggle::text_style`]).
/// It shares the existing toggle engine and forwards its builders; the
/// label is measured with the supplied paragraph font, size and line
/// height, and the final bounds (intrinsic text plus padding, at least the
/// configured size, capped by the parent limits) are used for both drawing
/// and click routing.
#[allow(missing_debug_implementations)]
pub struct StyledToggle<'a, Message, F> {
    toggle: Toggle<'a, Message>,
    text: TextStyle<F>,
    padding: Padding,
}

impl<'a, Message, F> StyledToggle<'a, Message, F> {
    /// Enables the toggle; a press publishes the new state.
    pub fn on_toggle(mut self, callback: impl Fn(bool) -> Message + 'a) -> Self {
        self.toggle = self.toggle.on_toggle(callback);
        self
    }

    /// Use the alert colour when on.
    pub fn alert(mut self, alert: bool) -> Self {
        self.toggle = self.toggle.alert(alert);
        self
    }

    /// The minimum size in logical pixels (the intrinsic label bounds win
    /// when larger).
    pub fn size(mut self, width: f32, height: f32) -> Self {
        self.toggle = self.toggle.size(width, height);
        self
    }

    /// Colours; the theme's `audio_style` unless set (`theme::Catalog`).
    pub fn style(mut self, style: AudioStyle) -> Self {
        self.toggle = self.toggle.style(style);
        self
    }

    /// Padding around the label inside the allocated bounds.
    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.padding = padding.into();
        self
    }
}

impl<Message, Theme, Renderer, F> Widget<Message, Theme, Renderer> for StyledToggle<'_, Message, F>
where
    Theme: Catalog,
    Renderer: text::Renderer<Font = F>,
    F: Copy + PartialEq,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::stateless()
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        // Measure the label at the supplied font, size and line height; the
        // configured size is only a minimum on this opt-in path.
        let paragraph = Renderer::Paragraph::with_text(text::Text {
            content: self.toggle.label.as_str(),
            bounds: Size::INFINITE,
            size: self.text.size.into(),
            line_height: self.text.line_height_or_default(),
            font: self.text.font,
            align_x: text::Alignment::Center,
            align_y: iced_core::alignment::Vertical::Center,
            shaping: text::Shaping::Advanced,
            wrapping: text::Wrapping::None,
            ellipsis: text::Ellipsis::None,
            hint_factor: renderer.hint_factor(),
        });
        let intrinsic = paragraph.min_bounds();
        let width = intrinsic.width.max(self.toggle.width) + self.padding.x();
        let height = intrinsic.height.max(self.toggle.height) + self.padding.y();
        layout::atomic(limits, width, height)
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        // The shared engine: press callback, disabled behaviour and local
        // same-batch on/off state are the legacy toggle's own.
        Widget::<Message, Theme, Renderer>::update(
            &mut self.toggle,
            tree,
            event,
            layout,
            cursor,
            renderer,
            shell,
            viewport,
        );
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let style = self.toggle.style.unwrap_or_else(|| theme.audio_style());
        let (fill, label) = self.toggle.colours(style);
        quad(renderer, bounds, fill, style.radius, Some(style.border));
        let inner = bounds.shrink(self.padding);
        renderer.fill_text(
            text::Text {
                content: self.toggle.label.clone(),
                bounds: inner.size(),
                size: self.text.size.into(),
                line_height: self.text.line_height_or_default(),
                font: self.text.font,
                align_x: text::Alignment::Center,
                align_y: iced_core::alignment::Vertical::Center,
                shaping: text::Shaping::Advanced,
                wrapping: text::Wrapping::None,
                ellipsis: text::Ellipsis::None,
                hint_factor: renderer.hint_factor(),
            },
            Point::new(inner.center_x(), inner.center_y()),
            label,
            inner,
        );
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if self.toggle.on_toggle.is_some() && cursor.is_over(layout.bounds()) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::None
        }
    }
}

impl<'a, Message: 'a, F: 'a, Theme: Catalog + 'a, Renderer: text::Renderer<Font = F> + 'a>
    From<StyledToggle<'a, Message, F>> for Element<'a, Message, Theme, Renderer>
where
    F: Copy + PartialEq,
{
    fn from(toggle: StyledToggle<'a, Message, F>) -> Self {
        Element::new(toggle)
    }
}

impl<Message, Theme: Catalog, Renderer: text::Renderer> Widget<Message, Theme, Renderer>
    for Toggle<'_, Message>
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::stateless()
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.width), Length::Fixed(self.height))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::atomic(limits, self.width, self.height)
    }

    fn update(
        &mut self,
        _tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event
            && let Some(on_toggle) = &self.on_toggle
            && !shell.is_event_captured()
            && cursor.is_over(layout.bounds())
        {
            // Flip locally too, so a second press in the same batch flips back.
            self.on = !self.on;
            shell.publish(on_toggle(self.on));
            shell.capture_event();
        }
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let style = self.style.unwrap_or_else(|| theme.audio_style());
        let (fill, label) = self.colours(style);
        quad(renderer, bounds, fill, style.radius, Some(style.border));
        renderer.fill_text(
            text::Text {
                content: self.label.clone(),
                bounds: bounds.size(),
                size: (bounds.height * 0.6).max(8.0).into(),
                line_height: text::LineHeight::default(),
                font: renderer.default_font(),
                align_x: text::Alignment::Center,
                align_y: iced_core::alignment::Vertical::Center,
                shaping: text::Shaping::Basic,
                wrapping: text::Wrapping::None,
                ellipsis: text::Ellipsis::None,
                hint_factor: None,
            },
            Point::new(bounds.center_x(), bounds.center_y()),
            label,
            bounds,
        );
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if self.on_toggle.is_some() && cursor.is_over(layout.bounds()) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::None
        }
    }
}

impl<'a, Message: 'a, Theme: Catalog + 'a, Renderer: text::Renderer + 'a> From<Toggle<'a, Message>>
    for Element<'a, Message, Theme, Renderer>
{
    fn from(toggle: Toggle<'a, Message>) -> Self {
        Element::new(toggle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_renderer::LayoutRenderer;

    fn press(toggle: &mut Toggle<'_, bool>, at: Point) -> Vec<bool> {
        let mut tree = Tree::empty();
        let node = layout::Node::new(Size::new(24.0, 20.0));
        let mut messages = iced_core::shell::Bus::new();
        let mut shell = Shell::new(
            &iced_core::window::Headless,
            iced_core::shell::Waker::noop(),
            &mut messages,
        );
        Widget::<bool, iced_core::Theme, crate::test_renderer::LayoutRenderer>::update(
            toggle,
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Layout::new(&node),
            mouse::Cursor::Available(at),
            &crate::test_renderer::LayoutRenderer::new(),
            &mut shell,
            &Rectangle::with_size(Size::INFINITE),
        );
        messages.into_iter().collect()
    }

    #[test]
    fn press_inside_flips_and_outside_is_ignored() {
        let mut toggle = Toggle::new("M", false).on_toggle(|on| on).alert(true);
        assert_eq!(
            press(&mut toggle, Point::new(30.0, 5.0)),
            Vec::<bool>::new()
        );
        assert_eq!(press(&mut toggle, Point::new(5.0, 5.0)), [true]);
        assert_eq!(press(&mut toggle, Point::new(5.0, 5.0)), [false]);
        let mut disabled: Toggle<'_, bool> = Toggle::new("S", false);
        assert!(press(&mut disabled, Point::new(5.0, 5.0)).is_empty());
    }

    #[test]
    fn colours_follow_state_and_kind() {
        let style = AudioStyle::default();
        assert_eq!(
            Toggle::<()>::new("S", true).colours(style),
            (style.active, style.active_text)
        );
        assert_eq!(
            Toggle::<()>::new("M", true).alert(true).colours(style),
            (style.alert, style.alert_text)
        );
        assert_eq!(
            Toggle::<()>::new("M", false).alert(true).colours(style),
            (style.track, style.muted_text)
        );
    }

    fn styled(label: &str) -> StyledToggle<'static, bool, iced_core::Font> {
        Toggle::new(label, false)
            .on_toggle(|on| on)
            .text_style(TextStyle {
                font: iced_core::Font::MONOSPACE,
                size: 14.0,
                line_height: Some(30.0),
            })
            .padding(Padding::from([4.0, 8.0]))
    }

    fn styled_layout(
        toggle: &mut StyledToggle<'_, bool, iced_core::Font>,
        max: Size,
    ) -> layout::Node {
        let renderer = LayoutRenderer::new();
        let mut tree = Tree::empty();
        Widget::<bool, iced_core::Theme, LayoutRenderer>::layout(
            toggle,
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, max),
        )
    }

    #[test]
    fn styled_bounds_are_intrinsic_text_plus_padding_with_the_size_as_a_minimum() {
        // The absolute line height (30) is the content height; padding (8
        // vertical) grows it, and the legacy 24x20 size is only a minimum.
        let mut toggle = styled("M");
        let node = styled_layout(&mut toggle, Size::new(200.0, 100.0));
        assert!(node.size().width >= 24.0 + 16.0);
        assert!(node.size().height >= 30.0 + 8.0);
        assert!(node.size().width.is_finite() && node.size().height.is_finite());
        // Parent limits are authoritative: a tight slot caps the widget.
        let mut toggle = styled("M");
        let node = styled_layout(&mut toggle, Size::new(40.0, 32.0));
        assert_eq!(node.size().width, 40.0);
        assert_eq!(node.size().height, 32.0);
        // A larger label grows the intrinsic width beyond the minimum.
        let mut wide = Toggle::new("STEREO LINK", false)
            .on_toggle(|on| on)
            .text_style(TextStyle {
                font: iced_core::Font::MONOSPACE,
                size: 14.0,
                line_height: None,
            })
            .padding(Padding::ZERO);
        let narrow = styled_layout(
            &mut Toggle::new("M", false).text_style(TextStyle {
                font: iced_core::Font::MONOSPACE,
                size: 14.0,
                line_height: None,
            }),
            Size::new(400.0, 100.0),
        );
        let node = styled_layout(&mut wide, Size::new(400.0, 100.0));
        assert!(node.size().width > narrow.size().width);
    }

    fn press_styled(toggle: &mut StyledToggle<'_, bool, iced_core::Font>, at: Point) -> Vec<bool> {
        let renderer = LayoutRenderer::new();
        let mut tree = Tree::empty();
        let node = styled_layout(toggle, Size::new(400.0, 100.0));
        let mut messages = iced_core::shell::Bus::new();
        let mut shell = Shell::new(
            &iced_core::window::Headless,
            iced_core::shell::Waker::noop(),
            &mut messages,
        );
        Widget::<bool, iced_core::Theme, LayoutRenderer>::update(
            toggle,
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Layout::new(&node),
            mouse::Cursor::Available(at),
            &renderer,
            &mut shell,
            &Rectangle::with_size(Size::INFINITE),
        );
        messages.into_iter().collect()
    }

    #[test]
    fn styled_click_routing_uses_the_final_bounds() {
        let mut toggle = styled("M");
        // The bounds include the padding: (0,0) is inside, the legacy
        // 24x20 corner (30, 5) may be outside the narrower legacy box but
        // inside the padded styled one.
        let node = styled_layout(&mut toggle, Size::new(400.0, 100.0));
        assert_eq!(press_styled(&mut toggle, Point::ORIGIN), [true]);
        let bounds = node.size();
        assert_eq!(
            press_styled(&mut toggle, Point::new(bounds.width + 1.0, 5.0)),
            Vec::<bool>::new(),
            "outside the final bounds publishes nothing"
        );
        // Two presses in one batch flip twice, locally.
        let renderer = LayoutRenderer::new();
        let mut tree = Tree::empty();
        let mut messages = iced_core::shell::Bus::new();
        let mut shell = Shell::new(
            &iced_core::window::Headless,
            iced_core::shell::Waker::noop(),
            &mut messages,
        );
        for _ in 0..2 {
            Widget::<bool, iced_core::Theme, LayoutRenderer>::update(
                &mut toggle,
                &mut tree,
                &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Layout::new(&node),
                mouse::Cursor::Available(Point::ORIGIN),
                &renderer,
                &mut shell,
                &Rectangle::with_size(Size::INFINITE),
            );
        }
        assert_eq!(messages.into_iter().collect::<Vec<_>>(), [false, true]);
        // Disabled: no press callback, no flip.
        let mut disabled = Toggle::new("M", false).text_style(TextStyle {
            font: iced_core::Font::MONOSPACE,
            size: 14.0,
            line_height: None,
        });
        assert!(press_styled(&mut disabled, Point::ORIGIN).is_empty());
    }
}
