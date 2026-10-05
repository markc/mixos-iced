// SPDX-License-Identifier: MIT OR Apache-2.0
//! The inner rows of a [`SelectionList`](super::SelectionList): hover and
//! selection tracking, viewport-limited drawing, virtual text operations.

use std::collections::hash_map::DefaultHasher;
use std::fmt::Display;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;

use iced_core::layout::{self, Limits, Node};
use iced_core::layout::Layout;
use iced_core::mouse::{self, Cursor};
use iced_core::renderer;
use iced_core::text;
use iced_core::widget::text::{Ellipsis, LineHeight, Shaping, Wrapping};
use iced_core::widget::tree::{State, Tag};
use iced_core::widget::Tree;
use iced_core::{Border, Color, Element, Event, Length, Point, Rectangle, Shell, Size, Widget};
use iced_core::alignment::Vertical;

use super::{Catalog, Status};

/// Renders the option rows of a [`SelectionList`](super::SelectionList).
#[allow(missing_debug_implementations)]
pub struct List<'a, 'c, T: 'a, Message, Theme, Renderer>
where
    T: Clone + Display + Eq + Hash,
    [T]: ToOwned<Owned = Vec<T>>,
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog,
{
    /// The options to render.
    pub options: &'a [T],
    /// The label font.
    pub font: Renderer::Font,
    /// The style class.
    pub class: &'a <Theme as Catalog>::Class<'c>,
    /// Called with the index and value of the option picked.
    pub on_selected: &'a dyn Fn(usize, T) -> Message,
    /// The row padding.
    pub padding: iced_core::Padding,
    /// The text size.
    pub text_size: f32,
    /// The index of the selected option, if any.
    pub selected: Option<usize>,
    #[allow(clippy::missing_docs_in_private_items)]
    pub phantomdata: PhantomData<Renderer>,
}

/// Tracks hover and the last selection (by index and value hash, so a
/// changed option list drops a stale selection).
#[derive(Debug, Clone, Default)]
pub struct ListState {
    /// The row under the pointer, if any.
    pub hovered_option: Option<usize>,
    /// The last row clicked, as `(index, hash of its value)`.
    pub last_selected_index: Option<(usize, u64)>,
}

impl<T, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for List<'_, '_, T, Message, Theme, Renderer>
where
    T: Clone + Display + Eq + Hash,
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog + iced_core::widget::text::Catalog,
{
    fn tag(&self) -> Tag {
        Tag::of::<ListState>()
    }

    fn state(&self) -> State {
        State::new(ListState::default())
    }

    fn diff(&mut self, state: &mut Tree) {
        let list_state = state.state.downcast_mut::<ListState>();

        if let Some(id) = self.selected {
            if let Some(option) = self.options.get(id) {
                let mut hasher = DefaultHasher::new();
                option.hash(&mut hasher);

                list_state.last_selected_index = Some((id, hasher.finish()));
            } else {
                list_state.last_selected_index = None;
            }
        } else if let Some((id, hash)) = list_state.last_selected_index
            && let Some(option) = self.options.get(id)
        {
            let mut hasher = DefaultHasher::new();
            option.hash(&mut hasher);

            if hash != hasher.finish() {
                list_state.last_selected_index = None;
            }
        }
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(&mut self, _tree: &mut Tree, _renderer: &Renderer, limits: &Limits) -> Node {
        let limits = limits.height(Length::Fill).width(Length::Fill);

        let intrinsic = Size::new(
            limits.max().width,
            (self.text_size + self.padding.y()) * self.options.len() as f32,
        );

        Node::new(intrinsic)
    }

    fn update(
        &mut self,
        state: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        _renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let list_state = state.state.downcast_mut::<ListState>();
        let cursor = cursor.position().unwrap_or_default();

        if bounds.contains(cursor) {
            match event {
                Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                    list_state.hovered_option = Some(
                        ((cursor.y - bounds.y) / (self.text_size + self.padding.y())) as usize,
                    );

                    shell.request_redraw();
                }
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                | Event::Touch(iced_core::touch::Event::FingerPressed { .. }) => {
                    list_state.hovered_option = Some(
                        ((cursor.y - bounds.y) / (self.text_size + self.padding.y())) as usize,
                    );

                    if let Some(index) = list_state.hovered_option
                        && let Some(option) = self.options.get(index)
                    {
                        let mut hasher = DefaultHasher::new();
                        option.hash(&mut hasher);
                        list_state.last_selected_index = Some((index, hasher.finish()));
                    }

                    if let Some((index, _)) = list_state.last_selected_index
                        && let Some(option) = self.options.get(index)
                    {
                        shell.publish((self.on_selected)(index, option.clone()));
                        shell.capture_event();
                    }

                    shell.request_redraw();
                }
                _ => {}
            }
        } else if list_state.hovered_option.is_some() {
            list_state.hovered_option = None;
            shell.request_redraw();
        }
    }

    fn mouse_interaction(
        &self,
        _state: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        let bounds = layout.bounds();

        if bounds.contains(cursor.position().unwrap_or_default()) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }

    fn draw(
        &self,
        state: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let option_height = self.text_size + self.padding.y();
        let offset = viewport.y - bounds.y;
        let start = (offset / option_height) as usize;
        let end = ((offset + viewport.height) / option_height).ceil() as usize;
        let list_state = state.state.downcast_ref::<ListState>();

        for i in start..end.min(self.options.len()) {
            let is_selected = list_state.last_selected_index.is_some_and(|u| u.0 == i);
            let is_hovered = list_state.hovered_option == Some(i);

            let bounds = Rectangle {
                x: bounds.x,
                y: bounds.y + option_height * i as f32,
                width: bounds.width,
                height: self.text_size + self.padding.y(),
            };

            let status = if is_selected {
                Status::Selected
            } else if is_hovered {
                Status::Hovered
            } else {
                Status::Active
            };
            let style = <Theme as Catalog>::style(theme, self.class, status);

            if (is_selected || is_hovered) && (bounds.width > 0.) && (bounds.height > 0.) {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds,
                        border: Border {
                            radius: (0.0).into(),
                            width: 0.0,
                            color: Color::TRANSPARENT,
                        },
                        ..renderer::Quad::default()
                    },
                    style.background,
                );
            }

            renderer.fill_text(
                text::Text {
                    content: self.options[i].to_string(),
                    bounds: Size::new(f32::INFINITY, bounds.height),
                    size: self.text_size.into(),
                    font: self.font,
                    align_x: text::Alignment::Left,
                    align_y: Vertical::Center,
                    line_height: LineHeight::default(),
                    shaping: Shaping::Advanced,
                    wrapping: Wrapping::default(),
                    ellipsis: Ellipsis::None,
                    hint_factor: renderer.hint_factor(),
                },
                Point::new(bounds.x, bounds.center_y()),
                style.text_color,
                bounds,
            );
        }
    }

    fn operate(
        &mut self,
        _state: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn iced_core::widget::Operation,
    ) {
        // Expose every option's text to operations (focus, measurement,
        // accessibility) by presenting virtual Text widgets.
        let bounds = layout.bounds();
        let option_height = self.text_size + self.padding.y();

        for (i, option) in self.options.iter().enumerate() {
            let text_widget =
                iced_core::widget::text::Text::<Theme, Renderer>::new(option.to_string())
                    .size(self.text_size)
                    .font(self.font);

            // A node with just the size, at this option's offset.
            let text_node = Node::new(Size::new(bounds.width, option_height));
            let text_layout = layout::Layout::with_offset(
                iced_core::Vector::new(0.0, option_height * i as f32),
                &text_node,
            );

            let mut element: Element<(), Theme, Renderer> = Element::new(text_widget);
            let mut text_tree = Tree::new(element.as_widget());
            element
                .as_widget_mut()
                .operate(&mut text_tree, text_layout, renderer, operation);
        }
    }
}

impl<'a, 'c, T, Message, Theme, Renderer> From<List<'a, 'c, T, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    T: Clone + Display + Eq + Hash,
    'c: 'a,
    Message: 'a,
    Renderer: 'a + renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: 'a + Catalog + iced_core::widget::text::Catalog,
{
    fn from(list: List<'a, 'c, T, Message, Theme, Renderer>) -> Self {
        Element::new(list)
    }
}
