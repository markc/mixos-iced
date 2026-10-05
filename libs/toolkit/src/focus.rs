// SPDX-License-Identifier: MIT OR Apache-2.0
//! Focus conventions for widget authors: [`Source`] records *how* a
//! widget most recently gained focus, and [`ring`] draws the
//! `:focus-visible` halo that follows from it.
//!
//! A focus ring that appears on every click teaches users to ignore it.
//! The rule the web settled on — and we follow — is that the ring is a
//! *keyboard navigation* affordance: draw it when focus arrived by
//! keyboard, not by mouse. A widget stores `Option<Source>` in its state
//! (`None` = not focused), paints the ring only for
//! [`Source::Keyboard`], and re-arms it on the next keyboard interaction.

use iced_core::border::Border;
use iced_core::{Background, Color, Rectangle, Renderer, renderer};

/// Cycles through a complete focus domain, wrapping at both ends and ensuring
/// exactly one focusable owns focus. Apply this to the shared parent of peers.
pub fn cycle(backwards: bool) -> impl iced_core::widget::Operation {
    use iced_core::widget::{Operation, operation};
    struct Cycle {
        target: Option<usize>,
        current: usize,
    }
    impl Operation for Cycle {
        fn focusable(
            &mut self,
            _: Option<&iced_core::widget::Id>,
            _: Rectangle,
            state: &mut dyn operation::Focusable,
        ) {
            if Some(self.current) == self.target {
                state.focus();
            } else {
                state.unfocus();
            }
            self.current += 1;
        }
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
    }
    operation::then(operation::focusable::count(), move |count| {
        let target = if count.total == 0 {
            None
        } else {
            Some(match count.focused {
                Some(current) if backwards => {
                    if current == 0 {
                        count.total - 1
                    } else {
                        current - 1
                    }
                }
                Some(current) => (current + 1) % count.total,
                None if backwards => count.total - 1,
                None => 0,
            })
        };
        Cycle { target, current: 0 }
    })
}

/// A focusable region around a composite keyboard control. Its children keep
/// their own state; keyboard/IME input enters the region only while focused.
/// Clicks focus it, and the normal focus operations support Tab traversal.
pub struct Region<'a, Message, R> {
    content: iced_core::Element<'a, Message, crate::Theme, R>,
    id: iced_core::widget::Id,
}

pub fn region<'a, Message, R>(
    content: impl Into<iced_core::Element<'a, Message, crate::Theme, R>>,
    id: iced_core::widget::Id,
) -> Region<'a, Message, R> {
    Region {
        content: content.into(),
        id,
    }
}

#[derive(Debug, Default)]
struct RegionState {
    source: Option<Source>,
}

impl iced_core::widget::operation::Focusable for RegionState {
    fn is_focused(&self) -> bool {
        self.source.is_some()
    }
    fn focus(&mut self) {
        self.source = Some(Source::Keyboard);
    }
    fn unfocus(&mut self) {
        self.source = None;
    }
}

impl<Message, R: Renderer> iced_core::Widget<Message, crate::Theme, R> for Region<'_, Message, R> {
    fn tag(&self) -> iced_core::widget::tree::Tag {
        iced_core::widget::tree::Tag::of::<RegionState>()
    }
    fn state(&self) -> iced_core::widget::tree::State {
        iced_core::widget::tree::State::new(RegionState::default())
    }
    fn diff(&mut self, tree: &mut iced_core::widget::Tree) {
        tree.diff_children(&mut [&mut self.content]);
    }
    fn size(&self) -> iced_core::Size<iced_core::Length> {
        self.content.as_widget().size()
    }
    fn layout(
        &mut self,
        tree: &mut iced_core::widget::Tree,
        renderer: &R,
        limits: &iced_core::layout::Limits,
    ) -> iced_core::layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }
    fn operate(
        &mut self,
        tree: &mut iced_core::widget::Tree,
        layout: iced_core::Layout<'_>,
        renderer: &R,
        operation: &mut dyn iced_core::widget::Operation,
    ) {
        operation.container(Some(&self.id), layout.bounds());
        operation.focusable(
            Some(&self.id),
            layout.bounds(),
            tree.state.downcast_mut::<RegionState>(),
        );
        operation.traverse(&mut |operation| {
            self.content
                .as_widget_mut()
                .operate(&mut tree.children[0], layout, renderer, operation)
        });
    }
    fn update(
        &mut self,
        tree: &mut iced_core::widget::Tree,
        event: &iced_core::Event,
        layout: iced_core::Layout<'_>,
        cursor: iced_core::mouse::Cursor,
        renderer: &R,
        shell: &mut iced_core::Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        use iced_core::{Event, mouse, window};
        let state = tree.state.downcast_mut::<RegionState>();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                state.source = cursor.is_over(layout.bounds()).then_some(Source::Mouse);
            }
            Event::Window(window::Event::Unfocused) => state.source = None,
            Event::Keyboard(_) | Event::InputMethod(_) if state.source.is_none() => return,
            Event::Keyboard(_) => state.source = Some(Source::Keyboard),
            _ => {}
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
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
        tree: &iced_core::widget::Tree,
        renderer: &mut R,
        theme: &crate::Theme,
        style: &renderer::Style,
        layout: iced_core::Layout<'_>,
        cursor: iced_core::mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
        if tree.state.downcast_ref::<RegionState>().source == Some(Source::Keyboard) {
            ring(
                renderer,
                layout.bounds(),
                theme.metrics().radius.md,
                theme.palette().ring,
            );
        }
    }
    fn mouse_interaction(
        &self,
        tree: &iced_core::widget::Tree,
        layout: iced_core::Layout<'_>,
        cursor: iced_core::mouse::Cursor,
        viewport: &Rectangle,
        renderer: &R,
    ) -> iced_core::mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }
    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut iced_core::widget::Tree,
        layout: iced_core::Layout<'b>,
        renderer: &R,
        viewport: &Rectangle,
        translation: iced_core::Vector,
    ) -> Option<iced_core::overlay::Element<'b, Message, crate::Theme, R>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: 'a, R: Renderer + 'a> From<Region<'a, Message, R>>
    for iced_core::Element<'a, Message, crate::Theme, R>
{
    fn from(region: Region<'a, Message, R>) -> Self {
        Self::new(region)
    }
}

/// How a focusable widget most recently gained focus.
///
/// Widgets that draw a focus ring only under keyboard navigation store
/// this behind an [`Option`] — `None` meaning "not focused" — and paint
/// the ring only for [`Keyboard`](Self::Keyboard), the analog of CSS
/// `:focus-visible`. Clicking a widget focuses it for subsequent keyboard
/// use but records [`Mouse`](Self::Mouse), so no ring is shown until the
/// next keyboard interaction re-arms [`Keyboard`](Self::Keyboard).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Focus arrived via Tab, a programmatic focus operation, or keyboard
    /// navigation / activation.
    Keyboard,
    /// Focus arrived by clicking or tapping the widget.
    Mouse,
}

/// Gap between the control's edge and the ring band.
const GAP: f32 = 3.0;

/// Draws a soft `:focus-visible` halo hugging a control.
///
/// `bounds` and `radius` are the control's own bounds and corner radius
/// (pass `height / 2.0` for a circle like a radio dot, or the box radius
/// for a checkbox). The band is expanded outward by a small gap and kept
/// concentric with the control — `radius + GAP` — so its corners parallel
/// the control's. It is thin and drawn at reduced alpha so it reads as a
/// glow rather than a hard outline; `color` is the theme's focus colour
/// (for toolkit tokens, `Palette::ring`).
pub fn ring<R: Renderer>(renderer: &mut R, bounds: Rectangle, radius: f32, color: Color) {
    renderer.fill_quad(
        renderer::Quad {
            bounds: bounds.expand(GAP),
            border: Border {
                radius: (radius + GAP).into(),
                width: 2.0,
                color: color.scale_alpha(0.4),
            },
            ..renderer::Quad::default()
        },
        Background::Color(Color::TRANSPARENT),
    );
}
