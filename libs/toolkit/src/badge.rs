// SPDX-License-Identifier: MIT OR Apache-2.0
//! A badge for highlighting small information: [`Badge`], a rounded chip
//! around any content, coloured by the theme (primary, neutral, destructive).

use iced_core::layout::{Limits, Node};
use iced_core::mouse::{self, Cursor};
use iced_core::renderer;
use iced_core::widget::{Operation, Tree};
use iced_core::window;
use iced_core::{
    Alignment, Background, Border, Color, Element, Event, Layout, Length, Padding, Point,
    Rectangle, Shell, Size, Widget,
};

/// The interaction status of a [`Badge`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    /// Idle.
    #[default]
    Active,
    /// The pointer is over the badge.
    Hovered,
}

/// The style of a [`Badge`].
#[derive(Clone, Copy, Debug)]
pub struct Style {
    /// The background of the [`Badge`].
    pub background: Background,
    /// The border radius of the [`Badge`]; `None` uses the pill default.
    pub border_radius: Option<f32>,
    /// The border width of the [`Badge`].
    pub border_width: f32,
    /// The border color of the [`Badge`]; `None` draws no border.
    pub border_color: Option<Color>,
    /// The text color the content is drawn with.
    pub text_color: Color,
}

/// The theme catalog of a [`Badge`].
pub trait Catalog {
    /// The item class of [`Catalog`].
    type Class<'a>;
    /// The default class produced by the [`Catalog`].
    fn default<'a>() -> Self::Class<'a>;
    /// The [`Style`] of a class with the given status.
    fn style(&self, class: &Self::Class<'_>, status: Status) -> Style;
}

/// A styling function for a [`Badge`].
pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme, Status) -> Style + 'a>;

/// The ratio of the border radius to the height, for the pill default.
const BORDER_RADIUS_RATIO: f32 = 34.0 / 15.0;

/// A badge for highlighting small information.
///
/// ```no_run
/// # use toolkit::badge::Badge;
/// fn view<'a, Message, Theme, Renderer>() -> iced_core::Element<'a, Message, Theme, Renderer>
/// where
///     Message: Clone,
///     Theme: toolkit::badge::Catalog + iced_core::widget::text::Catalog + 'a,
///     Renderer: iced_core::text::Renderer + 'a,
/// {
///     Badge::new("new").into()
/// }
/// ```
#[allow(missing_debug_implementations)]
pub struct Badge<'a, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
    Theme: Catalog,
{
    /// The padding of the [`Badge`].
    padding: u16,
    /// The width of the [`Badge`].
    width: Length,
    /// The height of the [`Badge`].
    height: Length,
    /// The horizontal alignment of the [`Badge`].
    horizontal_alignment: Alignment,
    /// The vertical alignment of the [`Badge`].
    vertical_alignment: Alignment,
    /// The style of the [`Badge`].
    class: Theme::Class<'a>,
    /// The content [`Element`] of the [`Badge`].
    content: Element<'a, Message, Theme, Renderer>,
    /// The [`Status`] of the [`Badge`].
    status: Option<Status>,
}

impl<'a, Message, Theme, Renderer> Badge<'a, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
    Theme: Catalog,
{
    /// Creates a new [`Badge`] with the given content.
    pub fn new<T>(content: T) -> Self
    where
        T: Into<Element<'a, Message, Theme, Renderer>>,
    {
        Badge {
            padding: 7,
            width: Length::Shrink,
            height: Length::Shrink,
            horizontal_alignment: Alignment::Center,
            vertical_alignment: Alignment::Center,
            class: Theme::default(),
            content: content.into(),
            status: None,
        }
    }

    /// Sets the horizontal alignment of the content of the [`Badge`].
    #[must_use]
    pub fn align_x(mut self, alignment: Alignment) -> Self {
        self.horizontal_alignment = alignment;
        self
    }

    /// Sets the vertical alignment of the content of the [`Badge`].
    #[must_use]
    pub fn align_y(mut self, alignment: Alignment) -> Self {
        self.vertical_alignment = alignment;
        self
    }

    /// Sets the height of the [`Badge`].
    #[must_use]
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// Sets the padding of the [`Badge`].
    #[must_use]
    pub fn padding(mut self, units: u16) -> Self {
        self.padding = units;
        self
    }

    /// Sets the style of the [`Badge`].
    #[must_use]
    pub fn style(mut self, style: impl Fn(&Theme, Status) -> Style + 'a) -> Self
    where
        Theme::Class<'a>: From<StyleFn<'a, Theme>>,
    {
        self.class = (Box::new(style) as StyleFn<'a, Theme>).into();
        self
    }

    /// Sets the class of the [`Badge`].
    #[must_use]
    pub fn class(mut self, class: impl Into<Theme::Class<'a>>) -> Self {
        self.class = class.into();
        self
    }

    /// Sets the width of the [`Badge`].
    #[must_use]
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }
}

impl<'a, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Badge<'a, Message, Theme, Renderer>
where
    Message: 'a + Clone,
    Renderer: 'a + renderer::Renderer,
    Theme: Catalog,
{
    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_mut(&mut self.content));
    }

    fn size(&self) -> Size<Length> {
        Size {
            width: self.width,
            height: self.height,
        }
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> Node {
        let padding: Padding = self.padding.into();
        let limits = limits
            .loose()
            .width(self.width)
            .height(self.height)
            .shrink(padding);

        let mut content =
            self.content
                .as_widget_mut()
                .layout(&mut tree.children[0], renderer, &limits.loose());
        let size = limits.resolve(self.width, self.height, content.size());

        content = content
            .move_to(Point::new(padding.left, padding.top))
            .align(self.horizontal_alignment, self.vertical_alignment, size);

        Node::with_children(size.expand(padding), vec![content])
    }

    fn update(
        &mut self,
        state: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &Renderer,
        shell: &mut Shell<Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut state.children[0],
            event,
            layout
                .children()
                .next()
                .expect("widget: Layout should have a children layout for a badge."),
            cursor,
            renderer,
            shell,
            viewport,
        );

        let current_status = if cursor.is_over(layout.bounds()) {
            Status::Hovered
        } else {
            Status::Active
        };

        if let Event::Window(window::Event::RedrawRequested(_now)) = event {
            self.status = Some(current_status);
        } else if self.status.is_some_and(|status| status != current_status) {
            shell.request_redraw();
        }
    }

    fn mouse_interaction(
        &self,
        state: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &state.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let mut children = layout.children();

        let style_sheet = theme.style(&self.class, self.status.unwrap_or(Status::Active));

        let border_radius = style_sheet
            .border_radius
            .unwrap_or(bounds.height / BORDER_RADIUS_RATIO);

        if bounds.intersects(viewport) {
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    border: Border {
                        radius: border_radius.into(),
                        width: style_sheet.border_width,
                        color: style_sheet.border_color.unwrap_or(Color::TRANSPARENT),
                    },
                    ..renderer::Quad::default()
                },
                style_sheet.background,
            );
        }

        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            &renderer::Style {
                text_color: style_sheet.text_color,
            },
            children
                .next()
                .expect("Graphics: Layout should have a children layout for Badge"),
            cursor,
            viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                layout
                    .children()
                    .next()
                    .expect("Badge layout should have a content child"),
                renderer,
                operation,
            );
        });
    }
}

impl<'a, Message, Theme, Renderer> From<Badge<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a + Clone,
    Renderer: 'a + renderer::Renderer,
    Theme: 'a + Catalog,
{
    fn from(badge: Badge<'a, Message, Theme, Renderer>) -> Self {
        Self::new(badge)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_renderer::LayoutRenderer;

    type TestBadge<'a> = Badge<'a, String, iced_core::Theme, LayoutRenderer>;

    #[test]
    fn badge_new_has_default_values() {
        let badge = TestBadge::new("Test");
        assert_eq!(badge.padding, 7);
        assert_eq!(badge.width, Length::Shrink);
        assert_eq!(badge.height, Length::Shrink);
        assert_eq!(badge.horizontal_alignment, Alignment::Center);
        assert_eq!(badge.vertical_alignment, Alignment::Center);
        assert!(badge.status.is_none());
    }

    #[test]
    fn badge_builders_set_values() {
        let badge = TestBadge::new("Test")
            .padding(15)
            .width(200)
            .height(50)
            .align_x(Alignment::Start)
            .align_y(Alignment::End);
        assert_eq!(badge.padding, 15);
        assert_eq!(badge.width, Length::Fixed(200.0));
        assert_eq!(badge.height, Length::Fixed(50.0));
        assert_eq!(badge.horizontal_alignment, Alignment::Start);
        assert_eq!(badge.vertical_alignment, Alignment::End);
    }
}
