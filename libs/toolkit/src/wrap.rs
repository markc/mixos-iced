// SPDX-License-Identifier: MIT OR Apache-2.0
//! A container that distributes its children in multiple horizontal or
//! vertical runs: [`Wrap`] flows like text — items fill a line, then wrap
//! onto the next.
//!
//! Unlike a grid, no item is pinned to a column: a long item simply starts
//! the next line. Alignment is per line (each line's items align with each
//! other, not across lines), which is what a flow layout means.

use iced_core::layout::{Limits, Node};
use iced_core::mouse::{self, Cursor};
use iced_core::overlay;
use iced_core::renderer;
use iced_core::widget::{Operation, Tree};
use iced_core::{
    Alignment, Element, Event, Layout, Length, Padding, Pixels, Point, Rectangle, Shell, Size,
    Vector, Widget,
};
use std::marker::PhantomData;

/// A container that distributes its contents horizontally, wrapping onto a
/// new line when the next item would overflow.
#[allow(missing_debug_implementations)]
pub struct Wrap<'a, Message, Direction, Theme, Renderer> {
    /// The elements to distribute.
    pub elements: Vec<Element<'a, Message, Theme, Renderer>>,
    /// The alignment of the [`Wrap`].
    pub alignment: Alignment,
    /// The width of the [`Wrap`].
    pub width: Length,
    /// The height of the [`Wrap`].
    pub height: Length,
    /// The maximum width of the [`Wrap`].
    pub max_width: f32,
    /// The maximum height of the [`Wrap`].
    pub max_height: f32,
    /// The padding of each element of the [`Wrap`].
    pub padding: Padding,
    /// The spacing between each element of the [`Wrap`].
    pub spacing: Pixels,
    /// The spacing between each line of the [`Wrap`].
    pub line_spacing: Pixels,
    /// The minimal length of each line of the [`Wrap`].
    pub line_minimal_length: f32,
    #[allow(clippy::missing_docs_in_private_items)]
    _direction: PhantomData<Direction>,
}

impl<'a, Message, Theme, Renderer> Wrap<'a, Message, direction::Horizontal, Theme, Renderer> {
    /// Creates an empty horizontal [`Wrap`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_elements(Vec::new())
    }

    /// Creates a horizontal [`Wrap`] with the given elements.
    #[must_use]
    pub fn with_elements(elements: Vec<Element<'a, Message, Theme, Renderer>>) -> Self {
        Self {
            elements,
            ..Wrap::default()
        }
    }
}

impl<'a, Message, Theme, Renderer> Wrap<'a, Message, direction::Vertical, Theme, Renderer> {
    /// Creates an empty vertical [`Wrap`].
    #[must_use]
    pub fn new_vertical() -> Self {
        Self::with_elements_vertical(Vec::new())
    }

    /// Creates a vertical [`Wrap`] with the given elements.
    #[must_use]
    pub fn with_elements_vertical(elements: Vec<Element<'a, Message, Theme, Renderer>>) -> Self {
        Self {
            elements,
            ..Wrap::default()
        }
    }
}

impl<'a, Message, Renderer, Direction, Theme> Wrap<'a, Message, Direction, Theme, Renderer> {
    /// Sets the spacing of the [`Wrap`].
    #[must_use]
    pub fn spacing(mut self, spacing: impl Into<Pixels>) -> Self {
        self.spacing = spacing.into();
        self
    }

    /// Sets the spacing of the lines of the [`Wrap`].
    #[must_use]
    pub fn line_spacing(mut self, spacing: impl Into<Pixels>) -> Self {
        self.line_spacing = spacing.into();
        self
    }

    /// Sets the minimal length of the lines of the [`Wrap`].
    #[must_use]
    pub const fn line_minimal_length(mut self, units: f32) -> Self {
        self.line_minimal_length = units;
        self
    }

    /// Sets the padding of the elements in the [`Wrap`].
    #[must_use]
    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.padding = padding.into();
        self
    }

    /// Sets the width of the [`Wrap`].
    #[must_use]
    pub const fn width_items(mut self, width: Length) -> Self {
        self.width = width;
        self
    }

    /// Sets the height of the [`Wrap`].
    #[must_use]
    pub const fn height_items(mut self, height: Length) -> Self {
        self.height = height;
        self
    }

    /// Sets the maximum width of the [`Wrap`].
    #[must_use]
    pub const fn max_width(mut self, max_width: f32) -> Self {
        self.max_width = max_width;
        self
    }

    /// Sets the maximum height of the [`Wrap`].
    #[must_use]
    pub const fn max_height(mut self, max_height: f32) -> Self {
        self.max_height = max_height;
        self
    }

    /// Sets the alignment of the [`Wrap`].
    #[must_use]
    pub const fn align_items(mut self, align: Alignment) -> Self {
        self.alignment = align;
        self
    }

    /// Pushes an [`Element`] to the [`Wrap`].
    #[must_use]
    pub fn push<E>(mut self, element: E) -> Self
    where
        E: Into<Element<'a, Message, Theme, Renderer>>,
    {
        self.elements.push(element.into());
        self
    }
}

impl<Message, Renderer, Direction, Theme> Widget<Message, Theme, Renderer>
    for Wrap<'_, Message, Direction, Theme, Renderer>
where
    Self: WrapLayout<Renderer>,
    Renderer: renderer::Renderer,
{
    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(&mut self.elements);
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> Node {
        self.inner_layout(tree, renderer, limits)
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
        self.elements
            .iter_mut()
            .zip(&mut state.children)
            .zip(layout.children())
            .for_each(|((child, state), layout)| {
                child
                    .as_widget_mut()
                    .update(state, event, layout, cursor, renderer, shell, viewport);
            });
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.elements
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
            .find_map(|((child, state), layout)| {
                child
                    .as_widget_mut()
                    .overlay(state, layout, renderer, viewport, translation)
            })
    }

    fn mouse_interaction(
        &self,
        state: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.elements
            .iter()
            .zip(&state.children)
            .zip(layout.children())
            .map(|((child, state), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(state, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
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
        for ((child, state), layout) in self
            .elements
            .iter()
            .zip(&state.children)
            .zip(layout.children())
        {
            child
                .as_widget()
                .draw(state, renderer, theme, style, layout, cursor, viewport);
        }
    }

    fn operate(
        &mut self,
        state: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        for ((element, state), layout) in self
            .elements
            .iter_mut()
            .zip(&mut state.children)
            .zip(layout.children())
        {
            element
                .as_widget_mut()
                .operate(state, layout, renderer, operation);
        }
    }
}

impl<'a, Message, Theme, Renderer> From<Wrap<'a, Message, direction::Vertical, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Renderer: 'a + renderer::Renderer,
    Message: 'a,
    Theme: 'a,
{
    fn from(wrap: Wrap<'a, Message, direction::Vertical, Theme, Renderer>) -> Self {
        Element::new(wrap)
    }
}

impl<'a, Message, Theme, Renderer> From<Wrap<'a, Message, direction::Horizontal, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Renderer: 'a + renderer::Renderer,
    Message: 'a,
    Theme: 'a,
{
    fn from(wrap: Wrap<'a, Message, direction::Horizontal, Theme, Renderer>) -> Self {
        Element::new(wrap)
    }
}

impl<Message, Renderer, Direction, Theme> Default
    for Wrap<'_, Message, Direction, Theme, Renderer>
{
    fn default() -> Self {
        Self {
            elements: vec![],
            alignment: Alignment::Start,
            width: Length::Shrink,
            height: Length::Shrink,
            max_width: 4_294_967_295.0,
            max_height: 4_294_967_295.0,
            padding: Padding::ZERO,
            spacing: Pixels::ZERO,
            line_spacing: Pixels::ZERO,
            line_minimal_length: 10.0,
            _direction: PhantomData,
        }
    }
}

/// The inner layout of the [`Wrap`] for one direction.
pub trait WrapLayout<Renderer>
where
    Renderer: renderer::Renderer,
{
    /// The inner layout of the [`Wrap`].
    fn inner_layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> Node;
}

impl<'a, Message, Theme, Renderer> WrapLayout<Renderer>
    for Wrap<'a, Message, direction::Horizontal, Theme, Renderer>
where
    Renderer: renderer::Renderer + 'a,
{
    #[inline(always)]
    fn inner_layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> Node {
        let padding = self.padding;
        let spacing = self.spacing;
        let line_spacing = self.line_spacing;
        let line_minimal_length = self.line_minimal_length;
        let limits = limits
            .shrink(padding)
            .width(self.width.max(self.max_width))
            .height(self.height.max(self.max_height));
        let max_width = limits.max().width;

        let mut children = tree.children.iter_mut();
        let mut curse = padding.left;
        let mut deep_curse = padding.left;
        let mut current_line_height = line_minimal_length;
        let mut max_main = curse;
        let mut align = vec![];
        let mut start = 0;
        let mut end = 0;
        let mut nodes: Vec<Node> = self
            .elements
            .iter_mut()
            .map(|elem| {
                let node_limit =
                    Limits::new(Size::new(limits.min().width, line_minimal_length), limits.max());
                let mut node = elem
                    .as_widget_mut()
                    .layout(
                        children.next().expect("wrap missing expected child"),
                        renderer,
                        &node_limit,
                    );

                let size = node.size();

                let offset_init = size.width + spacing.0;
                let offset = curse + offset_init;

                if offset > max_width {
                    deep_curse += current_line_height + line_spacing.0;
                    align.push((start..end, current_line_height));
                    start = end;
                    end += 1;
                    current_line_height = line_minimal_length;
                    node.move_to_mut(Point::new(padding.left, deep_curse));
                    curse = offset_init + padding.left;
                } else {
                    node.move_to_mut(Point::new(curse, deep_curse));
                    curse = offset;
                    end += 1;
                }
                current_line_height = current_line_height.max(size.height);
                max_main = max_main.max(curse);

                node
            })
            .collect();
        if end != start {
            align.push((start..end, current_line_height));
        }
        for (range, max_length) in align {
            nodes[range].iter_mut().for_each(|node| {
                let size = node.size();
                let space = Size::new(size.width, max_length);
                node.align_mut(Alignment::Start, self.alignment, space);
            });
        }
        let (width, height) = (
            max_main - padding.left,
            deep_curse - padding.left + current_line_height,
        );
        let size = limits.resolve(self.width, self.height, Size::new(width, height));

        Node::with_children(size.expand(padding), nodes)
    }
}

impl<'a, Message, Theme, Renderer> WrapLayout<Renderer>
    for Wrap<'a, Message, direction::Vertical, Theme, Renderer>
where
    Renderer: renderer::Renderer + 'a,
{
    #[inline(always)]
    fn inner_layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> Node {
        let padding = self.padding;
        let spacing = self.spacing;
        let line_spacing = self.line_spacing;
        let line_minimal_length = self.line_minimal_length;
        let limits = limits
            .shrink(padding)
            .width(self.width.max(self.max_width))
            .height(self.height.max(self.max_height));
        let max_height = limits.max().height;

        let mut children = tree.children.iter_mut();
        let mut curse = padding.left;
        let mut wide_curse = padding.left;
        let mut current_line_width = line_minimal_length;
        let mut max_main = curse;
        let mut align = vec![];
        let mut start = 0;
        let mut end = 0;
        let mut nodes: Vec<Node> = self
            .elements
            .iter_mut()
            .map(|elem| {
                let node_limit = Limits::new(
                    Size::new(line_minimal_length, limits.min().height),
                    limits.max(),
                );
                let mut node = elem
                    .as_widget_mut()
                    .layout(
                        children.next().expect("wrap missing expected child"),
                        renderer,
                        &node_limit,
                    );

                let size = node.size();

                let offset_init = size.height + spacing.0;
                let offset = curse + offset_init;

                if offset > max_height {
                    wide_curse += current_line_width + line_spacing.0;
                    align.push((start..end, current_line_width));
                    start = end;
                    end += 1;
                    current_line_width = line_minimal_length;
                    node = node.move_to(Point::new(wide_curse, padding.left));
                    curse = offset_init + padding.left;
                } else {
                    node = node.move_to(Point::new(wide_curse, curse));
                    end += 1;
                    curse = offset;
                }
                current_line_width = current_line_width.max(size.width);
                max_main = max_main.max(curse);

                node
            })
            .collect();
        if end != start {
            align.push((start..end, current_line_width));
        }

        for (range, max_length) in align {
            nodes[range].iter_mut().for_each(|node| {
                let size = node.size();
                let space = Size::new(max_length, size.height);
                node.align_mut(self.alignment, Alignment::Start, space);
            });
        }

        let (width, height) = (
            wide_curse - padding.left + current_line_width,
            max_main - padding.left,
        );
        let size = limits.resolve(self.width, self.height, Size::new(width, height));

        Node::with_children(size.expand(padding), nodes)
    }
}

/// The direction a [`Wrap`] flows in.
pub mod direction {
    /// A vertical direction of the [`Wrap`](super::Wrap).
    #[derive(Debug)]
    pub struct Vertical;

    /// A horizontal direction of the [`Wrap`](super::Wrap).
    #[derive(Debug)]
    pub struct Horizontal;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_renderer::LayoutRenderer;
    use iced_widget::Space;

    type TestWrap<'a> =
        Wrap<'a, (), direction::Horizontal, iced_core::Theme, LayoutRenderer>;

    fn wrap_with(widths: &[f32], limit: f32) -> layout::Node {
        let mut wrap = TestWrap::new();
        for width in widths {
            wrap = wrap.push(Space::new().width(*width).height(20.0));
        }
        let renderer = LayoutRenderer::new();
        let mut tree = Tree::new(&wrap);
        let limits = Limits::new(Size::ZERO, Size::new(limit, f32::INFINITY));
        Widget::<(), iced_core::Theme, LayoutRenderer>::layout(
            &mut wrap,
            &mut tree,
            &renderer,
            &limits,
        )
    }

    #[test]
    fn items_that_fit_stay_on_one_line() {
        let node = wrap_with(&[50.0, 50.0], 200.0);
        let laid: Vec<_> = Layout::new(&node).children().collect();
        assert_eq!(laid.len(), 2);
        assert_eq!(laid[0].bounds().y, laid[1].bounds().y, "same line");
        assert_eq!(laid[1].bounds().x - laid[0].bounds().x, 50.0);
    }

    #[test]
    fn overflowing_items_wrap_onto_the_next_line() {
        let node = wrap_with(&[80.0, 80.0], 100.0);
        let laid: Vec<_> = Layout::new(&node).children().collect();
        assert_eq!(laid.len(), 2);
        assert!(laid[1].bounds().y > laid[0].bounds().y, "second line");
    }

    #[test]
    fn the_wrap_size_holds_every_line() {
        let node = wrap_with(&[80.0, 80.0], 100.0);
        let bounds = node.bounds();
        assert_eq!(bounds.height, 40.0, "two 20 px lines");
        assert_eq!(bounds.width, 80.0, "widest line");
    }
}
