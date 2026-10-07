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
use iced_core::text::{self, Shaping, Wrapping};
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

use crate::typography::TextStyle;

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
    /// A prepared icon style; overrides the icon font/size builders.
    icon_style: Option<TextStyle>,
    /// A prepared label style; overrides the text font/size builders.
    text_style: Option<TextStyle>,
    /// A prepared close style; overrides the close size and font.
    close_text_style: Option<TextStyle>,
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
            icon_style: None,
            text_style: None,
            close_text_style: None,
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

    /// A prepared icon style: font, size and line height together. It
    /// overrides `icon_font`/`icon_size` regardless of builder order.
    #[must_use]
    pub fn icon_style(mut self, text: TextStyle) -> Self {
        self.icon_style = Some(text);
        self
    }

    /// A prepared label style: font, size and line height together. It
    /// overrides `text_font`/`text_size` regardless of builder order.
    #[must_use]
    pub fn text_style(mut self, text: TextStyle) -> Self {
        self.text_style = Some(text);
        self
    }

    /// A prepared close style for the × glyph and its slot. It overrides
    /// `close_size` and the renderer's default font regardless of builder
    /// order.
    #[must_use]
    pub fn close_text_style(mut self, text: TextStyle) -> Self {
        self.close_text_style = Some(text);
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

/// The resolved styles a tab draws and measures with: a prepared style when
/// supplied, else the legacy font/size builders and renderer defaults. The
/// `*_prepared` flags select each role's geometry: a prepared role measures
/// its exact resolved style, an unprepared role keeps the legacy allowances
/// (the `+1.0` measurement slack and the `close_size * 1.3 + 1.0` close
/// slot), so a partially prepared bar never silently tightens an unset
/// role.
#[derive(Clone, Copy, Debug)]
struct Resolved {
    icon: TextStyle,
    text: TextStyle,
    close: TextStyle,
    icon_prepared: bool,
    text_prepared: bool,
    close_prepared: bool,
}

impl<'a, Message, TabId, Theme, Renderer> TabBar<'a, Message, TabId, Theme, Renderer>
where
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog,
    TabId: Eq + Clone,
{
    /// Whether any prepared style was supplied. The prepared path resolves
    /// every role through one shared row hierarchy; roles without a
    /// prepared style keep their legacy geometry.
    fn prepared(&self) -> bool {
        self.icon_style.is_some() || self.text_style.is_some() || self.close_text_style.is_some()
    }

    /// The styles every tab resolves to, with the per-role prepared flags.
    fn resolved(&self, renderer: &Renderer) -> Resolved {
        Resolved {
            icon: self.icon_style.unwrap_or(TextStyle {
                font: self.font.unwrap_or_default(),
                size: self.icon_size,
                line_height: None,
            }),
            text: self.text_style.unwrap_or(TextStyle {
                font: self.text_font.unwrap_or_default(),
                size: self.text_size,
                line_height: None,
            }),
            close: self.close_text_style.unwrap_or(TextStyle {
                font: renderer.default_font(),
                size: self.close_size,
                line_height: None,
            }),
            icon_prepared: self.icon_style.is_some(),
            text_prepared: self.text_style.is_some(),
            close_prepared: self.close_text_style.is_some(),
        }
    }
}

impl<'a, Message, TabId, Theme, Renderer> TabBar<'a, Message, TabId, Theme, Renderer>
where
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog + iced_core::widget::text::Catalog,
    TabId: Eq + Clone,
{
    /// One prepared row per tab: the label content (with the IconText
    /// position variants) then the close slot. A prepared role measures its
    /// exact resolved style; an unprepared role keeps its legacy allowances.
    /// Layout, drawing, hit testing and operations all consume this one
    /// hierarchy.
    fn resolved_row<'b, M: 'b>(
        &self,
        tab: &'b TabLabel,
        resolved: &Resolved,
    ) -> Row<'b, M, Theme, Renderer>
    where
        Theme: 'b,
        Renderer: 'b,
    {
        // Unprepared roles keep the legacy `+1.0` measurement allowance;
        // prepared roles measure their exact resolved styles.
        let measured_icon = TextStyle {
            size: resolved.icon.size + if resolved.icon_prepared { 0.0 } else { 1.0 },
            ..resolved.icon
        };
        let measured_text = TextStyle {
            size: resolved.text.size + if resolved.text_prepared { 0.0 } else { 1.0 },
            ..resolved.text
        };
        let content = match tab {
            TabLabel::Icon(icon) => Column::new()
                .align_x(Alignment::Center)
                .push(styled_icon(*icon, measured_icon))
                .width(self.tab_width)
                .height(self.height),
            TabLabel::Text(label) => Column::new()
                .align_x(Alignment::Center)
                // The legacy text tab's 5-pixel column padding: a prepared
                // text tab keeps it, so adopting a prepared style never
                // restructures the tab.
                .padding(5.0)
                .push(styled_label(label, measured_text))
                .width(self.tab_width)
                .height(self.height),
            TabLabel::IconText(icon, label) => {
                let icon = styled_icon(*icon, measured_icon);
                let label = styled_label(label, measured_text);
                let column = match self.position {
                    Position::Top => Column::new()
                        .align_x(Alignment::Center)
                        .push(icon)
                        .push(label),
                    Position::Bottom => Column::new()
                        .align_x(Alignment::Center)
                        .push(label)
                        .push(icon),
                    Position::Left => Column::new()
                        .align_x(Alignment::Center)
                        .push(Row::new().align_y(Alignment::Center).push(icon).push(label)),
                    Position::Right => Column::new()
                        .align_x(Alignment::Center)
                        .push(Row::new().align_y(Alignment::Center).push(label).push(icon)),
                };
                column.width(self.tab_width).height(self.height)
            }
        };
        let mut label_row = Row::new().push(content);
        if self.on_close.is_some() {
            // A prepared close slot is the exact resolved line box; an
            // unprepared one keeps the legacy slot that reserves the hover
            // growth. Drawing and hit testing use this same rectangle, and
            // the × never grows past it.
            let slot = if resolved.close_prepared {
                resolved.close.minimum_height()
            } else {
                resolved.close.size * 1.3 + 1.0
            };
            label_row = label_row.push(
                Row::new()
                    .width(Length::Fixed(slot))
                    .height(Length::Fixed(slot))
                    .align_y(Alignment::Center),
            );
        }
        label_row
            .align_y(Alignment::Center)
            .padding(self.padding)
            .width(self.tab_width)
    }
}

/// The prepared icon text of a tab label.
fn styled_icon<'a, Theme: iced_core::widget::text::Catalog + 'a, Renderer>(
    icon: char,
    text: TextStyle,
) -> Text<'a, Theme, Renderer>
where
    Renderer: iced_core::text::Renderer<Font = iced_core::Font>,
{
    Text::<Theme, Renderer>::new(icon.to_string())
        .size(text.size)
        .height(text.minimum_height())
        .font(text.font)
        .line_height(text.line_height_or_default())
        .align_x(alignment::Horizontal::Center)
        .align_y(alignment::Vertical::Center)
        .shaping(Shaping::Advanced)
        .width(Length::Shrink)
}

/// The prepared label text of a tab.
fn styled_label<'a, Theme: iced_core::widget::text::Catalog + 'a, Renderer>(
    label: &'a str,
    text: TextStyle,
) -> Text<'a, Theme, Renderer>
where
    Renderer: iced_core::text::Renderer<Font = iced_core::Font>,
{
    Text::<Theme, Renderer>::new(label)
        .size(text.size)
        .height(text.minimum_height())
        .font(text.font)
        .line_height(text.line_height_or_default())
        .align_x(alignment::Horizontal::Center)
        .shaping(Shaping::Advanced)
        .width(Length::Shrink)
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
        let resolved = self.resolved(renderer);
        let prepared = self.prepared();
        let row =
            self.tab_labels
                .iter()
                .fold(Row::<Message, Theme, Renderer>::new(), |row, tab_label| {
                    if prepared {
                        return row.push(self.resolved_row::<Message>(tab_label, &resolved));
                    }
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
                self.resolved(renderer),
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
        // accessibility) can see each label. The prepared path rebuilds the
        // same resolved hierarchy layout used; the legacy path keeps its
        // historical reconstruction.
        let resolved = self.resolved(renderer);
        let row = if self.prepared() {
            self.tab_labels
                .iter()
                .fold(Row::<(), Theme, Renderer>::new(), |row, tab_label| {
                    row.push(self.resolved_row::<()>(tab_label, &resolved))
                })
        } else {
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
                })
        };

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
    resolved: Resolved,
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
                    size: Pixels(resolved.icon.size),
                    font: resolved.icon.font,
                    align_x: text::Alignment::Center,
                    align_y: Vertical::Center,
                    line_height: resolved.icon.line_height_or_default(),
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
                size: Pixels(resolved.text.size),
                font: resolved.text.font,
                align_x: text::Alignment::Center,
                align_y: Vertical::Center,
                line_height: resolved.text.line_height_or_default(),
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
                // A prepared close keeps the glyph at its resolved size:
                // hover is colour-only and the glyph never grows past its
                // allocated hit region. An unprepared close keeps the
                // legacy hover growth, which its legacy-sized slot
                // reserves before hit testing.
                size: Pixels(
                    resolved.close.size
                        + if !resolved.close_prepared && is_mouse_over_cross {
                            1.0
                        } else {
                            0.0
                        },
                ),
                font: resolved.close.font,
                align_x: text::Alignment::Center,
                align_y: Vertical::Center,
                line_height: resolved.close.line_height_or_default(),
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
    use crate::typography::TextStyle;
    use iced_core::layout;

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

    /// Records every drawn text with its resolved font, size and line
    /// height; the prepared draw path fills text directly.
    #[derive(Default)]
    struct TextRecorder {
        texts: Vec<(String, Font, Pixels, text::LineHeight)>,
    }

    impl renderer::Renderer for TextRecorder {
        fn start_layer(&mut self, _: Rectangle) {}
        fn end_layer(&mut self) {}
        fn start_transformation(&mut self, _: iced_core::Transformation) {}
        fn end_transformation(&mut self) {}
        fn hint(&mut self, _: renderer::Scale) {}
        fn scale(&self) -> Option<renderer::Scale> {
            None
        }
        fn reset(&mut self, _: Rectangle) {}
        fn settings(&self) -> renderer::Settings {
            renderer::Settings::default()
        }
        fn fill_quad(&mut self, _: renderer::Quad, _: impl Into<iced_core::Background>) {}
        fn allocate_image(
            &mut self,
            handle: &iced_core::image::Handle,
            callback: impl FnOnce(Result<iced_core::image::Allocation, iced_core::image::Error>)
            + Send
            + 'static,
        ) {
            let _ = handle;
            callback(Err(iced_core::image::Error::Unsupported));
        }
    }

    impl text::Renderer for TextRecorder {
        type Font = Font;
        type Paragraph = iced_graphics::text::Paragraph;
        type Editor = iced_graphics::text::Editor;

        const ICON_FONT: Font = Font::new("Iced-Icons");
        const CHECKMARK_ICON: char = '\u{f00c}';
        const ARROW_DOWN_ICON: char = '\u{e800}';
        const SCROLL_UP_ICON: char = '\u{e802}';
        const SCROLL_DOWN_ICON: char = '\u{e803}';
        const SCROLL_LEFT_ICON: char = '\u{e804}';
        const SCROLL_RIGHT_ICON: char = '\u{e805}';
        const ICED_LOGO: char = '\u{e801}';

        fn default_font(&self) -> Font {
            Font::DEFAULT
        }
        fn default_size(&self) -> Pixels {
            Pixels(16.0)
        }
        fn fill_paragraph(&mut self, _: &Self::Paragraph, _: Point, _: Color, _: Rectangle) {}
        fn fill_editor(&mut self, _: &Self::Editor, _: Point, _: Color, _: Rectangle) {}
        fn fill_text(&mut self, text: text::Text, _: Point, _: Color, _: Rectangle) {
            self.texts
                .push((text.content, text.font, text.size, text.line_height));
        }
    }

    fn icon_style() -> TextStyle {
        TextStyle {
            font: Font::new("icons"),
            size: 20.0,
            line_height: Some(8.0),
        }
    }
    fn text_style() -> TextStyle {
        TextStyle {
            font: Font::MONOSPACE,
            size: 14.0,
            line_height: Some(30.0),
        }
    }
    fn close_style() -> TextStyle {
        TextStyle {
            font: Font::DEFAULT,
            size: 12.0,
            line_height: None,
        }
    }

    fn prepared_bar<R>(styles_first: bool) -> TabBar<'static, u8, u8, iced_core::Theme, R>
    where
        R: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    {
        let mut bar = TabBar::new(|id| id)
            .push(0, TabLabel::IconText('♣', "tab".into()))
            .set_active_tab(&0)
            .on_close(|id| id + 200)
            .close_size(30.0);
        if styles_first {
            bar = bar
                .icon_style(icon_style())
                .text_style(text_style())
                .close_text_style(close_style());
        } else {
            bar = bar
                .icon_font(Font::DEFAULT)
                .icon_size(9.0)
                .text_font(Font::DEFAULT)
                .text_size(9.0)
                .close_text_style(close_style())
                .text_style(text_style())
                .icon_style(icon_style());
        }
        bar
    }

    fn layout_bar(bar: &mut TestBar<'static>) -> (Tree, layout::Node) {
        let renderer = LayoutRenderer::new();
        let mut tree = Tree::new(bar as &dyn Widget<u8, iced_core::Theme, LayoutRenderer>);
        bar.diff(&mut tree);
        let node = Widget::layout(
            bar,
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, Size::new(400.0, 100.0)),
        );
        (tree, node)
    }

    #[test]
    fn prepared_styles_resolve_regardless_of_builder_order() {
        let mut prepared = prepared_bar::<LayoutRenderer>(true);
        let (tree, node) = layout_bar(&mut prepared);
        let texts_of = |first: bool| {
            let bar = prepared_bar::<TextRecorder>(first);
            let mut recorder = TextRecorder::default();
            Widget::draw(
                &bar,
                &tree,
                &mut recorder,
                &iced_core::Theme::Dark,
                &renderer::Style::default(),
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &Rectangle::with_size(Size::new(400.0, 100.0)),
            );
            recorder.texts
        };
        let a = texts_of(true);
        let b = texts_of(false);
        assert_eq!(a, b, "builder order must not change the prepared geometry");
        // The icon draws at its own resolved style: line height below the
        // size stays absolute.
        assert!(
            a.iter().any(|(content, font, size, line)| {
                content == "♣"
                    && *font == icon_style().font
                    && size.0 == 20.0
                    && *line == text::LineHeight::Absolute(Pixels(8.0))
            }),
            "icon style missing: {:#?}",
            a
        );
        // The label draws at its own resolved style: line height above the
        // size stays absolute.
        assert!(
            a.iter().any(|(content, font, size, line)| {
                content == "tab"
                    && *font == text_style().font
                    && size.0 == 14.0
                    && *line == text::LineHeight::Absolute(Pixels(30.0))
            }),
            "label style missing: {:#?}",
            a
        );
        // The close glyph uses the close style and never grows past its
        // allocated slot: the size is the resolved one, not 30 + hover.
        assert!(
            a.iter().any(|(content, font, size, line)| {
                content == "×"
                    && *font == close_style().font
                    && size.0 == 12.0
                    && *line == text::LineHeight::default()
            }),
            "close style missing: {:#?}",
            a
        );
    }

    #[test]
    fn prepared_rows_keep_deterministic_label_then_close_children() {
        for position in [
            Position::Top,
            Position::Right,
            Position::Bottom,
            Position::Left,
        ] {
            let mut bar = prepared_bar::<LayoutRenderer>(true).set_position(position);
            let (_, node) = layout_bar(&mut bar);
            let mut tabs = Layout::new(&node).children();
            let tab = tabs.next().expect("one tab");
            let mut children = tab.children();
            let content = children.next().expect("label content");
            let close = children.next().expect("close slot");
            assert!(
                children.next().is_none(),
                "one deterministic row child per tab"
            );
            let slot = close_style().minimum_height();
            assert_eq!(
                close.bounds().height,
                slot,
                "the close slot fits its line box"
            );
            assert_eq!(close.bounds().width, slot);
            // The label content children follow the position variant.
            let content_children: Vec<_> = content.children().collect();
            match position {
                Position::Top => {
                    assert_eq!(content_children.len(), 2);
                    assert_eq!(content_children[0].bounds().height, 20.0);
                    assert_eq!(content_children[1].bounds().height, 30.0);
                }
                Position::Bottom => {
                    assert_eq!(content_children.len(), 2);
                    assert_eq!(content_children[0].bounds().height, 30.0);
                    assert_eq!(content_children[1].bounds().height, 20.0);
                }
                Position::Left | Position::Right => {
                    assert_eq!(content_children.len(), 1, "one inner row");
                    let row: Vec<_> = content_children[0].children().collect();
                    assert_eq!(row.len(), 2);
                    let (first, second) = if matches!(position, Position::Left) {
                        (20.0, 30.0)
                    } else {
                        (30.0, 20.0)
                    };
                    assert_eq!(row[0].bounds().height, first);
                    assert_eq!(row[1].bounds().height, second);
                }
            }
        }
    }

    #[test]
    fn prepared_close_hits_only_the_allocated_slot() {
        let mut bar = prepared_bar::<LayoutRenderer>(true);
        let (mut tree, node) = layout_bar(&mut bar);
        let tab = Layout::new(&node).children().next().expect("one tab");
        let mut children = tab.children();
        let content = children.next().expect("label content");
        let close = children.next().expect("close slot");
        let mut click = |at: Point| {
            let mut bus = iced_core::shell::Bus::new();
            let mut shell = Shell::new(
                &iced_core::window::Headless,
                iced_core::shell::Waker::noop(),
                &mut bus,
            );
            Widget::update(
                &mut bar,
                &mut tree,
                &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Layout::new(&node),
                mouse::Cursor::Available(at),
                &LayoutRenderer::new(),
                &mut shell,
                &Rectangle::with_size(Size::new(400.0, 100.0)),
            );
            bus.drain().collect::<Vec<_>>()
        };
        // The close slot centre publishes the close callback for the tab.
        assert_eq!(click(close.bounds().center()), [200]);
        // The label publishes the select callback instead.
        assert_eq!(click(content.bounds().center()), [0]);
    }

    #[test]
    fn prepared_text_tabs_keep_the_legacy_five_pixel_padding() {
        let mut bar: TestBar<'static> = TabBar::new(|id| id)
            .push(0, TabLabel::Text("One".into()))
            .tab_width(Length::Shrink)
            .text_style(TextStyle {
                font: Font::MONOSPACE,
                size: 14.0,
                line_height: Some(30.0),
            });
        let (_, node) = layout_bar(&mut bar);
        let tab = Layout::new(&node).children().next().expect("one tab");
        let content = tab.children().next().expect("label content");
        let label = content.children().next().expect("label");
        assert_eq!(
            label.bounds().x - content.bounds().x,
            5.0,
            "five pixels left"
        );
        assert_eq!(
            content.bounds().width - label.bounds().width,
            10.0,
            "five pixels on each side"
        );
    }

    #[test]
    fn partially_prepared_bars_keep_legacy_geometry_for_unset_roles() {
        // Only the text role is prepared. The unset icon keeps its legacy
        // `+1.0` measurement allowance and the unset close keeps the
        // legacy `close_size * 1.3 + 1.0` slot, so adopting one prepared
        // style never silently tightens the others.
        let geometry = |text_style: Option<TextStyle>| {
            let mut bar: TestBar<'static> = TabBar::new(|id| id)
                .push(0, TabLabel::IconText('♣', "tab".into()))
                .set_active_tab(&0)
                .on_close(|id| id + 200);
            if let Some(text) = text_style {
                bar = bar.text_style(text);
            }
            let (_, node) = layout_bar(&mut bar);
            let tab = Layout::new(&node).children().next().expect("one tab");
            let mut children = tab.children();
            let content = children.next().expect("label content");
            let close = children.next().expect("close slot");
            let inner = content.children().next().expect("position row");
            let mut inner = inner.children();
            let icon = inner.next().expect("icon");
            let label = inner.next().expect("label");
            (
                close.bounds().height,
                icon.bounds().height,
                label.bounds().height,
            )
        };
        let legacy = geometry(None);
        let partial = geometry(Some(TextStyle {
            font: Font::MONOSPACE,
            size: 14.0,
            line_height: Some(30.0),
        }));
        assert_eq!(partial.0, legacy.0, "the unset close slot is not tightened");
        assert_eq!(
            partial.1, legacy.1,
            "the unset icon allowance is not tightened"
        );
        // The prepared text role measures its exact resolved line box.
        assert_eq!(partial.2, 30.0);
        assert_ne!(partial.2, legacy.2);
    }

    #[test]
    fn retained_cache_swaps_prepared_styles_keeping_order_and_close_target() {
        let small = TextStyle {
            font: Font::DEFAULT,
            size: 12.0,
            line_height: None,
        };
        let big = TextStyle {
            font: Font::MONOSPACE,
            size: 16.0,
            line_height: Some(40.0),
        };
        let close = TextStyle {
            font: Font::DEFAULT,
            size: 12.0,
            line_height: None,
        };
        let bar = |text: TextStyle| -> Element<'static, u8, iced_core::Theme, LayoutRenderer> {
            TabBar::new(|id| id)
                .push(0, TabLabel::Text("one".into()))
                .push(1, TabLabel::Text("two".into()))
                .set_active_tab(&1)
                .on_close(|id| id + 200)
                .text_style(text)
                .close_text_style(close)
                .into()
        };
        let mut renderer = LayoutRenderer::new();
        let ui = iced_runtime::UserInterface::build(
            bar(small),
            Size::new(400.0, 100.0),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let cache = ui.into_cache();
        let mut ui = iced_runtime::UserInterface::build(
            bar(big),
            Size::new(400.0, 100.0),
            cache,
            &mut renderer,
        );
        // The same prepared bar laid out standalone gives the close slot of
        // the second tab under the new geometry.
        let mut standalone: TestBar<'static> = TabBar::new(|id| id)
            .push(0, TabLabel::Text("one".into()))
            .push(1, TabLabel::Text("two".into()))
            .set_active_tab(&1)
            .on_close(|id| id + 200)
            .text_style(big)
            .close_text_style(close);
        let (_, node) = layout_bar(&mut standalone);
        let second = Layout::new(&node).children().nth(1).expect("second tab");
        let close_bounds = second.children().nth(1).expect("close slot").bounds();
        let mut messages = vec![];
        let mut bus = iced_core::shell::Bus::new();
        ui.update(
            &iced_core::window::Headless,
            &iced_core::shell::Waker::noop(),
            &[Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Left,
            ))],
            mouse::Cursor::Available(close_bounds.center()),
            &mut renderer,
            &mut bus,
        );
        messages.extend(bus);
        assert_eq!(
            messages,
            [201],
            "the retained cache relays the new close target"
        );
        // The tab order is unchanged: the first tab's label selects id 0.
        let first = Layout::new(&node).children().next().expect("first tab");
        let label_bounds = first.children().next().expect("label").bounds();
        let mut bus = iced_core::shell::Bus::new();
        ui.update(
            &iced_core::window::Headless,
            &iced_core::shell::Waker::noop(),
            &[Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Left,
            ))],
            mouse::Cursor::Available(label_bounds.center()),
            &mut renderer,
            &mut bus,
        );
        assert_eq!(bus.drain().collect::<Vec<_>>(), [0]);
    }
}
