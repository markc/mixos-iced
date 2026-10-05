// SPDX-License-Identifier: MIT OR Apache-2.0
//! A scrollable single-choice list: [`SelectionList`] shows every option
//! at once (unlike a drop-down, which hides them until opened), with hover
//! and selection highlighting from the theme.

pub mod list;

use iced_core::layout::{Limits, Node};
use iced_core::mouse::{self, Cursor};
use iced_core::renderer;
use iced_core::text::paragraph::{self, Paragraph};
use iced_core::text::Text;
use iced_core::widget::tree::{self, Tree};
use iced_core::widget::Operation;
use iced_core::{
    Border, Element, Event, Layout, Length, Padding, Rectangle, Shell, Size, Widget,
};
use iced_widget::container::{self, Container};
use iced_widget::scrollable::{self, Scrollable};
use iced_widget::text::{LineHeight, Wrapping};
use std::borrow::Borrow;
use std::fmt::Display;
use std::hash::Hash;
use std::marker::PhantomData;

pub use list::List;

/// The interaction status of a [`SelectionList`] row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    /// Idle.
    #[default]
    Active,
    /// The pointer is over the row.
    Hovered,
    /// The row is the chosen one.
    Selected,
}

/// The style of a [`SelectionList`]: queried per row status, so
/// `background` and `text_color` answer for the status asked about, and
/// `border` frames the whole list.
#[derive(Clone, Copy, Debug)]
pub struct Style {
    /// The background of the list, or of the row status asked about.
    pub background: iced_core::Background,
    /// The text color of the list, or of the row status asked about.
    pub text_color: iced_core::Color,
    /// The border of the list.
    pub border: Border,
}

/// The theme catalog of a [`SelectionList`].
pub trait Catalog {
    /// The item class of [`Catalog`].
    type Class<'a>;
    /// The default class produced by the [`Catalog`].
    fn default<'a>() -> Self::Class<'a>;
    /// The [`Style`] of a class with the given status.
    fn style(&self, class: &Self::Class<'_>, status: Status) -> Style;
}

/// A styling function for a [`SelectionList`].
pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme, Status) -> Style + 'a>;

/// A widget for selecting a single value from a scrollable list of options.
///
/// ```no_run
/// # use toolkit::selection_list::SelectionList;
/// #[derive(Clone)]
/// enum Message { Picked(usize, String) }
///
/// fn view(options: &[String]) -> iced_core::Element<'_, Message, toolkit::theme::Theme> {
///     SelectionList::new(options.to_vec(), Message::Picked).into()
/// }
/// ```
#[allow(missing_debug_implementations)]
pub struct SelectionList<'a, T, L, Message, Theme, Renderer>
where
    T: Clone + ToString + Eq + Hash,
    L: Borrow<[T]>,
    [T]: ToOwned<Owned = Vec<T>>,
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog + container::Catalog,
{
    /// The options to render.
    pub options: L,
    /// Called with the index and value of the option picked.
    on_selected: Box<dyn Fn(usize, T) -> Message>,
    /// The index of the selected option, if any.
    selected: Option<usize>,
    /// The label font.
    font: Renderer::Font,
    /// The width of the list.
    width: Length,
    /// The height of the list.
    height: Length,
    /// The row padding.
    padding: Padding,
    /// The text size.
    text_size: f32,
    /// The style class.
    class: <Theme as Catalog>::Class<'a>,
    #[allow(clippy::missing_docs_in_private_items)]
    phantomdata: PhantomData<&'a T>,
}

impl<'a, T, L, Message, Theme, Renderer> SelectionList<'a, T, L, Message, Theme, Renderer>
where
    Message: 'a + Clone,
    Renderer: 'a + renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: 'a
        + Catalog
        + container::Catalog
        + scrollable::Catalog
        + iced_core::widget::text::Catalog,
    T: Clone + Display + Eq + Hash,
    L: Borrow<[T]>,
    [T]: ToOwned<Owned = Vec<T>>,
{
    /// Creates a new [`SelectionList`] with the given options and the
    /// message produced when an option is picked.
    pub fn new(options: L, on_selected: impl Fn(usize, T) -> Message + 'static) -> Self {
        Self {
            options,
            on_selected: Box::new(on_selected),
            selected: None,
            font: Renderer::Font::default(),
            class: <Theme as Catalog>::default(),
            width: Length::Fill,
            height: Length::Fill,
            padding: 5.0.into(),
            text_size: 12.0,
            phantomdata: PhantomData,
        }
    }

    /// Sets the index of the selected option.
    #[must_use]
    pub fn selected(mut self, selected: Option<usize>) -> Self {
        self.selected = selected;
        self
    }

    /// Sets the font of the labels.
    #[must_use]
    pub fn font(mut self, font: impl Into<Renderer::Font>) -> Self {
        self.font = font.into();
        self
    }

    /// Sets the text size of the labels.
    #[must_use]
    pub fn text_size(mut self, text_size: impl Into<f32>) -> Self {
        self.text_size = text_size.into();
        self
    }

    /// Sets the row padding.
    #[must_use]
    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.padding = padding.into();
        self
    }

    /// Sets the width of the [`SelectionList`].
    #[must_use]
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Sets the height of the [`SelectionList`].
    #[must_use]
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// Sets the style of the [`SelectionList`].
    #[must_use]
    pub fn style(mut self, style: impl Fn(&Theme, Status) -> Style + 'a) -> Self
    where
        <Theme as Catalog>::Class<'a>: From<StyleFn<'a, Theme>>,
    {
        self.class = (Box::new(style) as StyleFn<'a, Theme>).into();
        self
    }

    /// Sets the style class of the [`SelectionList`].
    #[must_use]
    pub fn class(mut self, class: impl Into<<Theme as Catalog>::Class<'a>>) -> Self {
        self.class = class.into();
        self
    }

    /// Builds the internal scrollable list container from the options.
    fn list_container(&self) -> Container<'_, Message, Theme, Renderer> {
        Container::new(Scrollable::new(List {
            options: self.options.borrow(),
            font: self.font,
            text_size: self.text_size,
            padding: self.padding,
            class: &self.class,
            on_selected: self.on_selected.as_ref(),
            selected: self.selected,
            phantomdata: PhantomData,
        }))
        .padding(1)
    }
}

impl<'a, T, L, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for SelectionList<'a, T, L, Message, Theme, Renderer>
where
    T: 'a + Clone + ToString + Eq + Hash + Display,
    L: Borrow<[T]>,
    [T]: ToOwned<Owned = Vec<T>>,
    Message: 'static + Clone,
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font> + 'a,
    Theme: Catalog + container::Catalog + scrollable::Catalog + iced_core::widget::text::Catalog + 'a,
{
    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(&mut [&mut self.list_container() as &mut dyn Widget<_, _, _>]);
        let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();

        let options = self.options.borrow();
        if state.values.len() != options.len() {
            state.values = options
                .iter()
                .map(|_| paragraph::Plain::<Renderer::Paragraph>::default())
                .collect();
        }
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, Length::Shrink)
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<Renderer::Paragraph>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::<Renderer::Paragraph>::new(self.options.borrow()))
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> Node {
        let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();

        let limits = limits.width(self.width).height(self.height);

        let max_width = match self.width {
            Length::Shrink => self
                .options
                .borrow()
                .iter()
                .enumerate()
                .map(|(id, val)| {
                    let s: &str = &val.to_string();
                    let text = Text {
                        content: s,
                        size: self.text_size.into(),
                        line_height: LineHeight::default(),
                        bounds: Size::INFINITE,
                        font: self.font,
                        align_x: iced_core::text::Alignment::Left,
                        align_y: iced_core::alignment::Vertical::Top,
                        shaping: iced_core::text::Shaping::Advanced,
                        wrapping: Wrapping::default(),
                        ellipsis: iced_core::text::Ellipsis::None,
                        hint_factor: renderer.hint_factor(),
                    };

                    let _ = state.values[id].update(text);
                    (state.values[id].min_bounds().width + self.padding.x()).round() as u32
                })
                .max()
                .unwrap_or(100),
            _ => limits.max().width as u32,
        };

        let limits = limits.width(self.width.max(max_width as f32 + self.padding.x()));

        let content = self
            .list_container()
            .layout(&mut tree.children[0], renderer, &limits);
        let size = limits.resolve(self.width, self.height, content.size());
        Node::with_children(size, vec![content])
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
        self.list_container().update(
            &mut state.children[0],
            event,
            layout
                .children()
                .next()
                .expect("Scrollable Child Missing in Selection List"),
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
        self.list_container().mouse_interaction(
            &state.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
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
        let bounds = layout.bounds();
        let style_sheet = <Theme as Catalog>::style(theme, &self.class, Status::Active);

        if let Some(clipped_viewport) = bounds.intersection(viewport) {
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    border: style_sheet.border,
                    ..renderer::Quad::default()
                },
                style_sheet.background,
            );

            self.list_container().draw(
                &state.children[0],
                renderer,
                theme,
                style,
                layout
                    .children()
                    .next()
                    .expect("Scrollable Child Missing in Selection List"),
                cursor,
                &clipped_viewport,
            );
        }
    }

    fn operate(
        &mut self,
        state: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        Widget::<Message, Theme, Renderer>::operate(
            &mut self.list_container(),
            &mut state.children[0],
            layout
                .children()
                .next()
                .expect("Scrollable Child Missing in Selection List"),
            renderer,
            operation,
        );
    }
}

impl<'a, T, L, Message, Theme, Renderer> From<SelectionList<'a, T, L, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    T: 'a + Clone + ToString + Eq + Hash + Display,
    L: 'a + Borrow<[T]>,
    [T]: ToOwned<Owned = Vec<T>>,
    Message: 'static + Clone,
    Renderer: 'a + renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: 'a + Catalog + container::Catalog + scrollable::Catalog + iced_core::widget::text::Catalog,
{
    fn from(selection_list: SelectionList<'a, T, L, Message, Theme, Renderer>) -> Self {
        Element::new(selection_list)
    }
}

/// A paragraph cache to speed up layout.
#[derive(Default, Clone)]
pub struct State<P: Paragraph> {
    #[allow(clippy::missing_docs_in_private_items)]
    values: Vec<paragraph::Plain<P>>,
}

impl<P: Paragraph> State<P> {
    /// Creates a new [`State`] sized to the options.
    pub fn new<T>(options: &[T]) -> Self
    where
        T: Clone + Display + Eq + Hash,
    {
        Self {
            values: options
                .iter()
                .map(|_| paragraph::Plain::<P>::default())
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_renderer::LayoutRenderer;

    type TestList<'a> =
        SelectionList<'a, String, &'a [String], String, iced_core::Theme, LayoutRenderer>;

    fn options() -> Vec<String> {
        vec!["Option 1".to_owned(), "Option 2".to_owned()]
    }

    #[test]
    fn new_has_default_values() {
        let options = options();
        let list = TestList::new(&options, |_, value| value);
        assert_eq!(list.options.len(), 2);
        assert_eq!(list.width, Length::Fill);
        assert_eq!(list.height, Length::Fill);
        assert_eq!(list.text_size, 12.0);
        assert!(list.selected.is_none());
    }

    #[test]
    fn owned_vec_options_work_inline() {
        // An owned Vec produced inside a view function must not require a
        // value that outlives the widget.
        fn build() -> Vec<String> {
            vec!["a".to_owned(), "b".to_owned()]
        }
        let list: SelectionList<'_, String, Vec<String>, String, iced_core::Theme, LayoutRenderer> =
            SelectionList::new(build(), |_, value| value);
        assert_eq!(list.options.len(), 2);
    }
}
