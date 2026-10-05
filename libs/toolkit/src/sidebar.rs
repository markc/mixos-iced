// SPDX-License-Identifier: MIT OR Apache-2.0
//! A vertical tab bar: [`Sidebar`], the [`TabBar`](crate::tab_bar::TabBar)
//! shape stood upright — tabs stacked in a column, each row sized to the
//! widest ([`FlushColumn`](crate::flush_column::FlushColumn)), with the
//! close glyph at the start or the end of the row. The content switch is
//! the caller's, like [`TabBar`].

use iced_core::Widget;
use iced_core::alignment;
use iced_core::layout::{Limits, Node};
use iced_core::mouse::{self, Cursor};
use iced_core::renderer;
use iced_core::text::{self, LineHeight, Shaping, Wrapping};
use iced_core::widget::text::Text;
use iced_core::widget::{Operation, Tree, tree};
use iced_core::{
    Alignment, Border, Color, Element, Event, Font, Layout, Length, Padding, Pixels,
    Point, Rectangle, Shadow, Shell, Size, alignment::Vertical,
};
use iced_widget::Row;
use std::marker::PhantomData;

use crate::flush_column::FlushColumn;

pub use crate::tab_bar::TabLabel;

/// The style of a [`Sidebar`], shared with the tab bar.
pub use crate::tab_bar::Style;

/// The default glyph size.
const DEFAULT_ICON_SIZE: f32 = 16.0;
/// The default text size.
const DEFAULT_TEXT_SIZE: f32 = 16.0;
/// The default size of the close glyph.
const DEFAULT_CLOSE_SIZE: f32 = 16.0;
/// The default padding between the tabs.
const DEFAULT_PADDING: Padding = Padding::new(1.0);
/// The default spacing around the tabs.
const DEFAULT_SPACING: Pixels = Pixels::ZERO;

/// The interaction status of a sidebar tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The tab is the active one.
    Active,
    /// The pointer is over the tab.
    Hovered,
    /// The tab is inactive.
    Disabled,
}

/// The theme catalog of a [`Sidebar`]; the style itself is
/// [`tab_bar::Style`](crate::tab_bar::Style), shared with the tab bar.
pub trait Catalog {
    /// The item class of [`Catalog`].
    type Class<'a>;
    /// The default class produced by the [`Catalog`].
    fn default<'a>() -> Self::Class<'a>;
    /// The [`Style`] of a class with the given status.
    fn style(&self, class: &Self::Class<'_>, status: Status) -> Style;
}

/// A styling function for a [`Sidebar`].
pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme, Status) -> Style + 'a>;

/// The position of the glyph relative to the text, for
/// [`TabLabel::IconText`] labels, and of the close glyph in the row.
#[derive(Clone, Copy, Default)]
pub enum Position {
    /// At the start of the row.
    #[default]
    Start,
    /// At the end of the row.
    End,
}

impl From<Status> for crate::tab_bar::Status {
    fn from(status: Status) -> Self {
        match status {
            Status::Active => Self::Active,
            Status::Hovered => Self::Hovered,
            Status::Disabled => Self::Disabled,
        }
    }
}

/// A sidebar to show tabs vertically.
///
/// ```no_run
/// # use toolkit::sidebar::{Sidebar, TabLabel};
/// #[derive(Clone, PartialEq, Eq)]
/// enum TabId { One, Two }
/// #[derive(Clone)]
/// enum Message { Selected(TabId) }
///
/// fn view<'a, Theme, Renderer>() -> iced_core::Element<'a, Message, Theme, Renderer>
/// where
///     Theme: toolkit::sidebar::Catalog + iced_core::widget::text::Catalog + 'a,
///     Renderer: iced_core::text::Renderer<Font = iced_core::Font> + 'static,
/// {
///     Sidebar::new(Message::Selected)
///         .push(TabId::One, TabLabel::Text("One".into()))
///         .push(TabId::Two, TabLabel::Text("Two".into()))
///         .into()
/// }
/// ```
#[allow(missing_debug_implementations)]
pub struct Sidebar<'a, Message, TabId, Theme, Renderer>
where
    Renderer: renderer::Renderer + iced_core::text::Renderer,
    Theme: Catalog,
    TabId: Eq + Clone,
{
    /// The index of the active tab.
    active_tab: usize,
    /// The labels of the tabs.
    tab_labels: Vec<TabLabel>,
    /// The ids of the tabs.
    tab_indices: Vec<TabId>,
    /// The statuses of the labels and their close glyphs.
    tab_statuses: Vec<(Option<Status>, Option<bool>)>,
    /// The alignment of the tabs.
    align_tabs: Alignment,
    /// The message produced when a tab is selected.
    on_select: Box<dyn Fn(TabId) -> Message>,
    /// The message produced when a tab's close glyph is pressed.
    on_close: Option<Box<dyn Fn(TabId) -> Message>>,
    width: Length,
    height: Length,
    tab_height: Length,
    icon_size: f32,
    text_size: f32,
    close_size: f32,
    padding: Padding,
    spacing: Pixels,
    font: Option<Font>,
    text_font: Option<Font>,
    class: <Theme as Catalog>::Class<'a>,
    /// Where the glyph sits relative to the text.
    position: Position,
    /// Where the close glyph sits in the row.
    close_position: Position,
    #[allow(clippy::missing_docs_in_private_items)]
    _renderer: PhantomData<Renderer>,
}

impl<'a, Message, TabId, Theme, Renderer> Sidebar<'a, Message, TabId, Theme, Renderer>
where
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog,
    TabId: Eq + Clone,
{
    /// Creates an empty [`Sidebar`] that produces a message when a tab is
    /// selected.
    pub fn new<F>(on_select: F) -> Self
    where
        F: 'static + Fn(TabId) -> Message,
    {
        Self::with_tab_labels(Vec::new(), on_select)
    }

    /// Creates a [`Sidebar`] from `(id, label)` pairs.
    pub fn with_tab_labels<F>(tab_labels: Vec<(TabId, TabLabel)>, on_select: F) -> Self
    where
        F: 'static + Fn(TabId) -> Message,
    {
        let count = tab_labels.len();
        Self {
            active_tab: 0,
            tab_indices: tab_labels.iter().map(|(id, _)| id.clone()).collect(),
            tab_labels: tab_labels.into_iter().map(|(_, label)| label).collect(),
            tab_statuses: vec![(None, None); count],
            align_tabs: Alignment::Start,
            on_select: Box::new(on_select),
            on_close: None,
            width: Length::Shrink,
            height: Length::Fill,
            tab_height: Length::Shrink,
            icon_size: DEFAULT_ICON_SIZE,
            text_size: DEFAULT_TEXT_SIZE,
            close_size: DEFAULT_CLOSE_SIZE,
            padding: DEFAULT_PADDING,
            spacing: DEFAULT_SPACING,
            font: None,
            text_font: None,
            class: <Theme as Catalog>::default(),
            position: Position::Start,
            close_position: Position::End,
            _renderer: PhantomData,
        }
    }

    /// Sets the alignment of the tabs.
    #[must_use]
    pub fn align_tabs(mut self, align: Alignment) -> Self {
        self.align_tabs = align;
        self
    }

    /// Sets the size of the close glyph.
    #[must_use]
    pub fn close_size(mut self, close_size: f32) -> Self {
        self.close_size = close_size;
        self
    }

    /// The id of the active tab, if any.
    #[must_use]
    pub fn get_active_tab_id(&self) -> Option<&TabId> {
        self.tab_indices.get(self.active_tab)
    }

    /// The index of the active tab.
    #[must_use]
    pub fn get_active_tab_idx(&self) -> usize {
        self.active_tab
    }

    /// Sets the height of the [`Sidebar`].
    #[must_use]
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// Sets the font of the glyphs of the labels.
    #[must_use]
    pub fn icon_font(mut self, font: Font) -> Self {
        self.font = Some(font);
        self
    }

    /// Sets the glyph size of the labels.
    #[must_use]
    pub fn icon_size(mut self, icon_size: f32) -> Self {
        self.icon_size = icon_size;
        self
    }

    /// Sets the message produced when a tab's close glyph is pressed;
    /// setting this is what draws the glyphs.
    #[must_use]
    pub fn on_close<F>(mut self, on_close: F) -> Self
    where
        F: 'static + Fn(TabId) -> Message,
    {
        self.on_close = Some(Box::new(on_close));
        self
    }

    /// Sets the padding of the tabs.
    #[must_use]
    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.padding = padding.into();
        self
    }

    /// Pushes a tab.
    #[must_use]
    pub fn push(mut self, id: TabId, tab_label: TabLabel) -> Self {
        self.tab_labels.push(tab_label);
        self.tab_indices.push(id);
        self.tab_statuses.push((None, None));
        self
    }

    /// The number of tabs.
    #[must_use]
    pub fn size(&self) -> usize {
        self.tab_indices.len()
    }

    /// Sets the spacing between the tabs.
    #[must_use]
    pub fn spacing(mut self, spacing: impl Into<Pixels>) -> Self {
        self.spacing = spacing.into();
        self
    }

    /// Sets the font of the texts of the labels.
    #[must_use]
    pub fn text_font(mut self, text_font: Font) -> Self {
        self.text_font = Some(text_font);
        self
    }

    /// Sets the text size of the labels.
    #[must_use]
    pub fn text_size(mut self, text_size: f32) -> Self {
        self.text_size = text_size;
        self
    }

    /// Sets the height of a tab.
    #[must_use]
    pub fn tab_height(mut self, height: Length) -> Self {
        self.tab_height = height;
        self
    }

    /// Selects the active tab by id.
    #[must_use]
    pub fn set_active_tab(mut self, active_tab: &TabId) -> Self {
        self.active_tab = self
            .tab_indices
            .iter()
            .position(|id| id == active_tab)
            .map_or(0, |a| a);
        self
    }

    /// Sets where the close glyph sits in the row.
    #[must_use]
    pub fn set_close_position(mut self, position: Position) -> Self {
        self.close_position = position;
        self
    }

    /// Sets where the label glyph sits relative to the text.
    #[must_use]
    pub fn set_position(mut self, position: Position) -> Self {
        self.position = position;
        self
    }

    /// Sets the style of the [`Sidebar`].
    #[must_use]
    pub fn style(mut self, style: impl Fn(&Theme, Status) -> Style + 'a) -> Self
    where
        <Theme as Catalog>::Class<'a>: From<StyleFn<'a, Theme>>,
    {
        self.class = (Box::new(style) as StyleFn<'a, Theme>).into();
        self
    }

    /// Sets the class of the [`Sidebar`].
    #[must_use]
    pub fn class(mut self, class: impl Into<<Theme as Catalog>::Class<'a>>) -> Self {
        self.class = class.into();
        self
    }

    /// Sets the width of the [`Sidebar`].
    #[must_use]
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }
}

fn layout_icon<Theme, Renderer>(icon: &char, size: f32, font: Option<Font>) -> Text<'_, Theme, Renderer>
where
    Renderer: iced_core::text::Renderer,
    Renderer::Font: From<Font>,
    Theme: iced_core::widget::text::Catalog,
{
    Text::<Theme, Renderer>::new(icon.to_string())
        .size(size)
        .font(font.unwrap_or_default())
        .align_x(alignment::Horizontal::Center)
        .align_y(alignment::Vertical::Center)
        .shaping(Shaping::Advanced)
        .width(Length::Shrink)
}

fn layout_text<Theme, Renderer>(
    label: &str,
    size: f32,
    font: Option<Font>,
) -> Text<'_, Theme, Renderer>
where
    Renderer: iced_core::text::Renderer,
    Renderer::Font: From<Font>,
    Theme: iced_core::widget::text::Catalog,
{
    Text::<Theme, Renderer>::new(label)
        .size(size)
        .font(font.unwrap_or_default())
        .align_x(alignment::Horizontal::Center)
        .align_y(alignment::Vertical::Center)
        .shaping(Shaping::Advanced)
        .width(Length::Shrink)
}

impl<Message, TabId, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Sidebar<'_, Message, TabId, Theme, Renderer>
where
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog + iced_core::widget::text::Catalog,
    TabId: Eq + Clone,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::stateless()
    }

    fn state(&self) -> tree::State {
        tree::State::None
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> Node {
        let column = self
            .tab_labels
            .iter()
            .fold(FlushColumn::new(), |column, tab_label| {
                let label = match tab_label {
                    TabLabel::Icon(icon) => Row::new()
                        .align_y(Alignment::Center)
                        .push(layout_icon::<Theme, Renderer>(
                            icon,
                            self.icon_size + 1.0,
                            self.font,
                        )),
                    TabLabel::Text(label) => Row::new()
                        .padding(5.0)
                        .align_y(Alignment::Center)
                        .push(layout_text::<Theme, Renderer>(
                            label,
                            self.text_size + 1.0,
                            self.text_font,
                        )),
                    TabLabel::IconText(icon, label) => {
                        let mut row = Row::new().align_y(Alignment::Center);
                        match self.position {
                            Position::Start => {
                                row = row
                                    .push(layout_icon::<Theme, Renderer>(
                                        icon,
                                        self.icon_size + 1.0,
                                        self.font,
                                    ))
                                    .push(layout_text::<Theme, Renderer>(
                                        label,
                                        self.text_size + 1.0,
                                        self.text_font,
                                    ));
                            }
                            Position::End => {
                                row = row
                                    .push(layout_text::<Theme, Renderer>(
                                        label,
                                        self.text_size + 1.0,
                                        self.text_font,
                                    ))
                                    .push(layout_icon::<Theme, Renderer>(
                                        icon,
                                        self.icon_size + 1.0,
                                        self.font,
                                    ));
                            }
                        }
                        row
                    }
                };
                let mut tab = Row::new();
                if self.on_close.is_some() {
                    let close = Row::new()
                        .width(Length::Fixed(self.close_size * 1.3 + 1.0))
                        .height(Length::Fixed(self.close_size * 1.3 + 1.0))
                        .align_y(Alignment::Center);
                    match self.close_position {
                        Position::Start => tab = tab.push(close).push(label),
                        Position::End => tab = tab.push(label).push(close),
                    }
                } else {
                    tab = tab.push(label);
                }
                tab = tab
                    .align_y(Alignment::Center)
                    .padding(self.padding)
                    .height(self.tab_height)
                    .width(self.width);
                column.push(tab)
            })
            .width(self.width)
            .height(self.height)
            .spacing(self.spacing)
            .align_x(self.align_tabs);

        let mut element: Element<Message, Theme, Renderer> = Element::new(column);
        let tab_tree = if let Some(child_tree) = tree.children.get_mut(0) {
            child_tree.diff(element.as_widget_mut());
            child_tree
        } else {
            let mut child_tree = Tree::new(element.as_widget());
            element.as_widget_mut().diff(&mut child_tree);
            tree.children.insert(0, child_tree);
            &mut tree.children[0]
        };
        element
            .as_widget_mut()
            .layout(tab_tree, renderer, &limits.loose())
    }

    fn update(
        &mut self,
        _state: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        _renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        if matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                | Event::Touch(iced_core::touch::Event::FingerPressed { .. })
        ) && cursor
            .position()
            .is_some_and(|pos| layout.bounds().contains(pos))
        {
            let tabs_map: Vec<bool> = layout
                .children()
                .map(|layout| {
                    cursor
                        .position()
                        .is_some_and(|pos| layout.bounds().contains(pos))
                })
                .collect();

            if let Some(new_selected) = tabs_map.iter().position(|b| *b) {
                shell.publish(
                    self.on_close
                        .as_ref()
                        .filter(|_on_close| {
                            let tab_layout = layout.children().nth(new_selected).expect(
                                "widget: Layout should have a tab layout at the selected index",
                            );
                            let cross_layout = tab_layout
                                .children()
                                .next_back()
                                .expect("widget: Layout should have a close layout");

                            cursor
                                .position()
                                .is_some_and(|pos| cross_layout.bounds().contains(pos))
                        })
                        .map_or_else(
                            || (self.on_select)(self.tab_indices[new_selected].clone()),
                            |on_close| (on_close)(self.tab_indices[new_selected].clone()),
                        ),
                );
                shell.capture_event();
            }
        }

        // Track hover for restyling.
        let mut request_redraw = false;
        let active_idx = self.get_active_tab_idx();
        let children = layout.children();
        for ((i, _tab), layout) in self.tab_labels.iter().enumerate().zip(children) {
            let tab_status = self.tab_statuses.get_mut(i).expect("Should have a status.");

            let current_status = if cursor.is_over(layout.bounds()) {
                Status::Hovered
            } else if i == active_idx {
                Status::Active
            } else {
                Status::Disabled
            };

            let mut is_cross_hovered = None;
            let mut children = layout.children();
            if self.on_close.is_some()
                && let Some(cross_layout) = children.next_back()
            {
                is_cross_hovered = Some(cursor.is_over(cross_layout.bounds()));
            }

            if let Event::Window(iced_core::window::Event::RedrawRequested(_now)) = event {
                *tab_status = (Some(current_status), is_cross_hovered);
            } else if tab_status.0.is_some_and(|status| status != current_status)
                || tab_status.1 != is_cross_hovered
            {
                request_redraw = true;
            }
        }

        if request_redraw {
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
        let children = layout.children();
        let mut mouse_interaction = mouse::Interaction::default();
        for layout in children {
            let is_mouse_over = cursor
                .position()
                .is_some_and(|pos| layout.bounds().contains(pos));
            let new_mouse_interaction = if is_mouse_over {
                mouse::Interaction::Pointer
            } else {
                mouse::Interaction::default()
            };
            if new_mouse_interaction > mouse_interaction {
                mouse_interaction = new_mouse_interaction;
            }
        }
        mouse_interaction
    }

    fn draw(
        &self,
        _state: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let children = layout.children();
        let is_mouse_over = cursor.position().is_some_and(|pos| bounds.contains(pos));
        let style_sheet = if is_mouse_over {
            <Theme as Catalog>::style(theme, &self.class, Status::Hovered)
        } else {
            <Theme as Catalog>::style(theme, &self.class, Status::Disabled)
        };
        if bounds.intersects(viewport) {
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    border: Border {
                        radius: (0.0).into(),
                        width: style_sheet.border_width,
                        color: style_sheet.border_color.unwrap_or(Color::TRANSPARENT),
                    },
                    ..renderer::Quad::default()
                },
                style_sheet
                    .background
                    .unwrap_or_else(|| Color::TRANSPARENT.into()),
            );
        }
        for ((i, tab), layout) in self.tab_labels.iter().enumerate().zip(children) {
            let tab_status = self
                .tab_statuses
                .get(i)
                .expect("Should have a status.")
                .0
                .unwrap_or(Status::Disabled);

            draw_tab(
                renderer,
                tab,
                layout,
                self.position,
                theme,
                &self.class,
                i == self.get_active_tab_idx(),
                tab_status,
                (self.font.unwrap_or_default(), self.icon_size),
                (self.text_font.unwrap_or_default(), self.text_size),
                self.close_size,
                viewport,
                self.on_close.is_some(),
                &self.close_position,
            );
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());

        // Rebuild the internal rows so operations can see each label.
        let mut rows: Vec<Element<'_, (), Theme, Renderer>> = Vec::with_capacity(self.tab_labels.len());
        for tab_label in &self.tab_labels {
            let row: Element<'_, (), Theme, Renderer> = match tab_label {
                TabLabel::Icon(icon) => Row::<(), Theme, Renderer>::new()
                    .push(Text::<Theme, Renderer>::new(icon.to_string()).size(self.icon_size))
                    .into(),
                TabLabel::Text(label) => Row::<(), Theme, Renderer>::new()
                    .push(Text::<Theme, Renderer>::new(label.clone()).size(self.text_size))
                    .into(),
                TabLabel::IconText(icon, label) => {
                    let mut row = Row::<(), Theme, Renderer>::new();
                    let (icon_text, label_text) = match self.position {
                        Position::Start => (
                            Text::<Theme, Renderer>::new(icon.to_string()).size(self.icon_size),
                            Text::<Theme, Renderer>::new(label.clone()).size(self.text_size),
                        ),
                        Position::End => (
                            Text::<Theme, Renderer>::new(label.clone()).size(self.text_size),
                            Text::<Theme, Renderer>::new(icon.to_string()).size(self.icon_size),
                        ),
                    };
                    row = row.push(icon_text).push(label_text);
                    row.into()
                }
            };
            rows.push(row);
        }

        let mut element: Element<'_, (), Theme, Renderer> = Element::new(FlushColumn::from_vec(rows));
        let tab_tree = if let Some(child_tree) = tree.children.get_mut(0) {
            child_tree.diff(element.as_widget_mut());
            child_tree
        } else {
            let mut child_tree = Tree::new(element.as_widget());
            element.as_widget_mut().diff(&mut child_tree);
            tree.children.insert(0, child_tree);
            &mut tree.children[0]
        };

        element
            .as_widget_mut()
            .operate(tab_tree, layout, renderer, operation);
    }
}

/// Draws one sidebar tab.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn draw_tab<Theme, Renderer>(
    renderer: &mut Renderer,
    tab: &TabLabel,
    layout: Layout<'_>,
    position: Position,
    theme: &Theme,
    class: &<Theme as Catalog>::Class<'_>,
    is_active: bool,
    status: Status,
    icon_data: (Font, f32),
    text_data: (Font, f32),
    close_size: f32,
    viewport: &Rectangle,
    closeable: bool,
    close_position: &Position,
) where
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog + iced_core::widget::text::Catalog,
{
    let style = match status {
        Status::Active => <Theme as Catalog>::style(theme, class, Status::Active),
        _ => <Theme as Catalog>::style(theme, class, Status::Disabled),
    };
    let _ = is_active;

    let bounds = layout.bounds();
    let mut children = layout.children();

    let mut label_layout = children.next();
    if closeable && matches!(close_position, Position::Start) {
        label_layout = children.next();
    }

    if bounds.intersects(viewport) {
        renderer.fill_quad(
            renderer::Quad {
                bounds,
                border: Border {
                    radius: style.tab_border_radius,
                    width: style.tab_label_border_width,
                    color: style.tab_label_border_color,
                },
                shadow: Shadow::default(),
                ..renderer::Quad::default()
            },
            style.tab_label_background,
        );
    }

    if let Some(label_layout) = label_layout {
        let mut label_children = label_layout.children();

        let (icon_bounds, text_bounds) = match tab {
            TabLabel::Icon(_) => {
                let icon_bounds = label_children
                    .next()
                    .map(|l| l.bounds())
                    .unwrap_or_default();
                (Some(icon_bounds), None)
            }
            TabLabel::Text(_) => {
                let text_bounds = label_children
                    .next()
                    .map(|l| l.bounds())
                    .unwrap_or_default();
                (None, Some(text_bounds))
            }
            TabLabel::IconText(..) => {
                let first = label_children.next().map(|l| l.bounds());
                let second = label_children.next().map(|l| l.bounds());
                match position {
                    Position::Start => (first, second),
                    Position::End => (second, first),
                }
            }
        };

        if let (TabLabel::Icon(icon) | TabLabel::IconText(icon, _), Some(icon_bounds)) =
            (tab, icon_bounds)
        {
            renderer.fill_text(
                iced_core::text::Text {
                    content: icon.to_string(),
                    bounds: Size::new(icon_bounds.width, icon_bounds.height),
                    size: Pixels(icon_data.1),
                    font: icon_data.0,
                    align_x: text::Alignment::Center,
                    align_y: Vertical::Center,
                    line_height: LineHeight::Relative(1.3),
                    shaping: Shaping::Advanced,
                    wrapping: Wrapping::default(),
                    ellipsis: text::Ellipsis::None,
                    hint_factor: renderer.hint_factor(),
                },
                Point::new(icon_bounds.center_x(), icon_bounds.center_y()),
                style.icon_color,
                icon_bounds,
            );
        }

        if let (TabLabel::Text(label) | TabLabel::IconText(_, label), Some(text_bounds)) =
            (tab, text_bounds)
        {
            renderer.fill_text(
                iced_core::text::Text {
                    content: label.clone(),
                    bounds: Size::new(text_bounds.width, text_bounds.height),
                    size: Pixels(text_data.1),
                    font: text_data.0,
                    align_x: text::Alignment::Center,
                    align_y: Vertical::Center,
                    line_height: LineHeight::Relative(1.3),
                    shaping: Shaping::Advanced,
                    wrapping: Wrapping::default(),
                    ellipsis: text::Ellipsis::None,
                    hint_factor: renderer.hint_factor(),
                },
                Point::new(text_bounds.center_x(), text_bounds.center_y()),
                style.text_color,
                text_bounds,
            );
        }
    }

    if closeable
        && let Some(cross_layout) = children.next()
    {
        let cross_bounds = cross_layout.bounds();

        renderer.fill_text(
            iced_core::text::Text {
                content: "×".to_owned(),
                bounds: Size::new(cross_bounds.width, cross_bounds.height),
                size: Pixels(close_size),
                font: renderer.default_font(),
                align_x: text::Alignment::Center,
                align_y: Vertical::Center,
                line_height: LineHeight::Relative(1.3),
                shaping: Shaping::Advanced,
                wrapping: Wrapping::default(),
                ellipsis: text::Ellipsis::None,
                hint_factor: renderer.hint_factor(),
            },
            Point::new(cross_bounds.center_x(), cross_bounds.center_y()),
            style.text_color,
            cross_bounds,
        );
    }
}

impl<'a, Message, TabId, Theme, Renderer> From<Sidebar<'a, Message, TabId, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Renderer: 'a + renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: 'a + Catalog + iced_core::widget::text::Catalog,
    Message: 'a,
    TabId: 'a + Eq + Clone,
{
    fn from(sidebar: Sidebar<'a, Message, TabId, Theme, Renderer>) -> Self {
        Element::new(sidebar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_renderer::LayoutRenderer;

    type TestSidebar<'a> = Sidebar<'a, u8, u8, iced_core::Theme, LayoutRenderer>;

    #[test]
    fn tabs_stack_vertically() {
        let sidebar = TestSidebar::new(|id| id)
            .push(0, TabLabel::Text("One".into()))
            .push(1, TabLabel::Text("Two".into()))
            .set_active_tab(&1);
        assert_eq!(sidebar.get_active_tab_idx(), 1);
        assert_eq!(sidebar.size(), 2);

        let renderer = LayoutRenderer::new();
        let mut element: Element<'_, u8, iced_core::Theme, LayoutRenderer> = sidebar.into();
        let mut tree = Tree::new(&element);
        element.as_widget_mut().diff(&mut tree);
        let limits = Limits::new(Size::ZERO, Size::new(200.0, 400.0));
        let node = element.as_widget_mut().layout(&mut tree, &renderer, &limits);
        let laid: Vec<_> = Layout::new(&node).children().collect();
        assert_eq!(laid.len(), 2);
        assert!(laid[1].bounds().y > laid[0].bounds().y, "stacked, not a row");
    }
}
