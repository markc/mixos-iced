// SPDX-License-Identifier: MIT OR Apache-2.0
//! A column whose rows all take the width of the widest row:
//! [`FlushColumn`]. With `Alignment::Start` the last element of each row
//! is flushed to the right edge; with `End` the first element is flushed
//! to the left — the shape of a settings list, where a control at the
//! end of each row lines up however long the labels are.

use iced_core::Widget;
use iced_core::alignment;
use iced_core::event::Event;
use iced_core::layout::{self, Node};
use iced_core::widget::{Operation, tree::Tree};
use iced_core::{
    Alignment, Element, Layout, Length, Padding, Pixels, Point, Rectangle, Shell, Size, Vector,
    mouse, overlay, renderer,
};

/// A container that distributes its contents vertically, sizing every row
/// to the widest one and flushing the row's end (or start) element.
#[allow(missing_debug_implementations)]
pub struct FlushColumn<'a, Message, Theme, Renderer> {
    spacing: Pixels,
    padding: Padding,
    width: Length,
    height: Length,
    max_width: f32,
    align: Alignment,
    clip: bool,
    children: Vec<Element<'a, Message, Theme, Renderer>>,
    flush: bool,
}

impl<'a, Message: 'a, Theme: 'a, Renderer> FlushColumn<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer + 'a,
{
    /// Creates an empty [`FlushColumn`].
    #[must_use]
    pub fn new() -> Self {
        Self::from_vec(Vec::new())
    }

    /// Creates a [`FlushColumn`] from an iterator of rows.
    #[must_use]
    pub fn from_children(
        children: impl IntoIterator<Item = Element<'a, Message, Theme, Renderer>>,
    ) -> Self {
        children.into_iter().fold(Self::new(), Self::push)
    }

    /// Creates a [`FlushColumn`] with the given capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self::from_vec(Vec::with_capacity(capacity))
    }

    /// Creates a [`FlushColumn`] from an already allocated [`Vec`] of rows.
    #[must_use]
    pub fn from_vec(children: Vec<Element<'a, Message, Theme, Renderer>>) -> Self {
        Self {
            spacing: Pixels::ZERO,
            padding: Padding::ZERO,
            width: Length::Fit,
            height: Length::Fit,
            max_width: f32::INFINITY,
            align: Alignment::Start,
            clip: false,
            children,
            flush: true,
        }
    }

    /// Sets the vertical spacing between elements.
    #[must_use]
    pub fn spacing(mut self, amount: impl Into<Pixels>) -> Self {
        self.spacing = amount.into();
        self
    }

    /// Sets the padding of the [`FlushColumn`].
    #[must_use]
    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.padding = padding.into();
        self
    }

    /// Sets the width of the [`FlushColumn`].
    #[must_use]
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Sets the height of the [`FlushColumn`].
    #[must_use]
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// Sets the maximum width of the [`FlushColumn`].
    #[must_use]
    pub fn max_width(mut self, max_width: impl Into<Pixels>) -> Self {
        self.max_width = max_width.into().0;
        self
    }

    /// Sets the horizontal alignment of the rows' contents.
    #[must_use]
    pub fn align_x(mut self, align: impl Into<alignment::Vertical>) -> Self {
        self.align = Alignment::from(align.into());
        self
    }

    /// Sets whether overflowing contents are clipped.
    #[must_use]
    pub fn clip(mut self, clip: bool) -> Self {
        self.clip = clip;
        self
    }

    /// Sets whether the end element flushes to the end (for `Start`
    /// alignment) or the start element to the start (for `End`).
    #[must_use]
    pub fn flush(mut self, flush: bool) -> Self {
        self.flush = flush;
        self
    }

    /// Adds a row.
    #[must_use]
    pub fn push(mut self, child: impl Into<Element<'a, Message, Theme, Renderer>>) -> Self {
        let child = child.into();

        if !child.as_widget().is_void() {
            self.children.push(child);
        }

        self
    }

    /// Adds a row, if `Some`.
    #[must_use]
    pub fn push_maybe(
        self,
        child: Option<impl Into<Element<'a, Message, Theme, Renderer>>>,
    ) -> Self {
        if let Some(child) = child {
            self.push(child)
        } else {
            self
        }
    }

    /// Adds every row from the iterator.
    #[must_use]
    pub fn extend(
        self,
        children: impl IntoIterator<Item = Element<'a, Message, Theme, Renderer>>,
    ) -> Self {
        children.into_iter().fold(self, Self::push)
    }
}

impl<'a, Message: 'a, Theme: 'a, Renderer> Default for FlushColumn<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer + 'a,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<'a, Message: 'a, Theme: 'a, Renderer> Widget<Message, Theme, Renderer>
    for FlushColumn<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer,
{
    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(&mut self.children);

        if self.width.is_fit() || self.height.is_fit() {
            for child in &self.children {
                let size = child.as_widget().size();

                self.width = self.width.cross(size.width);
                self.height = self.height.stack(size.height);
            }
        }
    }

    fn size(&self) -> Size<Length> {
        Size {
            width: self.width,
            height: self.height,
        }
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let limits = limits.width(self.width.max(self.max_width));
        let node = layout::flex::resolve(
            layout::flex::Axis::Vertical,
            renderer,
            &limits,
            self.width,
            self.height,
            self.padding,
            self.spacing.0,
            self.align,
            &mut self.children,
            &mut tree.children,
        );
        // Every row is as wide as the widest; each row's flush element
        // moves by the difference, so the flush edges line up.
        let mut container_x = f32::MAX;
        let mut container_width = 0.0f32;
        for row in node.children() {
            if row.size().width > container_width {
                container_width = row.size().width;
            }
            if row.bounds().x < container_x {
                container_x = row.bounds().x;
            }
        }
        let mut children = Vec::<Node>::new();
        for row in node.children() {
            let mut row_children = Vec::<Node>::new();
            let bounds = row.bounds();
            let width_diff = container_width - bounds.width;
            if !row.children().is_empty() {
                for element in row.children() {
                    let bounds = element.bounds();
                    let x = bounds.x
                        + match self.align {
                            Alignment::Start => 0.0,
                            Alignment::Center => width_diff / 2.0,
                            Alignment::End => width_diff,
                        };
                    let mut element_node =
                        Node::with_children(element.size(), element.children().to_owned());
                    element_node.move_to_mut(Point::new(x, bounds.y));
                    row_children.push(element_node);
                }
                if self.flush && row_children.len() > 1 {
                    match self.align {
                        Alignment::Start => {
                            let element = row_children.last().expect("Always exists.");
                            let bounds = element.bounds();
                            let mut position = bounds.position();
                            let mut element_node =
                                Node::with_children(bounds.size(), element.children().to_owned());
                            position.x += width_diff;
                            element_node.move_to_mut(position);
                            let node = row_children.last_mut().expect("Always exists.");
                            *node = element_node;
                        }
                        Alignment::Center => {}
                        Alignment::End => {
                            let element = row_children.first().expect("Always exists.");
                            let bounds = element.bounds();
                            let mut position = bounds.position();
                            let mut element_node =
                                Node::with_children(bounds.size(), element.children().to_owned());
                            position.x -= width_diff;
                            element_node.move_to_mut(position);
                            let node = row_children.first_mut().expect("Always exists.");
                            *node = element_node;
                        }
                    }
                }
            }
            let mut row_node =
                Node::with_children(Size::new(container_width, row.size().height), row_children);
            row_node.move_to_mut(Point::new(container_x, bounds.y));
            children.push(row_node);
        }
        Node::with_children(node.size(), children)
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
            self.children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
                .for_each(|((child, state), layout)| {
                    child
                        .as_widget_mut()
                        .operate(state, layout, renderer, operation);
                });
        });
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
        for ((child, state), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                state, event, layout, cursor, renderer, shell, viewport,
            );
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        if self.clip {
            let bounds = layout.bounds();
            let clip = bounds.intersection(viewport).unwrap_or(*viewport);

            renderer.with_layer(clip, |renderer| {
                self.draw_children(tree, renderer, theme, style, layout, cursor, &clip);
            });
        } else {
            self.draw_children(tree, renderer, theme, style, layout, cursor, viewport);
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, state), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(state, layout, cursor, viewport, renderer)
            })
            .fold(mouse::Interaction::None, mouse::Interaction::max)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
            .find_map(|((child, state), layout)| {
                child
                    .as_widget_mut()
                    .overlay(state, layout, renderer, viewport, translation)
            })
    }
}

impl<'a, Message, Theme, Renderer> FlushColumn<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer,
{
    fn draw_children(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        for ((child, state), layout) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
        {
            child
                .as_widget()
                .draw(state, renderer, theme, style, layout, cursor, viewport);
        }
    }
}

impl<'a, Message: 'a, Theme: 'a, Renderer> FromIterator<Element<'a, Message, Theme, Renderer>>
    for FlushColumn<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer + 'a,
{
    fn from_iter<T: IntoIterator<Item = Element<'a, Message, Theme, Renderer>>>(iter: T) -> Self {
        Self::from_children(iter)
    }
}

impl<'a, Message, Theme, Renderer> From<FlushColumn<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: iced_core::Renderer + 'a,
{
    fn from(column: FlushColumn<'a, Message, Theme, Renderer>) -> Self {
        Element::new(column)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_renderer::LayoutRenderer;
    use iced_widget::{Row, Space, Text};

    #[test]
    fn every_row_takes_the_widest_rows_width() {
        let rows: Vec<Element<'_, (), iced_core::Theme, LayoutRenderer>> = vec![
            Row::new().push(Text::new("short")).into(),
            Row::new().push(Text::new("a considerably longer row")).into(),
            Row::new().push(Space::new()).into(),
        ];
        let column = FlushColumn::from_vec(rows);
        let renderer = LayoutRenderer::new();
        let mut element: Element<'_, (), iced_core::Theme, LayoutRenderer> = column.into();
        let mut tree = Tree::new(&element);
        element.as_widget_mut().diff(&mut tree);
        let limits = layout::Limits::new(Size::ZERO, Size::new(400.0, f32::INFINITY));
        let node = element.as_widget_mut().layout(&mut tree, &renderer, &limits);
        let widths: Vec<f32> = Layout::new(&node)
            .children()
            .map(|row| row.bounds().width)
            .collect();
        assert_eq!(widths.len(), 3);
        assert!(
            (widths[0] - widths[1]).abs() < f32::EPSILON
                && (widths[1] - widths[2]).abs() < f32::EPSILON,
            "all rows share one width: {widths:?}"
        );
        assert!(widths[0] > 0.0);
    }
}
