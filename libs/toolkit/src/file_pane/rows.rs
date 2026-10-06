// SPDX-License-Identifier: MIT OR Apache-2.0
//! Viewport-only file rows with cached paragraphs and stable click identity.
use super::*;
use iced_core::text::{self as atext, Paragraph as _};
use iced_core::widget::{Tree, tree};
use iced_core::{Event, Layout, Shell, Widget, alignment, keyboard, layout, mouse, renderer};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
/// One column contract for headers and rows. Secondary widths are measured
/// in the resolved mono role, including the widest absolute time form.
#[derive(Clone, Copy, Debug)]
pub struct Columns {
    pub name_min: f32,
    pub size: f32,
    pub modified: f32,
    pub gap: f32,
    pub pad: f32,
}
impl Columns {
    /// Local x/width pairs shared by headers and rows. Preserve a usable
    /// name before secondary columns: hide Modified, then Size. Numeric
    /// values are never shortened into an ambiguous number.
    pub fn cells(self, width: f32) -> [(f32, f32); 3] {
        let pad = self.pad.min(width.max(0.0) / 2.0);
        let available = (width - 2.0 * pad).max(0.0);
        let modified = if available >= self.name_min + self.size + self.modified + 2.0 * self.gap {
            self.modified
        } else {
            0.0
        };
        let size_budget = available - self.name_min - self.gap;
        let size = if modified > 0.0 || size_budget >= self.size {
            self.size
        } else {
            0.0
        };
        let name = available
            - size
            - modified
            - if size > 0.0 { self.gap } else { 0.0 }
            - if modified > 0.0 { self.gap } else { 0.0 };
        let size_x = pad + name + if size > 0.0 { self.gap } else { 0.0 };
        [
            (pad, name),
            (size_x, size),
            (width.max(0.0) - pad - modified, modified),
        ]
    }

    /// Tree indentation yields to the same minimum name budget as columns.
    pub fn indentation(self, width: f32, depth: usize, icon: f32) -> f32 {
        (depth as f32 * icon).min((self.cells(width)[0].1 - self.name_min).max(0.0))
    }

    /// The actual text rectangle inside Name, in pane-local coordinates.
    /// Both shaping and drawing use this; decoration is subtracted once.
    pub fn name_text(self, width: f32, depth: usize, icon: f32, padding: f32) -> (f32, f32) {
        let (start, cell_width) = self.cells(width)[0];
        let decoration = self.indentation(width, depth, icon) + 2.0 * icon + padding;
        (start + decoration, (cell_width - decoration).max(0.0))
    }
}

// Size/count strings use the mono role, assuming width follows grapheme count.
// Overrides may use a proportional family: retain every tie at the fourth
// longest length so equal-length values cannot hide a wider candidate.
const SIZE_CANDIDATES: usize = 4;

pub fn listing_size_width<S: AsRef<str>>(
    values: impl Iterator<Item = S>,
    padding: f32,
    measure: impl Fn(&str) -> f32,
) -> f32 {
    let floor = measure("99.9 MiB");
    let ceiling = measure("999999 items").max(floor);
    use unicode_segmentation::UnicodeSegmentation;
    let mut longest: Vec<(usize, S)> = Vec::with_capacity(SIZE_CANDIDATES);
    for value in values {
        let length = value.as_ref().graphemes(true).count();
        let position = longest.partition_point(|(n, _)| *n >= length);
        if longest.len() < SIZE_CANDIDATES || length >= longest[SIZE_CANDIDATES - 1].0 {
            longest.insert(position, (length, value));
            if longest.len() > SIZE_CANDIDATES {
                let cutoff = longest[SIZE_CANDIDATES - 1].0;
                let keep = longest.partition_point(|(n, _)| *n >= cutoff);
                longest.truncate(keep);
            }
        }
    }
    let mut measured = HashSet::new();
    longest
        .iter()
        .map(|(_, s)| s.as_ref())
        .filter(|s| measured.insert(*s))
        .map(measure)
        .fold(floor, f32::max)
        .min(ceiling)
        + 2.0 * padding
}

/// List rows per wheel notch.
const WHEEL_ROWS: f32 = 3.0;
/// A second press on the same row inside this window is a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Press and release within this distance is a click.
const CLICK_SLOP: f32 = 5.0;

/// One cached row: the shaped paragraphs and the text they were shaped from.
struct Cached<P> {
    name: P,
    name_of: String,
    name_width: u32,
    size: P,
    size_of: String,
    modified: P,
    modified_of: String,
}

/// Tree state: the scroll offset, the shaped-row cache, the row height.
struct RowState<P> {
    /// Pixels scrolled past the top of the list.
    offset: f32,
    row_h: f32,
    metrics_key: Option<(iced_core::Font, u32, iced_core::Font, u32)>,
    /// The tint the cache was built for (a re-tint clears it).
    tint: String,
    last_selected: Option<PathBuf>,
    /// The listing the scroll state belongs to (the pane's root path): a new
    /// listing starts at the top.
    listing: Option<PathBuf>,
    /// Where a button went down, waiting for its release.
    press: Option<(Point, usize, PathBuf)>,
    drag_epoch: u64,
    /// The last completed click: `(when, row)` — a second on the same row
    /// inside [`DOUBLE_CLICK`] is a double-click.
    last_click: Option<(Instant, usize, PathBuf)>,
    modifiers: keyboard::Modifiers,
    cache: HashMap<PathBuf, Cached<P>>,
}

impl<P> RowState<P> {
    fn new(look: Presentation) -> Self {
        Self {
            offset: 0.0,
            row_h: look.px.max(look.small_px) * 1.4 + 2.0 * look.chrome.small,
            metrics_key: None,
            tint: String::new(),
            last_selected: None,
            listing: None,
            press: None,
            drag_epoch: 0,
            last_click: None,
            modifiers: keyboard::Modifiers::empty(),
            cache: HashMap::new(),
        }
    }

    fn clamp(&mut self, rows: usize, height: f32) {
        let max = (rows as f32 * self.row_h - height).max(0.0);
        self.offset = self.offset.clamp(0.0, max);
    }

    /// The row index under list-space `y`, if any.
    fn row_at(&self, y: f32, rows: usize) -> Option<usize> {
        if y < 0.0 {
            return None;
        }
        let index = ((y + self.offset) / self.row_h) as usize;
        (index < rows).then_some(index)
    }

    fn is_visible(&self, index: usize, height: f32) -> bool {
        let top = index as f32 * self.row_h - self.offset;
        top + self.row_h >= 0.0 && top <= height
    }
}

/// One pane's listing, with only visible rows shaped and drawn.
pub struct FilePane<'a, Theme, Renderer> {
    columns: Columns,
    source: Box<dyn Source + 'a>,
    selected_paths: Option<&'a HashSet<PathBuf>>,
    look: Presentation,
    tint: &'a str,
    tips: Vec<Element<'a, Message, Theme, Renderer>>,
    tooltip: Option<Box<dyn Fn(String, Size) -> Element<'a, Message, Theme, Renderer> + 'a>>,
    decoration: Box<dyn Fn(&mut Renderer, usize, Decoration, Rectangle, Rectangle) + 'a>,
    open_label: String,
    transfer: Option<Box<dyn Transfer + 'a>>,
    busy: bool,
}

impl<'a, Theme, Renderer> FilePane<'a, Theme, Renderer>
where
    Renderer: atext::Renderer<Font = iced_core::Font> + 'static,
{
    pub fn new(source: impl Source + 'a, look: Presentation, columns: Columns) -> Self {
        Self {
            source: Box::new(source),
            selected_paths: None,
            look,
            columns,
            tint: "",
            tips: Vec::new(),
            tooltip: None,
            decoration: Box::new(|_, _, _, _, _| {}),
            open_label: "Open".into(),
            transfer: None,
            busy: false,
        }
    }
    pub fn tint(mut self, tint: &'a str) -> Self {
        self.tint = tint;
        self
    }
    pub fn busy(mut self, busy: bool) -> Self {
        self.busy = busy;
        self
    }
    pub fn open_label(mut self, label: String) -> Self {
        self.open_label = label;
        self
    }
    pub fn tooltip(
        mut self,
        build: impl Fn(String, Size) -> Element<'a, Message, Theme, Renderer> + 'a,
    ) -> Self {
        self.tooltip = Some(Box::new(build));
        self
    }
    pub fn decoration(
        mut self,
        draw: impl Fn(&mut Renderer, usize, Decoration, Rectangle, Rectangle) + 'a,
    ) -> Self {
        self.decoration = Box::new(draw);
        self
    }
    pub fn transfer(mut self, bridge: impl Transfer + 'a) -> Self {
        self.transfer = Some(Box::new(bridge));
        self
    }
    pub fn selected_paths(mut self, paths: &'a HashSet<PathBuf>) -> Self {
        self.selected_paths = Some(paths);
        self
    }
    fn is_selected(&self, path: &Path) -> bool {
        self.selected_paths.map_or_else(
            || self.source.is_selected(path),
            |paths| paths.contains(path),
        )
    }

    /// Row height from the theme's font metrics (ced's `ensure_metrics`
    /// trick): shape a sample line once per `(font, px)` and pad it.
    fn ensure_metrics(&self, st: &mut RowState<Renderer::Paragraph>) {
        let key = (
            self.look.ui_font,
            self.look.px.to_bits(),
            self.look.mono_font,
            self.look.small_px.to_bits(),
        );
        if st.metrics_key == Some(key) {
            return;
        }
        if st.metrics_key.is_some() {
            // Typography changed: every shaped row is stale (the cache keys
            // on text + tint, not on the font), so shape from scratch.
            st.cache.clear();
        }
        let line_h = self.look.px * 1.4;
        let sample = Renderer::Paragraph::with_text(atext::Text {
            content: "Ag",
            bounds: Size::INFINITE,
            size: iced_core::Pixels(self.look.px),
            line_height: atext::LineHeight::Absolute(iced_core::Pixels(line_h)),
            font: self.look.ui_font,
            align_x: atext::Alignment::Left,
            align_y: alignment::Vertical::Top,
            shaping: atext::Shaping::Advanced,
            wrapping: atext::Wrapping::None,
            ellipsis: iced_core::text::Ellipsis::None,
            hint_factor: None,
        });
        st.row_h = sample
            .min_bounds()
            .height
            .max(line_h)
            .max(self.look.small_px * 1.4)
            + 2.0 * self.look.chrome.small;
        st.metrics_key = Some(key);
    }

    pub fn shape(content: &str, font: iced_core::Font, px: f32) -> Renderer::Paragraph {
        Renderer::Paragraph::with_text(atext::Text {
            content,
            bounds: Size::INFINITE,
            size: iced_core::Pixels(px),
            line_height: atext::LineHeight::Absolute(iced_core::Pixels(px * 1.4)),
            font,
            align_x: atext::Alignment::Left,
            align_y: alignment::Vertical::Top,
            shaping: atext::Shaping::Advanced,
            wrapping: atext::Wrapping::None,
            ellipsis: iced_core::text::Ellipsis::None,
            hint_factor: None,
        })
    }

    /// Shape (or re-shape) a row's three columns. An entry whose source text
    /// differs — a relist or size landed — re-shapes.
    fn cache_row(
        &self,
        st: &mut RowState<Renderer::Paragraph>,
        index: usize,
        row: &Row<'_>,
        width: f32,
    ) {
        if st.tint != self.tint {
            // A re-tint means a new theme: fonts and colours may all differ.
            st.tint = self.tint.to_owned();
            st.cache.clear();
            st.metrics_key = None;
        }
        let size_text = self.source.size_text(index);
        let modified_text = self.source.modified_text(index);
        let name = row.name.to_owned();
        let (_, name_width) = self.columns.name_text(
            width,
            row.depth,
            self.look.chrome.icon,
            self.look.chrome.small,
        );
        if let Some(cached) = st.cache.get(row.path)
            && cached.name_of == name
            && cached.name_width == name_width.to_bits()
            && cached.size_of == size_text
            && cached.modified_of == modified_text
        {
            return;
        }
        let elided = crate::elide::middle(&name, name_width, |s| {
            Self::shape(s, self.look.ui_font, self.look.px)
                .min_bounds()
                .width
        });
        let shaped = Cached {
            name: Self::shape(&elided, self.look.ui_font, self.look.px),
            name_of: name,
            name_width: name_width.to_bits(),
            size: Self::shape(&size_text, self.look.mono_font, self.look.small_px),
            size_of: size_text,
            modified: Self::shape(&modified_text, self.look.mono_font, self.look.small_px),
            modified_of: modified_text,
        };
        if st.cache.len() >= 512 {
            st.cache.clear();
        }
        st.cache.insert(row.path.to_path_buf(), shaped);
        // The cache only ever holds what viewports asked for; drop anything
        // the current listing no longer shows once it grows past a screenful
        // of headroom.
    }

    /// A new listing (the pane navigated) resets the scroll state: the deep
    /// offset of the previous directory must not carry into the next one.
    fn reset_on_relist(&self, st: &mut RowState<Renderer::Paragraph>) {
        if st.listing.as_deref() == Some(self.source.root()) {
            return;
        }
        st.listing = Some(self.source.root().to_path_buf());
        st.offset = 0.0;
        st.last_selected = None;
        st.press = None;
        st.last_click = None;
    }

    /// Follow the selection when it changes: keep the selected row visible.
    fn follow_selection(&self, st: &mut RowState<Renderer::Paragraph>, height: f32) {
        let Some(selected) = self.source.selected() else {
            return;
        };
        if st.last_selected.as_deref() == Some(selected) {
            return;
        }
        st.last_selected = Some(selected.to_path_buf());
        if let Some(index) = self.source.selected_index() {
            let top = index as f32 * st.row_h;
            let bottom = top + st.row_h;
            if top < st.offset {
                st.offset = top;
            } else if bottom > st.offset + height {
                st.offset = bottom - height;
            }
        }
    }

    /// Shape every row the viewport will draw (called from `update`, which
    /// owns the `&mut Tree` the cache lives in).
    fn sync_cache(&self, st: &mut RowState<Renderer::Paragraph>, height: f32, width: f32) {
        let first = (st.offset / st.row_h).floor().max(0.0) as usize;
        for index in first..self.source.len() {
            let Some(row) = self.source.row(index) else {
                break;
            };
            if index > first && !st.is_visible(index, height) {
                break;
            }
            self.cache_row(st, index, &row, width);
        }
    }
}

impl<Theme, Renderer> Widget<Message, Theme, Renderer> for FilePane<'_, Theme, Renderer>
where
    Renderer: atext::Renderer<Font = iced_core::Font> + 'static,
{
    fn diff(&mut self, _tree: &mut Tree) {
        // Preserve hover state until layout reconciles visible icon regions.
    }
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<RowState<Renderer::Paragraph>>()
    }
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn state(&self) -> tree::State {
        tree::State::new(RowState::<Renderer::Paragraph>::new(self.look))
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = limits.resolve(Length::Fill, Length::Fill, Size::ZERO);
        let st = tree.state.downcast_mut::<RowState<Renderer::Paragraph>>();
        self.ensure_metrics(st);
        self.reset_on_relist(st);
        self.follow_selection(st, size.height);
        st.clamp(self.source.len(), size.height);
        let icon = self.look.chrome.icon;
        let cell = self.columns.cells(size.width)[0];
        let clip = Rectangle {
            x: cell.0,
            y: 0.0,
            width: cell.1,
            height: size.height,
        };
        let mut regions = Vec::new();
        let first = (st.offset / st.row_h).floor().max(0.0) as usize;
        for index in first..self.source.len() {
            let Some(row) = self.source.row(index) else {
                break;
            };
            let y = index as f32 * st.row_h - st.offset;
            if y >= size.height {
                break;
            }
            let x = cell.0 + self.columns.indentation(size.width, row.depth, icon);
            let y = y + (st.row_h - icon) / 2.0;
            if row.is_dir {
                let label = format!(
                    "{} {}",
                    if self.source.is_expanded(row.path) {
                        "Collapse"
                    } else {
                        "Expand"
                    },
                    row.name
                );
                if let Some(bounds) = (Rectangle {
                    x,
                    y,
                    width: icon,
                    height: icon,
                })
                .intersection(&clip)
                {
                    regions.push((bounds, label));
                }
            }
            if let Some(bounds) = (Rectangle {
                x: x + icon,
                y,
                width: icon,
                height: icon,
            })
            .intersection(&clip)
            {
                regions.push((
                    bounds,
                    format!(
                        "{}: {} — {}",
                        if row.is_dir { "Folder" } else { "File" },
                        row.name,
                        self.open_label
                    ),
                ));
            }
        }
        self.tips = match &self.tooltip {
            Some(build) => regions
                .iter()
                .map(|(bounds, label)| build(label.clone(), bounds.size()))
                .collect(),
            None => Vec::new(),
        };
        tree.diff_children(&mut self.tips);
        let children = self
            .tips
            .iter_mut()
            .zip(&mut tree.children)
            .zip(regions)
            .map(|((child, state), (bounds, _))| {
                child
                    .as_widget_mut()
                    .layout(
                        state,
                        renderer,
                        &layout::Limits::new(Size::ZERO, bounds.size()),
                    )
                    .move_to(bounds.position())
            })
            .collect();
        layout::Node::with_children(size, children)
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
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };
        let hover = if cursor.is_over(clip) {
            cursor
        } else {
            mouse::Cursor::Unavailable
        };
        for ((tip, state), child) in self
            .tips
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            tip.as_widget_mut()
                .update(state, event, child, hover, renderer, shell, &clip);
        }
        let st = tree.state.downcast_mut::<RowState<Renderer::Paragraph>>();
        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                st.modifiers = *modifiers;
            }
            Event::Window(iced_core::window::Event::Unfocused) => {
                st.modifiers = keyboard::Modifiers::empty();
            }
            _ => {}
        }
        self.ensure_metrics(st);
        self.reset_on_relist(st);
        self.follow_selection(st, clip.height);
        st.clamp(self.source.len(), clip.height);
        self.sync_cache(st, clip.height, bounds.width);

        // Async listings and sorting can replace the pressed index before the
        // drag threshold. Never transfer whichever entry happens to occupy it.
        if st.press.as_ref().is_some_and(|(_, index, path)| {
            self.source
                .row(*index)
                .is_none_or(|row| row.path != path.as_path())
        }) {
            st.press = None;
            st.last_click = None;
        }

        if let Some(transfer) = &self.transfer {
            let epoch = transfer.cancel_epoch();
            if st.drag_epoch != epoch {
                st.press = None;
                st.last_click = None;
                st.drag_epoch = epoch;
            }
            if transfer.active() {
                st.press = None;
                st.last_click = None;
                if cursor.is_over(clip) {
                    let pointer = cursor.position().unwrap_or_default();
                    let index = st.row_at(pointer.y - bounds.y, self.source.len());
                    let directory = index
                        .and_then(|index| self.source.row(index))
                        .filter(|row| row.is_dir);
                    let highlight = if directory.is_some() {
                        Rectangle {
                            y: bounds.y + index.unwrap() as f32 * st.row_h - st.offset,
                            height: st.row_h,
                            ..clip
                        }
                        .intersection(&clip)
                        .unwrap_or(clip)
                    } else {
                        clip
                    };
                    transfer.hover(
                        directory.map(|row| row.path),
                        self.source.root(),
                        clip,
                        highlight,
                        pointer,
                        self.busy,
                    );
                }
                return;
            }
        }

        match event {
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                if let Some((pressed_at, index, _)) = st.press.as_ref()
                    && ((position.x - pressed_at.x).powi(2) + (position.y - pressed_at.y).powi(2))
                        .sqrt()
                        > CLICK_SLOP
                    && let Some(row) = self.source.row(*index)
                    && !self.busy
                    && self.transfer.is_some()
                {
                    st.press = None;
                    st.last_click = None;
                    if let Some(transfer) = &self.transfer {
                        transfer.start(row.path, row.is_dir, self.source.root(), *position);
                    }
                    shell.request_redraw();
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if cursor.is_over(clip) => {
                let lines = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y * WHEEL_ROWS,
                    mouse::ScrollDelta::Pixels { y, .. } => *y / st.row_h,
                };
                if lines != 0.0 {
                    st.offset -= lines * st.row_h;
                    st.clamp(self.source.len(), clip.height);
                    shell.invalidate_layout();
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                if cursor.is_over(clip) =>
            {
                // Any press in the listing activates the pane it belongs to
                // (the caller maps this onto `set_active_pane`), then the
                // press starts the row-click tracker.
                shell.publish(Message::Press);
                let position = cursor.position().unwrap_or_default();
                if let Some(index) = st.row_at(position.y - bounds.y, self.source.len()) {
                    st.press = Some((
                        position,
                        index,
                        self.source.row(index).expect("hit row").path.to_path_buf(),
                    ));
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right))
                if cursor.is_over(clip) =>
            {
                st.press = None;
                st.last_click = None;
                let position = cursor.position().unwrap_or_default();
                let path = st
                    .row_at(position.y - bounds.y, self.source.len())
                    .map(|index| self.source.row(index).expect("hit row").path.to_path_buf());
                shell.publish(Message::ContextMenu(path, position));
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let Some((pressed_at, index, _)) = st.press.take() else {
                    return;
                };
                if !cursor.is_over(clip) {
                    return;
                }
                let position = cursor.position().unwrap_or_default();
                let moved = ((position.x - pressed_at.x).powi(2)
                    + (position.y - pressed_at.y).powi(2))
                .sqrt()
                    > CLICK_SLOP;
                if moved || index >= self.source.len() {
                    return;
                }
                let Some(row) = self.source.row(index) else {
                    return;
                };
                // The toggle zone: the leftmost TOGGLE_W of the row, on a
                // directory — click it to expand. A toggle is not a row
                // click: it stays out of the double-click tracker, so a fast
                // double-click in the chevron zone toggles once.
                let row_x = position.x
                    - bounds.x
                    - self.columns.cells(bounds.width)[0].0
                    - self
                        .columns
                        .indentation(bounds.width, row.depth, self.look.chrome.icon);
                let in_toggle = row.is_dir && row_x >= 0.0 && row_x < self.look.chrome.icon;
                if st.modifiers.control() || st.modifiers.shift() {
                    st.last_click = None;
                    shell.publish(Message::SelectModified(
                        row.path.to_path_buf(),
                        st.modifiers.control(),
                        st.modifiers.shift(),
                    ));
                } else if in_toggle {
                    st.last_click = None;
                    shell.publish(Message::Toggle(row.path.to_path_buf()));
                } else {
                    let double = st.last_click.as_ref().is_some_and(|(when, at, path)| {
                        when.elapsed() < DOUBLE_CLICK && *at == index && path.as_path() == row.path
                    });
                    st.last_click = Some((Instant::now(), index, row.path.to_path_buf()));
                    if double && row.is_dir {
                        shell.publish(Message::Toggle(row.path.to_path_buf()));
                    } else if !double {
                        shell.publish(Message::Select(row.path.to_path_buf()));
                    }
                }
                shell.capture_event();
            }
            Event::Window(iced_core::window::Event::Unfocused) => {
                st.press = None;
                st.last_click = None;
                st.modifiers = keyboard::Modifiers::empty();
            }
            Event::Window(iced_core::window::Event::Resized(_))
            | Event::Mouse(mouse::Event::CursorLeft) => {
                st.press = None;
                st.last_click = None;
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };
        let st = tree.state.downcast_ref::<RowState<Renderer::Paragraph>>();
        let t = self.look.tokens;
        let cells = self.columns.cells(bounds.width);
        let icon_px = self.look.chrome.icon;
        let row_pad = self.look.chrome.small;
        renderer.with_layer(clip, |renderer| {
            if let Some(highlight) = self
                .transfer
                .as_ref()
                .and_then(|bridge| bridge.highlight())
                .and_then(|highlight| highlight.intersection(&clip))
            {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: highlight,
                        ..Default::default()
                    },
                    t.palette.muted_surface,
                );
            }
            // Every selected row's full-width background, under everything.
            let first = (st.offset / st.row_h).floor().max(0.0) as usize;
            let last = ((st.offset + clip.height) / st.row_h).ceil() as usize;
            for index in first..last.min(self.source.len()) {
                let Some(row) = self.source.row(index) else {
                    break;
                };
                if !self.is_selected(row.path) || !st.is_visible(index, clip.height) {
                    continue;
                }
                let rect = Rectangle {
                    x: bounds.x,
                    y: bounds.y + index as f32 * st.row_h - st.offset,
                    width: bounds.width,
                    height: st.row_h,
                };
                if let Some(clipped) = rect.intersection(&clip) {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: clipped,
                            ..renderer::Quad::default()
                        },
                        t.palette.selection,
                    );
                }
            }

            // Selection keeps its text/background contrast. Paint the target
            // outline afterwards so an already selected folder cannot hide it.
            if let Some(highlight) = self
                .transfer
                .as_ref()
                .and_then(|bridge| bridge.highlight())
                .and_then(|highlight| highlight.intersection(&clip))
            {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: highlight,
                        border: iced_core::Border {
                            color: t.palette.ring,
                            width: self.look.chrome.edge * 2.0,
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    iced_core::Color::TRANSPARENT,
                );
            }

            let first = (st.offset / st.row_h).floor().max(0.0) as usize;
            for index in first..self.source.len() {
                let Some(row) = self.source.row(index) else {
                    break;
                };
                let y = bounds.y + index as f32 * st.row_h - st.offset;
                if y > clip.y + clip.height {
                    break;
                }
                if y + st.row_h < clip.y {
                    continue;
                }
                let baseline = y + row_pad;
                let x = bounds.x
                    + cells[0].0
                    + self.columns.indentation(bounds.width, row.depth, icon_px);
                let name_clip = Rectangle {
                    x: bounds.x + cells[0].0,
                    y: clip.y,
                    width: cells[0].1,
                    height: clip.height,
                };
                renderer.with_layer(name_clip, |renderer| {
                    // Directories paint the chevron in their toggle zone; every row
                    // paints its file icon (open when the directory is expanded).
                    if row.is_dir {
                        let chevron_bounds = Rectangle {
                            x,
                            y: baseline + (st.row_h - 2.0 * row_pad - icon_px) / 2.0,
                            width: icon_px,
                            height: icon_px,
                        };
                        let chevron = if self.source.is_expanded(row.path) {
                            Decoration::ChevronDown
                        } else {
                            Decoration::ChevronRight
                        };
                        (self.decoration)(renderer, index, chevron, chevron_bounds, clip);
                    }
                    let icon_bounds = Rectangle {
                        x: x + icon_px,
                        y: baseline + (st.row_h - 2.0 * row_pad - icon_px) / 2.0,
                        width: icon_px,
                        height: icon_px,
                    };
                    (self.decoration)(renderer, index, Decoration::Entry, icon_bounds, clip);

                    // Name; secondary columns right-aligned, in the mono role.
                    if let Some(cached) = st.cache.get(row.path) {
                        let color = if self.is_selected(row.path) {
                            t.palette.selection_text
                        } else {
                            t.palette.text
                        };
                        renderer.fill_paragraph(
                            &cached.name,
                            Point::new(
                                bounds.x
                                    + self
                                        .columns
                                        .name_text(
                                            bounds.width,
                                            row.depth,
                                            icon_px,
                                            self.look.chrome.small,
                                        )
                                        .0,
                                baseline,
                            ),
                            color,
                            name_clip,
                        );
                    }
                });
                let Some(cached) = st.cache.get(row.path) else {
                    continue;
                };
                for (para, (start, width)) in
                    [(&cached.size, cells[1]), (&cached.modified, cells[2])]
                {
                    // Never show a clipped numeric prefix as a different value.
                    if width <= 0.0 || para.min_bounds().width > width {
                        continue;
                    }
                    let cell = Rectangle {
                        x: bounds.x + start,
                        y: clip.y,
                        width,
                        height: clip.height,
                    };
                    let color = if self.is_selected(row.path) {
                        t.palette.selection_text
                    } else {
                        t.palette.muted_text
                    };
                    renderer.with_layer(cell, |renderer| {
                        renderer.fill_paragraph(
                            para,
                            Point::new(cell.x + width - para.min_bounds().width, baseline),
                            color,
                            cell,
                        )
                    });
                }
            }
        });
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        mouse::Interaction::Idle
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: iced_core::Vector,
    ) -> Option<iced_core::overlay::Element<'b, Message, Theme, Renderer>> {
        iced_core::overlay::from_children(
            &mut self.tips,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Theme: 'a, Renderer: atext::Renderer<Font = iced_core::Font> + 'static>
    From<FilePane<'a, Theme, Renderer>> for Element<'a, Message, Theme, Renderer>
{
    fn from(list: FilePane<'a, Theme, Renderer>) -> Self {
        Element::new(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_renderer::LayoutRenderer;
    use std::cell::Cell;

    struct Listing<'a> {
        entries: &'a [(PathBuf, String)],
        reads: &'a Cell<usize>,
    }
    impl Source for Listing<'_> {
        fn root(&self) -> &Path {
            Path::new("/listing")
        }
        fn len(&self) -> usize {
            self.entries.len()
        }
        fn row(&self, index: usize) -> Option<Row<'_>> {
            self.reads.set(self.reads.get() + 1);
            self.entries.get(index).map(|(path, name)| Row {
                path,
                name,
                depth: 0,
                is_dir: false,
            })
        }
        fn size_text(&self, _index: usize) -> String {
            "1 KiB".into()
        }
        fn modified_text(&self, _index: usize) -> String {
            "01/01/26 12:00".into()
        }
    }
    fn columns() -> Columns {
        Columns {
            name_min: 80.0,
            size: 80.0,
            modified: 100.0,
            gap: 8.0,
            pad: 8.0,
        }
    }
    fn deliver(
        list: &mut FilePane<'_, crate::Theme, LayoutRenderer>,
        tree: &mut Tree,
        renderer: &LayoutRenderer,
        event: Event,
    ) -> Vec<Message> {
        let viewport = Rectangle::with_size(Size::new(600.0, 280.0));
        let node = list.layout(
            tree,
            renderer,
            &layout::Limits::new(Size::ZERO, viewport.size()),
        );
        let mut bus = iced_core::shell::Bus::new();
        list.update(
            tree,
            &event,
            Layout::new(&node),
            mouse::Cursor::Available(Point::new(80.0, 10.0)),
            renderer,
            &mut Shell::new(
                &iced_core::window::Headless,
                iced_core::shell::Waker::noop(),
                &mut bus,
            ),
            &viewport,
        );
        bus.drain().collect()
    }

    #[test]
    fn hundred_thousand_entries_access_and_shape_only_the_viewport() {
        let entries: Vec<_> = (0..100_000)
            .map(|i| {
                (
                    PathBuf::from(format!("/listing/{i}")),
                    format!("file-{i}.txt"),
                )
            })
            .collect();
        let reads = Cell::new(0);
        let mut list: FilePane<'_, crate::Theme, LayoutRenderer> = FilePane::new(
            Listing {
                entries: &entries,
                reads: &reads,
            },
            Presentation::default(),
            columns(),
        );
        let renderer = LayoutRenderer::new();
        let mut tree = Tree::new(&list as &dyn Widget<Message, crate::Theme, LayoutRenderer>);
        for _ in 0..20 {
            deliver(
                &mut list,
                &mut tree,
                &renderer,
                Event::Window(iced_core::window::Event::Focused),
            );
        }
        assert!(
            reads.get() < 1000,
            "accessed {} offscreen entries",
            reads.get()
        );
        let state = tree
            .state
            .downcast_ref::<RowState<<LayoutRenderer as atext::Renderer>::Paragraph>>();
        assert!(state.cache.len() > 1 && state.cache.len() < 20);
    }

    #[test]
    fn relisting_between_press_and_release_never_selects_the_replacement() {
        let first = [(PathBuf::from("/listing/first"), "first".into())];
        let replacement = [(PathBuf::from("/listing/other"), "other".into())];
        let reads = Cell::new(0);
        let make = |entries| {
            FilePane::<crate::Theme, LayoutRenderer>::new(
                Listing {
                    entries,
                    reads: &reads,
                },
                Presentation::default(),
                columns(),
            )
        };
        let mut list = make(&first);
        let renderer = LayoutRenderer::new();
        let mut tree = Tree::new(&list as &dyn Widget<Message, crate::Theme, LayoutRenderer>);
        assert_eq!(
            deliver(
                &mut list,
                &mut tree,
                &renderer,
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            ),
            vec![Message::Press]
        );
        let mut list = make(&replacement);
        assert!(
            deliver(
                &mut list,
                &mut tree,
                &renderer,
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            )
            .is_empty()
        );
    }
}
