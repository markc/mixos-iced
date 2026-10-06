// SPDX-License-Identifier: MIT OR Apache-2.0
//! Intrinsic content centred inside its allocated bounds, without Fill spacers.
//! Use [`centered`] for a group and [`CenteredButton`] for a pressable target.

use iced_core::{
    Element, Event, Layout, Length, Padding, Rectangle, Renderer, Shell, Size, Vector,
    Widget, alignment, layout, mouse, overlay, renderer, widget,
};
use iced_widget::{Button, button, container};

/// Centre intrinsic content after the container's size constraints are applied.
///
/// The returned layout-only wrapper accepts width, height and padding
/// builders. In particular, a minimum-sized Shrink box centres its content
/// without compressible Fill spacers. Individual axis alignment can be changed
/// with `align_x` and `align_y`.
pub fn centered<'a, Message: 'a, Theme: 'a, R: Renderer + 'a>(
    content: impl Into<Element<'a, Message, Theme, R>>,
) -> Centered<'a, Message, Theme, R> {
    Centered::new(content)
}

/// Centred layout that adds no background, text colour or theme catalog.
/// The parent's drawing style, child state, events and overlays pass through.
#[must_use]
pub struct Centered<'a, Message, Theme = crate::Theme, R = iced_widget::Renderer> {
    content: Element<'a, Message, Theme, R>,
    width: Length,
    height: Length,
    padding: Padding,
    horizontal: alignment::Horizontal,
    vertical: alignment::Vertical,
}

impl<'a, Message, Theme, R> Centered<'a, Message, Theme, R> {
    pub fn new(content: impl Into<Element<'a, Message, Theme, R>>) -> Self {
        Self {
            content: content.into(), width: Length::Fit, height: Length::Fit,
            padding: Padding::ZERO,
            horizontal: alignment::Horizontal::Center,
            vertical: alignment::Vertical::Center,
        }
    }
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }
    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.padding = padding.into();
        self
    }
    pub fn align_x(mut self, alignment: alignment::Horizontal) -> Self {
        self.horizontal = alignment;
        self
    }
    pub fn align_y(mut self, alignment: alignment::Vertical) -> Self {
        self.vertical = alignment;
        self
    }
}

impl<Message, Theme, R: Renderer> Widget<Message, Theme, R> for Centered<'_, Message, Theme, R> {
    fn tag(&self) -> widget::tree::Tag { self.content.as_widget().tag() }
    fn state(&self) -> widget::tree::State { self.content.as_widget().state() }
    fn diff(&mut self, tree: &mut widget::Tree) {
        self.content.as_widget_mut().diff(tree);
        let size = self.content.as_widget().size();
        self.width = self.width.stack(size.width);
        self.height = self.height.stack(size.height);
    }
    fn size(&self) -> Size<Length> { Size::new(self.width, self.height) }
    fn layout(&mut self, tree: &mut widget::Tree, renderer: &R, limits: &layout::Limits) -> layout::Node {
        container::layout(limits, self.width, self.height, self.padding, self.horizontal, self.vertical,
            |limits| self.content.as_widget_mut().layout(tree, renderer, limits))
    }
    fn operate(&mut self, tree: &mut widget::Tree, layout: Layout<'_>, renderer: &R, operation: &mut dyn widget::Operation) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            self.content.as_widget_mut().operate(tree, layout.child(0), renderer, operation);
        });
    }
    fn update(&mut self, tree: &mut widget::Tree, event: &Event, layout: Layout<'_>, cursor: mouse::Cursor,
        renderer: &R, shell: &mut Shell<'_, Message>, viewport: &Rectangle) {
        self.content.as_widget_mut().update(tree, event, layout.child(0), cursor, renderer, shell, viewport);
    }
    fn draw(&self, tree: &widget::Tree, renderer: &mut R, theme: &Theme, style: &renderer::Style,
        layout: Layout<'_>, cursor: mouse::Cursor, viewport: &Rectangle) {
        self.content.as_widget().draw(tree, renderer, theme, style, layout.child(0), cursor, viewport);
    }
    fn mouse_interaction(&self, tree: &widget::Tree, layout: Layout<'_>, cursor: mouse::Cursor,
        viewport: &Rectangle, renderer: &R) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(tree, layout.child(0), cursor, viewport, renderer)
    }
    fn overlay<'b>(&'b mut self, tree: &'b mut widget::Tree, layout: Layout<'b>, renderer: &R,
        viewport: &Rectangle, translation: Vector) -> Option<overlay::Element<'b, Message, Theme, R>> {
        self.content.as_widget_mut().overlay(tree, layout.child(0), renderer, viewport, translation)
    }
}

impl<'a, Message: 'a, Theme: 'a, R: Renderer + 'a> From<Centered<'a, Message, Theme, R>>
    for Element<'a, Message, Theme, R> {
    fn from(centered: Centered<'a, Message, Theme, R>) -> Self { Element::new(centered) }
}

/// An iced button whose intrinsic label or group is centred inside its target.
///
/// Sizes and padding belong to the caller; colours come from the host's button
/// catalog. Defaults fit the content with no extra padding. The underlying
/// iced button retains its normal press, disabled, hover and clipping behaviour.
///
/// ```
/// use toolkit::{CenteredButton, Theme, widget::text};
/// use toolkit::core::{Element, Length};
///
/// let button: Element<'_, u8, Theme, toolkit::widget::Renderer> =
///     CenteredButton::new(text("1"))
///         .width(Length::Shrink.min(30))
///         .height(30)
///         .padding(8)
///         .on_press(1)
///         .into();
/// ```
#[must_use]
pub struct CenteredButton<'a, Message, Theme = crate::Theme, R = iced_widget::Renderer>
where
    Theme: button::Catalog,
    R: Renderer,
{
    content: Element<'a, Message, Theme, R>,
    width: Length,
    height: Length,
    padding: Padding,
    horizontal: alignment::Horizontal,
    vertical: alignment::Vertical,
    on_press: Option<Message>,
    clip: bool,
    class: <Theme as button::Catalog>::Class<'a>,
}

impl<'a, Message: 'a, Theme: 'a, R: Renderer + 'a> CenteredButton<'a, Message, Theme, R>
where
    Theme: button::Catalog,
{
    pub fn new(content: impl Into<Element<'a, Message, Theme, R>>) -> Self {
        Self {
            content: content.into(),
            width: Length::Fit,
            height: Length::Fit,
            padding: Padding::ZERO,
            horizontal: alignment::Horizontal::Center,
            vertical: alignment::Vertical::Center,
            on_press: None,
            clip: false,
            class: <Theme as button::Catalog>::default(),
        }
    }

    /// Set the complete target width, including padding.
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Set the complete target height, including padding.
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// Set both target dimensions to the same caller-supplied length.
    pub fn square(self, side: impl Into<Length>) -> Self {
        let side = side.into();
        self.width(side).height(side)
    }

    /// Centre within the padded area. Symmetric padding preserves the target's centre.
    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.padding = padding.into();
        self
    }

    /// Override horizontal alignment, for groups that centre only vertically.
    pub fn align_x(mut self, alignment: alignment::Horizontal) -> Self {
        self.horizontal = alignment;
        self
    }

    /// Override vertical alignment, for groups that centre only horizontally.
    pub fn align_y(mut self, alignment: alignment::Vertical) -> Self {
        self.vertical = alignment;
        self
    }

    pub fn on_press(self, message: Message) -> Self {
        self.on_press_maybe(Some(message))
    }

    /// A missing message disables the button.
    pub fn on_press_maybe(mut self, message: Option<Message>) -> Self {
        self.on_press = message;
        self
    }

    pub fn clip(mut self, clip: bool) -> Self {
        self.clip = clip;
        self
    }

    pub fn style(mut self, style: impl Fn(&Theme, button::Status) -> button::Style + 'a) -> Self
    where
        <Theme as button::Catalog>::Class<'a>: From<button::StyleFn<'a, Theme>>,
    {
        self.class = (Box::new(style) as button::StyleFn<'a, Theme>).into();
        self
    }

    pub fn class(mut self, class: impl Into<<Theme as button::Catalog>::Class<'a>>) -> Self {
        self.class = class.into();
        self
    }

    /// Finish size/alignment configuration and obtain the ordinary iced button.
    /// Configure layout on this builder before calling `build`; the content
    /// content and hit target must receive the same dimensions.
    pub fn build(self) -> Button<'a, Message, Theme, R> {
        let content = centered(self.content)
            .width(self.width)
            .height(self.height)
            .padding(self.padding)
            .align_x(self.horizontal)
            .align_y(self.vertical);
        Button::new(content)
            .width(self.width)
            .height(self.height)
            .padding(0)
            .class(self.class)
            .clip(self.clip)
            .on_press_maybe(self.on_press)
    }
}

impl<'a, Message: Clone + 'a, Theme: 'a, R: Renderer + 'a>
    From<CenteredButton<'a, Message, Theme, R>> for Element<'a, Message, Theme, R>
where
    Theme: button::Catalog,
{
    fn from(button: CenteredButton<'a, Message, Theme, R>) -> Self {
        button.build().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_renderer::LayoutRenderer;
    use iced_widget::{column, row, text};

    type View<'a> = Element<'a, u8, iced_core::Theme, LayoutRenderer>;

    fn draw(mut view: View<'_>, size: Size) -> (Rectangle, Vec<Rectangle>) {
        let mut renderer = LayoutRenderer::new();
        let mut tree = widget::Tree::new(&view);
        view.as_widget_mut().diff(&mut tree);
        let node = view.as_widget_mut().layout(
            &mut tree, &renderer, &layout::Limits::new(Size::ZERO, size),
        );
        view.as_widget().draw(
            &tree, &mut renderer, &iced_core::Theme::Dark,
            &renderer::Style::default(), Layout::new(&node), mouse::Cursor::Unavailable,
            &Rectangle::with_size(size),
        );
        (node.bounds(), renderer.paragraphs)
    }

    fn near(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < 0.1, "{actual} != {expected}");
    }

    #[test]
    fn digits_centre_in_minimum_targets_and_respect_narrow_parent_limits() {
        for width in [100.0_f32, 25.0] {
            for digit in ["1", "2", "3", "4"] {
                let button = CenteredButton::new(text(digit).size(10))
                    .width(Length::Shrink.min(30))
                    .height(30)
                    .padding(8)
                    .on_press(1);
                let (target, glyphs) = draw(button.into(), Size::new(width, 52.0));
                assert_eq!(glyphs.len(), 1);
                near(target.width, width.min(30.0));
                near(target.height, 30.0);
                near(glyphs[0].center().x, target.center().x);
                near(glyphs[0].center().y, target.center().y);
                assert!(glyphs[0].x >= target.x && glyphs[0].x + glyphs[0].width <= target.x + target.width);
            }
        }
    }

    #[test]
    fn fixed_and_fill_targets_centre_a_group_without_changing_its_gap() {
        for (width, height, expected) in [
            (Length::Fixed(90.0), Length::Fixed(40.0), Size::new(90.0, 40.0)),
            (Length::Fill, Length::Fill, Size::new(120.0, 52.0)),
        ] {
            let content = row![text("1").size(10), text("2").size(10)].spacing(6);
            let (target, glyphs) = draw(
                CenteredButton::new(content).width(width).height(height).padding(4).on_press(1).into(),
                Size::new(120.0, 52.0),
            );
            assert_eq!(target.size(), expected);
            assert_eq!(glyphs.len(), 2);
            near((glyphs[0].x + glyphs[1].x + glyphs[1].width) / 2.0, target.center().x);
            near(glyphs[0].center().y, target.center().y);
            near(glyphs[1].x - glyphs[0].x - glyphs[0].width, 6.0);
        }
    }

    #[test]
    fn minimum_sized_vertical_groups_share_the_same_alignment() {
        let content = column![text("1").size(10), text("2").size(10)].spacing(6);
        let (target, glyphs) = draw(
            centered(content).width(60).height(Length::Shrink.min(60).max(80)).padding(4).into(),
            Size::new(120.0, 120.0),
        );
        assert_eq!(target.size(), Size::new(60.0, 60.0));
        assert_eq!(glyphs.len(), 2);
        near((glyphs[0].y + glyphs[1].y + glyphs[1].height) / 2.0, target.center().y);
        near(glyphs[1].y - glyphs[0].y - glyphs[0].height, 6.0);
    }

    #[test]
    fn fit_grows_for_long_labels_and_padding_is_counted_once() {
        let (target, glyphs) = draw(
            CenteredButton::new(text("Long label").size(12))
                .width(Length::Fit.min(30)).height(40).padding(8).on_press(1).into(),
            Size::new(200.0, 52.0),
        );
        assert_eq!(glyphs.len(), 1);
        assert!(target.width > 30.0);
        near(target.width, glyphs[0].width + 16.0);
        near(glyphs[0].center().x, target.center().x);
        near(glyphs[0].center().y, target.center().y);
    }

    // No container catalog and no closure-based style classes: a generic host
    // needs only its button/text contracts, and its button owns all painting.
    struct ButtonTheme;
    impl button::Catalog for ButtonTheme {
        type Class<'a> = ();
        fn default<'a>() {}
        fn style(&self, _: &(), status: button::Status) -> button::Style {
            let palette = crate::Tokens::dark().palette;
            button::Style {
                text_color: if status == button::Status::Disabled { palette.muted_text } else { palette.primary_text },
                background: Some(palette.primary.into()),
                ..button::Style::default()
            }
        }
    }
    impl iced_core::widget::text::Catalog for ButtonTheme {
        type Class<'a> = ();
        fn default<'a>() {}
        fn style(&self, _: &()) -> iced_core::widget::text::Style {
            iced_core::widget::text::Style::default()
        }
    }

    #[test]
    fn a_button_only_theme_keeps_its_disabled_ink_and_owns_all_background_painting() {
        let mut renderer = LayoutRenderer::new();
        let mut view: Element<'_, u8, ButtonTheme, LayoutRenderer> =
            CenteredButton::new(text("1").size(10)).square(30).class(()).into();
        let mut tree = widget::Tree::new(&view);
        view.as_widget_mut().diff(&mut tree);
        let viewport = Rectangle::with_size(Size::new(100.0, 52.0));
        let node = view.as_widget_mut().layout(
            &mut tree, &renderer, &layout::Limits::new(Size::ZERO, viewport.size()),
        );
        view.as_widget().draw(
            &tree, &mut renderer, &ButtonTheme, &renderer::Style::default(),
            Layout::new(&node), mouse::Cursor::Unavailable, &viewport,
        );
        let palette = crate::Tokens::dark().palette;
        assert_eq!(renderer.paragraph_colours, vec![palette.muted_text]);
        assert_eq!(renderer.quads, vec![(node.bounds(), palette.primary.into())]);
    }
}
