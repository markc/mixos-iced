// SPDX-License-Identifier: MIT OR Apache-2.0
//! A reusable drag grip. The caller translates pointer coordinates into its
//! layout's ratio or absolute position; the control owns gesture and drawing.
use iced_core::{
    Color, Element, Event, Layout, Length, Rectangle, Shell, Size, Widget, layout, mouse, renderer,
    widget::{Tree, tree},
};
use std::time::{Duration, Instant};
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
#[derive(Default)]
struct DividerState {
    dragging: bool,
    moved: bool,
    last_click: Option<Instant>,
}
pub struct Grip<'a, Message> {
    width: f32,
    edge: f32,
    border: Color,
    accent: Color,
    on_drag: Box<dyn Fn(f32, &Rectangle) -> Message + 'a>,
    reset: Message,
}
impl<'a, Message> Grip<'a, Message> {
    pub fn new(
        width: f32,
        edge: f32,
        border: Color,
        accent: Color,
        reset: Message,
        on_drag: impl Fn(f32, &Rectangle) -> Message + 'a,
    ) -> Self {
        assert!(
            width.is_finite() && width > 0.0 && edge.is_finite() && edge >= 0.0 && edge <= width
        );
        Self {
            width,
            edge,
            border,
            accent,
            reset,
            on_drag: Box::new(on_drag),
        }
    }
}
impl<Message: Clone, Theme, Renderer: iced_core::Renderer> Widget<Message, Theme, Renderer>
    for Grip<'_, Message>
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<DividerState>()
    }
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.width), Length::Fill)
    }

    fn state(&self) -> tree::State {
        tree::State::new(DividerState::default())
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.resolve(Length::Fixed(self.width), Length::Fill, Size::ZERO))
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };
        let st = tree.state.downcast_mut::<DividerState>();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                if cursor.is_over(clip) =>
            {
                if st
                    .last_click
                    .is_some_and(|when| when.elapsed() < DOUBLE_CLICK)
                {
                    // Double-click: exactly half; this press does not start
                    // a drag.
                    st.last_click = None;
                    shell.publish(self.reset.clone());
                } else {
                    st.dragging = true;
                    st.moved = false;
                }
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                // The double-click window runs click-to-click: only a
                // release that did NOT drag stamps it, so a drag-and-repress
                // gesture cannot snap the split to 0.5.
                if st.dragging && !st.moved {
                    st.last_click = Some(Instant::now());
                }
                st.dragging = false;
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) if st.dragging => {
                st.moved = true;
                shell.publish((self.on_drag)(position.x, viewport));
                shell.capture_event();
            }
            // A release outside the window never arrives, and iced's
            // CursorMoved carries no button state, so a held drag cannot be
            // detected as orphaned per-event: losing focus or a resize ends
            // the drag instead (the split keeps its last published ratio).
            Event::Window(
                iced_core::window::Event::Unfocused | iced_core::window::Event::Resized(_),
            ) => {
                st.dragging = false;
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };
        let st = tree.state.downcast_ref::<DividerState>();
        // The grip: a 2 px hairline centred in the 6 px strip. Hovering or
        // dragging lights it with the accent (tokens, zero literals).
        let active = st.dragging || cursor.is_over(clip);
        let color = if active { self.accent } else { self.border };
        let grip = Rectangle {
            x: bounds.center_x() - self.edge / 2.0,
            y: bounds.y,
            width: self.edge,
            height: bounds.height,
        };
        if let Some(clipped) = grip.intersection(&clip) {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: clipped,
                    ..renderer::Quad::default()
                },
                color,
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        // Keep the resize cursor while dragging outside the grip.
        let st = tree.state.downcast_ref::<DividerState>();
        if st.dragging || cursor.is_over(layout.bounds()) {
            mouse::Interaction::ResizingHorizontally
        } else {
            mouse::Interaction::None
        }
    }
}

impl<'a, M: Clone + 'a, T: 'a, R: iced_core::Renderer + 'a> From<Grip<'a, M>>
    for Element<'a, M, T, R>
{
    fn from(grip: Grip<'a, M>) -> Self {
        Element::new(grip)
    }
}
