// SPDX-License-Identifier: MIT OR Apache-2.0
//! A drop-down: [`DropDown`] shows an overlay over (or beside, or above)
//! an underlay while `expanded` is set, dismisses on Escape or an outside
//! click, and clamps itself inside the viewport.
//!
//! The expanded state is the caller's: the widget takes `expanded: bool`
//! and an `on_dismiss` message, so opening and closing is one application
//! message, not hidden state.

use iced_core::Element;
use iced_core::Event;
use iced_core::Layout;
use iced_core::Length;
use iced_core::Point;
use iced_core::Rectangle;
use iced_core::Shell;
use iced_core::Size;
use iced_core::Vector;
use iced_core::Widget;
use iced_core::keyboard::{self, key::Named};
use iced_core::layout::{Limits, Node};
use iced_core::mouse::{self, Cursor};
use iced_core::overlay;
use iced_core::renderer;
use iced_core::touch;
use iced_core::widget::{self, Operation, Tree};

/// Where a [`DropDown`]'s overlay appears, relative to its underlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Alignment {
    /// Above the underlay, aligned to its start edge.
    TopStart,
    /// Above the underlay, centered on it.
    #[default]
    Top,
    /// Above the underlay, aligned to its end edge.
    TopEnd,
    /// Beside the underlay, on its end side, centered on it.
    End,
    /// Below the underlay, aligned to its end edge.
    BottomEnd,
    /// Below the underlay, centered on it.
    Bottom,
    /// Below the underlay, aligned to its start edge.
    BottomStart,
    /// Beside the underlay, on its start side, centered on it.
    Start,
}

/// The gap between a [`DropDown`]'s underlay and its overlay.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Offset {
    /// Offset on the x-axis.
    pub x: f32,
    /// Offset on the y-axis.
    pub y: f32,
}

impl Offset {
    /// Constructs a new [`Offset`].
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

impl From<f32> for Offset {
    fn from(value: f32) -> Self {
        Self { x: value, y: value }
    }
}

/// A drop-down menu: an underlay with an overlay shown while expanded.
///
/// ```no_run
/// # use toolkit::drop_down::DropDown;
/// #[derive(Clone)]
/// enum Message { Toggled, Dismissed }
///
/// fn view(expanded: bool) -> iced_core::Element<'static, Message, toolkit::theme::Theme> {
///     DropDown::new(
///         iced_widget::button("Open").on_press(Message::Toggled),
///         iced_widget::text("The menu"),
///         expanded,
///     )
///     .on_dismiss(Message::Dismissed)
///     .into()
/// }
/// ```
#[allow(missing_debug_implementations)]
pub struct DropDown<'a, Message, Theme, Renderer>
where
    Message: Clone,
    Renderer: renderer::Renderer,
{
    underlay: Element<'a, Message, Theme, Renderer>,
    overlay: Element<'a, Message, Theme, Renderer>,
    on_dismiss: Option<Message>,
    width: Option<Length>,
    height: Length,
    alignment: Alignment,
    offset: Offset,
    expanded: bool,
}

impl<'a, Message, Theme, Renderer> DropDown<'a, Message, Theme, Renderer>
where
    Message: Clone,
    Renderer: renderer::Renderer,
{
    /// Creates a new [`DropDown`] from the underlay, the overlay and the
    /// expanded state.
    pub fn new<U, B>(underlay: U, overlay: B, expanded: bool) -> Self
    where
        U: Into<Element<'a, Message, Theme, Renderer>>,
        B: Into<Element<'a, Message, Theme, Renderer>>,
    {
        DropDown {
            underlay: underlay.into(),
            overlay: overlay.into(),
            expanded,
            on_dismiss: None,
            width: None,
            height: Length::Shrink,
            alignment: Alignment::default(),
            offset: Offset::from(5.0),
        }
    }

    /// Sets the width of the overlay (the underlay's width by default).
    #[must_use]
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = Some(width.into());
        self
    }

    /// Sets the height of the overlay.
    #[must_use]
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// Sets the alignment of the overlay relative to the underlay.
    #[must_use]
    pub fn alignment(mut self, alignment: Alignment) -> Self {
        self.alignment = alignment;
        self
    }

    /// Sets the offset of the overlay.
    #[must_use]
    pub fn offset(mut self, offset: impl Into<Offset>) -> Self {
        self.offset = offset.into();
        self
    }

    /// Sends a message when a click occurs outside of the overlay (and
    /// the underlay) while expanded.
    #[must_use]
    pub fn on_dismiss(mut self, message: Message) -> Self {
        self.on_dismiss = Some(message);
        self
    }
}

impl<'a, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for DropDown<'a, Message, Theme, Renderer>
where
    Message: 'a + Clone,
    Renderer: 'a + renderer::Renderer,
{
    fn size(&self) -> Size<Length> {
        self.underlay.as_widget().size()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> Node {
        self.underlay
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn draw(
        &self,
        state: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
    ) {
        self.underlay
            .as_widget()
            .draw(&state.children[0], renderer, theme, style, layout, cursor, viewport);
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(&mut [&mut self.underlay, &mut self.overlay]);
    }

    fn operate(
        &mut self,
        state: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.underlay
            .as_widget_mut()
            .operate(&mut state.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        state: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.underlay.as_widget_mut().update(
            &mut state.children[0],
            event,
            layout,
            cursor,
            renderer,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        state: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.underlay.as_widget().mouse_interaction(
            &state.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn overlay<'b>(
        &'b mut self,
        state: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        if !self.expanded {
            return self.underlay.as_widget_mut().overlay(
                &mut state.children[0],
                layout,
                renderer,
                viewport,
                translation,
            );
        }

        Some(overlay::Element::new(Box::new(DropDownOverlay::new(
            &mut state.children[1],
            &mut self.overlay,
            self.on_dismiss.as_ref(),
            self.width.as_ref(),
            &self.height,
            &self.alignment,
            &self.offset,
            layout.bounds(),
            layout.position() + translation,
            *viewport,
        ))))
    }
}

impl<'a, Message, Theme: 'a, Renderer> From<DropDown<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a + Clone,
    Renderer: 'a + renderer::Renderer,
{
    fn from(drop_down: DropDown<'a, Message, Theme, Renderer>) -> Self {
        Element::new(drop_down)
    }
}

struct DropDownOverlay<'a, 'b, Message, Theme, Renderer>
where
    Message: Clone,
{
    state: &'b mut Tree,
    element: &'b mut Element<'a, Message, Theme, Renderer>,
    on_dismiss: Option<&'b Message>,
    width: Option<&'b Length>,
    height: &'b Length,
    alignment: &'b Alignment,
    offset: &'b Offset,
    underlay_bounds: Rectangle,
    position: Point,
    viewport: Rectangle,
}

impl<'a, 'b, Message, Theme, Renderer> DropDownOverlay<'a, 'b, Message, Theme, Renderer>
where
    Message: Clone,
    Renderer: renderer::Renderer,
{
    #[allow(clippy::too_many_arguments)]
    fn new(
        state: &'b mut Tree,
        element: &'b mut Element<'a, Message, Theme, Renderer>,
        on_dismiss: Option<&'b Message>,
        width: Option<&'b Length>,
        height: &'b Length,
        alignment: &'b Alignment,
        offset: &'b Offset,
        underlay_bounds: Rectangle,
        position: Point,
        viewport: Rectangle,
    ) -> Self {
        DropDownOverlay {
            state,
            element,
            on_dismiss,
            width,
            height,
            alignment,
            offset,
            underlay_bounds,
            position,
            viewport,
        }
    }
}

impl<Message, Theme, Renderer> overlay::Overlay<Message, Theme, Renderer>
    for DropDownOverlay<'_, '_, Message, Theme, Renderer>
where
    Message: Clone,
    Renderer: renderer::Renderer,
{
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> Node {
        let limits = Limits::new(Size::ZERO, bounds)
            .width(*self.width.unwrap_or(&Length::Fixed(self.underlay_bounds.width)));

        let previous_position = self.position;
        let max = limits.max();

        let height_above = (previous_position.y - self.offset.y).max(0.0);
        let height_below =
            (max.height - previous_position.y - self.underlay_bounds.height - self.offset.y)
                .max(0.0);

        let ref_center_y = previous_position.y + self.underlay_bounds.height / 2.0;
        let max_height_symmetric = (ref_center_y.min(max.height - ref_center_y) * 2.0).max(0.0);

        let limits = match self.alignment {
            Alignment::Top => limits.height(self.height.max(height_above)),
            Alignment::TopStart | Alignment::TopEnd => limits.height(
                self.height
                    .max((height_above + self.underlay_bounds.height).max(0.0)),
            ),
            Alignment::Bottom => limits.height(self.height.max(height_below)),
            Alignment::BottomEnd | Alignment::BottomStart => limits.height(
                self.height
                    .max((height_below + self.underlay_bounds.height).max(0.0)),
            ),
            Alignment::Start | Alignment::End => {
                limits.height(self.height.max(max_height_symmetric))
            }
        };

        let node = self
            .element
            .as_widget_mut()
            .layout(self.state, renderer, &limits);

        let mut new_position = match self.alignment {
            Alignment::TopStart => Point::new(
                previous_position.x - node.bounds().width - self.offset.x,
                previous_position.y - node.bounds().height + self.underlay_bounds.height
                    - self.offset.y,
            ),
            Alignment::Top => Point::new(
                previous_position.x + self.underlay_bounds.width / 2.0 - node.bounds().width / 2.0,
                previous_position.y - node.bounds().height - self.offset.y,
            ),
            Alignment::TopEnd => Point::new(
                previous_position.x + self.underlay_bounds.width + self.offset.x,
                previous_position.y - node.bounds().height + self.underlay_bounds.height
                    - self.offset.y,
            ),
            Alignment::End => Point::new(
                previous_position.x + self.underlay_bounds.width + self.offset.x,
                previous_position.y + self.underlay_bounds.height / 2.0
                    - node.bounds().height / 2.0,
            ),
            Alignment::BottomEnd => Point::new(
                previous_position.x + self.underlay_bounds.width + self.offset.x,
                previous_position.y + self.offset.y,
            ),
            Alignment::Bottom => Point::new(
                previous_position.x + self.underlay_bounds.width / 2.0 - node.bounds().width / 2.0,
                previous_position.y + self.underlay_bounds.height + self.offset.y,
            ),
            Alignment::BottomStart => Point::new(
                previous_position.x - node.bounds().width - self.offset.x,
                previous_position.y + self.offset.y,
            ),
            Alignment::Start => Point::new(
                previous_position.x - node.bounds().width - self.offset.x,
                previous_position.y + self.underlay_bounds.height / 2.0
                    - node.bounds().height / 2.0,
            ),
        };

        // Keep the overlay inside the viewport, preferring to flip it over
        // the underlay when it would run off an edge.
        if new_position.x + node.bounds().width > self.viewport.width {
            new_position.x -= node.bounds().width;
        }

        if new_position.x < 0.0 {
            new_position.x = 0.0;
        }

        if new_position.y + node.bounds().height > self.viewport.height {
            new_position.y -= node.bounds().height;
        }
        if new_position.y < 0.0 {
            new_position.y = 0.0;
        }

        node.move_to(new_position)
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
    ) {
        let bounds = layout.bounds();
        self.element
            .as_widget()
            .draw(self.state, renderer, theme, style, layout, cursor, &bounds);
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &Renderer,
        shell: &mut Shell<Message>,
    ) {
        self.underlay_bounds = Rectangle {
            x: self.position.x,
            y: self.position.y,
            width: self.underlay_bounds.width,
            height: self.underlay_bounds.height,
        };

        if let Some(on_dismiss) = self.on_dismiss {
            match &event {
                Event::Keyboard(keyboard::Event::KeyPressed { key, .. })
                    if key == &keyboard::Key::Named(Named::Escape) =>
                {
                    shell.publish(on_dismiss.clone());
                }

                Event::Mouse(mouse::Event::ButtonPressed(
                    mouse::Button::Left | mouse::Button::Right,
                ))
                | Event::Touch(touch::Event::FingerPressed { .. })
                    if !cursor.is_over(layout.bounds())
                        && !cursor.is_over(self.underlay_bounds) =>
                {
                    shell.publish(on_dismiss.clone());
                }

                _ => {}
            }
        }

        self.element.as_widget_mut().update(
            self.state,
            event,
            layout,
            cursor,
            renderer,
            shell,
            &layout.bounds(),
        );
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let interaction = self.element.as_widget().mouse_interaction(
            self.state,
            layout,
            cursor,
            &self.viewport,
            renderer,
        );

        if interaction == mouse::Interaction::None && cursor.is_over(layout.bounds()) {
            mouse::Interaction::Idle
        } else {
            interaction
        }
    }

    fn operate(
        &mut self,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        self.element
            .as_widget_mut()
            .operate(self.state, layout, renderer, operation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_renderer::LayoutRenderer;

    #[test]
    fn defaults_center_above_with_a_five_pixel_gap() {
        let drop: DropDown<'_, (), iced_core::Theme, LayoutRenderer> =
            DropDown::new(iced_widget::Space::new(), iced_widget::Space::new(), false);
        assert_eq!(drop.alignment, Alignment::Top);
        assert_eq!(drop.offset, Offset::new(5.0, 5.0));
        assert!(!drop.expanded);
        assert!(drop.on_dismiss.is_none());
        assert!(drop.width.is_none());
        assert_eq!(drop.height, Length::Shrink);
    }
}
