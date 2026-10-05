// SPDX-License-Identifier: MIT OR Apache-2.0
//! An anchored popover: [`Popover`], an overlay surface placed relative
//! to its trigger by [`anchor::place`] (flip, then shift, inside the
//! viewport), shown while `open` is set, dismissed by an outside click
//! or Escape via `on_dismiss` — the mechanics of [`crate::drop_down`]
//! with A-Disruption's placement core instead of fixed alignments.

use iced_core::Element;
use iced_core::Event;
use iced_core::Layout;
use iced_core::Length;
use iced_core::Rectangle;
use iced_core::Shell;
use iced_core::Size;
use iced_core::Vector;
use iced_core::Widget;
use iced_core::keyboard::{self, key::Named};
use iced_core::layout::Limits;
use iced_core::mouse::{self, Cursor};
use iced_core::overlay;
use iced_core::renderer;
use iced_core::widget::{self, Operation, Tree};

pub use crate::anchor::{Align, Placement, Side};

/// An anchored popover: a trigger with an overlay while open.
///
/// ```no_run
/// # use toolkit::popover::{Popover, Placement, Side};
/// #[derive(Clone)]
/// enum Message { Toggled, Dismissed }
///
/// fn view<'a, Theme, Renderer>(
///     open: bool,
/// ) -> iced_core::Element<'a, Message, Theme, Renderer>
/// where
///     Theme: iced_core::widget::text::Catalog + 'a,
///     Renderer: iced_core::renderer::Renderer + 'a,
/// {
///     Popover::new(
///         iced_widget::button("Show").on_press(Message::Toggled),
///         iced_widget::text("The popover"),
///         open,
///     )
///     .placement(Placement::new(Side::Bottom).gap(4.0))
///     .on_dismiss(Message::Dismissed)
///     .into()
/// }
/// ```
#[allow(missing_debug_implementations)]
pub struct Popover<'a, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    trigger: Element<'a, Message, Theme, Renderer>,
    surface: Element<'a, Message, Theme, Renderer>,
    open: bool,
    on_dismiss: Option<Message>,
    placement: Placement,
    min_width: Option<f32>,
}

impl<'a, Message, Theme, Renderer> Popover<'a, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    /// Creates a new [`Popover`] from the trigger, the surface and the
    /// open state.
    #[must_use]
    pub fn new<U, S>(
        trigger: U,
        surface: S,
        open: bool,
    ) -> Self
    where
        U: Into<Element<'a, Message, Theme, Renderer>>,
        S: Into<Element<'a, Message, Theme, Renderer>>,
    {
        Self {
            trigger: trigger.into(),
            surface: surface.into(),
            open,
            on_dismiss: None,
            placement: Placement::default(),
            min_width: None,
        }
    }

    /// Sets the [`Placement`] of the surface relative to the trigger.
    #[must_use]
    pub fn placement(mut self, placement: Placement) -> Self {
        self.placement = placement;
        self
    }

    /// The minimum surface width (the trigger's width by default).
    #[must_use]
    pub fn min_width(mut self, min_width: f32) -> Self {
        self.min_width = Some(min_width);
        self
    }

    /// Sets the message produced when the popover is dismissed by an
    /// outside click or Escape while open.
    #[must_use]
    pub fn on_dismiss(mut self, message: Message) -> Self {
        self.on_dismiss = Some(message);
        self
    }
}

impl<'a, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Popover<'a, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
    Message: 'a + Clone,
{
    fn size(&self) -> Size<Length> {
        self.trigger.as_widget().size()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> iced_core::layout::Node {
        self.trigger
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
    ) {
        self.trigger
            .as_widget()
            .draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(&mut [&mut self.trigger, &mut self.surface]);
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.trigger
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.trigger.as_widget_mut().update(
            &mut tree.children[0],
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
        tree: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.trigger.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        if !self.open {
            return self.trigger.as_widget_mut().overlay(
                &mut tree.children[0],
                layout,
                renderer,
                viewport,
                translation,
            );
        }

        Some(overlay::Element::new(Box::new(PopoverSurface::new(
            &mut tree.children[1],
            &mut self.surface,
            self.on_dismiss.as_ref(),
            self.placement,
            self.min_width,
            layout.bounds(),
            layout.position() + translation,
            *viewport,
        ))))
    }
}

impl<'a, Message, Theme: 'a, Renderer> From<Popover<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer + 'a,
    Message: 'a + Clone,
{
    fn from(popover: Popover<'a, Message, Theme, Renderer>) -> Self {
        Element::new(popover)
    }
}

struct PopoverSurface<'a, 'b, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    tree: &'b mut Tree,
    element: &'b mut Element<'a, Message, Theme, Renderer>,
    on_dismiss: Option<&'b Message>,
    placement: Placement,
    min_width: Option<f32>,
    trigger_bounds: Rectangle,
    position: iced_core::Point,
    viewport: Rectangle,
}

impl<'a, 'b, Message, Theme, Renderer> PopoverSurface<'a, 'b, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    #[allow(clippy::too_many_arguments)]
    fn new(
        tree: &'b mut Tree,
        element: &'b mut Element<'a, Message, Theme, Renderer>,
        on_dismiss: Option<&'b Message>,
        placement: Placement,
        min_width: Option<f32>,
        trigger_bounds: Rectangle,
        position: iced_core::Point,
        viewport: Rectangle,
    ) -> Self {
        Self {
            tree,
            element,
            on_dismiss,
            placement,
            min_width,
            trigger_bounds,
            position,
            viewport,
        }
    }
}

impl<Message, Theme, Renderer> overlay::Overlay<Message, Theme, Renderer>
    for PopoverSurface<'_, '_, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
    Message: Clone,
{
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> iced_core::layout::Node {
        let base = Rectangle {
            x: self.position.x,
            y: self.position.y,
            width: self.trigger_bounds.width,
            height: self.trigger_bounds.height,
        };
        let width = self
            .min_width
            .unwrap_or(base.width)
            .max(base.width);

        let limits = Limits::new(Size::ZERO, Size::new(width.max(1.0), bounds.height))
            .width(width);

        let node = self
            .element
            .as_widget_mut()
            .layout(self.tree, renderer, &limits);

        let placed = crate::anchor::place(
            base,
            node.size(),
            self.viewport,
            self.placement,
        );

        node.move_to(placed.position)
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
            .draw(self.tree, renderer, theme, style, layout, cursor, &bounds);
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
    ) {
        self.trigger_bounds = Rectangle {
            x: self.position.x,
            y: self.position.y,
            ..self.trigger_bounds
        };

        if let Some(on_dismiss) = self.on_dismiss {
            match &event {
                Event::Keyboard(keyboard::Event::KeyPressed { key, .. })
                    if key == &keyboard::Key::Named(Named::Escape) =>
                {
                    shell.publish((*on_dismiss).clone());
                }

                Event::Mouse(mouse::Event::ButtonPressed(
                    mouse::Button::Left | mouse::Button::Right,
                ))
                | Event::Touch(iced_core::touch::Event::FingerPressed { .. })
                    if !cursor.is_over(layout.bounds())
                        && !cursor.is_over(self.trigger_bounds) =>
                {
                    shell.publish((*on_dismiss).clone());
                }

                _ => {}
            }
        }

        self.element.as_widget_mut().update(
            self.tree,
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
            self.tree,
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
            .operate(self.tree, layout, renderer, operation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_renderer::LayoutRenderer;

    #[test]
    fn defaults_place_below_with_no_gap() {
        let popover: Popover<'_, (), iced_core::Theme, LayoutRenderer> = Popover::new(
            iced_widget::Space::new(),
            iced_widget::Space::new(),
            false,
        );
        assert!(!popover.open);
        assert_eq!(popover.placement.side, Side::Bottom);
        assert_eq!(popover.placement.gap, 0.0);
        assert!(popover.placement.flip);
        assert!(popover.on_dismiss.is_none());
    }
}
