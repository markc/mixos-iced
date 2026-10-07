// SPDX-License-Identifier: MIT OR Apache-2.0
//! The inner rows of a [`SelectionList`](super::SelectionList): hover and
//! selection tracking, viewport-limited drawing, virtual text operations.

use std::collections::hash_map::DefaultHasher;
use std::fmt::Display;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;

use iced_core::alignment::Vertical;
use iced_core::layout::Layout;
use iced_core::layout::{self, Limits, Node};
use iced_core::mouse::{self, Cursor};
use iced_core::renderer;
use iced_core::text;
use iced_core::widget::Tree;
use iced_core::widget::text::{Ellipsis, LineHeight, Shaping, Wrapping};
use iced_core::widget::tree::{State, Tag};
use iced_core::{Border, Color, Element, Event, Length, Point, Rectangle, Shell, Size, Widget};

use super::{Catalog, Status};
use crate::typography::TextStyle;

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

impl<'a, 'c, T, Message, Theme, Renderer> List<'a, 'c, T, Message, Theme, Renderer>
where
    T: Clone + Display + Eq + Hash,
    [T]: ToOwned<Owned = Vec<T>>,
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog,
{
    /// The same rows with the given absolute line height: one prepared row
    /// height drives layout, hit testing, drawing and operations, never
    /// shorter than the text size.
    #[must_use]
    pub fn line_height(
        self,
        line_height: impl Into<f32>,
    ) -> StyledList<'a, 'c, T, Message, Theme, Renderer> {
        StyledList {
            list: self,
            line_height: Some(line_height.into()),
        }
    }

    /// The same rows with a prepared text style: font, size and line height
    /// together. Without a line height the rows grow to the 1.3 default
    /// factor of the text size.
    #[must_use]
    pub fn text_style(
        mut self,
        text: TextStyle,
    ) -> StyledList<'a, 'c, T, Message, Theme, Renderer> {
        self.font = text.font;
        self.text_size = text.size;
        StyledList {
            list: self,
            line_height: text.line_height,
        }
    }
}

/// [`List`] rows with a prepared text style (see [`List::line_height`] and
/// [`List::text_style`]). The one row height drives layout, hit testing,
/// visible rows and virtual operations.
#[allow(missing_debug_implementations)]
pub struct StyledList<'a, 'c, T: 'a, Message, Theme, Renderer>
where
    T: Clone + Display + Eq + Hash,
    [T]: ToOwned<Owned = Vec<T>>,
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog,
{
    list: List<'a, 'c, T, Message, Theme, Renderer>,
    line_height: Option<f32>,
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

// One row engine, shared by the legacy [`List`] and the prepared
// [`StyledList`]: they differ only in the row height and the drawn line
// height, and keep the same [`ListState`] tree.
trait Rows<'a, 'c, T, Message, Theme, Renderer>
where
    T: Clone + Display + Eq + Hash + 'a,
    [T]: ToOwned<Owned = Vec<T>>,
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog + iced_core::widget::text::Catalog + 'a,
    'c: 'a,
{
    fn options(&self) -> &'a [T];
    fn font(&self) -> Renderer::Font;
    fn text_size(&self) -> f32;
    fn padding(&self) -> iced_core::Padding;
    fn row_height(&self) -> f32;
    fn text_line_height(&self) -> LineHeight;
    fn selected(&self) -> Option<usize>;
    fn on_selected(&self) -> &dyn Fn(usize, T) -> Message;
    fn class(&self) -> &'a <Theme as Catalog>::Class<'c>;

    fn tag(&self) -> Tag {
        Tag::of::<ListState>()
    }

    fn state(&self) -> State {
        State::new(ListState::default())
    }

    fn diff(&mut self, tree: &mut Tree) {
        let list_state = tree.state.downcast_mut::<ListState>();

        if let Some(id) = self.selected() {
            if let Some(option) = self.options().get(id) {
                let mut hasher = DefaultHasher::new();
                option.hash(&mut hasher);

                list_state.last_selected_index = Some((id, hasher.finish()));
            } else {
                list_state.last_selected_index = None;
            }
        } else if let Some((id, hash)) = list_state.last_selected_index
            && let Some(option) = self.options().get(id)
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
            self.row_height() * self.options().len() as f32,
        );

        Node::new(intrinsic)
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        shell: &mut Shell<'_, Message>,
    ) {
        let bounds = layout.bounds();
        let list_state = tree.state.downcast_mut::<ListState>();
        let cursor = cursor.position().unwrap_or_default();

        if bounds.contains(cursor) {
            match event {
                Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                    list_state.hovered_option =
                        Some(((cursor.y - bounds.y) / self.row_height()) as usize);

                    shell.request_redraw();
                }
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                | Event::Touch(iced_core::touch::Event::FingerPressed { .. }) => {
                    list_state.hovered_option =
                        Some(((cursor.y - bounds.y) / self.row_height()) as usize);

                    if let Some(index) = list_state.hovered_option
                        && let Some(option) = self.options().get(index)
                    {
                        let mut hasher = DefaultHasher::new();
                        option.hash(&mut hasher);
                        list_state.last_selected_index = Some((index, hasher.finish()));
                    }

                    if let Some((index, _)) = list_state.last_selected_index
                        && let Some(option) = self.options().get(index)
                    {
                        shell.publish((self.on_selected())(index, option.clone()));
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

    fn mouse_interaction(&self, layout: Layout<'_>, cursor: Cursor) -> mouse::Interaction {
        let bounds = layout.bounds();

        if bounds.contains(cursor.position().unwrap_or_default()) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        layout: Layout<'_>,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let option_height = self.row_height();
        let offset = viewport.y - bounds.y;
        let start = (offset / option_height) as usize;
        let end = ((offset + viewport.height) / option_height).ceil() as usize;
        let list_state = tree.state.downcast_ref::<ListState>();

        for i in start..end.min(self.options().len()) {
            let is_selected = list_state.last_selected_index.is_some_and(|u| u.0 == i);
            let is_hovered = list_state.hovered_option == Some(i);

            let bounds = Rectangle {
                x: bounds.x,
                y: bounds.y + option_height * i as f32,
                width: bounds.width,
                height: option_height,
            };

            let status = if is_selected {
                Status::Selected
            } else if is_hovered {
                Status::Hovered
            } else {
                Status::Active
            };
            let style = <Theme as Catalog>::style(theme, self.class(), status);

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
                    content: self.options()[i].to_string(),
                    bounds: Size::new(f32::INFINITY, bounds.height),
                    size: self.text_size().into(),
                    font: self.font(),
                    align_x: text::Alignment::Left,
                    align_y: Vertical::Center,
                    line_height: self.text_line_height(),
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
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn iced_core::widget::Operation,
    ) {
        // Expose every option's text to operations (focus, measurement,
        // accessibility) by presenting virtual Text widgets.
        let bounds = layout.bounds();
        let option_height = self.row_height();

        for (i, option) in self.options().iter().enumerate() {
            let text_widget =
                iced_core::widget::text::Text::<Theme, Renderer>::new(option.to_string())
                    .size(self.text_size())
                    .font(self.font())
                    .line_height(self.text_line_height());

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

impl<'a, 'c, T, Message, Theme, Renderer> Rows<'a, 'c, T, Message, Theme, Renderer>
    for List<'a, 'c, T, Message, Theme, Renderer>
where
    T: Clone + Display + Eq + Hash,
    [T]: ToOwned<Owned = Vec<T>>,
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog + iced_core::widget::text::Catalog + 'a,
    'c: 'a,
{
    fn options(&self) -> &'a [T] {
        self.options
    }
    fn font(&self) -> Renderer::Font {
        self.font
    }
    fn text_size(&self) -> f32 {
        self.text_size
    }
    fn padding(&self) -> iced_core::Padding {
        self.padding
    }
    // The legacy rows: the text size plus the vertical padding.
    fn row_height(&self) -> f32 {
        self.text_size + self.padding.y()
    }
    fn text_line_height(&self) -> LineHeight {
        LineHeight::default()
    }
    fn selected(&self) -> Option<usize> {
        self.selected
    }
    fn on_selected(&self) -> &dyn Fn(usize, T) -> Message {
        self.on_selected
    }
    fn class(&self) -> &'a <Theme as Catalog>::Class<'c> {
        self.class
    }
}

impl<'a, 'c, T, Message, Theme, Renderer> Rows<'a, 'c, T, Message, Theme, Renderer>
    for StyledList<'a, 'c, T, Message, Theme, Renderer>
where
    T: Clone + Display + Eq + Hash,
    [T]: ToOwned<Owned = Vec<T>>,
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog + iced_core::widget::text::Catalog + 'a,
    'c: 'a,
{
    fn options(&self) -> &'a [T] {
        self.list.options
    }
    fn font(&self) -> Renderer::Font {
        self.list.font
    }
    fn text_size(&self) -> f32 {
        self.list.text_size
    }
    fn padding(&self) -> iced_core::Padding {
        self.list.padding
    }
    // The prepared rows: the content height (never less than the text size;
    // an absent line height is the 1.3 default factor) plus the vertical
    // padding. The renderer still receives the requested line height
    // separately ([`Rows::text_line_height`]).
    fn row_height(&self) -> f32 {
        self.list
            .text_size
            .max(self.line_height.unwrap_or(self.list.text_size * 1.3))
            + self.list.padding.y()
    }
    fn text_line_height(&self) -> LineHeight {
        match self.line_height {
            Some(height) => LineHeight::Absolute(height.into()),
            None => LineHeight::default(),
        }
    }
    fn selected(&self) -> Option<usize> {
        self.list.selected
    }
    fn on_selected(&self) -> &dyn Fn(usize, T) -> Message {
        self.list.on_selected
    }
    fn class(&self) -> &'a <Theme as Catalog>::Class<'c> {
        self.list.class
    }
}

impl<'a, 'c, T, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for List<'a, 'c, T, Message, Theme, Renderer>
where
    T: Clone + Display + Eq + Hash,
    [T]: ToOwned<Owned = Vec<T>>,
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog + iced_core::widget::text::Catalog + 'a,
    'c: 'a,
{
    fn tag(&self) -> Tag {
        Rows::tag(self)
    }

    fn state(&self) -> State {
        Rows::state(self)
    }

    fn diff(&mut self, state: &mut Tree) {
        Rows::diff(self, state);
    }

    fn size(&self) -> Size<Length> {
        Rows::size(self)
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> Node {
        Rows::layout(self, tree, renderer, limits)
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
        Rows::update(self, state, event, layout, cursor, shell);
    }

    fn mouse_interaction(
        &self,
        _state: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        Rows::mouse_interaction(self, layout, cursor)
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
        Rows::draw(self, state, renderer, theme, layout, viewport);
    }

    fn operate(
        &mut self,
        _state: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn iced_core::widget::Operation,
    ) {
        Rows::operate(self, layout, renderer, operation);
    }
}

impl<'a, 'c, T, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for StyledList<'a, 'c, T, Message, Theme, Renderer>
where
    T: Clone + Display + Eq + Hash,
    [T]: ToOwned<Owned = Vec<T>>,
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog + iced_core::widget::text::Catalog + 'a,
    'c: 'a,
{
    fn tag(&self) -> Tag {
        Rows::tag(self)
    }

    fn state(&self) -> State {
        Rows::state(self)
    }

    fn diff(&mut self, state: &mut Tree) {
        Rows::diff(self, state);
    }

    fn size(&self) -> Size<Length> {
        Rows::size(self)
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> Node {
        Rows::layout(self, tree, renderer, limits)
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
        Rows::update(self, state, event, layout, cursor, shell);
    }

    fn mouse_interaction(
        &self,
        _state: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        Rows::mouse_interaction(self, layout, cursor)
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
        Rows::draw(self, state, renderer, theme, layout, viewport);
    }

    fn operate(
        &mut self,
        _state: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn iced_core::widget::Operation,
    ) {
        Rows::operate(self, layout, renderer, operation);
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

impl<'a, 'c, T, Message, Theme, Renderer> From<StyledList<'a, 'c, T, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    T: Clone + Display + Eq + Hash,
    'c: 'a,
    Message: 'a,
    Renderer: 'a + renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: 'a + Catalog + iced_core::widget::text::Catalog,
{
    fn from(list: StyledList<'a, 'c, T, Message, Theme, Renderer>) -> Self {
        Element::new(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_renderer::LayoutRenderer;

    type TestList<'a> = List<'a, 'a, String, String, iced_core::Theme, LayoutRenderer>;
    type TestStyled<'a> = StyledList<'a, 'a, String, String, iced_core::Theme, LayoutRenderer>;

    fn options() -> Vec<String> {
        vec![
            "Option 1".to_owned(),
            "Option 2".to_owned(),
            "Option 3".to_owned(),
        ]
    }

    fn list<'a>(options: &'a [String]) -> TestList<'a> {
        // Leak the (capture-free) class and callback so the returned rows
        // can borrow them for the caller's lifetime.
        let class: &'a <iced_core::Theme as Catalog>::Class<'a> =
            Box::leak(<iced_core::Theme as Catalog>::default());
        let on_selected: &'a dyn Fn(usize, String) -> String = Box::leak(
            Box::new(|_: usize, _: String| String::new())
                as Box<dyn Fn(usize, String) -> String>,
        );
        List {
            options,
            font: iced_core::Font::DEFAULT,
            class,
            on_selected,
            padding: 5.0.into(),
            text_size: 12.0,
            selected: None,
            phantomdata: PhantomData,
        }
    }

    fn layout_height<W>(mut rows: W, renderer: &LayoutRenderer) -> f32
    where
        W: Widget<String, iced_core::Theme, LayoutRenderer>,
    {
        let mut tree = Tree::new(&rows as &dyn Widget<String, iced_core::Theme, LayoutRenderer>);
        rows.diff(&mut tree);
        rows.layout(
            &mut tree,
            renderer,
            &Limits::new(Size::ZERO, Size::new(200.0, 200.0)),
        )
        .size()
        .height
    }

    #[test]
    fn the_legacy_literal_keeps_its_height_and_a_prepared_style_grows_rows() {
        let options = options();
        let renderer = LayoutRenderer::new();
        // The original `List` literal, unchanged: rows are the text size
        // plus the vertical padding.
        let rows = list(&options);
        assert_eq!(Widget::tag(&rows), Tag::of::<ListState>());
        assert_eq!(layout_height(rows, &renderer), (12.0 + 10.0) * 3.0);
        // A prepared line height drives the same, taller rows everywhere.
        let rows: TestStyled<'_> = list(&options).line_height(30.0_f32);
        assert_eq!(layout_height(rows, &renderer), (30.0 + 10.0) * 3.0);
        // A prepared style without a line height uses the 1.3 default
        // factor, unlike the legacy rows above.
        let rows: TestStyled<'_> = list(&options).text_style(TextStyle {
            font: iced_core::Font::DEFAULT,
            size: 12.0,
            line_height: None,
        });
        assert_eq!(layout_height(rows, &renderer), (12.0 * 1.3 + 10.0) * 3.0);
    }

    #[test]
    fn pointer_hover_maps_rows_with_the_prepared_line_height() {
        let options = options();
        let mut rows: TestStyled<'_> = list(&options).line_height(30.0_f32);
        let mut tree = Tree::new(&rows as &dyn Widget<String, iced_core::Theme, LayoutRenderer>);
        Widget::diff(&mut rows, &mut tree);
        let renderer = LayoutRenderer::new();
        let node = Widget::layout(
            &mut rows,
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, Size::new(200.0, 200.0)),
        );
        let mut messages = iced_core::shell::Bus::new();
        let mut shell = Shell::new(
            &iced_core::window::Headless,
            iced_core::shell::Waker::noop(),
            &mut messages,
        );
        Widget::update(
            &mut rows,
            &mut tree,
            &Event::Mouse(mouse::Event::CursorMoved {
                position: Point::new(5.0, 55.0),
            }),
            Layout::new(&node),
            mouse::Cursor::Available(Point::new(5.0, 55.0)),
            &renderer,
            &mut shell,
            &Rectangle::with_size(Size::new(200.0, 200.0)),
        );
        // Rows are 30 + 10 = 40px: y=55 is row 1 (the legacy 22px rows would
        // put it in row 2).
        assert_eq!(
            tree.state.downcast_ref::<ListState>().hovered_option,
            Some(1)
        );
    }
}
