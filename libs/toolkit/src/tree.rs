// SPDX-License-Identifier: MIT OR Apache-2.0
//! A tree view over the [`VirtualList`]: the caller keeps a [`Nodes`] model
//! (an arena of keyed nodes, expanded or collapsed, children loaded or
//! still to come), and [`TreeView`] shows the visible nodes as list rows
//! with indentation guides and an expander.
//!
//! Children are lazy: a node made with [`Children::Lazy`] shows an
//! expander, and expanding it is the caller's cue ([`Nodes::needs_children`])
//! to load them and call [`Nodes::set_children`]. Expand and collapse by
//! the expander, a double-click, Right and Left; Right on an expanded node
//! moves to its first child (a childless expanded node stays put), Left on
//! a collapsed one to its parent.
//! Selection and activation are the list's, by visible row index; map a row
//! back to its node with [`Nodes::visible`].
//!
//! The expander is the installed icon font's `chevron_right` and
//! `expand_more` glyphs when there is one, else a drawn plus or minus box.
//! Colours come from [`Catalog`] for the guides and expander, and the
//! list's catalog for everything else.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use iced_core::keyboard::{self, key::Named};
use iced_core::widget::{Tree, tree};
use iced_core::{
    Border, Color, Element, Event, Font, Layout, Length, Pixels, Point, Rectangle, Shell, Size,
    Widget, alignment, layout, mouse, renderer, text,
};

use crate::controls;
use crate::typography::{Glyph, TextStyle};
use crate::virtual_list::{self, KeyPress, Selection, VirtualList};

/// The explicit expander glyphs of a [`TreeView`]: one for the collapsed
/// state, one for the expanded state. A `None` glyph selects the geometric
/// fallback (a drawn plus or minus box) for that state, without consulting
/// the global icon font.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Expanders<F = Font> {
    collapsed: Option<Glyph<F>>,
    expanded: Option<Glyph<F>>,
}

impl<F> Expanders<F> {
    /// The collapsed and expanded glyphs. `None` for either state draws the
    /// geometric fallback there.
    pub fn new(collapsed: Option<Glyph<F>>, expanded: Option<Glyph<F>>) -> Self {
        Self {
            collapsed,
            expanded,
        }
    }
}

/// Whether a node has children, and whether they are loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Children {
    /// A leaf.
    #[default]
    None,
    /// Has children the caller will supply on expansion.
    Lazy,
    /// Children are in the model (possibly none, after loading).
    Loaded,
}

#[derive(Debug)]
struct Node<K, T> {
    key: K,
    data: T,
    parent: Option<usize>,
    children: Vec<usize>,
    depth: usize,
    expanded: bool,
    state: Children,
}

/// One visible row of the flattened tree.
#[derive(Debug, Clone, Copy)]
struct Entry {
    node: usize,
    /// Bit `d` set: the ancestor at depth `d` has a later sibling, so a
    /// guide line runs through this row at that depth.
    guides: u64,
    /// This node is its parent's last child.
    last: bool,
}

/// A visible node as [`Nodes::visible`] reports it.
#[derive(Debug, Clone, Copy)]
pub struct Row<'a, K, T> {
    pub key: &'a K,
    pub data: &'a T,
    pub depth: usize,
    pub expanded: bool,
    pub children: Children,
    /// The node is its parent's last child.
    pub last: bool,
    guides: u64,
}

impl<K, T> Row<'_, K, T> {
    pub fn has_children(&self) -> bool {
        self.children != Children::None
    }
}

/// The tree model: nodes by key, each expanded or collapsed. Mutations
/// invalidate the flattened view, which is rebuilt lazily on the next read.
#[derive(Debug)]
pub struct Nodes<K, T> {
    nodes: Vec<Node<K, T>>,
    roots: Vec<usize>,
    index: HashMap<K, usize>,
    visible: OnceCell<Vec<Entry>>,
}

impl<K, T> Default for Nodes<K, T>
where
    K: Hash + Eq + Clone,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K, T> Nodes<K, T>
where
    K: Hash + Eq + Clone,
{
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            roots: Vec::new(),
            index: HashMap::new(),
            visible: OnceCell::new(),
        }
    }

    /// The number of nodes, visible or not.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Adds a node under `parent` (a root when `None`), collapsed. Returns
    /// false if the key is taken or the parent unknown.
    pub fn push(&mut self, parent: Option<&K>, key: K, data: T, children: Children) -> bool {
        if self.index.contains_key(&key) {
            return false;
        }
        let parent_index = match parent {
            Some(parent) => match self.index.get(parent) {
                Some(index) => Some(*index),
                None => return false,
            },
            None => None,
        };
        let depth = parent_index.map_or(0, |parent| self.nodes[parent].depth + 1);
        let index = self.nodes.len();
        self.nodes.push(Node {
            key: key.clone(),
            data,
            parent: parent_index,
            children: Vec::new(),
            depth,
            expanded: false,
            state: children,
        });
        self.index.insert(key, index);
        match parent_index {
            Some(parent) => {
                self.nodes[parent].children.push(index);
                if self.nodes[parent].state == Children::None {
                    self.nodes[parent].state = Children::Loaded;
                }
            }
            None => self.roots.push(index),
        }
        self.visible.take();
        true
    }

    /// Replaces the children of `parent` with `items` and marks them
    /// loaded. False if the parent is unknown.
    pub fn set_children(&mut self, parent: &K, items: Vec<(K, T, Children)>) -> bool {
        let Some(&parent_index) = self.index.get(parent) else {
            return false;
        };
        for child in std::mem::take(&mut self.nodes[parent_index].children) {
            self.detach(child);
        }
        self.nodes[parent_index].state = Children::Loaded;
        let parent = parent.clone();
        for (key, data, children) in items {
            let _ = self.push(Some(&parent), key, data, children);
        }
        self.visible.take();
        true
    }

    /// Removes a node and its descendants. False if unknown.
    pub fn remove(&mut self, key: &K) -> bool {
        let Some(&index) = self.index.get(key) else {
            return false;
        };
        match self.nodes[index].parent {
            Some(parent) => self.nodes[parent].children.retain(|child| *child != index),
            None => self.roots.retain(|root| *root != index),
        }
        self.detach(index);
        self.visible.take();
        true
    }

    /// Forgets a subtree's keys; the arena slots stay (unreachable) until
    /// `clear`.
    fn detach(&mut self, index: usize) {
        let mut stack = vec![index];
        while let Some(index) = stack.pop() {
            self.index.remove(&self.nodes[index].key);
            stack.extend(std::mem::take(&mut self.nodes[index].children));
        }
    }

    pub fn clear(&mut self) {
        self.nodes.clear();
        self.roots.clear();
        self.index.clear();
        self.visible.take();
    }

    pub fn contains(&self, key: &K) -> bool {
        self.index.contains_key(key)
    }

    pub fn get(&self, key: &K) -> Option<&T> {
        self.index.get(key).map(|index| &self.nodes[*index].data)
    }

    pub fn get_mut(&mut self, key: &K) -> Option<&mut T> {
        self.index
            .get(key)
            .map(|index| &mut self.nodes[*index].data)
    }

    pub fn parent(&self, key: &K) -> Option<&K> {
        let node = &self.nodes[*self.index.get(key)?];
        node.parent.map(|parent| &self.nodes[parent].key)
    }

    /// The keys of `key`'s children, in order.
    pub fn children(&self, key: &K) -> impl Iterator<Item = &K> + '_ {
        self.index
            .get(key)
            .map(|index| self.nodes[*index].children.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|child| &self.nodes[*child].key)
    }

    pub fn children_state(&self, key: &K) -> Children {
        self.index
            .get(key)
            .map_or(Children::None, |index| self.nodes[*index].state)
    }

    pub fn is_expanded(&self, key: &K) -> bool {
        self.index
            .get(key)
            .is_some_and(|index| self.nodes[*index].expanded)
    }

    /// Expanded with children still to load.
    pub fn needs_children(&self, key: &K) -> bool {
        self.index.get(key).is_some_and(|index| {
            let node = &self.nodes[*index];
            node.expanded && node.state == Children::Lazy
        })
    }

    /// Sets a node's expansion. A leaf never expands. Returns the new state.
    pub fn set_expanded(&mut self, key: &K, expanded: bool) -> Option<bool> {
        let index = *self.index.get(key)?;
        let node = &mut self.nodes[index];
        if node.state == Children::None {
            return Some(false);
        }
        if node.expanded != expanded {
            node.expanded = expanded;
            self.visible.take();
        }
        Some(expanded)
    }

    /// Flips a node's expansion; returns the new state.
    pub fn toggle(&mut self, key: &K) -> Option<bool> {
        let expanded = self.is_expanded(key);
        self.set_expanded(key, !expanded)
    }

    /// Expands every ancestor of `key`, so it is visible.
    pub fn expand_to(&mut self, key: &K) {
        let mut current = self
            .index
            .get(key)
            .and_then(|index| self.nodes[*index].parent);
        while let Some(index) = current {
            if !self.nodes[index].expanded {
                self.nodes[index].expanded = true;
                self.visible.take();
            }
            current = self.nodes[index].parent;
        }
    }

    fn entries(&self) -> &[Entry] {
        self.visible.get_or_init(|| {
            let mut out = Vec::new();
            // Depth-first, children in order; a stack of (node, guides, last).
            let mut stack: Vec<Entry> = Vec::new();
            for (position, root) in self.roots.iter().enumerate().rev() {
                stack.push(Entry {
                    node: *root,
                    guides: 0,
                    last: position + 1 == self.roots.len(),
                });
            }
            while let Some(entry) = stack.pop() {
                out.push(entry);
                let node = &self.nodes[entry.node];
                if !node.expanded {
                    continue;
                }
                let mut guides = entry.guides;
                if !entry.last && node.depth < 64 {
                    guides |= 1 << node.depth;
                }
                for (position, child) in node.children.iter().enumerate().rev() {
                    stack.push(Entry {
                        node: *child,
                        guides,
                        last: position + 1 == node.children.len(),
                    });
                }
            }
            out
        })
    }

    /// The number of visible rows.
    pub fn visible_len(&self) -> usize {
        self.entries().len()
    }

    /// The node at visible row `row`.
    pub fn visible(&self, row: usize) -> Option<Row<'_, K, T>> {
        let entry = self.entries().get(row)?;
        let node = &self.nodes[entry.node];
        Some(Row {
            key: &node.key,
            data: &node.data,
            depth: node.depth,
            expanded: node.expanded,
            children: node.state,
            last: entry.last,
            guides: entry.guides,
        })
    }

    /// The visible row of `key`, if it is visible.
    pub fn position(&self, key: &K) -> Option<usize> {
        let index = *self.index.get(key)?;
        self.entries().iter().position(|entry| entry.node == index)
    }

    /// Every visible row, in order.
    pub fn visible_rows(&self) -> impl Iterator<Item = Row<'_, K, T>> + '_ {
        (0..self.visible_len()).filter_map(|row| self.visible(row))
    }
}

/// A row key for the list that is stable across expansion.
fn row_key<K: Hash>(key: &K) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

/// The row builder: the content after a visible node's guides and expander.
pub type BuildFn<'a, K, T, Message, Theme, Renderer> =
    dyn Fn(Row<'a, K, T>) -> Element<'a, Message, Theme, Renderer> + 'a;

/// The tree widget builder; `.into()` makes the [`VirtualList`] element.
pub struct TreeView<'a, K, T, Message, Theme, Renderer>
where
    Theme: virtual_list::Catalog,
{
    nodes: &'a Nodes<K, T>,
    build: Rc<BuildFn<'a, K, T, Message, Theme, Renderer>>,
    on_toggle: Option<Rc<dyn Fn(K) -> Message + 'a>>,
    on_select: Option<Rc<dyn Fn(Selection) -> Message + 'a>>,
    selection: Selection,
    list: VirtualList<'a, Message, Theme, Renderer>,
    indent: f32,
    expanders: Option<Expanders<Font>>,
}

impl<'a, K, T, Message, Theme, Renderer> TreeView<'a, K, T, Message, Theme, Renderer>
where
    K: Hash + Eq + Clone + 'a,
    T: 'a,
    Message: 'a,
    Theme: virtual_list::Catalog + Catalog + 'a,
    Renderer: text::Renderer<Font = Font> + 'a,
{
    /// A tree over `nodes`; `build(row)` makes the content of one visible
    /// row (after the guides and expander).
    pub fn new<E>(nodes: &'a Nodes<K, T>, build: impl Fn(Row<'a, K, T>) -> E + 'a) -> Self
    where
        E: Into<Element<'a, Message, Theme, Renderer>>,
    {
        let list = VirtualList::new(nodes.visible_len(), |_| {
            Element::new(iced_widget::Space::new())
        });
        Self {
            nodes,
            build: Rc::new(move |row| build(row).into()),
            on_toggle: None,
            on_select: None,
            selection: Selection::default(),
            list,
            indent: 20.0,
            expanders: None,
        }
    }

    /// Published with a node's key when its expander, a double-click,
    /// Right or Left asks to flip it; the caller calls `Nodes::toggle`.
    pub fn on_toggle(mut self, on_toggle: impl Fn(K) -> Message + 'a) -> Self {
        self.on_toggle = Some(Rc::new(on_toggle));
        self
    }

    pub fn on_select(mut self, on_select: impl Fn(Selection) -> Message + 'a) -> Self {
        self.on_select = Some(Rc::new(on_select));
        self
    }

    pub fn selection(mut self, selection: &Selection) -> Self {
        self.selection = selection.clone();
        self
    }

    /// Pixels per depth level (default 20).
    pub fn indent(mut self, indent: f32) -> Self {
        self.indent = indent.max(4.0);
        self
    }

    /// Derives the prepared per-depth indent from the shared control
    /// metrics: row height and [`controls::Metrics::indent`] of the caller's row `text`
    /// style — at least `lg + sm` and at least the row height, so the
    /// expander square and a guide always fit. The expander square follows
    /// the indent (0.6 × indent, clamped 8–16). Applied after
    /// [`TreeView::indent`] it wins; without either builder the legacy
    /// 20-pixel default is unchanged.
    pub fn metrics(mut self, metrics: controls::Metrics, text: TextStyle) -> Self {
        self.indent = metrics.indent(&text);
        self.list = self.list.row_height(metrics.row_height(&text));
        self
    }

    /// Explicit expander glyphs. Without a supplied value the installed
    /// icon font is looked up as before; with one, a `None` glyph draws
    /// the geometric fallback for that state instead of consulting the
    /// global icon font.
    pub fn expanders(mut self, expanders: Expanders<Font>) -> Self {
        self.expanders = Some(expanders);
        self
    }

    /// Settings of the underlying list (`id`, `row_height`, `mode`,
    /// `on_activate`, `on_context`, `type_ahead`, `reveal`, `style`...).
    pub fn list(
        mut self,
        configure: impl FnOnce(
            VirtualList<'a, Message, Theme, Renderer>,
        ) -> VirtualList<'a, Message, Theme, Renderer>,
    ) -> Self {
        self.list = configure(self.list);
        self
    }

    fn into_list(self) -> VirtualList<'a, Message, Theme, Renderer> {
        let nodes = self.nodes;
        let build = self.build;
        let on_toggle = self.on_toggle.clone();
        let indent = self.indent;
        let expanders = self.expanders;
        let selection = self.selection.clone();
        let on_select = self.on_select.clone();
        let toggle_keys = self.on_toggle.clone();
        let mut list = self
            .list
            .key(move |row| {
                nodes
                    .visible(row)
                    .map_or(row as u64, |row| row_key(row.key))
            })
            .selection(&selection)
            .on_key(move |press| {
                let cursor = press.cursor?;
                let row = nodes.visible(cursor)?;
                let KeyPress { key, .. } = &press;
                match key {
                    keyboard::Key::Named(Named::ArrowRight) => {
                        if row.has_children() && !row.expanded {
                            toggle_keys.as_ref().map(|toggle| toggle(row.key.clone()))
                        } else if row.expanded
                            // The next row must actually be a child: an
                            // expanded node with zero loaded children is
                            // followed by its sibling, which Right must not
                            // jump onto.
                            && nodes
                                .visible(cursor + 1)
                                .is_some_and(|next| nodes.parent(next.key) == Some(row.key))
                        {
                            on_select
                                .as_ref()
                                .map(|select| select(Selection::single(cursor + 1)))
                        } else {
                            None
                        }
                    }
                    keyboard::Key::Named(Named::ArrowLeft) => {
                        if row.expanded {
                            toggle_keys.as_ref().map(|toggle| toggle(row.key.clone()))
                        } else {
                            let parent = nodes.parent(row.key)?;
                            let position = nodes.position(parent)?;
                            on_select
                                .as_ref()
                                .map(|select| select(Selection::single(position)))
                        }
                    }
                    _ => None,
                }
            });
        if let Some(on_select) = &self.on_select {
            let on_select = on_select.clone();
            list = list.on_select(move |selection| on_select(selection));
        }
        list.with_rows(nodes.visible_len(), move |index| {
            let Some(row) = nodes.visible(index) else {
                return Element::new(iced_widget::Space::new());
            };
            let toggle = on_toggle.clone();
            let key = row.key.clone();
            let guides = Guides {
                depth: row.depth,
                guides: row.guides,
                last: row.last,
                expanded: row.expanded,
                children: row.children,
                indent,
                expanders,
                on_toggle: toggle.map(|toggle| {
                    Box::new(move || toggle(key.clone())) as Box<dyn Fn() -> Message + 'a>
                }),
            };
            iced_widget::Row::with_children([Element::new(guides), build(row)])
                .height(Length::Fill)
                .align_y(alignment::Vertical::Center)
                .into()
        })
    }
}

impl<'a, K, T, Message, Theme, Renderer> From<TreeView<'a, K, T, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    K: Hash + Eq + Clone + 'a,
    T: 'a,
    Message: 'a,
    Theme: virtual_list::Catalog + Catalog + 'a,
    Renderer: text::Renderer<Font = Font> + 'a,
{
    fn from(tree: TreeView<'a, K, T, Message, Theme, Renderer>) -> Self {
        tree.into_list().into()
    }
}

/// The guide lines and expander at the start of a tree row.
struct Guides<'a, Message> {
    depth: usize,
    guides: u64,
    last: bool,
    expanded: bool,
    children: Children,
    indent: f32,
    expanders: Option<Expanders<Font>>,
    on_toggle: Option<Box<dyn Fn() -> Message + 'a>>,
}

impl<Message> Guides<'_, Message> {
    fn width(&self) -> f32 {
        (self.depth as f32 + 1.0) * self.indent
    }

    /// The expander's square, centred in the last indent slot.
    fn expander(&self, bounds: Rectangle) -> Rectangle {
        let size = (self.indent * 0.6).clamp(8.0, 16.0);
        Rectangle {
            x: bounds.x + self.depth as f32 * self.indent + (self.indent - size) / 2.0,
            y: bounds.y + (bounds.height - size) / 2.0,
            width: size,
            height: size,
        }
    }

    /// The clickable expander rectangle: with prepared glyphs the exact
    /// allocated rectangle (the same one the glyph draws into); the legacy
    /// path keeps its padded hit region around it.
    fn hit_region(&self, bounds: Rectangle) -> Rectangle {
        let expander = self.expander(bounds);
        if self.expanders.is_some() {
            expander
        } else {
            expander.expand(4.0)
        }
    }
}

/// The geometric expander fallback: a plus or minus in a box, with no
/// global font lookup.
fn draw_box<Renderer: text::Renderer>(
    renderer: &mut Renderer,
    style: &Style,
    expander: Rectangle,
    expanded: bool,
) {
    renderer.fill_quad(
        renderer::Quad {
            bounds: expander,
            border: Border {
                color: style.expander,
                width: 1.0,
                radius: 2.0.into(),
            },
            ..renderer::Quad::default()
        },
        style.expander_fill,
    );
    let inset = 3.0;
    let mark = |renderer: &mut Renderer, rect: Rectangle| {
        renderer.fill_quad(
            renderer::Quad {
                bounds: rect,
                ..renderer::Quad::default()
            },
            style.expander,
        );
    };
    mark(
        renderer,
        Rectangle {
            x: expander.x + inset,
            y: (expander.center_y() - 0.5).round(),
            width: expander.width - 2.0 * inset,
            height: 1.0,
        },
    );
    if !expanded {
        mark(
            renderer,
            Rectangle {
                x: (expander.center_x() - 0.5).round(),
                y: expander.y + inset,
                width: 1.0,
                height: expander.height - 2.0 * inset,
            },
        );
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Guides<'_, Message>
where
    Theme: Catalog,
    Renderer: text::Renderer<Font = Font>,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::stateless()
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.width()), Length::Fill)
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::atomic(limits, self.width(), Length::Fill)
    }

    fn update(
        &mut self,
        _tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event
            && let Some(on_toggle) = &self.on_toggle
            && self.children != Children::None
            && !shell.is_event_captured()
            && cursor.is_over(self.hit_region(layout.bounds()))
        {
            shell.publish(on_toggle());
            shell.capture_event();
        }
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let style = theme.style();
        let line = |renderer: &mut Renderer, rect: Rectangle| {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: rect,
                    ..renderer::Quad::default()
                },
                style.guide,
            );
        };
        // A guide through each ancestor depth that has a later sibling.
        for depth in 0..self.depth.min(64) {
            if self.guides & (1 << depth) != 0 {
                let x = bounds.x + depth as f32 * self.indent + self.indent / 2.0;
                line(
                    renderer,
                    Rectangle {
                        x: x.round(),
                        y: bounds.y,
                        width: 1.0,
                        height: bounds.height,
                    },
                );
            }
        }
        // This row's own elbow: down from the top, and (unless last) on
        // through the bottom, then across to the expander or content.
        if self.depth > 0 {
            let x =
                (bounds.x + (self.depth as f32 - 1.0) * self.indent + self.indent / 2.0).round();
            let mid = (bounds.y + bounds.height / 2.0).round();
            line(
                renderer,
                Rectangle {
                    x,
                    y: bounds.y,
                    width: 1.0,
                    height: if self.last {
                        mid - bounds.y
                    } else {
                        bounds.height
                    },
                },
            );
            line(
                renderer,
                Rectangle {
                    x,
                    y: mid,
                    width: self.indent / 2.0
                        + if self.children == Children::None {
                            self.indent / 2.0
                        } else {
                            0.0
                        },
                    height: 1.0,
                },
            );
        }
        if self.children == Children::None {
            return;
        }
        let expander = self.expander(bounds);
        match self.expanders {
            // No supplied value: the installed icon font's chevron, with
            // its historical oversized geometry, else the drawn box.
            None => {
                let name = if self.expanded {
                    "expand_more"
                } else {
                    "chevron_right"
                };
                if let Some((glyph, font)) = crate::fonts::icon(name) {
                    renderer.fill_text(
                        text::Text {
                            content: glyph.to_string(),
                            bounds: Size::new(expander.width * 1.5, expander.height * 1.5),
                            size: Pixels(expander.height * 1.4),
                            line_height: text::LineHeight::Relative(1.0),
                            font,
                            align_x: text::Alignment::Center,
                            align_y: alignment::Vertical::Center,
                            shaping: text::Shaping::Advanced,
                            wrapping: text::Wrapping::None,
                            ellipsis: text::Ellipsis::None,
                            hint_factor: None,
                        },
                        Point::new(expander.center_x(), expander.center_y()),
                        style.expander,
                        bounds,
                    );
                    return;
                }
                draw_box(renderer, &style, expander, self.expanded);
            }
            // Supplied: an explicit glyph draws at its own resolved style
            // and must fit the allocated expander rectangle; an explicit
            // None is the geometric fallback with no global font escape.
            Some(expanders) => {
                let glyph = if self.expanded {
                    expanders.expanded
                } else {
                    expanders.collapsed
                };
                if let Some(glyph) = glyph {
                    renderer.fill_text(
                        text::Text {
                            content: glyph.character.to_string(),
                            bounds: expander.size(),
                            size: glyph.text.size.into(),
                            line_height: glyph.text.line_height_or_default(),
                            font: glyph.text.font,
                            align_x: text::Alignment::Center,
                            align_y: alignment::Vertical::Center,
                            shaping: text::Shaping::Advanced,
                            wrapping: text::Wrapping::None,
                            ellipsis: text::Ellipsis::None,
                            hint_factor: None,
                        },
                        Point::new(expander.center_x(), expander.center_y()),
                        style.expander,
                        expander,
                    );
                } else {
                    draw_box(renderer, &style, expander, self.expanded);
                }
            }
        }
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if self.on_toggle.is_some()
            && self.children != Children::None
            && cursor.is_over(self.hit_region(layout.bounds()))
        {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::None
        }
    }
}

/// The guides' and expander's appearance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// The indentation guide lines.
    pub guide: Color,
    /// The expander glyph, or the box outline and its mark.
    pub expander: Color,
    /// The drawn box's fill (when there is no icon font).
    pub expander_fill: Color,
}

/// The theme catalog of the tree's guides and expander.
pub trait Catalog {
    fn style(&self) -> Style;
}

impl Catalog for crate::Theme {
    fn style(&self) -> Style {
        let p = self.palette();
        Style {
            guide: p.border,
            expander: p.muted_text,
            expander_fill: p.muted_surface,
        }
    }
}

impl Catalog for iced_core::Theme {
    fn style(&self) -> Style {
        let p = self.palette();
        Style {
            guide: p.background.strong.color,
            expander: p.secondary.base.color,
            expander_fill: p.background.weak.color,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Nodes<&'static str, u32> {
        let mut nodes = Nodes::new();
        assert!(nodes.push(None, "a", 1, Children::Loaded));
        assert!(nodes.push(Some(&"a"), "a1", 2, Children::None));
        assert!(nodes.push(Some(&"a"), "a2", 3, Children::Lazy));
        assert!(nodes.push(None, "b", 4, Children::None));
        nodes
    }

    #[test]
    fn collapsed_tree_shows_roots_only() {
        let nodes = sample();
        assert_eq!(nodes.visible_len(), 2);
        assert_eq!(nodes.visible(0).unwrap().key, &"a");
        assert_eq!(nodes.visible(1).unwrap().key, &"b");
        assert!(nodes.visible(0).unwrap().has_children());
        assert!(!nodes.visible(1).unwrap().has_children());
    }

    #[test]
    fn expanding_flattens_children_in_order_with_guides() {
        let mut nodes = sample();
        assert_eq!(nodes.toggle(&"a"), Some(true));
        let keys: Vec<_> = nodes.visible_rows().map(|row| *row.key).collect();
        assert_eq!(keys, ["a", "a1", "a2", "b"]);
        let a1 = nodes.visible(1).unwrap();
        assert_eq!(a1.depth, 1);
        assert!(!a1.last);
        // "a" has a later sibling ("b"), so a guide runs through its children.
        assert_eq!(a1.guides, 1);
        assert!(nodes.visible(2).unwrap().last);
        assert_eq!(nodes.position(&"b"), Some(3));
        assert_eq!(nodes.parent(&"a2"), Some(&"a"));
        assert_eq!(nodes.toggle(&"a"), Some(false));
        assert_eq!(nodes.visible_len(), 2);
        // A leaf never expands.
        assert_eq!(nodes.toggle(&"b"), Some(false));
        assert_eq!(nodes.toggle(&"zz"), None);
    }

    #[test]
    fn lazy_children_arrive_through_set_children() {
        let mut nodes = sample();
        nodes.set_expanded(&"a", true);
        nodes.set_expanded(&"a2", true);
        assert!(nodes.needs_children(&"a2"));
        assert_eq!(nodes.visible_len(), 4);
        assert!(nodes.set_children(
            &"a2",
            vec![("x", 5, Children::None), ("y", 6, Children::None)]
        ));
        assert!(!nodes.needs_children(&"a2"));
        assert_eq!(nodes.children_state(&"a2"), Children::Loaded);
        let keys: Vec<_> = nodes.visible_rows().map(|row| *row.key).collect();
        assert_eq!(keys, ["a", "a1", "a2", "x", "y", "b"]);
        assert_eq!(nodes.visible(3).unwrap().depth, 2);
        assert_eq!(
            nodes.children(&"a2").copied().collect::<Vec<_>>(),
            ["x", "y"]
        );
        // Replacing children drops the old keys.
        assert!(nodes.set_children(&"a2", vec![("z", 7, Children::None)]));
        assert!(!nodes.contains(&"x"));
        assert!(nodes.contains(&"z"));
        assert!(!nodes.set_children(&"missing", Vec::new()));
    }

    #[test]
    fn remove_and_expand_to() {
        let mut nodes = sample();
        nodes.set_children(&"a2", vec![("deep", 9, Children::None)]);
        nodes.expand_to(&"deep");
        assert_eq!(nodes.position(&"deep"), Some(3));
        assert!(nodes.remove(&"a2"));
        assert!(!nodes.contains(&"deep"));
        let keys: Vec<_> = nodes.visible_rows().map(|row| *row.key).collect();
        assert_eq!(keys, ["a", "a1", "b"]);
        assert!(!nodes.remove(&"a2"));
        assert_eq!(nodes.get(&"a1"), Some(&2));
        *nodes.get_mut(&"a1").unwrap() = 20;
        assert_eq!(nodes.get(&"a1"), Some(&20));
        nodes.clear();
        assert!(nodes.is_empty());
        assert_eq!(nodes.visible_len(), 0);
    }

    #[test]
    fn row_keys_are_stable_across_expansion() {
        let mut nodes = sample();
        let before = row_key(nodes.visible(1).unwrap().key);
        nodes.set_expanded(&"a", true);
        assert_eq!(row_key(nodes.visible(3).unwrap().key), before);
    }

    mod prepared {
        use iced_core::shell::{Bus, Waker};
        use iced_core::text::Paragraph as _;
        use iced_core::window::Headless;

        use super::*;
        use crate::tokens::Tokens;
        use crate::typography::TextStyle;

        #[derive(Debug, Clone, PartialEq)]
        enum Msg {
            Toggle(&'static str),
            Select(Selection),
        }

        const VIEW: Size = Size::new(300.0, 240.0);

        fn nodes() -> Nodes<&'static str, &'static str> {
            let mut nodes = Nodes::new();
            assert!(nodes.push(None, "a", "root", Children::Loaded));
            assert!(nodes.push(Some(&"a"), "a1", "leaf", Children::None));
            nodes
        }

        fn expanders() -> Expanders {
            Expanders::new(
                Some(Glyph {
                    character: '▶',
                    text: TextStyle {
                        font: Font::MONOSPACE,
                        size: 12.0,
                        line_height: Some(16.0),
                    },
                }),
                Some(Glyph {
                    character: '▼',
                    text: TextStyle {
                        font: Font::DEFAULT,
                        size: 10.0,
                        line_height: None,
                    },
                }),
            )
        }

        /// Records every drawn text and paragraph with its resolved font,
        /// size and line height, so caller rows and expanders are told apart.
        #[derive(Default)]
        struct Recorder {
            texts: Vec<(String, Font, Pixels, text::LineHeight)>,
            paragraphs: Vec<(Font, Pixels, text::LineHeight)>,
            quads: Vec<Rectangle>,
        }

        impl renderer::Renderer for Recorder {
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
            fn fill_quad(&mut self, quad: renderer::Quad, _: impl Into<iced_core::Background>) {
                self.quads.push(quad.bounds);
            }
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

        impl text::Renderer for Recorder {
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

            fn fill_paragraph(
                &mut self,
                paragraph: &Self::Paragraph,
                _: Point,
                _: Color,
                _: Rectangle,
            ) {
                self.paragraphs
                    .push((paragraph.font(), paragraph.size(), paragraph.line_height()));
            }

            fn fill_editor(&mut self, _: &Self::Editor, _: Point, _: Color, _: Rectangle) {}

            fn fill_text(&mut self, text: text::Text, _: Point, _: Color, _: Rectangle) {
                self.texts
                    .push((text.content, text.font, text.size, text.line_height));
            }
        }

        fn draw<'a>(
            element: &mut Element<'a, Msg, iced_core::Theme, Recorder>,
            tree: &mut Tree,
        ) -> Recorder {
            let mut renderer = Recorder::default();
            element.as_widget_mut().diff(tree);
            let node = element.as_widget_mut().layout(
                tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, VIEW),
            );
            element.as_widget().draw(
                tree,
                &mut renderer,
                &iced_core::Theme::Dark,
                &renderer::Style::default(),
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &Rectangle::with_size(VIEW),
            );
            renderer
        }

        #[test]
        fn explicit_expanders_draw_their_own_resolved_styles() {
            // Bind the test monospace family before real paragraphs are
            // shaped with Font::MONOSPACE.
            crate::test_renderer::LayoutRenderer::new();
            let mut nodes = nodes();
            nodes.set_expanded(&"a", true);
            let mut element: Element<'_, Msg, iced_core::Theme, Recorder> =
                TreeView::new(&nodes, |row| {
                    iced_widget::text(*row.data)
                        .font(Font::MONOSPACE)
                        .size(13.0)
                        .line_height(text::LineHeight::Absolute(Pixels(20.0)))
                })
                .expanders(expanders())
                .on_toggle(Msg::Toggle)
                .on_select(Msg::Select)
                .into();
            let mut tree = Tree::new(element.as_widget());
            let recorder = draw(&mut element, &mut tree);
            // The expanded root draws the ▼ glyph at its own resolved style.
            assert!(
                recorder.texts.iter().any(|(content, font, size, _)| {
                    content == "▼" && *font == Font::DEFAULT && size.0 == 10.0
                }),
                "expanded glyph style missing: {:#?}",
                recorder.texts
            );
            // Caller rows draw through their own supplied style, separately.
            assert!(
                recorder
                    .paragraphs
                    .iter()
                    .any(|(font, size, line)| *font == Font::MONOSPACE
                        && size.0 == 13.0
                        && *line == text::LineHeight::Absolute(Pixels(20.0))),
                "caller row style missing: {:#?}",
                recorder.paragraphs
            );
        }

        #[test]
        fn the_collapsed_glyph_and_the_geometric_fallback_draw_without_global_escape() {
            crate::test_renderer::LayoutRenderer::new();
            let nodes = nodes();
            // Collapsed root: the ▶ glyph at its own style.
            let mut element: Element<'_, Msg, iced_core::Theme, Recorder> =
                TreeView::new(&nodes, |_| Element::new(iced_widget::Space::new()))
                    .expanders(expanders())
                    .into();
            let mut tree = Tree::new(element.as_widget());
            let recorder = draw(&mut element, &mut tree);
            assert!(
                recorder
                    .texts
                    .iter()
                    .any(|(content, font, size, line)| content == "▶"
                        && *font == Font::MONOSPACE
                        && size.0 == 12.0
                        && *line == text::LineHeight::Absolute(Pixels(16.0))),
                "collapsed glyph style missing: {:#?}",
                recorder.texts
            );
            // An explicit None glyph for each state draws the geometric box
            // and records no glyph text at all.
            let mut element: Element<'_, Msg, iced_core::Theme, Recorder> =
                TreeView::new(&nodes, |_| Element::new(iced_widget::Space::new()))
                    .expanders(Expanders::new(None, None))
                    .into();
            let mut tree = Tree::new(element.as_widget());
            let recorder = draw(&mut element, &mut tree);
            assert!(
                recorder.texts.is_empty(),
                "the geometric fallback must not draw glyph text: {:#?}",
                recorder.texts
            );
            // The box outline itself is drawn: a small quad inside the
            // first row's expander slot.
            assert!(
                recorder
                    .quads
                    .iter()
                    .any(|rect| rect.height == 12.0 && rect.width == 12.0),
                "the fallback box quad is missing: {:#?}",
                recorder.quads
            );
        }

        #[test]
        fn prepared_expander_clicks_are_confined_to_the_expander_rectangle() {
            let mut nodes = nodes();
            nodes.set_expanded(&"a", true);
            let mut element: Element<
                '_,
                Msg,
                iced_core::Theme,
                crate::test_renderer::LayoutRenderer,
            > = TreeView::new(&nodes, |_| Element::new(iced_widget::Space::new()))
                .expanders(expanders())
                .on_toggle(Msg::Toggle)
                .on_select(Msg::Select)
                .into();
            let mut tree = Tree::new(element.as_widget());
            element.as_widget_mut().diff(&mut tree);
            let renderer = crate::test_renderer::LayoutRenderer::new();
            let node = element.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, VIEW),
            );
            let mut send = |event: Event, at: Point| {
                let mut bus = Bus::new();
                let mut shell = Shell::new(&Headless, Waker::noop(), &mut bus);
                element.as_widget_mut().update(
                    &mut tree,
                    &event,
                    Layout::new(&node),
                    mouse::Cursor::Available(at),
                    &renderer,
                    &mut shell,
                    &Rectangle::with_size(VIEW),
                );
                bus.drain().collect::<Vec<_>>()
            };
            let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
            // The expander rectangle of the first row (indent 20, box 12:
            // x 4..16, y 8..20). A click inside toggles and selects nothing.
            assert_eq!(
                send(press.clone(), Point::new(10.0, 14.0)),
                [Msg::Toggle("a")]
            );
            // The prepared hit region does not pad the rectangle: a click
            // beside it (still in the guides area) selects the row instead.
            assert_eq!(
                send(press.clone(), Point::new(1.0, 14.0)),
                [Msg::Select(Selection::single(0))]
            );
            // A neighbouring row's content selects that row only.
            assert_eq!(
                send(press.clone(), Point::new(150.0, 42.0)),
                [Msg::Select(Selection::single(1))]
            );
        }

        #[test]
        fn prepared_density_and_text_change_actual_row_clicks() {
            let mut nodes = nodes();
            nodes.set_expanded(&"a", true);
            let renderer = crate::test_renderer::LayoutRenderer::new();
            let mut retained = None;
            for (density, size, expected_row) in [(1.0, 20.0, 1), (2.0, 30.0, 0)] {
                let mut tokens = Tokens::default();
                tokens.metrics.spacing.xs *= density;
                tokens.metrics.spacing.sm *= density;
                tokens.metrics.spacing.lg *= density;
                let metrics = controls::Metrics::from_tokens(tokens);
                let text = TextStyle {
                    font: Font::DEFAULT,
                    size,
                    line_height: None,
                };
                let mut element: Element<
                    '_,
                    Msg,
                    iced_core::Theme,
                    crate::test_renderer::LayoutRenderer,
                > = TreeView::new(&nodes, |_| Element::new(iced_widget::Space::new()))
                    .metrics(metrics, text)
                    .expanders(Expanders::new(None, None))
                    .on_select(Msg::Select)
                    .into();
                let tree = retained.get_or_insert_with(|| Tree::new(element.as_widget()));
                element.as_widget_mut().diff(tree);
                let node = element.as_widget_mut().layout(
                    tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, VIEW),
                );
                let mut bus = Bus::new();
                let mut shell = Shell::new(&Headless, Waker::noop(), &mut bus);
                element.as_widget_mut().update(
                    tree,
                    &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    Layout::new(&node),
                    mouse::Cursor::Available(Point::new(150.0, 35.0)),
                    &renderer,
                    &mut shell,
                    &Rectangle::with_size(VIEW),
                );
                assert_eq!(
                    bus.drain().collect::<Vec<_>>(),
                    [Msg::Select(Selection::single(expected_row))]
                );
                assert!(nodes.is_expanded(&"a"));
                assert_eq!(nodes.visible_len(), 2);
            }
        }

        #[test]
        fn prepared_metrics_indent_grows_the_expander_slot() {
            // A row text style whose line box exceeds the default indent
            // floor: 20 * 1.3 + 4 = 30, so the indent derives from the row
            // height rather than the lg + sm floor (20).
            let text = TextStyle {
                font: Font::DEFAULT,
                size: 20.0,
                line_height: None,
            };
            let metrics = controls::Metrics::from_tokens(Tokens::default());
            assert_eq!(metrics.indent(&text), 30.0);
            let mut nodes = nodes();
            nodes.set_expanded(&"a", true);
            let mut element: Element<
                '_,
                Msg,
                iced_core::Theme,
                crate::test_renderer::LayoutRenderer,
            > = TreeView::new(&nodes, |_| Element::new(iced_widget::Space::new()))
                .metrics(metrics, text)
                .expanders(expanders())
                .on_toggle(Msg::Toggle)
                .on_select(Msg::Select)
                .into();
            let mut tree = Tree::new(element.as_widget());
            element.as_widget_mut().diff(&mut tree);
            let renderer = crate::test_renderer::LayoutRenderer::new();
            let node = element.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, VIEW),
            );
            let mut send = |event: Event, at: Point| {
                let mut bus = Bus::new();
                let mut shell = Shell::new(&Headless, Waker::noop(), &mut bus);
                element.as_widget_mut().update(
                    &mut tree,
                    &event,
                    Layout::new(&node),
                    mouse::Cursor::Available(at),
                    &renderer,
                    &mut shell,
                    &Rectangle::with_size(VIEW),
                );
                bus.drain().collect::<Vec<_>>()
            };
            let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
            // The expander rectangle of the first row: indent 30, box 16
            // (0.6 × 30 clamped), x 7..23, y 6..22. A click inside toggles.
            assert_eq!(
                send(press.clone(), Point::new(15.0, 14.0)),
                [Msg::Toggle("a")]
            );
            // A click at x 4 is inside the legacy indent-20 rectangle
            // (x 4..16) but outside the metrics-derived one, so it selects
            // the row instead of toggling.
            assert_eq!(
                send(press.clone(), Point::new(4.0, 14.0)),
                [Msg::Select(Selection::single(0))]
            );
        }

        #[test]
        fn expanded_and_selected_keys_survive_a_retained_rebuild() {
            let mut nodes = nodes();
            nodes.set_expanded(&"a", true);
            let selection = Selection::single(1);
            let mut element: Element<
                '_,
                Msg,
                iced_core::Theme,
                crate::test_renderer::LayoutRenderer,
            > = TreeView::new(&nodes, |_| Element::new(iced_widget::Space::new()))
                .expanders(expanders())
                .on_toggle(Msg::Toggle)
                .on_select(Msg::Select)
                .selection(&selection)
                .into();
            let mut tree = Tree::new(element.as_widget());
            element.as_widget_mut().diff(&mut tree);
            let renderer = crate::test_renderer::LayoutRenderer::new();
            let initial_node = element.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, VIEW),
            );
            // Expand another node through the model and rebuild the view in
            // the retained tree: the previous selection target and expander
            // keys still resolve.
            assert!(initial_node.size().height > 0.0);
            drop(element);
            nodes.set_expanded(&"a", false);
            let mut element: Element<'_, Msg, iced_core::Theme, crate::test_renderer::LayoutRenderer> = TreeView::new(&nodes, |_| Element::new(iced_widget::Space::new()))
                .expanders(expanders())
                .on_toggle(Msg::Toggle)
                .on_select(Msg::Select)
                .selection(&selection)
                .into();
            element.as_widget_mut().diff(&mut tree);
            let node = element.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, VIEW),
            );
            let mut bus = Bus::new();
            let mut shell = Shell::new(&Headless, Waker::noop(), &mut bus);
            element.as_widget_mut().update(
                &mut tree,
                &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Layout::new(&node),
                mouse::Cursor::Available(Point::new(10.0, 14.0)),
                &renderer,
                &mut shell,
                &Rectangle::with_size(VIEW),
            );
            assert_eq!(bus.drain().collect::<Vec<_>>(), [Msg::Toggle("a")]);
        }
    }

    mod keys {
        use iced_core::shell::{Bus, Waker};
        use iced_core::window::Headless;

        use super::*;
        use crate::test_renderer::LayoutRenderer;

        #[derive(Debug, Clone, PartialEq)]
        enum Msg {
            Toggle(&'static str),
            Select(Selection),
        }

        type El<'a> = Element<'a, Msg, iced_core::Theme, LayoutRenderer>;

        const VIEW: Size = Size::new(300.0, 240.0);

        fn view<'a>(nodes: &'a Nodes<&'static str, u32>, selection: &Selection) -> El<'a> {
            TreeView::new(nodes, |_row| Element::new(iced_widget::Space::new()))
                .on_toggle(Msg::Toggle)
                .on_select(Msg::Select)
                .selection(selection)
                .into()
        }

        fn layout(element: &mut El<'_>, tree: &mut Tree) -> layout::Node {
            element.as_widget_mut().diff(tree);
            element.as_widget_mut().layout(
                tree,
                &LayoutRenderer::new(),
                &layout::Limits::new(Size::ZERO, VIEW),
            )
        }

        fn send(
            element: &mut El<'_>,
            tree: &mut Tree,
            node: &layout::Node,
            event: Event,
            cursor: mouse::Cursor,
        ) -> Vec<Msg> {
            let mut bus = Bus::new();
            let mut shell = Shell::new(&Headless, Waker::noop(), &mut bus);
            element.as_widget_mut().update(
                tree,
                &event,
                Layout::new(node),
                cursor,
                &LayoutRenderer::new(),
                &mut shell,
                &Rectangle::with_size(Size::INFINITE),
            );
            bus.drain().collect()
        }

        fn arrow(name: Named) -> Event {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(name),
                modified_key: keyboard::Key::Named(name),
                physical_key: keyboard::key::Physical::Unidentified(
                    keyboard::key::NativeCode::Unidentified,
                ),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
                text: None,
                repeat: false,
            })
        }

        fn click_row(row: usize) -> mouse::Cursor {
            mouse::Cursor::Available(Point::new(150.0, row as f32 * 28.0 + 14.0))
        }

        fn press() -> Event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
        }

        #[test]
        fn right_expands_then_enters_and_left_collapses_then_leaves() {
            let mut nodes = sample();
            let mut selection = Selection::new();
            // Click the first root to focus and select it.
            let mut element = view(&nodes, &selection);
            let mut tree = Tree::new(element.as_widget());
            let mut node = layout(&mut element, &mut tree);
            assert_eq!(
                send(&mut element, &mut tree, &node, press(), click_row(0)),
                [Msg::Select(Selection::single(0))]
            );
            selection = Selection::single(0);
            // Right on a collapsed node asks to expand it.
            element = view(&nodes, &selection);
            node = layout(&mut element, &mut tree);
            assert_eq!(
                send(
                    &mut element,
                    &mut tree,
                    &node,
                    arrow(Named::ArrowRight),
                    click_row(0)
                ),
                [Msg::Toggle("a")]
            );
            drop(element);
            nodes.toggle(&"a");
            // Right on an expanded node moves to its first child.
            element = view(&nodes, &selection);
            node = layout(&mut element, &mut tree);
            assert_eq!(
                send(
                    &mut element,
                    &mut tree,
                    &node,
                    arrow(Named::ArrowRight),
                    click_row(0)
                ),
                [Msg::Select(Selection::single(1))]
            );
            selection = Selection::single(1);
            // Left on a child goes to the parent; Left on the expanded
            // parent collapses it.
            element = view(&nodes, &selection);
            node = layout(&mut element, &mut tree);
            assert_eq!(
                send(
                    &mut element,
                    &mut tree,
                    &node,
                    arrow(Named::ArrowLeft),
                    click_row(0)
                ),
                [Msg::Select(Selection::single(0))]
            );
            selection = Selection::single(0);
            element = view(&nodes, &selection);
            node = layout(&mut element, &mut tree);
            assert_eq!(
                send(
                    &mut element,
                    &mut tree,
                    &node,
                    arrow(Named::ArrowLeft),
                    click_row(0)
                ),
                [Msg::Toggle("a")]
            );
            // Down and Up are the list's own.
            assert_eq!(
                send(
                    &mut element,
                    &mut tree,
                    &node,
                    arrow(Named::ArrowDown),
                    click_row(0)
                ),
                [Msg::Select(Selection::single(1))]
            );
            // A press on the expander toggles without selecting.
            let expander = mouse::Cursor::Available(Point::new(10.0, 14.0));
            assert_eq!(
                send(&mut element, &mut tree, &node, press(), expander),
                [Msg::Toggle("a")]
            );
            // The leaf root has no expander: a press there selects.
            drop(element);
            nodes.set_expanded(&"a", false);
            element = view(&nodes, &selection);
            node = layout(&mut element, &mut tree);
            let leaf_expander = mouse::Cursor::Available(Point::new(10.0, 28.0 + 14.0));
            assert_eq!(
                send(&mut element, &mut tree, &node, press(), leaf_expander),
                [Msg::Select(Selection::single(1))]
            );
        }

        #[test]
        fn right_on_an_expanded_node_without_children_does_not_jump_to_a_sibling() {
            // "a" is expanded with loaded-but-empty children: the next
            // visible row is its sibling "b", not a child, so Right must
            // produce nothing instead of selecting the sibling.
            let mut nodes = Nodes::new();
            assert!(nodes.push(None, "a", 1, Children::Lazy));
            assert!(nodes.push(None, "b", 2, Children::None));
            nodes.set_expanded(&"a", true);
            assert!(nodes.set_children(&"a", Vec::new()));
            assert_eq!(nodes.visible_len(), 2);
            assert_eq!(nodes.children_state(&"a"), Children::Loaded);
            assert_eq!(nodes.parent(&"b"), None);
            let selection = Selection::single(0);
            let mut element = view(&nodes, &selection);
            let mut tree = Tree::new(element.as_widget());
            let node = layout(&mut element, &mut tree);
            assert_eq!(
                send(
                    &mut element,
                    &mut tree,
                    &node,
                    arrow(Named::ArrowRight),
                    click_row(0)
                ),
                Vec::<Msg>::new(),
                "Right on an expanded childless node must not select its sibling"
            );
        }
    }
}
