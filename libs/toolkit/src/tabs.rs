// SPDX-License-Identifier: MIT OR Apache-2.0
//! A tabbed container: [`Tabs`], a [`TabBar`] that owns and shows the
//! content of the active tab, with the bar at the top or the bottom.

use iced_core::layout::{Limits, Node};
use iced_core::mouse::{self, Cursor};
use iced_core::overlay;
use iced_core::renderer;
use iced_core::widget::tree::{State, Tag};
use iced_core::widget::{Operation, Tree, Widget};
use iced_core::{
    Element, Event, Font, Layout, Length, Point, Rectangle, Shell, Size, Vector,
};

use crate::tab_bar::{self, Position, Status, Style, StyleFn, TabBar, TabLabel};

/// Where the bar sits relative to the content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TabBarPosition {
    /// Above the content.
    #[default]
    Top,
    /// Below the content.
    Bottom,
}

/// A [`Tabs`] widget: a [`TabBar`] plus the content of the active tab.
///
/// ```no_run
/// # use toolkit::tab_bar::TabLabel;
/// # use toolkit::tabs::Tabs;
/// #[derive(Clone, PartialEq, Eq)]
/// enum TabId { One, Two }
/// #[derive(Clone)]
/// enum Message { Selected(TabId) }
///
/// fn view<'a, Theme, Renderer>() -> iced_core::Element<'a, Message, Theme, Renderer>
/// where
///     Theme: toolkit::tab_bar::Catalog + iced_core::widget::text::Catalog + 'a,
///     Renderer: iced_core::text::Renderer<Font = iced_core::Font> + 'a,
/// {
///     Tabs::new(Message::Selected)
///         .push(TabId::One, TabLabel::Text("One".into()), iced_widget::text("One"))
///         .push(TabId::Two, TabLabel::Text("Two".into()), iced_widget::text("Two"))
///         .into()
/// }
/// ```
#[allow(missing_debug_implementations)]
pub struct Tabs<'a, Message, TabId, Theme, Renderer>
where
    Renderer: 'a + renderer::Renderer + iced_core::text::Renderer,
    Theme: tab_bar::Catalog,
    TabId: Eq + Clone,
{
    /// The bar.
    tab_bar: TabBar<'a, Message, TabId, Theme, Renderer>,
    /// The content of each tab.
    children: Vec<Element<'a, Message, Theme, Renderer>>,
    /// The ids of the tabs.
    indices: Vec<TabId>,
    /// Where the bar sits.
    tab_bar_position: TabBarPosition,
    /// Where the label glyph sits.
    tab_icon_position: Position,
    width: Length,
    height: Length,
}

impl<'a, Message, TabId, Theme, Renderer> Tabs<'a, Message, TabId, Theme, Renderer>
where
    Renderer: 'a + renderer::Renderer + iced_core::text::Renderer<Font = Font>,
    Theme: tab_bar::Catalog + iced_core::widget::text::Catalog,
    TabId: Eq + Clone,
{
    /// Creates an empty [`Tabs`] that produces a message when a tab is
    /// selected.
    pub fn new<F>(on_select: F) -> Self
    where
        F: 'static + Fn(TabId) -> Message,
    {
        Self::new_with_tabs(Vec::new(), on_select)
    }

    /// Creates a [`Tabs`] from `(id, label, content)` triples.
    pub fn new_with_tabs<F>(
        tabs: impl IntoIterator<Item = (TabId, TabLabel, Element<'a, Message, Theme, Renderer>)>,
        on_select: F,
    ) -> Self
    where
        F: 'static + Fn(TabId) -> Message,
    {
        let tabs = tabs.into_iter();
        let n_tabs = tabs.size_hint().0;

        let mut tab_labels = Vec::with_capacity(n_tabs);
        let mut elements = Vec::with_capacity(n_tabs);
        let mut indices = Vec::with_capacity(n_tabs);

        for (id, tab_label, element) in tabs {
            tab_labels.push((id.clone(), tab_label));
            indices.push(id);
            elements.push(element);
        }

        Tabs {
            tab_bar: TabBar::with_tab_labels(tab_labels, on_select),
            children: elements,
            indices,
            tab_bar_position: TabBarPosition::Top,
            tab_icon_position: Position::Left,
            width: Length::Fill,
            height: Length::Fill,
        }
    }

    /// Sets the size of the close glyph of the labels.
    #[must_use]
    pub fn close_size(mut self, close_size: f32) -> Self {
        self.tab_bar = self.tab_bar.close_size(close_size);
        self
    }

    /// Sets the glyph position of the labels.
    #[must_use]
    pub fn tab_icon_position(mut self, position: Position) -> Self {
        self.tab_icon_position = position;
        self
    }

    /// Sets the height of the [`Tabs`].
    #[must_use]
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// Sets the font of the label glyphs.
    #[must_use]
    pub fn icon_font(mut self, font: Font) -> Self {
        self.tab_bar = self.tab_bar.icon_font(font);
        self
    }

    /// Sets the glyph size of the labels.
    #[must_use]
    pub fn icon_size(mut self, icon_size: f32) -> Self {
        self.tab_bar = self.tab_bar.icon_size(icon_size);
        self
    }

    /// Sets the message produced when a tab's close glyph is pressed.
    #[must_use]
    pub fn on_close<F>(mut self, on_close: F) -> Self
    where
        F: 'static + Fn(TabId) -> Message,
    {
        self.tab_bar = self.tab_bar.on_close(on_close);
        self
    }

    /// Pushes a tab with its label and content.
    #[must_use]
    pub fn push<E>(mut self, id: TabId, tab_label: TabLabel, element: E) -> Self
    where
        E: Into<Element<'a, Message, Theme, Renderer>>,
    {
        self.tab_bar = self
            .tab_bar
            .push(id.clone(), tab_label)
            .set_position(self.tab_icon_position);
        self.children.push(element.into());
        self.indices.push(id);
        self
    }

    /// Selects the active tab by id.
    #[must_use]
    pub fn set_active_tab(mut self, id: &TabId) -> Self {
        self.tab_bar = self.tab_bar.set_active_tab(id);
        self
    }

    /// Sets the height of the bar.
    #[must_use]
    pub fn tab_bar_height(mut self, height: Length) -> Self {
        self.tab_bar = self.tab_bar.height(height);
        self
    }

    /// Sets the width of the bar.
    #[must_use]
    pub fn tab_bar_width(mut self, width: Length) -> Self {
        self.tab_bar = self.tab_bar.width(width);
        self
    }

    /// Sets where the bar sits.
    #[must_use]
    pub fn tab_bar_position(mut self, position: TabBarPosition) -> Self {
        self.tab_bar_position = position;
        self
    }

    /// Sets the style of the bar.
    #[must_use]
    pub fn tab_bar_style(mut self, style: impl Fn(&Theme, Status) -> Style + 'a) -> Self
    where
        <Theme as tab_bar::Catalog>::Class<'a>: From<StyleFn<'a, Theme>>,
    {
        self.tab_bar = self.tab_bar.style(style);
        self
    }

    /// Sets the padding of the labels.
    #[must_use]
    pub fn tab_label_padding(mut self, padding: impl Into<iced_core::Padding>) -> Self {
        self.tab_bar = self.tab_bar.padding(padding);
        self
    }

    /// Sets the spacing between the labels.
    #[must_use]
    pub fn tab_label_spacing(mut self, spacing: impl Into<iced_core::Pixels>) -> Self {
        self.tab_bar = self.tab_bar.spacing(spacing);
        self
    }

    /// Sets the font of the label texts.
    #[must_use]
    pub fn text_font(mut self, text_font: Font) -> Self {
        self.tab_bar = self.tab_bar.text_font(text_font);
        self
    }

    /// Sets the text size of the labels.
    #[must_use]
    pub fn text_size(mut self, text_size: f32) -> Self {
        self.tab_bar = self.tab_bar.text_size(text_size);
        self
    }

    /// Sets the width of the [`Tabs`].
    #[must_use]
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }
}

impl<Message, TabId, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Tabs<'_, Message, TabId, Theme, Renderer>
where
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = Font>,
    Theme: tab_bar::Catalog + iced_core::widget::text::Catalog,
    TabId: Eq + Clone,
{
    fn tag(&self) -> Tag {
        Tag::stateless()
    }

    fn state(&self) -> State {
        State::None
    }

    fn diff(&mut self, tree: &mut Tree) {
        // children[0] belongs to the bar (managed by it); children[1]
        // holds the content trees.
        if tree.children.len() != 2 {
            let tabs = Tree {
                tag: Tag::stateless(),
                state: State::None,
                children: self.children.iter().map(Tree::new).collect(),
            };

            let bar = Tree {
                tag: self.tab_bar.tag(),
                state: self.tab_bar.state(),
                children: vec![],
            };

            tree.children = vec![bar, tabs];
        }

        if let Some(tabs) = tree.children.get_mut(1) {
            tabs.diff_children(&mut self.children);
        }
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> Node {
        let tab_bar_limits = limits.width(self.width).height(Length::Fill);
        let mut tab_bar_node = self
            .tab_bar
            .layout(&mut tree.children[0], renderer, &tab_bar_limits);

        let tab_content_limits = limits
            .width(self.width)
            .height(self.height)
            .shrink([0.0, tab_bar_node.size().height]);

        let mut tab_content_node = if let (Some(element), Some(child)) = (
            self.children.get_mut(self.tab_bar.get_active_tab_idx()),
            tree.children.get_mut(1),
        ) {
            element.as_widget_mut().layout(
                &mut child.children[self.tab_bar.get_active_tab_idx()],
                renderer,
                &tab_content_limits,
            )
        } else {
            iced_widget::Row::<Message, Theme, Renderer>::new()
                .width(Length::Fill)
                .height(Length::Fill)
                .layout(tree, renderer, &tab_content_limits)
        };

        let tab_bar_bounds = tab_bar_node.bounds();
        tab_bar_node = tab_bar_node.move_to(Point::new(
            tab_bar_bounds.x,
            tab_bar_bounds.y
                + match self.tab_bar_position {
                    TabBarPosition::Top => 0.0,
                    TabBarPosition::Bottom => tab_content_node.bounds().height,
                },
        ));

        let tab_content_bounds = tab_content_node.bounds();
        tab_content_node = tab_content_node.move_to(Point::new(
            tab_content_bounds.x,
            tab_content_bounds.y
                + match self.tab_bar_position {
                    TabBarPosition::Top => tab_bar_node.bounds().height,
                    TabBarPosition::Bottom => 0.0,
                },
        ));

        Node::with_children(
            Size::new(
                tab_content_node.size().width,
                tab_bar_node.size().height + tab_content_node.size().height,
            ),
            match self.tab_bar_position {
                TabBarPosition::Top => vec![tab_bar_node, tab_content_node],
                TabBarPosition::Bottom => vec![tab_content_node, tab_bar_node],
            },
        )
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
        let mut children = layout.children();
        let (tab_bar_layout, tab_content_layout) = match self.tab_bar_position {
            TabBarPosition::Top => {
                let tab_bar_layout = children
                    .next()
                    .expect("widget: Layout should have a TabBar layout at top position");
                let tab_content_layout = children
                    .next()
                    .expect("widget: Layout should have a tab content layout at top position");
                (tab_bar_layout, tab_content_layout)
            }
            TabBarPosition::Bottom => {
                let tab_content_layout = children
                    .next()
                    .expect("widget: Layout should have a tab content layout at bottom position");
                let tab_bar_layout = children
                    .next()
                    .expect("widget: Layout should have a TabBar layout at bottom position");
                (tab_bar_layout, tab_content_layout)
            }
        };

        self.tab_bar.update(
            &mut Tree::empty(),
            event,
            tab_bar_layout,
            cursor,
            renderer,
            shell,
            viewport,
        );
        let idx = self.tab_bar.get_active_tab_idx();
        if let Some(element) = self.children.get_mut(idx) {
            element.as_widget_mut().update(
                &mut state.children[1].children[idx],
                event,
                tab_content_layout,
                cursor,
                renderer,
                shell,
                viewport,
            );
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
        let mut children = layout.children();
        let (tab_bar_layout, tab_content_layout) = match self.tab_bar_position {
            TabBarPosition::Top => (
                children
                    .next()
                    .expect("widget: Layout should have a TabBar layout at top position"),
                children
                    .next()
                    .expect("widget: Layout should have a tab content layout at top position"),
            ),
            TabBarPosition::Bottom => (
                children
                    .next()
                    .expect("widget: Layout should have a tab content layout at bottom position"),
                children
                    .next()
                    .expect("widget: Layout should have a TabBar layout at bottom position"),
            ),
        };

        self.tab_bar
            .mouse_interaction(&state.children[0], tab_bar_layout, cursor, viewport, renderer)
            .max(
                self.children
                    .get(self.tab_bar.get_active_tab_idx())
                    .zip(state.children.get(1).and_then(|tabs| {
                        tabs.children.get(self.tab_bar.get_active_tab_idx())
                    }))
                    .map_or_else(
                        mouse::Interaction::default,
                        |(element, state)| {
                            element.as_widget().mouse_interaction(
                                state,
                                tab_content_layout,
                                cursor,
                                viewport,
                                renderer,
                            )
                        },
                    ),
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
        let mut children = layout.children();
        let (tab_bar_layout, tab_content_layout) = match self.tab_bar_position {
            TabBarPosition::Top => (
                children
                    .next()
                    .expect("Graphics: Layout should have a TabBar layout at top position"),
                children
                    .next()
                    .expect("Graphics: Layout should have a tab content layout at top position"),
            ),
            TabBarPosition::Bottom => (
                children
                    .next()
                    .expect("Graphics: Layout should have a tab content layout at bottom position"),
                children
                    .next()
                    .expect("Graphics: Layout should have a TabBar layout at bottom position"),
            ),
        };

        self.tab_bar.draw(
            &state.children[0],
            renderer,
            theme,
            style,
            tab_bar_layout,
            cursor,
            viewport,
        );

        let idx = self.tab_bar.get_active_tab_idx();
        if let (Some(element), Some(child)) = (
            self.children.get(idx),
            state
                .children
                .get(1)
                .and_then(|tabs| tabs.children.get(idx)),
        ) {
            element
                .as_widget()
                .draw(child, renderer, theme, style, tab_content_layout, cursor, viewport);
        }
    }

    fn operate(
        &mut self,
        state: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let mut children = layout.children();
        let (tab_bar_layout, tab_content_layout) = match self.tab_bar_position {
            TabBarPosition::Top => (
                children
                    .next()
                    .expect("widget: Layout should have a TabBar layout at top position"),
                children
                    .next()
                    .expect("widget: Layout should have a tab content layout at top position"),
            ),
            TabBarPosition::Bottom => (
                children
                    .next()
                    .expect("widget: Layout should have a tab content layout at bottom position"),
                children
                    .next()
                    .expect("widget: Layout should have a TabBar layout at bottom position"),
            ),
        };

        self.tab_bar
            .operate(&mut state.children[0], tab_bar_layout, renderer, operation);

        let idx = self.tab_bar.get_active_tab_idx();
        if let Some(element) = self.children.get_mut(idx) {
            element.as_widget_mut().operate(
                &mut state.children[1].children[idx],
                tab_content_layout,
                renderer,
                operation,
            );
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        let mut children = layout.children();
        let (_tab_bar_layout, tab_content_layout) = match self.tab_bar_position {
            TabBarPosition::Top => (
                children.next().expect("TabBar layout at top"),
                children.next().expect("tab content layout at top"),
            ),
            TabBarPosition::Bottom => (
                children.next().expect("tab content layout at bottom"),
                children.next().expect("TabBar layout at bottom"),
            ),
        };

        let idx = self.tab_bar.get_active_tab_idx();
        self.children
            .get_mut(idx)
            .zip(tree.children.get_mut(1).and_then(|tabs| tabs.children.get_mut(idx)))
            .and_then(|(element, state)| {
                element
                    .as_widget_mut()
                    .overlay(state, tab_content_layout, renderer, viewport, translation)
            })
    }
}

impl<'a, Message, TabId, Theme, Renderer> From<Tabs<'a, Message, TabId, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Renderer: 'a + renderer::Renderer + iced_core::text::Renderer<Font = Font>,
    Theme: 'a + tab_bar::Catalog + iced_core::widget::text::Catalog,
    Message: 'a,
    TabId: 'a + Eq + Clone,
{
    fn from(tabs: Tabs<'a, Message, TabId, Theme, Renderer>) -> Self {
        Element::new(tabs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_renderer::LayoutRenderer;
    use crate::tab_bar::TabLabel;
    use iced_core::widget::text::Text;

    type TestTabs<'a> = Tabs<'a, u8, u8, iced_core::Theme, LayoutRenderer>;

    fn tabs() -> TestTabs<'static> {
        TestTabs::new(|id| id)
            .push(0, TabLabel::Text("One".into()), Text::new("Content one"))
            .push(1, TabLabel::Text("Two".into()), Text::new("Content two"))
            .set_active_tab(&1)
    }

    #[test]
    fn only_the_active_tab_content_is_laid_out() {
        let mut tabs = tabs();
        let renderer = LayoutRenderer::new();
        let mut element: Element<'_, u8, iced_core::Theme, LayoutRenderer> = tabs.into();
        let mut tree = Tree::new(&element);
        element.as_widget_mut().diff(&mut tree);
        let limits = Limits::new(Size::ZERO, Size::new(400.0, 300.0));
        let node = element.as_widget_mut().layout(&mut tree, &renderer, &limits);
        // Two children: the bar and the active content.
        assert_eq!(Layout::new(&node).children().count(), 2);
        assert_eq!(
            node.bounds().width, 400.0,
            "the tabs fill the offered width"
        );
    }
}
