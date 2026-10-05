// SPDX-License-Identifier: MIT OR Apache-2.0
//! A resizable-column table: [`Table`], header and body (and optional
//! footer) as separate scrollables kept in sync by one `on_sync`
//! message, with drag-to-resize dividers on each column edge.
//!
//! The caller owns the widths: a column reports its width and any
//! in-flight resize offset ([`Column::width`],
//! [`Column::resize_offset`]); the table reports drags as
//! `on_drag(index, new_width - old_width)` and the finished resize as
//! `on_release`, and the caller stores the result. [`sort_label`] is the
//! sort-header helper: a label with `↑`/`↓` on the active column.
//!
//! The catalog is lazy like iced_table's: the class is a cheap `Clone`
//! token resolved against the theme at draw time, because the header,
//! rows and dividers are built before any theme is at hand.

use iced_core::layout::{self, Layout, Limits, Node};
use iced_core::mouse::{self, Cursor};
use iced_core::padding;
use iced_core::renderer;
use iced_core::widget::tree::{State, Tag};
use iced_core::widget::{Id, Operation, Tree, Widget};
use iced_core::widget::text::Text;
use iced_core::{
    Background, Color, Element, Event, Length, Padding, Point, Rectangle, Shell, Size, Vector,
    overlay,
};
use iced_widget::{column, container, row, scrollable, space, Space};

/// A background/text pair, what a band of the table paints.
#[derive(Clone, Copy, Debug, Default)]
pub struct Band {
    /// The band background; `None` paints nothing.
    pub background: Option<Background>,
    /// The band text colour; `None` keeps the inherited colour.
    pub text: Option<Color>,
}

/// The theme catalog of a [`Table`]. The `Style` is a cheap token the
/// caller picks (`()` for the default look), resolved against the theme
/// when each band is drawn.
pub trait Catalog {
    /// The caller-chosen style token.
    type Style: Default + Clone;

    /// The header band of the [`Table`].
    fn header(&self, style: &Self::Style) -> Band;
    /// The footer band of the [`Table`].
    fn footer(&self, style: &Self::Style) -> Band;
    /// One row band of the [`Table`], by row index (zebra striping is
    /// the catalog's call).
    fn row(&self, style: &Self::Style, index: usize) -> Band;
    /// The resize divider; `hovered` covers hover and drag.
    fn divider(&self, style: &Self::Style, hovered: bool) -> Option<Background>;
}

impl Catalog for crate::theme::Theme {
    type Style = ();

    fn header(&self, _style: &Self::Style) -> Band {
        let p = self.tokens().palette;
        Band {
            background: Some(p.elevated.into()),
            text: Some(p.elevated_text),
        }
    }

    fn footer(&self, style: &Self::Style) -> Band {
        self.header(style)
    }

    fn row(&self, _style: &Self::Style, index: usize) -> Band {
        let p = self.tokens().palette;
        let background = if index % 2 == 0 { p.card } else { p.muted_surface };
        Band {
            background: Some(background.into()),
            text: Some(p.card_text),
        }
    }

    fn divider(&self, _style: &Self::Style, hovered: bool) -> Option<Background> {
        let p = self.tokens().palette;
        Some(
            if hovered {
                p.primary
            } else {
                p.border
            }
            .into(),
        )
    }
}

impl Catalog for iced_core::Theme {
    type Style = ();

    fn header(&self, _style: &Self::Style) -> Band {
        let pair = self.palette().background.strong;
        Band {
            background: Some(pair.color.into()),
            text: Some(pair.text),
        }
    }

    fn footer(&self, style: &Self::Style) -> Band {
        self.header(style)
    }

    fn row(&self, _style: &Self::Style, index: usize) -> Band {
        let palette = self.palette();
        let pair = if index % 2 == 0 {
            palette.background.base
        } else {
            palette.background.weak
        };
        Band {
            background: Some(pair.color.into()),
            text: Some(pair.text),
        }
    }

    fn divider(&self, _style: &Self::Style, hovered: bool) -> Option<Background> {
        let palette = self.palette();
        Some(
            if hovered {
                palette.primary.base.color
            } else {
                palette.background.weak.color
            }
            .into(),
        )
    }
}

/// A column definition for a [`Table`]: what the header, each cell and
/// the optional footer show, and the column's width state.
pub trait Column<'a, Message, Theme, Renderer> {
    /// A row of data.
    type Row;

    /// The header [`Element`] for this column.
    fn header(&'a self, col_index: usize) -> Element<'a, Message, Theme, Renderer>;

    /// The cell [`Element`] for one row.
    fn cell(
        &'a self,
        col_index: usize,
        row_index: usize,
        row: &'a Self::Row,
    ) -> Element<'a, Message, Theme, Renderer>;

    /// The footer [`Element`] for this column, over all rows.
    fn footer(
        &'a self,
        _col_index: usize,
        _rows: &'a [Self::Row],
    ) -> Option<Element<'a, Message, Theme, Renderer>> {
        None
    }

    /// The fixed width for this column.
    fn width(&self) -> f32;

    /// The offset of an ongoing resize of this column.
    fn resize_offset(&self) -> Option<f32>;
}

/// A sort-header label: the column name with `↑` or `↓` when it is the
/// active sort — the generic shape of a file manager's column header.
#[must_use]
pub fn sort_label<Theme, Renderer>(
    label: &str,
    active: bool,
    ascending: bool,
) -> Text<'static, Theme, Renderer>
where
    Renderer: iced_core::text::Renderer,
    Theme: iced_core::widget::text::Catalog,
{
    let label = if active {
        format!("{label} {}", if ascending { "↑" } else { "↓" })
    } else {
        label.to_owned()
    };
    Text::new(label)
}

/// Creates a new [`Table`] with the provided [`Column`] definitions and
/// row data.
///
/// `header` and `body` are the widget [`Id`]s of the two scrollables;
/// `on_sync` is emitted when the body scrolls so the caller can keep the
/// header offset in step (answer it with
/// [`scrollable::scroll_to`](iced_widget::scrollable) on the header id).
#[must_use]
pub fn table<'a, Column, Row, Message, Theme>(
    header: Id,
    body: Id,
    columns: &'a [Column],
    rows: &'a [Row],
    on_sync: fn(scrollable::AbsoluteOffset) -> Message,
) -> Table<'a, Column, Row, Message, Theme>
where
    Theme: Catalog + container::Catalog,
{
    Table {
        header,
        body,
        footer: None,
        columns,
        rows,
        on_sync,
        on_column_drag: None,
        on_column_release: None,
        min_width: 0.0,
        min_column_width: 4.0,
        divider_width: 2.0,
        cell_padding: 4.into(),
        style: Default::default(),
        scrollbar: scrollable::Scrollbar::default(),
    }
}

/// An element displaying rows of data into resizable columns, with the
/// header (and optional footer) scroll-synced to the body.
#[allow(missing_debug_implementations)]
pub struct Table<'a, Column, Row, Message, Theme>
where
    Theme: Catalog + container::Catalog,
{
    header: Id,
    body: Id,
    footer: Option<Id>,
    columns: &'a [Column],
    rows: &'a [Row],
    on_sync: fn(scrollable::AbsoluteOffset) -> Message,
    on_column_drag: Option<fn(usize, f32) -> Message>,
    on_column_release: Option<Message>,
    min_width: f32,
    min_column_width: f32,
    divider_width: f32,
    cell_padding: Padding,
    style: <Theme as Catalog>::Style,
    scrollbar: scrollable::Scrollbar,
}

impl<'a, Column, Row, Message, Theme>
    Table<'a, Column, Row, Message, Theme>
where
    Theme: Catalog + container::Catalog,
{
    /// Enables column resizing: `on_drag(index, offset)` fires during a
    /// resize (return the offset from [`Column::resize_offset`]);
    /// `on_release` fires when it finishes (apply the last offset to the
    /// stored width).
    #[must_use]
    pub fn on_column_resize(
        self,
        on_drag: fn(usize, f32) -> Message,
        on_release: Message,
    ) -> Self {
        Self {
            on_column_drag: Some(on_drag),
            on_column_release: Some(on_release),
            ..self
        }
    }

    /// Shows the footers returned by [`Column::footer`], in a third
    /// scrollable with the given id.
    #[must_use]
    pub fn footer(self, footer: Id) -> Self {
        Self {
            footer: Some(footer),
            ..self
        }
    }

    /// Sets the minimum width of the whole table (use with
    /// [`responsive`](iced_widget::responsive) to fill a parent).
    #[must_use]
    pub fn min_width(self, min_width: f32) -> Self {
        Self { min_width, ..self }
    }

    /// Sets the minimum width a column can be resized to.
    #[must_use]
    pub fn min_column_width(self, min_column_width: f32) -> Self {
        Self {
            min_column_width,
            ..self
        }
    }

    /// Sets the width of the column dividers.
    #[must_use]
    pub fn divider_width(self, divider_width: f32) -> Self {
        Self {
            divider_width,
            ..self
        }
    }

    /// Sets the padding inside each cell.
    #[must_use]
    pub fn cell_padding(self, cell_padding: impl Into<Padding>) -> Self {
        Self {
            cell_padding: cell_padding.into(),
            ..self
        }
    }

    /// Sets the style token of the [`Table`].
    #[must_use]
    pub fn style(mut self, style: impl Into<<Theme as Catalog>::Style>) -> Self {
        self.style = style.into();
        self
    }

    /// Sets the scrollbar used for the table's body scrollable.
    #[must_use]
    pub fn scrollbar(self, scrollbar: scrollable::Scrollbar) -> Self {
        Self { scrollbar, ..self }
    }
}

impl<'a, Column, Row, Message, Theme, Renderer>
    From<Table<'a, Column, Row, Message, Theme>>
    for Element<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::text::Renderer + 'a,
    Theme: Catalog + container::Catalog + scrollable::Catalog + 'a,
    Column: self::Column<'a, Message, Theme, Renderer, Row = Row>,
    Message: 'a + Clone,
{
    fn from(table: Table<'a, Column, Row, Message, Theme>) -> Self {
        let Table {
            header,
            body,
            footer,
            columns,
            rows,
            on_sync,
            on_column_drag,
            on_column_release,
            min_width,
            min_column_width,
            divider_width,
            cell_padding,
            style,
            scrollbar,
        } = table;

        let hidden = || scrollable::Scrollbar::new().width(0).margin(0).scroller_width(0);

        let header = scrollable(Wrapper::header(
            row(columns
                .iter()
                .enumerate()
                .map(|(index, column)| {
                    header_container(
                        index,
                        column,
                        on_column_drag,
                        on_column_release.clone(),
                        min_column_width,
                        divider_width,
                        cell_padding,
                        style.clone(),
                    )
                })
                .chain(dummy_container(columns, min_width, min_column_width))),
            style.clone(),
        ))
        .id(header)
        .direction(scrollable::Direction::Both {
            vertical: hidden(),
            horizontal: hidden(),
        });

        let body = scrollable(column(rows.iter().enumerate().map(|(row_index, _row)| {
            Wrapper::row(
                row(columns
                    .iter()
                    .enumerate()
                    .map(|(col_index, column)| {
                        body_container(
                            col_index,
                            row_index,
                            column,
                            _row,
                            min_column_width,
                            divider_width,
                            cell_padding,
                        )
                    })
                    .chain(dummy_container(columns, min_width, min_column_width))),
                style.clone(),
                row_index,
            )
            .into()
        })))
        .id(body)
        .on_scroll(move |viewport| {
            let offset = viewport.absolute_offset();

            (on_sync)(scrollable::AbsoluteOffset { y: 0.0, ..offset })
        })
        .direction(scrollable::Direction::Both {
            horizontal: scrollbar.clone(),
            vertical: scrollbar,
        })
        .height(Length::Fill);

        let footer = footer.map(|footer| {
            scrollable(Wrapper::footer(
                row(columns
                    .iter()
                    .enumerate()
                    .map(|(index, column)| {
                        footer_container(
                            index,
                            column,
                            rows,
                            on_column_drag,
                            on_column_release.clone(),
                            min_column_width,
                            divider_width,
                            cell_padding,
                            style.clone(),
                        )
                    })
                    .chain(dummy_container(columns, min_width, min_column_width))),
                style,
            ))
            .id(footer)
            .direction(scrollable::Direction::Both {
                vertical: hidden(),
                horizontal: hidden(),
            })
        });

        let mut column = column![header, body];

        if let Some(footer) = footer {
            column = column.push(footer);
        }

        column.height(Length::Fill).into()
    }
}

fn header_container<'a, Column, Row, Message, Theme, Renderer>(
    index: usize,
    column: &'a Column,
    on_drag: Option<fn(usize, f32) -> Message>,
    on_release: Option<Message>,
    min_column_width: f32,
    divider_width: f32,
    cell_padding: Padding,
    style: <Theme as Catalog>::Style,
) -> Element<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer + 'a,
    Theme: Catalog + container::Catalog + 'a,
    Column: self::Column<'a, Message, Theme, Renderer, Row = Row>,
    Message: 'a + Clone,
{
    let content = container(column.header(index))
        .width(Length::Fill)
        .padding(cell_padding)
        .into();

    with_divider(
        index,
        column,
        content,
        on_drag,
        on_release,
        min_column_width,
        divider_width,
        style,
    )
}

fn body_container<'a, Column, Row, Message, Theme, Renderer>(
    col_index: usize,
    row_index: usize,
    column: &'a Column,
    row: &'a Row,
    min_column_width: f32,
    divider_width: f32,
    cell_padding: Padding,
) -> Element<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer + 'a,
    Theme: Catalog + container::Catalog + 'a,
    Column: self::Column<'a, Message, Theme, Renderer, Row = Row>,
    Message: 'a + Clone,
{
    let width = column.width() + column.resize_offset().unwrap_or_default();

    let content = container(column.cell(col_index, row_index, row))
        .width(Length::Fill)
        .padding(cell_padding);

    let spacing = Space::new().width(divider_width).height(Length::Shrink);

    row![content, spacing]
        .width(width.max(min_column_width))
        .into()
}

fn footer_container<'a, Column, Row, Message, Theme, Renderer>(
    index: usize,
    column: &'a Column,
    rows: &'a [Row],
    on_drag: Option<fn(usize, f32) -> Message>,
    on_release: Option<Message>,
    min_column_width: f32,
    divider_width: f32,
    cell_padding: Padding,
    style: <Theme as Catalog>::Style,
) -> Element<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer + 'a,
    Theme: Catalog + container::Catalog + 'a,
    Column: self::Column<'a, Message, Theme, Renderer, Row = Row>,
    Message: 'a + Clone,
{
    let content = if let Some(footer) = column.footer(index, rows) {
        container(footer)
            .width(Length::Fill)
            .padding(cell_padding)
            .into()
    } else {
        Element::from(space::horizontal())
    };

    with_divider(
        index,
        column,
        content,
        on_drag,
        on_release,
        min_column_width,
        divider_width,
        style,
    )
}

fn with_divider<'a, Column, Row, Message, Theme, Renderer>(
    index: usize,
    column: &'a Column,
    content: Element<'a, Message, Theme, Renderer>,
    on_drag: Option<fn(usize, f32) -> Message>,
    on_release: Option<Message>,
    min_column_width: f32,
    divider_width: f32,
    style: <Theme as Catalog>::Style,
) -> Element<'a, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer + 'a,
    Theme: Catalog + container::Catalog + 'a,
    Column: self::Column<'a, Message, Theme, Renderer, Row = Row>,
    Message: 'a + Clone,
{
    let width =
        (column.width() + column.resize_offset().unwrap_or_default()).max(min_column_width);

    if let Some((on_drag, on_release)) = on_drag.zip(on_release) {
        let old_width = column.width();

        container(Divider::new(
            content,
            divider_width,
            move |offset| {
                let new_width = (old_width + offset).max(min_column_width);

                (on_drag)(index, new_width - old_width)
            },
            on_release,
            style,
        ))
        .width(width)
        .into()
    } else {
        row![content, Space::new().width(divider_width).height(Length::Shrink)]
            .width(width)
            .into()
    }
}

impl<'a, Message, Theme, Renderer> From<Divider<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: Clone + 'a,
    Renderer: renderer::Renderer + 'a,
    Theme: Catalog + 'a,
{
    fn from(divider: Divider<'a, Message, Theme, Renderer>) -> Self {
        Element::new(divider)
    }
}

// Enforces `min_width` with a trailing spacer.
fn dummy_container<'a, Column, Row, Message, Theme, Renderer>(
    columns: &'a [Column],
    min_width: f32,
    min_column_width: f32,
) -> Option<Element<'a, Message, Theme, Renderer>>
where
    Renderer: iced_core::Renderer + 'a,
    Theme: Catalog + container::Catalog + 'a,
    Column: self::Column<'a, Message, Theme, Renderer, Row = Row>,
    Message: 'a + Clone,
{
    let total_width: f32 = columns
        .iter()
        .map(|column| {
            (column.width() + column.resize_offset().unwrap_or_default()).max(min_column_width)
        })
        .sum();

    let remaining = min_width - total_width;

    (remaining > 0.0).then(|| container(Space::new().width(remaining)).into())
}

/// Which band of the table a [`Wrapper`] paints behind its content.
#[derive(Clone, Copy)]
enum Target {
    Header,
    Footer,
    Row { index: usize },
}

/// Draws the header, footer or row band behind content, resolving the
/// style against the theme at draw time.
struct Wrapper<'a, Message, Theme, Renderer>
where
    Theme: Catalog,
{
    content: Element<'a, Message, Theme, Renderer>,
    target: Target,
    style: <Theme as Catalog>::Style,
}

impl<'a, Message, Theme, Renderer> Wrapper<'a, Message, Theme, Renderer>
where
    Theme: Catalog,
{
    fn header(
        content: impl Into<Element<'a, Message, Theme, Renderer>>,
        style: <Theme as Catalog>::Style,
    ) -> Self {
        Self {
            content: content.into(),
            target: Target::Header,
            style,
        }
    }

    fn footer(
        content: impl Into<Element<'a, Message, Theme, Renderer>>,
        style: <Theme as Catalog>::Style,
    ) -> Self {
        Self {
            content: content.into(),
            target: Target::Footer,
            style,
        }
    }

    fn row(
        content: impl Into<Element<'a, Message, Theme, Renderer>>,
        style: <Theme as Catalog>::Style,
        index: usize,
    ) -> Self {
        Self {
            content: content.into(),
            target: Target::Row { index },
            style,
        }
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Wrapper<'_, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
    Theme: Catalog,
{
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        state: &mut Tree,
        renderer: &Renderer,
        limits: &Limits,
    ) -> Node {
        self.content.as_widget_mut().layout(state, renderer, limits)
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
        let band = match self.target {
            Target::Header => theme.header(&self.style),
            Target::Footer => theme.footer(&self.style),
            Target::Row { index } => theme.row(&self.style, index),
        };

        if let Some(background) = band.background
            && layout.bounds().intersects(viewport)
        {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: layout.bounds(),
                    ..renderer::Quad::default()
                },
                background,
            );
        }

        let style = band
            .text
            .map(|text_color| renderer::Style { text_color })
            .unwrap_or(*style);

        self.content
            .as_widget()
            .draw(state, renderer, theme, &style, layout, cursor, viewport);
    }

    fn tag(&self) -> Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> State {
        self.content.as_widget().state()
    }

    fn diff(&mut self, tree: &mut Tree) {
        self.content.as_widget_mut().diff(tree);
    }

    fn operate(
        &mut self,
        state: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(state, layout, renderer, operation);
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
        self.content.as_widget_mut().update(
            state, event, layout, cursor, renderer, shell, viewport,
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
        self.content
            .as_widget()
            .mouse_interaction(state, layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        state: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(state, layout, renderer, viewport, translation)
    }
}

impl<'a, Message, Theme, Renderer> From<Wrapper<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer + 'a,
    Theme: Catalog + 'a,
    Message: 'a,
{
    fn from(wrapper: Wrapper<'a, Message, Theme, Renderer>) -> Self {
        Element::new(wrapper)
    }
}

/// The resize grip on a column's trailing edge: drag to resize, release
/// to commit, highlighted while hovered or dragged.
struct Divider<'a, Message, Theme, Renderer>
where
    Theme: Catalog,
{
    content: Element<'a, Message, Theme, Renderer>,
    width: f32,
    on_drag: Box<dyn Fn(f32) -> Message + 'a>,
    on_release: Message,
    style: <Theme as Catalog>::Style,
}

#[derive(Clone, Copy, Debug, Default)]
struct DividerState {
    drag_origin: Option<Point>,
    is_divider_hovered: bool,
}

impl<'a, Message, Theme, Renderer> Divider<'a, Message, Theme, Renderer>
where
    Theme: Catalog,
{
    fn new(
        content: impl Into<Element<'a, Message, Theme, Renderer>>,
        width: f32,
        on_drag: impl Fn(f32) -> Message + 'a,
        on_release: Message,
        style: <Theme as Catalog>::Style,
    ) -> Self {
        Self {
            content: content.into(),
            width,
            on_drag: Box::new(on_drag),
            on_release,
            style,
        }
    }

    fn divider_bounds(&self, bounds: Rectangle) -> Rectangle {
        Rectangle {
            x: bounds.x + bounds.width - self.width,
            width: self.width,
            ..bounds
        }
    }

    fn divider_hover_bounds(&self, bounds: Rectangle) -> Rectangle {
        let mut bounds = self.divider_bounds(bounds);
        bounds.x -= 5.0;
        bounds.width += 10.0;

        bounds
    }

    fn is_content_hovered(&self, mut bounds: Rectangle, cursor: Cursor) -> bool {
        // Ignore the left edge so neighbouring dividers do not fight.
        bounds.x = (bounds.x + 5.0).min(bounds.x + bounds.width - 5.0);

        cursor.is_over(bounds)
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Divider<'_, Message, Theme, Renderer>
where
    Message: Clone,
    Renderer: renderer::Renderer,
    Theme: Catalog,
{
    fn tag(&self) -> Tag {
        Tag::of::<DividerState>()
    }

    fn state(&self) -> State {
        State::new(DividerState::default())
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_mut(&mut self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &Limits,
    ) -> Node {
        let padding = padding::all(0).right(self.width);

        layout::padded(limits, Length::Fill, Length::Shrink, padding, |limits| {
            self.content
                .as_widget_mut()
                .layout(&mut tree.children[0], renderer, limits)
        })
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<DividerState>();

        let divider_hover_bounds = self.divider_hover_bounds(layout.bounds());

        state.is_divider_hovered = cursor.is_over(divider_hover_bounds);

        if let Event::Mouse(event) = event {
            match event {
                mouse::Event::ButtonPressed(mouse::Button::Left) => {
                    if let Some(origin) = cursor.position_over(divider_hover_bounds) {
                        state.drag_origin = Some(origin);
                        shell.capture_event();
                    }
                }
                mouse::Event::ButtonReleased(mouse::Button::Left) => {
                    if state.drag_origin.take().is_some() {
                        shell.publish(self.on_release.clone());
                        shell.capture_event();
                    }
                }
                mouse::Event::CursorMoved { .. } => {
                    if let Some(position) = cursor.position()
                        && let Some(origin) = state.drag_origin
                    {
                        shell.publish((self.on_drag)((position - origin).x));
                        shell.capture_event();
                    }
                }
                _ => {}
            }
        }

        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout.children().next().unwrap(),
            cursor,
            renderer,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<DividerState>();

        if state.drag_origin.is_some() || state.is_divider_hovered {
            mouse::Interaction::ResizingHorizontally
        } else {
            self.content.as_widget().mouse_interaction(
                &tree.children[0],
                layout.children().next().unwrap(),
                cursor,
                viewport,
                renderer,
            )
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<DividerState>();

        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout.children().next().unwrap(),
            cursor,
            viewport,
        );

        if self.is_content_hovered(layout.bounds(), cursor)
            || state.is_divider_hovered
            || state.drag_origin.is_some()
        {
            let hovered = state.is_divider_hovered || state.drag_origin.is_some();

            if let Some(background) = theme.divider(&self.style, hovered) {
                let bounds = self.divider_bounds(layout.bounds());

                renderer.fill_quad(
                    renderer::Quad {
                        bounds: Rectangle {
                            x: bounds.x.floor(),
                            y: bounds.y.floor(),
                            width: self.width,
                            ..bounds
                        },
                        ..renderer::Quad::default()
                    },
                    background,
                );
            }
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
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout.children().next().unwrap(),
            renderer,
            viewport,
            translation,
        )
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content.as_widget_mut().operate(
            &mut tree.children[0],
            layout.children().next().unwrap(),
            renderer,
            operation,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_renderer::LayoutRenderer;

    /// A three-column table over (name, size, kind) rows, resizable.
    struct TestColumn {
        label: &'static str,
        width: f32,
        resize: Option<f32>,
    }

    impl<'a> self::Column<'a, (), iced_core::Theme, LayoutRenderer> for TestColumn {
        type Row = (&'a str, u32, &'a str);

        fn header(&'a self, col_index: usize) -> Element<'a, (), iced_core::Theme, LayoutRenderer> {
            sort_label(self.label, col_index == 0, true).into()
        }

        fn cell(
            &'a self,
            _col_index: usize,
            _row_index: usize,
            row: &'a Self::Row,
        ) -> Element<'a, (), iced_core::Theme, LayoutRenderer> {
            iced_widget::text(row.0).into()
        }

        fn width(&self) -> f32 {
            self.width
        }

        fn resize_offset(&self) -> Option<f32> {
            self.resize
        }
    }

    fn columns() -> [TestColumn; 3] {
        [
            TestColumn { label: "Name", width: 120.0, resize: None },
            TestColumn { label: "Size", width: 80.0, resize: Some(10.0) },
            TestColumn { label: "Kind", width: 60.0, resize: None },
        ]
    }

    #[test]
    fn rows_and_columns_build_into_a_table() {
        let rows: [(&str, u32, &str); 2] = [("one", 1, "doc"), ("two", 2, "img")];
        let cols = columns();
        let built = table::<TestColumn, _, (), iced_core::Theme>(
            Id::new("header"),
            Id::new("body"),
            &cols,
            &rows,
            |_| (),
        );
        assert_eq!(built.columns.len(), 3);
        assert_eq!(built.rows.len(), 2);
        assert_eq!(built.min_column_width, 4.0);

        // The whole table builds into an element with a resize grip
        // wired for every column.
        let mut element: Element<'_, (), iced_core::Theme, LayoutRenderer> = built
            .on_column_resize(|_, _| (), ())
            .into();
        let mut tree = Tree::new(&element);
        element.as_widget_mut().diff(&mut tree);
        let renderer = LayoutRenderer::new();
        let limits = Limits::new(Size::ZERO, Size::new(400.0, 300.0));
        let node = element.as_widget_mut().layout(&mut tree, &renderer, &limits);
        assert!(node.bounds().width > 0.0);
    }
}
