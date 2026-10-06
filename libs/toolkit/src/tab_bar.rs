// SPDX-License-Identifier: MIT OR Apache-2.0
//! A horizontal tab bar: [`TabBar`], tabs with icon/text labels, optional
//! close glyphs, hover and active styling from the theme. The content
//! switch is the caller's — see [`crate::tabs::Tabs`] for the version
//! that owns the content too.

pub mod tab_label;

pub use tab_label::TabLabel;

use iced_core::layout::{Limits, Node};
use iced_core::mouse::{self, Cursor};
use iced_core::renderer;
use iced_core::text::{self, LineHeight, Shaping, Wrapping};
use iced_core::widget::text::Text;
use iced_core::widget::tree::{State, Tag};
use iced_core::widget::{Operation, Tree, Widget};
use iced_core::{
    Alignment, Background, Border, Color, Element, Event, Font, Layout, Length, Padding, Pixels,
    Point, Rectangle, Shadow, Shell, Size, alignment, alignment::Vertical, window,
};
use iced_widget::{Column, Row};
use std::marker::PhantomData;
type TextColour<'a, Id> = dyn Fn(&Id) -> Option<Color> + 'a;

/// The default icon size.
const DEFAULT_ICON_SIZE: f32 = 16.0;
/// The default text size.
const DEFAULT_TEXT_SIZE: f32 = 16.0;
/// The default size of the close glyph.
const DEFAULT_CLOSE_SIZE: f32 = 16.0;
/// The default padding between the tabs.
const DEFAULT_PADDING: Padding = Padding::new(5.0);
/// The default spacing around the tabs.
const DEFAULT_SPACING: Pixels = Pixels::ZERO;

/// The interaction status of a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The tab is the active one.
    Active,
    /// The pointer is over the tab.
    Hovered,
    /// The tab is inactive.
    Disabled,
}

/// The style of a [`TabBar`] and its tabs.
#[derive(Clone, Copy, Debug)]
pub struct Style {
    /// The background of the tab bar; `None` is transparent.
    pub background: Option<Background>,
    /// The border color of the tab bar; `None` draws none.
    pub border_color: Option<Color>,
    /// The border width of the tab bar.
    pub border_width: f32,
    /// The border radius of a tab.
    pub tab_border_radius: iced_core::border::Radius,
    /// The background of the tab labels.
    pub tab_label_background: Background,
    /// The border color of the tab labels.
    pub tab_label_border_color: Color,
    /// The border width of the tab labels.
    pub tab_label_border_width: f32,
    /// The glyph color of the tab labels.
    pub icon_color: Color,
    /// The background of the close glyph when hovered; `None` is none.
    pub icon_background: Option<Background>,
    /// The border radius of the close glyph.
    pub icon_border_radius: iced_core::border::Radius,
    /// The text color of the tab labels.
    pub text_color: Color,
}

/// The theme catalog of a [`TabBar`].
pub trait Catalog {
    /// The item class of [`Catalog`].
    type Class<'a>;
    /// The default class produced by the [`Catalog`].
    fn default<'a>() -> Self::Class<'a>;
    /// The [`Style`] of a class with the given status.
    fn style(&self, class: &Self::Class<'_>, status: Status) -> Style;
}

/// A styling function for a [`TabBar`].
pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme, Status) -> Style + 'a>;

/// The position of the glyph relative to the text, for
/// [`TabLabel::IconText`] labels.
#[derive(Clone, Copy, Default)]
pub enum Position {
    /// The glyph is above the text.
    Top,
    /// The glyph is right of the text.
    Right,
    /// The glyph is below the text.
    Bottom,
    /// The glyph is left of the text, the default.
    #[default]
    Left,
}

/// A tab bar to show tabs.
///
/// ```no_run
/// # use toolkit::tab_bar::{TabBar, TabLabel};
/// #[derive(Clone, PartialEq, Eq)]
/// enum TabId { One, Two }
/// #[derive(Clone)]
/// enum Message { Selected(TabId) }
///
/// fn view<'a, Theme, Renderer>() -> iced_core::Element<'a, Message, Theme, Renderer>
/// where
///     Theme: toolkit::tab_bar::Catalog + iced_core::widget::text::Catalog + 'a,
///     Renderer: iced_core::text::Renderer<Font = iced_core::Font> + 'static,
/// {
///     TabBar::new(Message::Selected)
///         .push(TabId::One, TabLabel::Text("One".into()))
///         .push(TabId::Two, TabLabel::Text("Two".into()))
///         .set_active_tab(&TabId::One)
///         .into()
/// }
/// ```
#[allow(missing_debug_implementations)]
pub struct TabBar<'a, Message, TabId, Theme, Renderer>
where
    Renderer: renderer::Renderer + iced_core::text::Renderer,
    Theme: Catalog,
    TabId: Eq + Clone,
{
    /// The index of the currently active tab.
    active_tab: usize,
    /// The labels of the tabs.
    tab_labels: Vec<TabLabel>,
    /// The ids of the tabs.
    tab_indices: Vec<TabId>,
    /// The statuses of the labels and their close glyphs.
    tab_statuses: Vec<(Option<Status>, Option<bool>)>,
    /// The message produced when a tab is selected.
    on_select: Box<dyn Fn(TabId) -> Message>,
    /// The message produced when a tab's close glyph is pressed.
    on_close: Option<Box<dyn Fn(TabId) -> Message>>,
    text_colour: Option<Box<TextColour<'a, TabId>>>,
    width: Length,
    tab_width: Length,
    height: Length,
    icon_size: f32,
    text_size: f32,
    close_size: f32,
    padding: Padding,
    spacing: Pixels,
    font: Option<Font>,
    text_font: Option<Font>,
    class: <Theme as Catalog>::Class<'a>,
    position: Position,
    #[allow(clippy::missing_docs_in_private_items)]
    _renderer: PhantomData<Renderer>,
}

impl<'a, Message, TabId, Theme, Renderer> TabBar<'a, Message, TabId, Theme, Renderer>
where
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog,
    TabId: Eq + Clone,
{
    /// A horizontally scrolling strip. Fill-width tabs use their intrinsic
    /// width; fixed widths are retained. The returned scrollable can receive
    /// a caller ID for runtime scroll operations (for example, revealing the
    /// newly active tab).
    pub fn scrollable(mut self) -> iced_widget::Scrollable<'a, Message, Theme, Renderer>
    where
        Message: Clone + 'a,
        TabId: 'a,
        Theme: iced_widget::scrollable::Catalog + iced_core::widget::text::Catalog + 'a,
        Renderer: 'a,
    {
        self.width = Length::Shrink;
        if self.tab_width.is_fill() {
            self.tab_width = Length::Shrink;
        }
        iced_widget::scrollable(self)
            .direction(iced_widget::scrollable::Direction::Horizontal(
                iced_widget::scrollable::Scrollbar::new(),
            ))
            .width(Length::Fill)
            .height(Length::Shrink)
    }

    /// Creates an empty [`TabBar`] that produces a message when a tab is
    /// selected.
    pub fn new<F>(on_select: F) -> Self
    where
        F: 'static + Fn(TabId) -> Message,
    {
        Self::with_tab_labels(Vec::new(), on_select)
    }

    /// Creates a [`TabBar`] from `(id, label)` pairs.
    pub fn with_tab_labels<F>(tab_labels: Vec<(TabId, TabLabel)>, on_select: F) -> Self
    where
        F: 'static + Fn(TabId) -> Message,
    {
        Self {
            active_tab: 0,
            tab_indices: tab_labels.iter().map(|(id, _)| id.clone()).collect(),
            tab_statuses: tab_labels.iter().map(|_| (None, None)).collect(),
            tab_labels: tab_labels.into_iter().map(|(_, label)| label).collect(),
            on_select: Box::new(on_select),
            on_close: None,
            text_colour: None,
            width: Length::Fill,
            tab_width: Length::Fill,
            height: Length::Shrink,
            icon_size: DEFAULT_ICON_SIZE,
            text_size: DEFAULT_TEXT_SIZE,
            close_size: DEFAULT_CLOSE_SIZE,
            padding: DEFAULT_PADDING,
            spacing: DEFAULT_SPACING,
            font: None,
            text_font: None,
            class: <Theme as Catalog>::default(),
            position: Position::default(),
            _renderer: PhantomData,
        }
    }

    /// Sets the size of the close glyph.
    #[must_use]
    pub fn close_size(mut self, close_size: f32) -> Self {
        self.close_size = close_size;
        self
    }

    /// Override a label's foreground by stable tab identity, for example a
    /// disconnected document. Selection and close behaviour stay enabled.
    pub fn text_colour(mut self, colour: impl Fn(&TabId) -> Option<Color> + 'a) -> Self {
        self.text_colour = Some(Box::new(colour));
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

    /// Sets the height of the [`TabBar`].
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

    /// Sets the width of a tab.
    #[must_use]
    pub fn tab_width(mut self, width: Length) -> Self {
        self.tab_width = width;
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

    /// Sets the [`Position`] of the glyph next to the text.
    #[must_use]
    pub fn set_position(mut self, position: Position) -> Self {
        self.position = position;
        self
    }

    /// Sets the style of the [`TabBar`].
    #[must_use]
    pub fn style(mut self, style: impl Fn(&Theme, Status) -> Style + 'a) -> Self
    where
        <Theme as Catalog>::Class<'a>: From<StyleFn<'a, Theme>>,
    {
        self.class = (Box::new(style) as StyleFn<'a, Theme>).into();
        self
    }

    /// Sets the class of the [`TabBar`].
    #[must_use]
    pub fn class(mut self, class: impl Into<<Theme as Catalog>::Class<'a>>) -> Self {
        self.class = class.into();
        self
    }

    /// Sets the width of the [`TabBar`].
    #[must_use]
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }
}

fn layout_icon<Theme, Renderer>(
    icon: &char,
    size: f32,
    font: Option<Font>,
) -> Text<'_, Theme, Renderer>
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
        .shaping(Shaping::Advanced)
        .width(Length::Shrink)
}

impl<Message, TabId, Theme, Renderer> Widget<Message, Theme, Renderer>
    for TabBar<'_, Message, TabId, Theme, Renderer>
where
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog + iced_core::widget::text::Catalog,
    TabId: Eq + Clone,
{
    fn tag(&self) -> Tag {
        // The rebuilt layout element's tree is diffed into children[0] by
        // `layout`/`operate`; the bar itself holds no state.
        Tag::stateless()
    }

    fn state(&self) -> State {
        State::None
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> Node {
        let row =
            self.tab_labels
                .iter()
                .fold(Row::<Message, Theme, Renderer>::new(), |row, tab_label| {
                    let mut label_row = Row::new()
                        .push(
                            match tab_label {
                                TabLabel::Icon(icon) => Column::new()
                                    .align_x(Alignment::Center)
                                    .push(layout_icon::<Theme, Renderer>(
                                        icon,
                                        self.icon_size + 1.0,
                                        self.font,
                                    )),
                                TabLabel::Text(label) => Column::new()
                                    .padding(5.0)
                                    .align_x(Alignment::Center)
                                    .push(layout_text::<Theme, Renderer>(
                                        label,
                                        self.text_size + 1.0,
                                        self.text_font,
                                    )),
                                TabLabel::IconText(icon, label) => {
                                    let mut column = Column::new().align_x(Alignment::Center);
                                    match self.position {
                                        Position::Top => {
                                            column = column
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
                                        Position::Right => {
                                            column = column.push(
                                                Row::new()
                                                    .align_y(Alignment::Center)
                                                    .push(layout_text::<Theme, Renderer>(
                                                        label,
                                                        self.text_size + 1.0,
                                                        self.text_font,
                                                    ))
                                                    .push(layout_icon::<Theme, Renderer>(
                                                        icon,
                                                        self.icon_size + 1.0,
                                                        self.font,
                                                    )),
                                            );
                                        }
                                        Position::Left => {
                                            column = column.push(
                                                Row::new()
                                                    .align_y(Alignment::Center)
                                                    .push(layout_icon::<Theme, Renderer>(
                                                        icon,
                                                        self.icon_size + 1.0,
                                                        self.font,
                                                    ))
                                                    .push(layout_text::<Theme, Renderer>(
                                                        label,
                                                        self.text_size + 1.0,
                                                        self.text_font,
                                                    )),
                                            );
                                        }
                                        Position::Bottom => {
                                            column = column
                                                .height(Length::Fill)
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
                                    column
                                }
                            }
                            .width(self.tab_width)
                            .height(self.height),
                        )
                        .align_y(Alignment::Center)
                        .padding(self.padding)
                        .width(self.tab_width);

                    if self.on_close.is_some() {
                        label_row = label_row.push(
                            Row::new()
                                .width(Length::Fixed(self.close_size * 1.3 + 1.0))
                                .height(Length::Fixed(self.close_size * 1.3 + 1.0))
                                .align_y(Alignment::Center),
                        );
                    }

                    row.push(label_row)
                })
                .width(self.width)
                .height(self.height)
                .spacing(self.spacing)
                .align_y(Alignment::Center);

        let mut element: Element<Message, Theme, Renderer> = Element::new(row);
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
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Middle))
            | Event::Touch(iced_core::touch::Event::FingerPressed { .. })
                if cursor
                    .position()
                    .is_some_and(|pos| layout.bounds().contains(pos)) =>
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
                    let middle = matches!(
                        event,
                        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Middle))
                    );
                    if middle && self.on_close.is_none() {
                        return;
                    }
                    shell.publish(
                        self.on_close
                            .as_ref()
                            .filter(|_on_close| {
                                let tab_layout = layout.children().nth(new_selected).expect(
                                    "widget: Layout should have a tab layout at the selected index",
                                );
                                let cross_layout = tab_layout
                                    .children()
                                    .nth(1)
                                    .expect("widget: Layout should have a close layout");

                                middle
                                    || cursor
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
            _ => {}
        }

        // Track hover for restyling on the next redraw.
        let mut request_redraw = false;
        let children = layout.children();
        for ((i, _tab), layout) in self.tab_labels.iter().enumerate().zip(children) {
            let active_idx = self.get_active_tab_idx();
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
            if let Some(cross_layout) = children.next_back() {
                is_cross_hovered = Some(cursor.is_over(cross_layout.bounds()));
            }

            if let Event::Window(window::Event::RedrawRequested(_now)) = event {
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
                    shadow: Shadow::default(),
                    ..renderer::Quad::default()
                },
                style_sheet
                    .background
                    .unwrap_or_else(|| Color::TRANSPARENT.into()),
            );
        }

        for ((i, tab), layout) in self.tab_labels.iter().enumerate().zip(children) {
            let tab_status = self.tab_statuses.get(i).expect("Should have a status.");

            draw_tab(
                renderer,
                tab,
                tab_status,
                layout,
                self.position,
                theme,
                &self.class,
                (self.font.unwrap_or_default(), self.icon_size),
                (self.text_font.unwrap_or_default(), self.text_size),
                self.close_size,
                self.text_colour
                    .as_ref()
                    .and_then(|colour| colour(&self.tab_indices[i])),
                viewport,
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

        // Rebuild the internal row so operations (focus, measurement,
        // accessibility) can see each label.
        let row =
            self.tab_labels
                .iter()
                .fold(Row::<(), Theme, Renderer>::new(), |row, tab_label| {
                    let label_content: Element<'_, (), Theme, Renderer> = match tab_label {
                        TabLabel::Icon(icon) => Text::<Theme, Renderer>::new(icon.to_string())
                            .size(self.icon_size)
                            .font(self.font.unwrap_or_default())
                            .into(),
                        TabLabel::Text(label) => Text::<Theme, Renderer>::new(label.clone())
                            .size(self.text_size)
                            .font(self.text_font.unwrap_or_default())
                            .into(),
                        TabLabel::IconText(icon, label) => {
                            let (first, second) = match self.position {
                                Position::Top | Position::Left => (
                                    Text::<Theme, Renderer>::new(icon.to_string())
                                        .size(self.icon_size)
                                        .font(self.font.unwrap_or_default()),
                                    Text::<Theme, Renderer>::new(label.clone())
                                        .size(self.text_size)
                                        .font(self.text_font.unwrap_or_default()),
                                ),
                                Position::Bottom | Position::Right => (
                                    Text::<Theme, Renderer>::new(label.clone())
                                        .size(self.text_size)
                                        .font(self.text_font.unwrap_or_default()),
                                    Text::<Theme, Renderer>::new(icon.to_string())
                                        .size(self.icon_size)
                                        .font(self.font.unwrap_or_default()),
                                ),
                            };
                            if matches!(self.position, Position::Top | Position::Bottom) {
                                Column::<(), Theme, Renderer>::new()
                                    .push(first)
                                    .push(second)
                                    .into()
                            } else {
                                Row::<(), Theme, Renderer>::new()
                                    .push(first)
                                    .push(second)
                                    .into()
                            }
                        }
                    };

                    let mut label_row = Row::<(), Theme, Renderer>::new().push(label_content);

                    if self.on_close.is_some() {
                        label_row =
                            label_row.push(Text::<Theme, Renderer>::new("×").size(self.close_size));
                    }

                    row.push(label_row)
                });

        let mut element: Element<(), Theme, Renderer> = Element::new(row);
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

/// Draws one tab.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn draw_tab<Theme, Renderer>(
    renderer: &mut Renderer,
    tab: &TabLabel,
    tab_status: &(Option<Status>, Option<bool>),
    layout: Layout<'_>,
    position: Position,
    theme: &Theme,
    class: &<Theme as Catalog>::Class<'_>,
    icon_data: (Font, f32),
    text_data: (Font, f32),
    close_size: f32,
    text_colour: Option<Color>,
    viewport: &Rectangle,
) where
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog + iced_core::widget::text::Catalog,
{
    fn icon_bound_rectangle(item: Option<Layout<'_>>) -> Rectangle {
        item.expect("Graphics: Layout should have an icons layout for an IconText")
            .bounds()
    }

    fn text_bound_rectangle(item: Option<Layout<'_>>) -> Rectangle {
        item.expect("Graphics: Layout should have an texts layout for an IconText")
            .bounds()
    }

    let bounds = layout.bounds();

    let mut style =
        <Theme as Catalog>::style(theme, class, tab_status.0.unwrap_or(Status::Disabled));
    if let Some(colour) = text_colour {
        style.text_color = colour;
    }

    let mut children = layout.children();
    let label_layout = children
        .next()
        .expect("Graphics: Layout should have a label layout");
    let mut label_layout_children = label_layout.children();

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

    let (icon_bounds, text_bounds) = match tab {
        TabLabel::Icon(_) => {
            let icon_bounds = icon_bound_rectangle(label_layout_children.next());
            (icon_bounds, None)
        }
        TabLabel::Text(_) => {
            let text_bounds = text_bound_rectangle(label_layout_children.next());
            (Rectangle::default(), Some(text_bounds))
        }
        TabLabel::IconText(..) => match position {
            Position::Top => {
                let icon_bounds = icon_bound_rectangle(label_layout_children.next());
                let text_bounds = text_bound_rectangle(label_layout_children.next());
                (icon_bounds, Some(text_bounds))
            }
            Position::Right => {
                let mut row_children = label_layout_children
                    .next()
                    .expect("Graphics: Right Layout should have a row child")
                    .children();
                let text_bounds = text_bound_rectangle(row_children.next());
                let icon_bounds = icon_bound_rectangle(row_children.next());
                (icon_bounds, Some(text_bounds))
            }
            Position::Left => {
                let mut row_children = label_layout_children
                    .next()
                    .expect("Graphics: Left Layout should have a row child")
                    .children();
                let icon_bounds = icon_bound_rectangle(row_children.next());
                let text_bounds = text_bound_rectangle(row_children.next());
                (icon_bounds, Some(text_bounds))
            }
            Position::Bottom => {
                let text_bounds = text_bound_rectangle(label_layout_children.next());
                let icon_bounds = icon_bound_rectangle(label_layout_children.next());
                (icon_bounds, Some(text_bounds))
            }
        },
    };

    match tab {
        TabLabel::Icon(icon) | TabLabel::IconText(icon, _) => {
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
        _ => {}
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

    if let Some(cross_layout) = children.next() {
        let cross_bounds = cross_layout.bounds();
        let is_mouse_over_cross = tab_status.1.unwrap_or(false);

        renderer.fill_text(
            iced_core::text::Text {
                content: "×".to_owned(),
                bounds: Size::new(cross_bounds.width, cross_bounds.height),
                size: Pixels(close_size + if is_mouse_over_cross { 1.0 } else { 0.0 }),
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

        if is_mouse_over_cross && cross_bounds.intersects(viewport) {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: cross_bounds,
                    border: Border {
                        radius: style.icon_border_radius,
                        width: style.border_width,
                        color: style.border_color.unwrap_or(Color::TRANSPARENT),
                    },
                    shadow: Shadow::default(),
                    ..renderer::Quad::default()
                },
                style
                    .icon_background
                    .unwrap_or(Background::Color(Color::TRANSPARENT)),
            );
        }
    }
}

impl<'a, Message, TabId, Theme, Renderer> From<TabBar<'a, Message, TabId, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Renderer: 'a + renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: 'a + Catalog + iced_core::widget::text::Catalog,
    Message: 'a,
    TabId: 'a + Eq + Clone,
{
    fn from(tab_bar: TabBar<'a, Message, TabId, Theme, Renderer>) -> Self {
        Element::new(tab_bar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_renderer::LayoutRenderer;

    type TestBar<'a> = TabBar<'a, u8, u8, iced_core::Theme, LayoutRenderer>;

    fn bar() -> TestBar<'static> {
        TestBar::new(|id| id)
            .push(0, TabLabel::Text("One".into()))
            .push(1, TabLabel::Text("Two".into()))
            .push(2, TabLabel::Text("Three".into()))
            .set_active_tab(&1)
    }

    #[test]
    fn active_tab_is_found_by_id() {
        assert_eq!(bar().get_active_tab_idx(), 1);
        assert_eq!(bar().get_active_tab_id(), Some(&1));
        assert_eq!(bar().size(), 3);
    }

    #[test]
    fn tabs_lay_out_as_one_row_of_labels() {
        let bar = bar();
        let renderer = LayoutRenderer::new();
        let mut element: Element<'_, u8, iced_core::Theme, LayoutRenderer> = bar.into();
        let mut tree = Tree::new(&element);
        element.as_widget_mut().diff(&mut tree);
        let limits = Limits::new(Size::ZERO, Size::new(600.0, 40.0));
        let node = element
            .as_widget_mut()
            .layout(&mut tree, &renderer, &limits);
        let laid: Vec<_> = Layout::new(&node).children().collect();
        assert_eq!(laid.len(), 3, "one child per tab");
        assert_eq!(laid[0].bounds().y, laid[1].bounds().y, "one row");
    }
}
