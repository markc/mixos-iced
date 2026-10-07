// SPDX-License-Identifier: MIT OR Apache-2.0
//! A list that builds, lays out and draws only the rows in view, so a
//! hundred thousand rows cost what a screenful costs.
//!
//! [`VirtualList::new(rows, build)`](VirtualList::new) takes the row count
//! and a row builder; the caller's data is never cloned or walked. Rows are
//! a fixed height or use a reusable [`RowHeights`] index. The widget owns its scroll offset and scrollbar, keeps
//! only the visible rows' widget state (keyed, so a row keeps its state
//! while it scrolls), and handles keyboard navigation (arrows, Page Up and
//! Down, Home and End, Space, Enter, Ctrl+A, Escape and a type-ahead hook),
//! single or multiple [`Selection`] with Ctrl and Shift, activation by Enter
//! or double-click, and scrolling to a row by [`reveal`](VirtualList::reveal)
//! or by [`scroll_to_row`] from a `Task`.
//!
//! Selection is the caller's state: the widget is given a [`Selection`] and
//! publishes the next one through [`on_select`](VirtualList::on_select).
//! [`Columns`] is a small helper for a header row and rows with matching
//! column widths.
//!
//! Colours come from [`Catalog`], implemented for the crate's
//! [`Theme`](crate::Theme) (from its tokens) and for iced's theme.

use std::any::Any;
use std::collections::HashMap;
use std::ops::Range;
use std::time::{Duration, Instant};

use iced_core::keyboard::{self, key::Named};
use iced_core::widget::operation::{Focusable, Operation};
use iced_core::widget::{Id, Tree, tree};
use iced_core::{
    Background, Border, Color, Element, Event, Layout, Length, Point, Rectangle, Shell, Size,
    Vector, Widget, layout, mouse, renderer, window,
};
use iced_runtime::Task;

use crate::Tokens;

/// Rows built beyond each edge of the viewport, so a one-row scroll is a
/// move and not a rebuild.
const OVERSCAN: usize = 2;
/// Rows a wheel "line" scrolls.
const WHEEL_ROWS: f32 = 3.0;
/// A second press on the same row within this is a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Typed characters this far apart start a new type-ahead prefix.
const TYPE_AHEAD: Duration = Duration::from_secs(1);
/// The shortest the scroller gets, in pixels.
const MIN_SCROLLER: f32 = 16.0;

/// Caller-owned cumulative row metrics, reused between frames. Building this
/// index walks the heights once; hit testing and visible ranges use binary
/// search. Widgets are never built to measure offscreen rows.
#[derive(Debug, Clone, PartialEq)]
pub struct RowHeights {
    offsets: Vec<f32>,
}

/// A bad metric, or a total that cannot preserve positive row extents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeightError {
    pub row: usize,
}

impl std::fmt::Display for HeightError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid or unrepresentable height at row {}", self.row)
    }
}
impl std::error::Error for HeightError {}

impl RowHeights {
    pub fn new(heights: impl IntoIterator<Item = f32>) -> Result<Self, HeightError> {
        let mut offsets = vec![0.0];
        let mut total = 0.0;
        for (row, height) in heights.into_iter().enumerate() {
            let next = total + height;
            if !height.is_finite() || height < 1.0 || !next.is_finite() || next <= total {
                return Err(HeightError { row });
            }
            total = next;
            offsets.push(total);
        }
        Ok(Self { offsets })
    }
    pub fn len(&self) -> usize {
        self.offsets.len() - 1
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn total(&self) -> f32 {
        *self.offsets.last().expect("initial zero")
    }
    pub fn top(&self, row: usize) -> Option<f32> {
        (row < self.len()).then(|| self.offsets[row])
    }
    pub fn height(&self, row: usize) -> Option<f32> {
        (row < self.len()).then(|| self.offsets[row + 1] - self.offsets[row])
    }
    /// Exact boundaries belong to the following row; total and non-finite
    /// coordinates are outside the list.
    pub fn row_at(&self, y: f32) -> Option<usize> {
        if !y.is_finite() || y < 0.0 || y >= self.total() {
            return None;
        }
        Some(self.offsets.partition_point(|offset| *offset <= y) - 1)
    }
    fn end_at(&self, y: f32) -> usize {
        self.offsets
            .partition_point(|offset| *offset < y)
            .min(self.len())
    }
}

/// Which rows are selected, plus the keyboard cursor and the Shift anchor.
/// Sorted, disjoint ranges, so "select all" on a million rows is one entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    ranges: Vec<Range<usize>>,
    anchor: Option<usize>,
    cursor: Option<usize>,
}

impl Selection {
    pub fn new() -> Self {
        Self::default()
    }

    /// Only `row` selected; it is the cursor and the anchor.
    pub fn single(row: usize) -> Self {
        Self {
            ranges: std::iter::once(row..row + 1).collect(),
            anchor: Some(row),
            cursor: Some(row),
        }
    }

    /// Every row of a list of `len` rows.
    pub fn all(len: usize) -> Self {
        Self {
            ranges: std::iter::once(0..len).filter(|r| !r.is_empty()).collect(),
            anchor: (len > 0).then_some(0),
            cursor: len.checked_sub(1),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// The number of selected rows.
    pub fn len(&self) -> usize {
        self.ranges.iter().map(ExactSizeIterator::len).sum()
    }

    pub fn contains(&self, row: usize) -> bool {
        self.ranges.iter().any(|range| range.contains(&row))
    }

    /// The selected rows in order.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.ranges.iter().flat_map(Clone::clone)
    }

    /// The sorted, disjoint ranges.
    pub fn ranges(&self) -> &[Range<usize>] {
        &self.ranges
    }

    pub fn first(&self) -> Option<usize> {
        self.ranges.first().map(|range| range.start)
    }

    /// The keyboard cursor row (the focus row), which need not be selected.
    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    /// Where a Shift-extended range starts.
    pub fn anchor(&self) -> Option<usize> {
        self.anchor
    }

    /// Moves the cursor without changing what is selected.
    pub fn with_cursor(mut self, row: usize) -> Self {
        self.cursor = Some(row);
        self
    }

    /// Selects `row` on its own (a plain click or arrow).
    pub fn select(&mut self, row: usize) {
        *self = Self::single(row);
    }

    /// Flips `row` (Ctrl+click, Space); it becomes the cursor and anchor.
    pub fn toggle(&mut self, row: usize) {
        if self.contains(row) {
            self.remove(row..row + 1);
        } else {
            self.insert(row..row + 1);
        }
        self.anchor = Some(row);
        self.cursor = Some(row);
    }

    /// Selects from the anchor to `row` (Shift+click, Shift+arrow). With
    /// `keep`, rows outside that span stay as they were (Ctrl+Shift).
    pub fn extend_to(&mut self, row: usize, keep: bool) {
        let anchor = self.anchor.or(self.cursor).unwrap_or(row);
        let range = anchor.min(row)..anchor.max(row) + 1;
        if !keep {
            self.ranges.clear();
        }
        self.insert(range);
        self.anchor = Some(anchor);
        self.cursor = Some(row);
    }

    /// Deselects everything; the cursor stays.
    pub fn clear(&mut self) {
        self.ranges.clear();
        self.anchor = None;
    }

    /// Drops rows at or past `len` (after the list shrank).
    pub fn truncate(&mut self, len: usize) {
        self.ranges.retain_mut(|range| {
            range.end = range.end.min(len);
            range.start < range.end
        });
        if self.cursor.is_some_and(|cursor| cursor >= len) {
            self.cursor = len.checked_sub(1);
        }
        if self.anchor.is_some_and(|anchor| anchor >= len) {
            self.anchor = None;
        }
    }

    fn insert(&mut self, range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        let mut merged = range;
        let mut out = Vec::with_capacity(self.ranges.len() + 1);
        for existing in self.ranges.drain(..) {
            if existing.end < merged.start || existing.start > merged.end {
                out.push(existing);
            } else {
                merged = existing.start.min(merged.start)..existing.end.max(merged.end);
            }
        }
        out.push(merged);
        out.sort_by_key(|range| range.start);
        self.ranges = out;
    }

    fn remove(&mut self, range: Range<usize>) {
        let mut out = Vec::with_capacity(self.ranges.len() + 1);
        for existing in self.ranges.drain(..) {
            if existing.end <= range.start || existing.start >= range.end {
                out.push(existing);
                continue;
            }
            if existing.start < range.start {
                out.push(existing.start..range.start);
            }
            if existing.end > range.end {
                out.push(range.end..existing.end);
            }
        }
        self.ranges = out;
    }
}

/// How many rows may be selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Rows are not selectable; the cursor still moves.
    None,
    /// One row at a time.
    Single,
    /// Ctrl toggles, Shift extends, Ctrl+A selects all.
    #[default]
    Multiple,
}

/// A key the list did not handle itself, offered to the caller with the
/// cursor row (the tree uses it for Left and Right).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPress {
    pub key: keyboard::Key,
    pub modifiers: keyboard::Modifiers,
    pub cursor: Option<usize>,
}

/// `on_context`: the row under the pointer, if any, and the pointer's
/// window position.
pub type ContextFn<'a, Message> = dyn Fn(Option<usize>, Point) -> Message + 'a;
/// `type_ahead`: the first row at or after `from` matching the prefix.
pub type TypeAheadFn<'a> = dyn Fn(&str, usize) -> Option<usize> + 'a;

/// The widget. `'a` is the lifetime of the rows it builds.
pub struct VirtualList<'a, Message, Theme, Renderer>
where
    Theme: Catalog,
{
    id: Option<Id>,
    rows: usize,
    row_height: f32,
    heights: Option<&'a RowHeights>,
    key: Box<dyn Fn(usize) -> u64 + 'a>,
    build: Box<dyn Fn(usize) -> Element<'a, Message, Theme, Renderer> + 'a>,
    header: Option<Element<'a, Message, Theme, Renderer>>,
    header_height: f32,
    selection: Selection,
    mode: Mode,
    on_select: Option<Box<dyn Fn(Selection) -> Message + 'a>>,
    on_activate: Option<Box<dyn Fn(usize) -> Message + 'a>>,
    on_context: Option<Box<ContextFn<'a, Message>>>,
    on_key: Option<Box<dyn Fn(KeyPress) -> Option<Message> + 'a>>,
    type_ahead: Option<Box<TypeAheadFn<'a>>>,
    reveal: Option<usize>,
    width: Length,
    height: Length,
    scrollbar_width: f32,
    overscan: usize,
    class: Theme::Class<'a>,
    /// The rows in view, built by `layout`.
    visible: Visible<'a, Message, Theme, Renderer>,
}

struct Visible<'a, Message, Theme, Renderer> {
    first: usize,
    rows: Vec<Element<'a, Message, Theme, Renderer>>,
}

impl<'a, Message, Theme, Renderer> VirtualList<'a, Message, Theme, Renderer>
where
    Theme: Catalog,
    Renderer: iced_core::Renderer,
{
    /// A list of `rows` rows; `build(index)` makes the widget for one row
    /// and is called only for rows in (or just beyond) the viewport.
    pub fn new<E>(rows: usize, build: impl Fn(usize) -> E + 'a) -> Self
    where
        E: Into<Element<'a, Message, Theme, Renderer>>,
    {
        Self {
            id: None,
            rows,
            row_height: 28.0,
            heights: None,
            key: Box::new(|index| index as u64),
            build: Box::new(move |index| build(index).into()),
            header: None,
            header_height: 28.0,
            selection: Selection::default(),
            mode: Mode::default(),
            on_select: None,
            on_activate: None,
            on_context: None,
            on_key: None,
            type_ahead: None,
            reveal: None,
            width: Length::Fill,
            height: Length::Fill,
            scrollbar_width: 8.0,
            overscan: OVERSCAN,
            class: Theme::default(),
            visible: Visible {
                first: 0,
                rows: Vec::new(),
            },
        }
    }

    /// Replaces the row count and builder (the tree view configures a
    /// list first and supplies its rows last).
    pub fn with_rows<E>(mut self, rows: usize, build: impl Fn(usize) -> E + 'a) -> Self
    where
        E: Into<Element<'a, Message, Theme, Renderer>>,
    {
        self.rows = rows;
        if self.heights.is_some_and(|heights| heights.len() != rows) {
            self.heights = None;
        }
        self.build = Box::new(move |index| build(index).into());
        self
    }

    /// An id for [`scroll_to_row`] and iced's focus operations.
    pub fn id(mut self, id: impl Into<Id>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// The height of every row (default 28).
    pub fn row_height(mut self, height: f32) -> Self {
        self.row_height = height.max(1.0);
        self.heights = None;
        self
    }

    /// Supplies known variable heights and makes their count authoritative.
    /// Rebuild the index when row ordering/expansion changes. A subsequent
    /// `with_rows` with a different count returns to fixed-height mode.
    pub fn row_heights(mut self, heights: &'a RowHeights) -> Self {
        self.rows = heights.len();
        self.heights = Some(heights);
        self
    }

    /// A stable key per row, so a row keeps its widget state (hover, a text
    /// input's cursor) when rows are inserted above it. Default: the index.
    pub fn key(mut self, key: impl Fn(usize) -> u64 + 'a) -> Self {
        self.key = Box::new(key);
        self
    }

    /// A header drawn above the rows, which does not scroll.
    pub fn header(mut self, header: impl Into<Element<'a, Message, Theme, Renderer>>) -> Self {
        self.header = Some(header.into());
        self
    }

    /// The header's height (default 28).
    pub fn header_height(mut self, height: f32) -> Self {
        self.header_height = height.max(1.0);
        self
    }

    /// The current selection (the caller's state).
    pub fn selection(mut self, selection: &Selection) -> Self {
        self.selection = selection.clone();
        self
    }

    pub fn mode(mut self, mode: Mode) -> Self {
        self.mode = mode;
        self
    }

    /// Published with the next selection after a click or key.
    pub fn on_select(mut self, on_select: impl Fn(Selection) -> Message + 'a) -> Self {
        self.on_select = Some(Box::new(on_select));
        self
    }

    /// Published with the row on Enter or a double-click.
    pub fn on_activate(mut self, on_activate: impl Fn(usize) -> Message + 'a) -> Self {
        self.on_activate = Some(Box::new(on_activate));
        self
    }

    /// Published on a right press with the row under the pointer, if any,
    /// and the pointer's window position.
    pub fn on_context(mut self, on_context: impl Fn(Option<usize>, Point) -> Message + 'a) -> Self {
        self.on_context = Some(Box::new(on_context));
        self
    }

    /// Keys the list leaves alone (Left, Right, Delete, F2, ...) while it has
    /// focus; a `Some` message captures the key.
    pub fn on_key(mut self, on_key: impl Fn(KeyPress) -> Option<Message> + 'a) -> Self {
        self.on_key = Some(Box::new(on_key));
        self
    }

    /// Type-ahead: `find(prefix, from)` returns the first row at or after
    /// `from` whose text starts with `prefix` (wrapping is the caller's
    /// choice). Typed characters within a second form one prefix.
    pub fn type_ahead(mut self, find: impl Fn(&str, usize) -> Option<usize> + 'a) -> Self {
        self.type_ahead = Some(Box::new(find));
        self
    }

    /// Scrolls `row` into view when it changes between builds.
    pub fn reveal(mut self, row: Option<usize>) -> Self {
        self.reveal = row;
        self
    }

    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// The scrollbar's width (default 8).
    pub fn scrollbar_width(mut self, width: f32) -> Self {
        self.scrollbar_width = width.max(0.0);
        self
    }

    /// Rows built beyond each edge of the viewport (default 2).
    pub fn overscan(mut self, rows: usize) -> Self {
        self.overscan = rows;
        self
    }

    pub fn style(mut self, style: impl Fn(&Theme, Status) -> Style + 'a) -> Self
    where
        Theme::Class<'a>: From<StyleFn<'a, Theme>>,
    {
        self.class = (Box::new(style) as StyleFn<'a, Theme>).into();
        self
    }

    pub fn class(mut self, class: impl Into<Theme::Class<'a>>) -> Self {
        self.class = class.into();
        self
    }

    /// The index of the first row built for the last layout, and how many.
    pub fn built(&self) -> (usize, usize) {
        (self.visible.first, self.visible.rows.len())
    }

    fn content_height(&self) -> f32 {
        self.heights
            .map_or(self.rows as f32 * self.row_height, RowHeights::total)
    }

    fn row_top(&self, row: usize) -> f32 {
        self.heights
            .map_or(row as f32 * self.row_height, |heights| {
                heights.top(row).unwrap_or(heights.total())
            })
    }

    fn row_size(&self, row: usize) -> f32 {
        self.heights
            .and_then(|heights| heights.height(row))
            .unwrap_or(self.row_height)
    }

    fn index_at(&self, y: f32) -> Option<usize> {
        if !y.is_finite() || y < 0.0 || y >= self.content_height() {
            return None;
        }
        self.heights.map_or_else(
            || Some((y / self.row_height) as usize),
            |heights| heights.row_at(y),
        )
    }

    fn header_height_if_any(&self) -> f32 {
        if self.header.is_some() {
            self.header_height
        } else {
            0.0
        }
    }

    /// The rows area inside `bounds` (below the header).
    fn body(&self, bounds: Rectangle) -> Rectangle {
        let header = self.header_height_if_any();
        Rectangle {
            y: bounds.y + header,
            height: (bounds.height - header).max(0.0),
            ..bounds
        }
    }

    fn overflows(&self, body_height: f32) -> bool {
        self.content_height() > body_height + 0.5
    }

    /// The scrollbar rail and scroller, if the content overflows.
    fn scrollbar(&self, state: &State, body: Rectangle) -> Option<(Rectangle, Rectangle)> {
        if !self.overflows(body.height) || self.scrollbar_width <= 0.0 {
            return None;
        }
        let rail = Rectangle {
            x: body.x + body.width - self.scrollbar_width,
            width: self.scrollbar_width,
            ..body
        };
        let content = self.content_height();
        let length = (body.height * body.height / content)
            .max(MIN_SCROLLER)
            .min(body.height);
        let travel = body.height - length;
        let max_offset = (content - body.height).max(0.0);
        let fraction = if max_offset > 0.0 {
            state.offset / max_offset
        } else {
            0.0
        };
        let scroller = Rectangle {
            y: rail.y + travel * fraction,
            height: length,
            ..rail
        };
        Some((rail, scroller))
    }

    fn max_offset(&self, body_height: f32) -> f32 {
        (self.content_height() - body_height).max(0.0)
    }

    /// The row under list-space `y` (relative to the body top).
    fn row_at(&self, state: &State, y: f32) -> Option<usize> {
        if y < 0.0 || self.rows == 0 {
            return None;
        }
        self.index_at(y + state.offset)
    }

    /// Bounds of `row` in window space, given the body.
    fn row_bounds(&self, state: &State, body: Rectangle, row: usize) -> Rectangle {
        Rectangle {
            x: body.x,
            y: body.y + self.row_top(row) - state.offset,
            width: body.width - self.rail_width(body.height),
            height: self.row_size(row),
        }
    }

    fn rail_width(&self, body_height: f32) -> f32 {
        if self.overflows(body_height) {
            self.scrollbar_width
        } else {
            0.0
        }
    }

    /// Scrolls so `row` is in view; true if the offset moved.
    fn reveal_row(&self, state: &mut State, body_height: f32, row: usize) -> bool {
        let before = state.offset;
        let top = self.row_top(row);
        let bottom = top + self.row_size(row);
        if top < state.offset || self.row_size(row) > body_height {
            state.offset = top;
        } else if bottom > state.offset + body_height {
            state.offset = bottom - body_height;
        }
        state.offset = state.offset.clamp(0.0, self.max_offset(body_height));
        state.offset != before
    }

    /// Applies a `reveal` or `scroll_to_row` request once.
    fn apply_requests(&self, state: &mut State, body_height: f32) -> bool {
        let mut moved = false;
        if state.last_reveal != self.reveal {
            state.last_reveal = self.reveal;
            if let Some(row) = self.reveal.filter(|row| *row < self.rows) {
                moved |= self.reveal_row(state, body_height, row);
            }
        }
        if let Some(row) = state.requested.take()
            && row < self.rows
        {
            moved |= self.reveal_row(state, body_height, row);
        }
        moved
    }

    fn publish_selection(&mut self, shell: &mut Shell<'_, Message>, selection: Selection) {
        self.selection = selection;
        if let Some(on_select) = &self.on_select {
            shell.publish(on_select(self.selection.clone()));
        }
    }

    /// Moves the cursor to `row` with the click/arrow semantics of
    /// `modifiers`, publishing the new selection.
    fn move_cursor(
        &mut self,
        state: &mut State,
        shell: &mut Shell<'_, Message>,
        body_height: f32,
        row: usize,
        modifiers: keyboard::Modifiers,
    ) {
        let row = row.min(self.rows.saturating_sub(1));
        let mut selection = self.selection.clone();
        match self.mode {
            Mode::None => selection.cursor = Some(row),
            Mode::Single => selection.select(row),
            Mode::Multiple => {
                if modifiers.shift() {
                    selection.extend_to(row, modifiers.command());
                } else if modifiers.command() {
                    selection.cursor = Some(row);
                } else {
                    selection.select(row);
                }
            }
        }
        if self.reveal_row(state, body_height, row) {
            shell.invalidate_layout();
        }
        shell.request_redraw();
        self.publish_selection(shell, selection);
    }

    fn visible_rows(&self, body_height: f32) -> usize {
        ((body_height / self.row_height).floor() as usize).max(1)
    }

    fn page_target(&self, cursor: usize, body_height: f32, down: bool) -> usize {
        if self.heights.is_none() {
            let page = self.visible_rows(body_height).saturating_sub(1).max(1);
            return if down {
                cursor.saturating_add(page).min(self.rows.saturating_sub(1))
            } else {
                cursor.saturating_sub(page)
            };
        }
        let distance = (body_height - self.row_size(cursor)).max(1.0);
        if down {
            self.index_at(self.row_top(cursor) + distance)
                .unwrap_or(self.rows.saturating_sub(1))
                .max(cursor.saturating_add(1))
                .min(self.rows.saturating_sub(1))
        } else {
            self.index_at((self.row_top(cursor) - distance).max(0.0))
                .unwrap_or(0)
                .min(cursor.saturating_sub(1))
        }
    }

    /// Keyboard handling while focused. Returns whether the key was used.
    fn on_key_press(
        &mut self,
        state: &mut State,
        shell: &mut Shell<'_, Message>,
        body_height: f32,
        press: KeyPress,
        text: Option<&str>,
    ) -> bool {
        let KeyPress {
            key,
            modifiers,
            cursor,
        } = press;
        let key = &key;
        let last = self.rows.saturating_sub(1);
        let target = match key {
            keyboard::Key::Named(Named::ArrowDown) => Some(cursor.map_or(0, |c| (c + 1).min(last))),
            keyboard::Key::Named(Named::ArrowUp) => Some(cursor.map_or(0, |c| c.saturating_sub(1))),
            keyboard::Key::Named(Named::PageDown) => {
                Some(cursor.map_or(0, |c| self.page_target(c, body_height, true)))
            }
            keyboard::Key::Named(Named::PageUp) => {
                Some(cursor.map_or(0, |c| self.page_target(c, body_height, false)))
            }
            keyboard::Key::Named(Named::Home) => Some(0),
            keyboard::Key::Named(Named::End) => Some(last),
            _ => None,
        };
        if let Some(target) = target {
            if self.rows > 0 {
                self.move_cursor(state, shell, body_height, target, modifiers);
            }
            state.typed.clear();
            return true;
        }
        match key {
            keyboard::Key::Named(Named::Space) => {
                if let Some(row) = cursor
                    && self.mode == Mode::Multiple
                {
                    let mut selection = self.selection.clone();
                    selection.toggle(row);
                    self.publish_selection(shell, selection);
                    shell.request_redraw();
                }
                true
            }
            keyboard::Key::Named(Named::Enter) => {
                if let (Some(row), Some(on_activate)) = (cursor, &self.on_activate) {
                    shell.publish(on_activate(row));
                }
                true
            }
            keyboard::Key::Named(Named::Escape) => {
                if !self.selection.is_empty() && self.mode == Mode::Multiple {
                    let mut selection = self.selection.clone();
                    selection.clear();
                    self.publish_selection(shell, selection);
                    shell.request_redraw();
                    true
                } else {
                    false
                }
            }
            keyboard::Key::Character(c) if modifiers.command() && c.as_str() == "a" => {
                if self.mode == Mode::Multiple && self.rows > 0 {
                    let mut selection = Selection::all(self.rows);
                    selection.cursor = cursor.or(selection.cursor);
                    self.publish_selection(shell, selection);
                    shell.request_redraw();
                }
                true
            }
            _ => {
                // Plainly typed text is type-ahead when there is a finder;
                // every other key is offered to the hook.
                if let (Some(find), Some(text)) = (&self.type_ahead, text)
                    && !modifiers.command()
                    && !modifiers.alt()
                    && !text.chars().any(char::is_control)
                {
                    let now = Instant::now();
                    let continuing = state
                        .typed_at
                        .is_some_and(|at| now.duration_since(at) < TYPE_AHEAD)
                        && !state.typed.is_empty();
                    if !continuing {
                        state.typed.clear();
                    }
                    state.typed.push_str(text);
                    state.typed_at = Some(now);
                    let from = if continuing {
                        cursor.unwrap_or(0)
                    } else {
                        cursor.map_or(0, |c| c + 1)
                    };
                    if let Some(row) = find(&state.typed, from).filter(|row| *row < self.rows) {
                        self.move_cursor(
                            state,
                            shell,
                            body_height,
                            row,
                            keyboard::Modifiers::empty(),
                        );
                    }
                    return true;
                }
                if let Some(on_key) = &self.on_key
                    && let Some(message) = on_key(KeyPress {
                        key: key.clone(),
                        modifiers,
                        cursor,
                    })
                {
                    shell.publish(message);
                    return true;
                }
                false
            }
        }
    }

    fn resolve_style(&self, theme: &Theme, state: &State, hovered: bool) -> Style {
        let status = if state.focused {
            Status::Focused
        } else if hovered {
            Status::Hovered
        } else {
            Status::Active
        };
        theme.style(&self.class, status)
    }
}

/// Widget state: the scroll offset, focus, the keys of the built rows and
/// the click and type-ahead trackers.
#[derive(Debug, Default)]
pub struct State {
    offset: f32,
    focused: bool,
    modifiers: keyboard::Modifiers,
    /// Keys of `tree.children[..rows]`, in order.
    keys: Vec<u64>,
    press: Option<usize>,
    last_click: Option<(Instant, usize)>,
    /// Pointer offset within the scroller while dragging it.
    drag: Option<f32>,
    typed: String,
    typed_at: Option<Instant>,
    last_reveal: Option<usize>,
    /// A `scroll_to_row` waiting for the next event.
    requested: Option<usize>,
}

impl State {
    /// Pixels scrolled past the top.
    pub fn offset(&self) -> f32 {
        self.offset
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }
}

impl Focusable for State {
    fn is_focused(&self) -> bool {
        self.focused
    }

    fn focus(&mut self) {
        self.focused = true;
    }

    fn unfocus(&mut self) {
        self.focused = false;
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for VirtualList<'_, Message, Theme, Renderer>
where
    Theme: Catalog,
    Renderer: iced_core::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn diff(&mut self, _tree: &mut Tree) {
        // The visible rows are reconciled in `layout`, which knows the
        // viewport; the header too.
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let limits = limits.width(self.width).height(self.height);
        let size = limits.resolve(self.width, self.height, Size::ZERO);
        let header = self.header_height_if_any();
        let body_height = (size.height - header).max(0.0);
        let state = tree.state.downcast_mut::<State>();
        let _ = self.apply_requests(state, body_height);
        state.offset = state.offset.clamp(0.0, self.max_offset(body_height));
        let row_width = (size.width - self.rail_width(body_height)).max(0.0);

        // The window of rows to build: the viewport plus the overscan.
        let first = self
            .index_at(state.offset)
            .unwrap_or(self.rows)
            .saturating_sub(self.overscan)
            .min(self.rows);
        let end = self
            .heights
            .map_or_else(
                || ((state.offset + body_height) / self.row_height).ceil() as usize,
                |heights| heights.end_at(state.offset + body_height),
            )
            .saturating_add(self.overscan)
            .min(self.rows);
        let keys: Vec<u64> = (first..end).map(|index| (self.key)(index)).collect();
        let mut rows: Vec<_> = (first..end).map(|index| (self.build)(index)).collect();

        // Reconcile the children by key: a row that is still in view keeps
        // its state; the rest are new.
        let old_header = if tree.children.len() > state.keys.len() {
            tree.children.pop()
        } else {
            None
        };
        let mut old: HashMap<u64, Tree> =
            state.keys.drain(..).zip(tree.children.drain(..)).collect();
        for (key, row) in keys.iter().zip(rows.iter_mut()) {
            let mut child = old
                .remove(key)
                .unwrap_or_else(|| Tree::new(row.as_widget()));
            child.diff(row.as_widget_mut());
            tree.children.push(child);
        }
        drop(old);
        state.keys = keys;

        let mut nodes: Vec<layout::Node> = Vec::with_capacity(rows.len() + 1);
        for (offset, (row, child)) in rows.iter_mut().zip(tree.children.iter_mut()).enumerate() {
            let index = first + offset;
            let height = self.row_size(index);
            let row_limits =
                layout::Limits::new(Size::new(row_width, height), Size::new(row_width, height));
            let y = header + self.row_top(index) - state.offset;
            nodes.push(
                row.as_widget_mut()
                    .layout(child, renderer, &row_limits)
                    .move_to(Point::new(0.0, y)),
            );
        }
        if let Some(header_element) = &mut self.header {
            let mut child = old_header.unwrap_or_else(|| Tree::new(header_element.as_widget()));
            child.diff(header_element.as_widget_mut());
            tree.children.push(child);
            let header_limits = layout::Limits::new(
                Size::new(row_width, self.header_height),
                Size::new(row_width, self.header_height),
            );
            nodes.push(
                header_element
                    .as_widget_mut()
                    .layout(tree.children.last_mut().unwrap(), renderer, &header_limits)
                    .move_to(Point::ORIGIN),
            );
        }
        self.visible = Visible { first, rows };
        layout::Node::with_children(size, nodes)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<State>();
        operation.focusable(self.id.as_ref(), bounds, state);
        operation.custom(self.id.as_ref(), bounds, state);
        operation.container(self.id.as_ref(), bounds);
        // The rows draw clipped to the list body inside the list bounds;
        // advertise that exact clip inside a traversal scope so it cannot
        // leak into the header or the next subtree.
        operation.traverse(&mut |operation| {
            operation.clip(self.body(bounds));
            operation.traverse(&mut |operation| {
                for ((row, child), node) in self
                    .visible
                    .rows
                    .iter_mut()
                    .zip(tree.children.iter_mut())
                    .zip(layout.children())
                {
                    row.as_widget_mut()
                        .operate(child, node, renderer, operation);
                }
            });
        });
        // The header draws with the list clip, not the body clip.
        if let Some(header) = &mut self.header
            && let (Some(child), Some(node)) = (tree.children.last_mut(), layout.children().last())
        {
            operation.traverse(&mut |operation| {
                header
                    .as_widget_mut()
                    .operate(child, node, renderer, operation);
            });
        }
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
        let bounds = layout.bounds();
        let body = self.body(bounds);
        let clip = bounds
            .intersection(viewport)
            .unwrap_or(Rectangle::new(bounds.position(), Size::ZERO));
        let body_clip = body
            .intersection(&clip)
            .unwrap_or(Rectangle::new(body.position(), Size::ZERO));

        // The visible rows and the header see the event first.
        {
            let hover = if cursor.is_over(clip) {
                cursor
            } else {
                mouse::Cursor::Unavailable
            };
            for ((row, child), node) in self
                .visible
                .rows
                .iter_mut()
                .chain(self.header.iter_mut())
                .zip(tree.children.iter_mut())
                .zip(layout.children())
            {
                row.as_widget_mut()
                    .update(child, event, node, hover, renderer, shell, &body_clip);
            }
        }

        let state = tree.state.downcast_mut::<State>();
        if let Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) = event {
            state.modifiers = *modifiers;
        }
        if (state.requested.is_some() || state.last_reveal != self.reveal)
            && self.apply_requests(state, body.height)
        {
            // A `scroll_to_row` or `reveal` arrived since the last layout.
            shell.invalidate_layout();
        }
        if shell.is_event_captured() {
            return;
        }
        match event {
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if cursor.is_over(body_clip) => {
                let pixels = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => y * WHEEL_ROWS * self.row_height,
                    mouse::ScrollDelta::Pixels { y, .. } => *y,
                };
                let before = state.offset;
                state.offset = (state.offset - pixels).clamp(0.0, self.max_offset(body.height));
                if state.offset != before {
                    shell.invalidate_layout();
                    shell.request_redraw();
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(position) = cursor.position() else {
                    return;
                };
                if !cursor.is_over(clip) {
                    if state.focused {
                        state.focused = false;
                        shell.request_redraw();
                    }
                    return;
                }
                if !state.focused {
                    state.focused = true;
                    shell.request_redraw();
                }
                if let Some((rail, scroller)) = self.scrollbar(state, body)
                    && rail.contains(position)
                {
                    if scroller.contains(position) {
                        state.drag = Some(position.y - scroller.y);
                    } else {
                        // A press on the rail jumps there, centring the scroller.
                        let travel = rail.height - scroller.height;
                        let fraction = ((position.y - rail.y - scroller.height / 2.0) / travel)
                            .clamp(0.0, 1.0);
                        state.offset = fraction * self.max_offset(body.height);
                        state.drag = Some(scroller.height / 2.0);
                        shell.invalidate_layout();
                    }
                    shell.capture_event();
                    return;
                }
                if !cursor.is_over(body_clip) {
                    return;
                }
                let modifiers = state.modifiers;
                if let Some(row) = self.row_at(state, position.y - body.y) {
                    let double = state
                        .last_click
                        .is_some_and(|(when, at)| when.elapsed() < DOUBLE_CLICK && at == row)
                        && !modifiers.shift()
                        && !modifiers.command();
                    state.last_click = Some((Instant::now(), row));
                    state.press = Some(row);
                    if double {
                        state.last_click = None;
                        if let Some(on_activate) = &self.on_activate {
                            shell.publish(on_activate(row));
                        }
                    } else if modifiers.command() && self.mode == Mode::Multiple {
                        let mut selection = self.selection.clone();
                        selection.toggle(row);
                        self.publish_selection(shell, selection);
                        shell.request_redraw();
                    } else {
                        self.move_cursor(state, shell, body.height, row, modifiers);
                    }
                } else {
                    state.last_click = None;
                }
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right))
                if cursor.is_over(body_clip) =>
            {
                let Some(position) = cursor.position() else {
                    return;
                };
                if !state.focused {
                    state.focused = true;
                    shell.request_redraw();
                }
                let row = self.row_at(state, position.y - body.y);
                if let Some(on_context) = &self.on_context {
                    shell.publish(on_context(row, position));
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                if let Some(grab) = state.drag
                    && let Some((rail, scroller)) = self.scrollbar(state, body)
                {
                    let travel = rail.height - scroller.height;
                    let fraction = if travel > 0.0 {
                        ((position.y - grab - rail.y) / travel).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let before = state.offset;
                    state.offset = fraction * self.max_offset(body.height);
                    if state.offset != before {
                        shell.invalidate_layout();
                        shell.request_redraw();
                    }
                    shell.capture_event();
                } else if cursor.is_over(body_clip) {
                    // Hover highlight follows the pointer.
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.press = None;
                if state.drag.take().is_some() {
                    shell.capture_event();
                }
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modifiers,
                text,
                ..
            }) if state.focused => {
                let press = KeyPress {
                    key: key.clone(),
                    modifiers: *modifiers,
                    cursor: self.selection.cursor,
                };
                if self.on_key_press(state, shell, body.height, press, text.as_deref()) {
                    shell.capture_event();
                }
            }
            Event::Window(window::Event::Unfocused) => {
                state.modifiers = keyboard::Modifiers::empty();
                state.press = None;
                state.drag = None;
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };
        let state = tree.state.downcast_ref::<State>();
        let body = self.body(bounds);
        let hovered = cursor.is_over(clip);
        let style = self.resolve_style(theme, state, hovered);
        if style.background.is_some() || style.border.width > 0.0 {
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    border: style.border,
                    ..renderer::Quad::default()
                },
                style
                    .background
                    .unwrap_or(Background::Color(Color::TRANSPARENT)),
            );
        }
        // Row, header and rail fills stay inside the outline.
        let inset = style.border.width;
        let Some(body_clip) = body.intersection(&clip) else {
            return;
        };
        let hover_row = cursor
            .position_in(body_clip)
            .and_then(|position| self.row_at(state, position.y));
        let row_text = renderer::Style {
            text_color: style.text,
        };
        let selected_text = renderer::Style {
            text_color: style.selection_text,
        };
        renderer.with_layer(body_clip, |renderer| {
            for (offset, (row, child)) in self.visible.rows.iter().zip(&tree.children).enumerate() {
                let index = self.visible.first + offset;
                let full = self.row_bounds(state, body, index);
                if full.y + full.height <= body_clip.y || full.y >= body_clip.y + body_clip.height {
                    continue;
                }
                let row_bounds = Rectangle {
                    x: full.x + inset,
                    width: (full.width - 2.0 * inset).max(0.0),
                    ..full
                };
                let selected = self.selection.contains(index);
                let background = if selected {
                    Some(style.selection)
                } else if hover_row == Some(index) {
                    style.hover
                } else if index % 2 == 1 {
                    style.stripe
                } else {
                    None
                };
                if let Some(background) = background {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: row_bounds,
                            ..renderer::Quad::default()
                        },
                        background,
                    );
                }
                if state.focused && self.selection.cursor == Some(index) && style.cursor.width > 0.0
                {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: row_bounds,
                            border: style.cursor,
                            ..renderer::Quad::default()
                        },
                        Background::Color(Color::TRANSPARENT),
                    );
                }
                row.as_widget().draw(
                    child,
                    renderer,
                    theme,
                    if selected { &selected_text } else { &row_text },
                    layout.child(offset),
                    cursor,
                    &body_clip,
                );
            }
        });
        if let Some((rail, scroller)) = self.scrollbar(state, body) {
            let rail = Rectangle {
                x: rail.x - inset,
                ..rail
            };
            let scroller = Rectangle {
                x: scroller.x - inset,
                ..scroller
            };
            if let Some(background) = style.rail {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: rail,
                        ..renderer::Quad::default()
                    },
                    background,
                );
            }
            renderer.fill_quad(
                renderer::Quad {
                    bounds: scroller,
                    border: Border {
                        radius: (self.scrollbar_width / 2.0).into(),
                        ..Border::default()
                    },
                    ..renderer::Quad::default()
                },
                style.scroller,
            );
        }
        if let Some(header) = &self.header {
            let header_bounds = Rectangle {
                x: bounds.x + inset,
                y: bounds.y + inset,
                width: (bounds.width - 2.0 * inset).max(0.0),
                height: (self.header_height - inset).max(0.0),
            };
            if let Some(background) = style.header {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: header_bounds,
                        ..renderer::Quad::default()
                    },
                    background,
                );
            }
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle {
                        y: header_bounds.y + header_bounds.height - 1.0,
                        height: 1.0,
                        ..header_bounds
                    },
                    ..renderer::Quad::default()
                },
                style.separator,
            );
            if let (Some(child), Some(node)) = (tree.children.last(), layout.children().last()) {
                renderer.with_layer(header_bounds, |renderer| {
                    header.as_widget().draw(
                        child,
                        renderer,
                        theme,
                        &renderer::Style {
                            text_color: style.header_text,
                        },
                        node,
                        cursor,
                        &clip,
                    );
                });
            }
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
        let bounds = layout.bounds();
        let state = tree.state.downcast_ref::<State>();
        if state.drag.is_some() {
            return mouse::Interaction::Grabbing;
        }
        if let Some((rail, _)) = self.scrollbar(state, self.body(bounds))
            && cursor.is_over(rail)
        {
            return mouse::Interaction::Idle;
        }
        self.visible
            .rows
            .iter()
            .chain(self.header.iter())
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((row, child), node)| {
                row.as_widget()
                    .mouse_interaction(child, node, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<iced_core::overlay::Element<'b, Message, Theme, Renderer>> {
        let children = self
            .visible
            .rows
            .iter_mut()
            .chain(self.header.iter_mut())
            .zip(tree.children.iter_mut())
            .zip(layout.children())
            .filter_map(|((row, child), node)| {
                row.as_widget_mut()
                    .overlay(child, node, renderer, viewport, translation)
            })
            .collect::<Vec<_>>();
        (!children.is_empty()).then(|| iced_core::overlay::Group::with_children(children).overlay())
    }
}

impl<'a, Message, Theme, Renderer> From<VirtualList<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: Catalog + 'a,
    Renderer: iced_core::Renderer + 'a,
{
    fn from(list: VirtualList<'a, Message, Theme, Renderer>) -> Self {
        Element::new(list)
    }
}

/// A `Task` that scrolls the list with `id` so `row` is in view.
pub fn scroll_to_row<T: Send + 'static>(id: impl Into<Id>, row: usize) -> Task<T> {
    struct ScrollToRow {
        target: Id,
        row: usize,
    }

    impl<T> Operation<T> for ScrollToRow {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>)) {
            operate(self);
        }

        fn custom(&mut self, id: Option<&Id>, _bounds: Rectangle, state: &mut dyn Any) {
            if id == Some(&self.target)
                && let Some(state) = state.downcast_mut::<State>()
            {
                state.requested = Some(self.row);
            }
        }
    }

    iced_runtime::task::widget(ScrollToRow {
        target: id.into(),
        row,
    })
}

/// Column titles and widths shared by a header and every row:
/// `Columns::new().column("Name", Fill).column("Size", 90)`, then
/// [`Columns::header`] for [`VirtualList::header`] and [`Columns::row`] in
/// the row builder. Plain data; the widgets are iced's row, container and
/// text, so the rows stay ordinary elements.
#[derive(Debug, Clone, PartialEq)]
pub struct Columns {
    titles: Vec<String>,
    widths: Vec<Length>,
    spacing: f32,
    padding: f32,
}

/// Caller-owned column resize lifecycle. Preview widths are applied to both
/// header and body; Commit persists them and Cancel discards the preview.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Resize {
    Preview { column: usize, width: f32 },
    Commit { column: usize },
    Cancel { column: usize },
}

impl Columns {
    pub fn new() -> Self {
        Self {
            titles: Vec::new(),
            widths: Vec::new(),
            spacing: 8.0,
            padding: 8.0,
        }
    }

    /// Adds a column titled `title` of the given width (`Fill`, or a
    /// number of pixels).
    pub fn column(mut self, title: impl Into<String>, width: impl Into<Length>) -> Self {
        self.titles.push(title.into());
        self.widths.push(width.into());
        self
    }

    /// Space between cells (default 8).
    pub fn spacing(mut self, spacing: f32) -> Self {
        self.spacing = spacing;
        self
    }

    /// Horizontal padding inside each cell (default 8).
    pub fn padding(mut self, padding: f32) -> Self {
        self.padding = padding;
        self
    }

    pub fn len(&self) -> usize {
        self.titles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.titles.is_empty()
    }

    pub fn titles(&self) -> &[String] {
        &self.titles
    }

    /// The width of column `index`.
    pub fn width(&self, index: usize) -> Length {
        self.widths.get(index).copied().unwrap_or(Length::Shrink)
    }

    /// Applies a caller-owned preview or committed width. Non-finite values
    /// and unknown columns are refused.
    pub fn set_width(&mut self, index: usize, width: f32) -> bool {
        if !width.is_finite() || width < 1.0 {
            return false;
        }
        let Some(slot) = self.widths.get_mut(index) else {
            return false;
        };
        *slot = Length::Fixed(width);
        true
    }

    /// Header with the same drag primitive as [`crate::table::Table`]. Fill
    /// columns resolve to their actual width at press time. Sizes are caller
    /// metrics; resize keys and window interruption cancel once.
    pub fn resizable_header<'a, Message, Theme, Renderer>(
        &self,
        sorted: Option<(usize, bool)>,
        on_sort: impl Fn(usize) -> Message + 'a,
        on_resize: impl Fn(Resize) -> Message + Clone + 'a,
        minimum: f32,
        grip: f32,
    ) -> Element<'a, Message, Theme, Renderer>
    where
        Message: Clone + 'a,
        Theme: iced_widget::container::Catalog
            + iced_core::widget::text::Catalog
            + crate::table::Catalog
            + 'a,
        Renderer: iced_core::text::Renderer + 'a,
    {
        let cells = self.titles.iter().enumerate().map(|(index, title)| {
            let mark = match sorted {
                Some((column, true)) if column == index => " ↑",
                Some((column, false)) if column == index => " ↓",
                _ => "",
            };
            let label = iced_widget::text(format!("{title}{mark}"))
                .wrapping(iced_core::text::Wrapping::None)
                .ellipsis(iced_core::text::Ellipsis::End);
            let cell = iced_widget::mouse_area(
                iced_widget::container(label)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .padding([0.0, self.padding])
                    .clip(true),
            )
            .on_press(on_sort(index));
            let resize = on_resize.clone();
            let divider = crate::table::Divider::new(
                cell,
                grip.max(1.0),
                |_| unreachable!("absolute width callback installed"),
                on_resize(Resize::Commit { column: index }),
                Default::default(),
            )
            .resizing(
                minimum,
                move |width| {
                    resize(Resize::Preview {
                        column: index,
                        width,
                    })
                },
                on_resize(Resize::Cancel { column: index }),
            );
            iced_widget::container(divider)
                .width(self.width(index))
                .height(Length::Fill)
                .into()
        });
        iced_widget::Row::with_children(cells)
            .spacing(self.spacing)
            .height(Length::Fill)
            .into()
    }

    /// The header row: each title in its column, the sorted one marked with
    /// an arrow (`sorted` is the column and whether it is ascending); a
    /// press publishes `on_sort(column)`.
    pub fn header<'a, Message, Theme, Renderer>(
        &self,
        sorted: Option<(usize, bool)>,
        on_sort: impl Fn(usize) -> Message + 'a,
    ) -> Element<'a, Message, Theme, Renderer>
    where
        Message: Clone + 'a,
        Theme: iced_widget::container::Catalog + iced_core::widget::text::Catalog + 'a,
        Renderer: iced_core::text::Renderer + 'a,
    {
        let cells = self.titles.iter().enumerate().map(|(index, title)| {
            let mark = match sorted {
                Some((column, true)) if column == index => " \u{2191}",
                Some((column, false)) if column == index => " \u{2193}",
                _ => "",
            };
            let label = iced_widget::text(format!("{title}{mark}"))
                .wrapping(iced_core::text::Wrapping::None)
                .ellipsis(iced_core::text::Ellipsis::End);
            iced_widget::mouse_area(self.cell_at(index, label.into()))
                .on_press(on_sort(index))
                .interaction(mouse::Interaction::Pointer)
                .into()
        });
        iced_widget::Row::with_children(cells)
            .spacing(self.spacing)
            .height(Length::Fill)
            .into()
    }

    /// A row of `cells`, one per column, each clipped to its width.
    pub fn row<'a, Message, Theme, Renderer>(
        &self,
        cells: impl IntoIterator<Item = Element<'a, Message, Theme, Renderer>>,
    ) -> Element<'a, Message, Theme, Renderer>
    where
        Message: 'a,
        Theme: iced_widget::container::Catalog + 'a,
        Renderer: iced_core::Renderer + 'a,
    {
        let cells = cells
            .into_iter()
            .enumerate()
            .map(|(index, cell)| self.cell_at(index, cell));
        iced_widget::Row::with_children(cells)
            .spacing(self.spacing)
            .height(Length::Fill)
            .into()
    }

    fn cell_at<'a, Message, Theme, Renderer>(
        &self,
        index: usize,
        content: Element<'a, Message, Theme, Renderer>,
    ) -> Element<'a, Message, Theme, Renderer>
    where
        Message: 'a,
        Theme: iced_widget::container::Catalog + 'a,
        Renderer: iced_core::Renderer + 'a,
    {
        iced_widget::container(content)
            .width(self.width(index))
            .height(Length::Fill)
            .align_y(iced_core::alignment::Vertical::Center)
            .padding([0.0, self.padding])
            .clip(true)
            .into()
    }
}

impl Default for Columns {
    fn default() -> Self {
        Self::new()
    }
}

/// The widget's status, for the [`Catalog`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Active,
    Hovered,
    /// Has keyboard focus; the cursor row shows its outline.
    Focused,
}

/// The widget's appearance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    pub background: Option<Background>,
    pub border: Border,
    /// Text colour handed to the row widgets.
    pub text: Color,
    /// Every second row.
    pub stripe: Option<Background>,
    /// The row under the pointer.
    pub hover: Option<Background>,
    pub selection: Background,
    pub selection_text: Color,
    /// Outline of the cursor row while focused (width 0 hides it).
    pub cursor: Border,
    pub header: Option<Background>,
    pub header_text: Color,
    /// The line under the header.
    pub separator: Background,
    pub rail: Option<Background>,
    pub scroller: Background,
}

/// The theme catalog of a [`VirtualList`].
pub trait Catalog {
    type Class<'a>;
    fn default<'a>() -> Self::Class<'a>;
    fn style(&self, class: &Self::Class<'_>, status: Status) -> Style;
}

pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme, Status) -> Style + 'a>;

impl Catalog for crate::Theme {
    type Class<'a> = StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(default)
    }

    fn style(&self, class: &Self::Class<'_>, status: Status) -> Style {
        class(self, status)
    }
}

/// The toolkit style: surface and text, `selection` rows, a `ring` outline
/// on the cursor row while focused, `muted_surface` header and hover.
pub fn default(theme: &crate::Theme, status: Status) -> Style {
    from_tokens(theme.tokens(), status)
}

/// [`default`] from a `Tokens` value.
pub fn from_tokens(tokens: Tokens, status: Status) -> Style {
    let p = tokens.palette;
    let m = tokens.metrics;
    Style {
        background: Some(p.surface.into()),
        border: Border {
            color: p.border,
            width: m.border.width,
            radius: m.radius.md.into(),
        },
        text: p.text,
        stripe: Some(p.surface.mix(p.muted_surface, 0.4).into()),
        hover: Some(p.muted_surface.into()),
        selection: p.selection.into(),
        selection_text: p.selection_text,
        cursor: Border {
            color: p.ring,
            width: if status == Status::Focused {
                m.border.width
            } else {
                0.0
            },
            radius: m.radius.sm.into(),
        },
        header: Some(p.muted_surface.into()),
        header_text: p.text,
        separator: p.border.into(),
        rail: if status == Status::Active {
            None
        } else {
            Some(p.muted_surface.into())
        },
        scroller: Color {
            a: 0.6,
            ..p.muted_text
        }
        .into(),
    }
}

impl Catalog for iced_core::Theme {
    type Class<'a> = StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(|theme: &iced_core::Theme, status| {
            let p = theme.palette();
            Style {
                background: Some(p.background.base.color.into()),
                border: Border {
                    color: p.background.strong.color,
                    width: 1.0,
                    radius: 4.0.into(),
                },
                text: p.background.base.text,
                stripe: Some(p.background.weakest.color.into()),
                hover: Some(p.background.weak.color.into()),
                selection: p.primary.base.color.into(),
                selection_text: p.primary.base.text,
                cursor: Border {
                    color: p.primary.strong.color,
                    width: if status == Status::Focused { 1.0 } else { 0.0 },
                    radius: 2.0.into(),
                },
                header: Some(p.background.weak.color.into()),
                header_text: p.background.weak.text,
                separator: p.background.strong.color.into(),
                rail: None,
                scroller: p.background.strongest.color.into(),
            }
        })
    }

    fn style(&self, class: &Self::Class<'_>, status: Status) -> Style {
        class(self, status)
    }
}

// The expected values here are lists of ranges, one range each.
#[allow(clippy::single_range_in_vec_init)]
#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use iced_core::shell::{Bus, Waker};
    use iced_core::window::Headless;

    use super::*;
    use crate::test_renderer::LayoutRenderer;

    #[derive(Debug, Clone, PartialEq)]
    enum Msg {
        Select(Selection),
        Activate(usize),
        Context(Option<usize>),
        Key(keyboard::Key),
    }

    type List<'a> = VirtualList<'a, Msg, iced_core::Theme, LayoutRenderer>;

    const ROW: f32 = 24.0;
    /// Ten rows fit.
    const VIEW: Size = Size::new(300.0, 240.0);

    #[test]
    fn height_index_validates_metrics_and_exact_boundaries() {
        let heights = RowHeights::new([10.0, 30.0, 20.0]).unwrap();
        assert_eq!(heights.total(), 60.0);
        assert_eq!(heights.row_at(0.0), Some(0));
        assert_eq!(heights.row_at(9.99), Some(0));
        assert_eq!(heights.row_at(10.0), Some(1));
        assert_eq!(heights.row_at(40.0), Some(2));
        assert_eq!(heights.row_at(60.0), None);
        assert_eq!(heights.row_at(f32::NAN), None);
        assert_eq!(heights.top(3), None);
        assert!(RowHeights::new([]).unwrap().is_empty());
        for bad in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(RowHeights::new([10.0, bad]), Err(HeightError { row: 1 }));
        }
        assert!(RowHeights::new([f32::MAX, 1.0]).is_err());
    }

    #[test]
    fn variable_heights_drive_layout_selection_paging_and_reveal() {
        let heights = RowHeights::new(
            (0usize..100_000).map(|row| if row.is_multiple_of(3) { 60.0 } else { 20.0 }),
        )
        .unwrap();
        let built = Cell::new(0);
        let mut list = list(heights.len(), &built).row_heights(&heights);
        let mut tree = Tree::new(&list as &dyn Widget<_, _, _>);
        let node = lay(&mut list, &mut tree);
        assert!(built.get() < 20, "100,000 rows build one screenful");
        assert_eq!(list.row_at(state(&tree), 59.0), Some(0));
        assert_eq!(list.row_at(state(&tree), 60.0), Some(1));
        assert_eq!(
            list.row_bounds(state(&tree), Rectangle::with_size(VIEW), 1)
                .height,
            20.0
        );
        assert_eq!(node.children()[0].size().height, 60.0);
        let (messages, _) = send(
            &mut list,
            &mut tree,
            &node,
            press(),
            mouse::Cursor::Available(Point::new(20.0, 61.0)),
        );
        assert_eq!(messages, vec![Msg::Select(Selection::single(1))]);
        let target = list.page_target(1, VIEW.height, true);
        assert!(target > 1 && target < 10);
        let node = lay(&mut list, &mut tree);
        let (messages, _) = send(
            &mut list,
            &mut tree,
            &node,
            named(Named::PageDown, keyboard::Modifiers::empty()),
            mouse::Cursor::Unavailable,
        );
        assert_eq!(messages, vec![Msg::Select(Selection::single(target))]);
        assert!(list.reveal_row(tree.state.downcast_mut::<State>(), VIEW.height, 99_999));
        built.set(0);
        let _ = lay(&mut list, &mut tree);
        assert!(built.get() < 20);
        assert!(built_range(&list).contains(&99_999));
        let (rail, scroller) = list
            .scrollbar(state(&tree), Rectangle::with_size(VIEW))
            .unwrap();
        assert!((scroller.y + scroller.height - rail.height).abs() < 0.01);
    }

    fn list(rows: usize, built: &Cell<usize>) -> List<'_> {
        VirtualList::new(rows, move |_| {
            built.set(built.get() + 1);
            Element::new(iced_widget::Space::new())
        })
        .row_height(ROW)
        .on_select(Msg::Select)
        .on_activate(Msg::Activate)
        .on_context(|row, _| Msg::Context(row))
        .on_key(|press| Some(Msg::Key(press.key)))
    }

    fn lay(list: &mut List<'_>, tree: &mut Tree) -> layout::Node {
        Widget::layout(
            list,
            tree,
            &LayoutRenderer::new(),
            &layout::Limits::new(Size::ZERO, VIEW),
        )
    }

    /// Sends one event; the messages published and whether a relayout was
    /// requested.
    fn send(
        list: &mut List<'_>,
        tree: &mut Tree,
        node: &layout::Node,
        event: Event,
        cursor: mouse::Cursor,
    ) -> (Vec<Msg>, bool) {
        let mut bus = Bus::new();
        let mut shell = Shell::new(&Headless, Waker::noop(), &mut bus);
        Widget::update(
            list,
            tree,
            &event,
            Layout::new(node),
            cursor,
            &LayoutRenderer::new(),
            &mut shell,
            &Rectangle::with_size(Size::INFINITE),
        );
        let relayout = shell.is_layout_invalid().is_some();
        (bus.drain().collect(), relayout)
    }

    fn key(key: keyboard::Key, modifiers: keyboard::Modifiers, text: Option<&str>) -> Event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers,
            text: text.map(Into::into),
            repeat: false,
        })
    }

    fn named(name: Named, modifiers: keyboard::Modifiers) -> Event {
        key(keyboard::Key::Named(name), modifiers, None)
    }

    fn press() -> Event {
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
    }

    fn at(row: usize) -> mouse::Cursor {
        mouse::Cursor::Available(Point::new(20.0, row as f32 * ROW + ROW / 2.0))
    }

    fn state(tree: &Tree) -> &State {
        tree.state.downcast_ref::<State>()
    }

    fn built_range(list: &List<'_>) -> Range<usize> {
        let (first, count) = list.built();
        first..first + count
    }

    /// A renderer that counts quads, to show `draw` touches only the rows
    /// in view.
    struct Quads(Cell<usize>);

    impl iced_core::Renderer for Quads {
        fn start_layer(&mut self, _bounds: Rectangle) {}
        fn end_layer(&mut self) {}
        fn start_transformation(&mut self, _transformation: iced_core::Transformation) {}
        fn end_transformation(&mut self) {}
        fn fill_quad(&mut self, _quad: renderer::Quad, _background: impl Into<Background>) {
            self.0.set(self.0.get() + 1);
        }
        fn allocate_image(
            &mut self,
            _handle: &iced_core::image::Handle,
            callback: impl FnOnce(Result<iced_core::image::Allocation, iced_core::image::Error>)
            + Send
            + 'static,
        ) {
            callback(Err(iced_core::image::Error::Unsupported));
        }
        fn hint(&mut self, _scale: renderer::Scale) {}
        fn scale(&self) -> Option<renderer::Scale> {
            None
        }
        fn reset(&mut self, _new_bounds: Rectangle) {}
        fn settings(&self) -> renderer::Settings {
            renderer::Settings::default()
        }
    }

    #[test]
    fn selection_ranges_merge_split_and_truncate() {
        let mut selection = Selection::new();
        assert!(selection.is_empty());
        selection.toggle(3);
        selection.toggle(5);
        selection.toggle(4);
        assert_eq!(selection.ranges(), &[3..6]);
        assert_eq!(selection.len(), 3);
        selection.toggle(4);
        assert_eq!(selection.ranges(), &[3..4, 5..6]);
        assert_eq!(selection.iter().collect::<Vec<_>>(), [3, 5]);
        assert_eq!(selection.cursor(), Some(4));
        selection.extend_to(9, true);
        assert_eq!(selection.ranges(), &[3..10]);
        assert_eq!(selection.anchor(), Some(4));
        selection.extend_to(1, false);
        assert_eq!(selection.ranges(), &[1..5]);
        selection.truncate(3);
        assert_eq!(selection.ranges(), &[1..3]);
        assert_eq!(selection.cursor(), Some(1));
        assert_eq!(Selection::all(0), Selection::new());
        assert_eq!(Selection::all(7).len(), 7);
        assert_eq!(Selection::single(2).first(), Some(2));
        selection.clear();
        assert!(selection.is_empty() && selection.anchor().is_none());
    }

    #[test]
    fn hundred_thousand_rows_cost_a_screenful() {
        const ROWS: usize = 100_000;
        let built = Cell::new(0);
        let mut list = list(ROWS, &built);
        let mut tree = Tree::new(&list as &dyn Widget<Msg, iced_core::Theme, LayoutRenderer>);
        let node = lay(&mut list, &mut tree);
        // Ten rows in view plus the overscan below.
        let screenful = 10 + OVERSCAN;
        assert_eq!(built.get(), screenful, "builder calls on first layout");
        assert_eq!(node.children().len(), screenful, "layout nodes");
        assert_eq!(tree.children.len(), screenful, "widget state kept");
        assert_eq!(built_range(&list), 0..screenful);
        assert_eq!(node.size(), VIEW);

        // A wheel notch scrolls three rows and asks for a relayout, which
        // builds one screenful again, no more.
        let (_, relayout) = send(
            &mut list,
            &mut tree,
            &node,
            Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 },
            }),
            at(0),
        );
        assert!(relayout);
        assert_eq!(state(&tree).offset(), 3.0 * ROW);
        built.set(0);
        let node = lay(&mut list, &mut tree);
        assert!(built.get() <= screenful + OVERSCAN, "{}", built.get());
        assert_eq!(tree.children.len(), node.children().len());
        assert!(tree.children.len() <= screenful + OVERSCAN);
        assert_eq!(built_range(&list), 3 - OVERSCAN..3 + screenful);

        // End jumps to the bottom: still one screenful, and the last row is
        // in it.
        let (_, relayout) = send(&mut list, &mut tree, &node, press(), at(0));
        assert!(!relayout);
        let (messages, relayout) = send(
            &mut list,
            &mut tree,
            &node,
            named(Named::End, keyboard::Modifiers::empty()),
            at(0),
        );
        assert!(relayout);
        assert_eq!(messages, [Msg::Select(Selection::single(ROWS - 1))]);
        built.set(0);
        let node = lay(&mut list, &mut tree);
        assert!(built.get() <= screenful + OVERSCAN);
        assert!(built_range(&list).contains(&(ROWS - 1)));
        assert_eq!(built_range(&list).end, ROWS);
        assert_eq!(node.children().len(), tree.children.len());

        // A thousand wheel notches: the state never grows.
        for _ in 0..1000 {
            let _ = send(
                &mut list,
                &mut tree,
                &node,
                Event::Mouse(mouse::Event::WheelScrolled {
                    delta: mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 },
                }),
                at(0),
            );
            built.set(0);
            let _ = lay(&mut list, &mut tree);
            assert!(built.get() <= screenful + OVERSCAN);
            assert!(tree.children.len() <= screenful + OVERSCAN);
        }
        assert_eq!(state(&tree).keys.len(), tree.children.len());
    }

    #[test]
    fn draw_fills_only_the_rows_in_view() {
        let built = Cell::new(0);
        let list: VirtualList<'_, Msg, iced_core::Theme, Quads> = VirtualList::new(100_000, |_| {
            built.set(built.get() + 1);
            Element::new(iced_widget::Space::new())
        })
        .row_height(ROW)
        .selection(&Selection::all(100_000));
        let mut list = list;
        let mut tree = Tree::new(&list as &dyn Widget<Msg, iced_core::Theme, Quads>);
        let node = Widget::layout(
            &mut list,
            &mut tree,
            &Quads(Cell::new(0)),
            &layout::Limits::new(Size::ZERO, VIEW),
        );
        let mut quads = Quads(Cell::new(0));
        Widget::draw(
            &list,
            &tree,
            &mut quads,
            &iced_core::Theme::Dark,
            &renderer::Style::default(),
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &Rectangle::with_size(VIEW),
        );
        // Background, one selection quad per row in view, the scroller.
        let in_view = 10;
        assert!(quads.0.get() >= in_view, "{}", quads.0.get());
        assert!(quads.0.get() <= in_view + 3, "{}", quads.0.get());
    }

    #[test]
    fn keys_move_select_and_activate() {
        let built = Cell::new(0);
        let mut list = list(50, &built);
        let mut tree = Tree::new(&list as &dyn Widget<Msg, iced_core::Theme, LayoutRenderer>);
        let node = lay(&mut list, &mut tree);
        let none = keyboard::Modifiers::empty();
        let shift = keyboard::Modifiers::SHIFT;
        let ctrl = keyboard::Modifiers::CTRL;

        // Keys do nothing until the list is focused by a click.
        assert_eq!(
            send(
                &mut list,
                &mut tree,
                &node,
                named(Named::ArrowDown, none),
                at(0)
            )
            .0,
            Vec::<Msg>::new()
        );
        assert!(!state(&tree).is_focused());
        let (messages, _) = send(&mut list, &mut tree, &node, press(), at(2));
        assert_eq!(messages, [Msg::Select(Selection::single(2))]);
        assert!(state(&tree).is_focused());

        let step =
            |list: &mut List<'_>, tree: &mut Tree, event| send(list, tree, &node, event, at(0)).0;
        assert_eq!(
            step(&mut list, &mut tree, named(Named::ArrowDown, none)),
            [Msg::Select(Selection::single(3))]
        );
        let mut extended = Selection::single(3);
        extended.extend_to(4, false);
        assert_eq!(
            step(&mut list, &mut tree, named(Named::ArrowDown, shift)),
            [Msg::Select(extended.clone())]
        );
        // Ctrl moves the cursor only.
        let moved = extended.clone().with_cursor(5);
        assert_eq!(
            step(&mut list, &mut tree, named(Named::ArrowDown, ctrl)),
            [Msg::Select(moved.clone())]
        );
        // Space toggles the cursor row.
        let mut toggled = moved;
        toggled.toggle(5);
        assert_eq!(toggled.ranges(), &[3..6]);
        assert_eq!(
            step(&mut list, &mut tree, named(Named::Space, none)),
            [Msg::Select(toggled)]
        );
        assert_eq!(
            step(&mut list, &mut tree, named(Named::Enter, none)),
            [Msg::Activate(5)]
        );
        // Page Down moves a screenful less one; Home and End the ends.
        assert_eq!(
            step(&mut list, &mut tree, named(Named::PageDown, none)),
            [Msg::Select(Selection::single(14))]
        );
        assert_eq!(
            step(&mut list, &mut tree, named(Named::PageUp, none)),
            [Msg::Select(Selection::single(5))]
        );
        assert_eq!(
            step(&mut list, &mut tree, named(Named::End, none)),
            [Msg::Select(Selection::single(49))]
        );
        assert_eq!(
            step(&mut list, &mut tree, named(Named::Home, none)),
            [Msg::Select(Selection::single(0))]
        );
        assert_eq!(
            step(&mut list, &mut tree, named(Named::ArrowUp, none)),
            [Msg::Select(Selection::single(0))]
        );
        // Ctrl+A, Escape.
        let all = Selection::all(50).with_cursor(0);
        assert_eq!(
            step(
                &mut list,
                &mut tree,
                key(keyboard::Key::Character("a".into()), ctrl, None)
            ),
            [Msg::Select(all)]
        );
        let mut cleared = Selection::all(50).with_cursor(0);
        cleared.clear();
        assert_eq!(
            step(&mut list, &mut tree, named(Named::Escape, none)),
            [Msg::Select(cleared)]
        );
        // Unhandled keys reach the hook.
        assert_eq!(
            step(&mut list, &mut tree, named(Named::ArrowRight, none)),
            [Msg::Key(keyboard::Key::Named(Named::ArrowRight))]
        );
        // A click outside unfocuses.
        let outside = mouse::Cursor::Available(Point::new(500.0, 500.0));
        assert_eq!(
            send(&mut list, &mut tree, &node, press(), outside).0,
            Vec::<Msg>::new()
        );
        assert!(!state(&tree).is_focused());
    }

    #[test]
    fn clicks_select_toggle_extend_activate_and_context() {
        let built = Cell::new(0);
        let mut list = list(50, &built);
        let mut tree = Tree::new(&list as &dyn Widget<Msg, iced_core::Theme, LayoutRenderer>);
        let node = lay(&mut list, &mut tree);
        let modifiers =
            |m: keyboard::Modifiers| Event::Keyboard(keyboard::Event::ModifiersChanged(m));
        assert_eq!(
            send(&mut list, &mut tree, &node, press(), at(1)).0,
            [Msg::Select(Selection::single(1))]
        );
        let _ = send(
            &mut list,
            &mut tree,
            &node,
            modifiers(keyboard::Modifiers::CTRL),
            at(1),
        );
        let mut toggled = Selection::single(1);
        toggled.toggle(3);
        assert_eq!(
            send(&mut list, &mut tree, &node, press(), at(3)).0,
            [Msg::Select(toggled.clone())]
        );
        let _ = send(
            &mut list,
            &mut tree,
            &node,
            modifiers(keyboard::Modifiers::SHIFT),
            at(1),
        );
        let mut extended = toggled;
        extended.extend_to(6, false);
        assert_eq!(extended.ranges(), &[3..7]);
        assert_eq!(
            send(&mut list, &mut tree, &node, press(), at(6)).0,
            [Msg::Select(extended)]
        );
        let _ = send(
            &mut list,
            &mut tree,
            &node,
            modifiers(keyboard::Modifiers::empty()),
            at(1),
        );
        // Two quick presses on one row activate it (one selection, then the
        // activation).
        assert_eq!(
            send(&mut list, &mut tree, &node, press(), at(8)).0,
            [Msg::Select(Selection::single(8))]
        );
        assert_eq!(
            send(&mut list, &mut tree, &node, press(), at(8)).0,
            [Msg::Activate(8)]
        );
        // Right press reports the row, or none below the last row.
        let right = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right));
        assert_eq!(
            send(&mut list, &mut tree, &node, right.clone(), at(4)).0,
            [Msg::Context(Some(4))]
        );
        let short = list.with_rows(2, |_| Element::new(iced_widget::Space::new()));
        let mut list = short;
        let node = lay(&mut list, &mut tree);
        assert_eq!(
            send(&mut list, &mut tree, &node, right, at(4)).0,
            [Msg::Context(None)]
        );
    }

    #[test]
    fn type_ahead_moves_to_the_match() {
        let names = ["apple", "apricot", "banana", "blueberry", "cherry"];
        let built = Cell::new(0);
        let mut list = list(names.len(), &built).type_ahead(|prefix, from| {
            (from..names.len()).find(|row| names[*row].starts_with(prefix))
        });
        let mut tree = Tree::new(&list as &dyn Widget<Msg, iced_core::Theme, LayoutRenderer>);
        let node = lay(&mut list, &mut tree);
        let _ = send(&mut list, &mut tree, &node, press(), at(0));
        let none = keyboard::Modifiers::empty();
        let typed = |c: &str| key(keyboard::Key::Character(c.into()), none, Some(c));
        // "b" from after the cursor finds banana; "l" within a second
        // continues the prefix to "bl": blueberry.
        assert_eq!(
            send(&mut list, &mut tree, &node, typed("b"), at(0)).0,
            [Msg::Select(Selection::single(2))]
        );
        assert_eq!(
            send(&mut list, &mut tree, &node, typed("l"), at(0)).0,
            [Msg::Select(Selection::single(3))]
        );
        // No match: nothing published, key still consumed.
        assert_eq!(
            send(&mut list, &mut tree, &node, typed("z"), at(0)).0,
            Vec::<Msg>::new()
        );
    }

    #[test]
    fn reveal_and_scroll_requests_move_the_window() {
        let built = Cell::new(0);
        let mut list = list(1000, &built).reveal(Some(500));
        let mut tree = Tree::new(&list as &dyn Widget<Msg, iced_core::Theme, LayoutRenderer>);
        let _ = lay(&mut list, &mut tree);
        assert!(built_range(&list).contains(&500));
        // The same reveal again does not fight the user's scrolling.
        let node = lay(&mut list, &mut tree);
        let _ = send(
            &mut list,
            &mut tree,
            &node,
            Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 },
            }),
            at(0),
        );
        let _ = lay(&mut list, &mut tree);
        assert!(!built_range(&list).contains(&500));
        // A `scroll_to_row` request lands in the state and is applied on
        // the next event.
        tree.state.downcast_mut::<State>().requested = Some(10);
        let (_, relayout) = send(
            &mut list,
            &mut tree,
            &node,
            Event::Window(window::Event::RedrawRequested(Instant::now())),
            mouse::Cursor::Unavailable,
        );
        assert!(relayout);
        let _ = lay(&mut list, &mut tree);
        assert!(built_range(&list).contains(&10));
        assert!(state(&tree).requested.is_none());
    }

    #[test]
    fn header_is_the_last_child_and_rows_sit_below_it() {
        let built = Cell::new(0);
        let mut list = list(100, &built).header(Element::new(iced_widget::Space::new()));
        let mut tree = Tree::new(&list as &dyn Widget<Msg, iced_core::Theme, LayoutRenderer>);
        let node = lay(&mut list, &mut tree);
        let rows = node.children().len() - 1;
        assert_eq!(tree.children.len(), rows + 1);
        assert_eq!(node.children().last().unwrap().bounds().y, 0.0);
        assert_eq!(node.children()[0].bounds().y, 28.0);
        // A press on the header does not select.
        assert_eq!(
            send(
                &mut list,
                &mut tree,
                &node,
                press(),
                mouse::Cursor::Available(Point::new(10.0, 10.0))
            )
            .0,
            Vec::<Msg>::new()
        );
        assert_eq!(
            send(
                &mut list,
                &mut tree,
                &node,
                press(),
                mouse::Cursor::Available(Point::new(10.0, 28.0 + ROW * 1.5))
            )
            .0,
            [Msg::Select(Selection::single(1))]
        );
        // Relayout after a scroll keeps the header child.
        let _ = send(
            &mut list,
            &mut tree,
            &node,
            Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 },
            }),
            at(3),
        );
        let node = lay(&mut list, &mut tree);
        assert_eq!(tree.children.len(), node.children().len());
    }

    /// An operation that records the clips a widget advertises while it is
    /// traversed.
    #[derive(Default)]
    struct ClipRecorder {
        clips: Vec<Rectangle>,
    }

    impl Operation for ClipRecorder {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn clip(&mut self, bounds: Rectangle) {
            self.clips.push(bounds);
        }
    }

    #[test]
    fn operate_advertises_the_exact_row_drawing_clip() {
        let built = Cell::new(0);
        let mut list = list(100, &built);
        let mut tree = Tree::new(&list as &dyn Widget<Msg, iced_core::Theme, LayoutRenderer>);
        let node = lay(&mut list, &mut tree);
        let mut recorder = ClipRecorder::default();
        Widget::operate(
            &mut list,
            &mut tree,
            Layout::new(&node),
            &LayoutRenderer::new(),
            &mut recorder,
        );
        // Without a header, the body is the whole list, so the advertised
        // clip is exactly the list bounds.
        assert_eq!(recorder.clips, vec![Rectangle::with_size(VIEW)]);
    }

    #[test]
    fn operate_scopes_the_row_clip_away_from_the_header() {
        let built = Cell::new(0);
        let mut list = list(100, &built).header(Element::new(iced_widget::Space::new()));
        let mut tree = Tree::new(&list as &dyn Widget<Msg, iced_core::Theme, LayoutRenderer>);
        let node = lay(&mut list, &mut tree);
        let mut recorder = ClipRecorder::default();
        Widget::operate(
            &mut list,
            &mut tree,
            Layout::new(&node),
            &LayoutRenderer::new(),
            &mut recorder,
        );
        // The row clip starts below the header, is advertised exactly once,
        // and the header walk (which draws with the list clip) does not see
        // it: a clip cannot leak past its traversal scope.
        assert_eq!(
            recorder.clips,
            vec![Rectangle {
                x: 0.0,
                y: 28.0,
                width: VIEW.width,
                height: VIEW.height - 28.0,
            }]
        );
    }

    #[test]
    fn scrollbar_drag_and_rail_jump() {
        let built = Cell::new(0);
        let mut list = list(1000, &built);
        let mut tree = Tree::new(&list as &dyn Widget<Msg, iced_core::Theme, LayoutRenderer>);
        let node = lay(&mut list, &mut tree);
        let rail_x = VIEW.width - 4.0;
        // A press low on the rail jumps there.
        let (messages, relayout) = send(
            &mut list,
            &mut tree,
            &node,
            press(),
            mouse::Cursor::Available(Point::new(rail_x, VIEW.height - 1.0)),
        );
        assert_eq!(messages, Vec::<Msg>::new());
        assert!(relayout);
        let jumped = state(&tree).offset();
        assert!(jumped > 0.0);
        // Dragging back up moves the offset up; release ends the drag.
        let (_, relayout) = send(
            &mut list,
            &mut tree,
            &node,
            Event::Mouse(mouse::Event::CursorMoved {
                position: Point::new(rail_x, VIEW.height / 2.0),
            }),
            mouse::Cursor::Available(Point::new(rail_x, VIEW.height / 2.0)),
        );
        assert!(relayout);
        assert!(state(&tree).offset() < jumped);
        let _ = send(
            &mut list,
            &mut tree,
            &node,
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            mouse::Cursor::Unavailable,
        );
        assert!(state(&tree).drag.is_none());
        // A short list has no scrollbar and no rail hit.
        let mut short = VirtualList::<Msg, iced_core::Theme, LayoutRenderer>::new(3, |_| {
            Element::new(iced_widget::Space::new())
        })
        .row_height(ROW);
        let mut tree = Tree::new(&short as &dyn Widget<Msg, iced_core::Theme, LayoutRenderer>);
        let node = lay(&mut short, &mut tree);
        assert_eq!(node.children()[0].bounds().width, VIEW.width);
        assert!(
            short
                .scrollbar(state(&tree), Rectangle::with_size(VIEW))
                .is_none()
        );
    }

    #[test]
    fn columns_share_widths() {
        let columns = Columns::new()
            .column("Name", Length::Fill)
            .column("Size", 90.0)
            .spacing(4.0);
        assert_eq!(columns.len(), 2);
        assert_eq!(columns.width(1), Length::Fixed(90.0));
        assert_eq!(columns.width(5), Length::Shrink);
        assert_eq!(columns.titles(), ["Name", "Size"]);
        let header: Element<'_, Msg, iced_core::Theme, LayoutRenderer> =
            columns.header(Some((0, true)), |_| Msg::Activate(0));
        let row: Element<'_, Msg, iced_core::Theme, LayoutRenderer> = columns.row([
            Element::new(iced_widget::Space::new()),
            Element::new(iced_widget::Space::new()),
        ]);
        for mut element in [header, row] {
            let mut tree = Tree::new(element.as_widget());
            element.as_widget_mut().diff(&mut tree);
            let node = element.as_widget_mut().layout(
                &mut tree,
                &LayoutRenderer::new(),
                &layout::Limits::new(Size::new(300.0, 28.0), Size::new(300.0, 28.0)),
            );
            let cells = node.children();
            assert_eq!(cells.len(), 2);
            assert_eq!(cells[1].bounds().width, 90.0);
            assert_eq!(cells[0].bounds().width, 300.0 - 90.0 - 4.0);
        }
    }
}
