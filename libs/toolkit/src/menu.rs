// SPDX-License-Identifier: MIT OR Apache-2.0
//! Application menus with app-owned action messages.
//!
//! By default the bar and context menus draw modal in-surface iced overlays.
//! With `Menu::external_popups` the widget draws no popup and reports a
//! `MenuState` instead, so a host can show each panel on its own surface
//! (xdg_popup) with `Panel`, driving it through the same `Navigator`.
//!
//! F10 activates the menu bar. A context target opens on right click, or
//! Shift+F10 after clicking the target. Arrows, Home/End, Enter/Space and
//! Escape navigate. Accelerator strings are labels; the app owns shortcuts.

mod nav;
mod panel;

pub use nav::{MenuState, NavOutcome, Navigator, PanelSpec};
pub use panel::{
    MIN_PANEL_WIDTH, Panel, SEPARATOR_HEIGHT, StyledPanel, panel_size, panel_size_text, row_at,
    row_at_text, row_bounds, row_bounds_text,
};

use iced_core::{
    Border, Color, Element, Event, Layout, Length, Point, Rectangle, Shell, Size, Vector, Widget,
    input_method, keyboard, layout, mouse, overlay, renderer, text,
    widget::{Operation, Tree, tree},
};

use crate::theme::Catalog;
use crate::typography::TextStyle;
use nav::next;
use panel::{draw_panel, panel_width, row_at_with, row_bounds_with, row_height};

/// An action, submenu, or separator. Disabled entries cannot be selected.
#[derive(Debug, Clone)]
pub struct Item<Message> {
    label: String,
    accelerator: String,
    enabled: bool,
    kind: Kind<Message>,
}

#[derive(Debug, Clone)]
enum Kind<Message> {
    Action(Message),
    Submenu(Vec<Item<Message>>),
    Separator,
}

impl<Message> Item<Message> {
    /// An entry that publishes `message` when activated, then closes the menu.
    pub fn action(label: impl Into<String>, message: Message) -> Self {
        Self {
            label: label.into(),
            accelerator: String::new(),
            enabled: true,
            kind: Kind::Action(message),
        }
    }

    /// An entry that opens a nested panel of `items`.
    pub fn submenu(label: impl Into<String>, items: Vec<Self>) -> Self {
        Self {
            label: label.into(),
            accelerator: String::new(),
            enabled: true,
            kind: Kind::Submenu(items),
        }
    }

    /// A horizontal rule; never selectable.
    pub fn separator() -> Self {
        Self {
            label: String::new(),
            accelerator: String::new(),
            enabled: false,
            kind: Kind::Separator,
        }
    }

    /// Right-aligned shortcut label, for display only. The app binds the key.
    pub fn accelerator(mut self, label: impl Into<String>) -> Self {
        self.accelerator = label.into();
        self
    }

    /// A disabled entry is drawn muted and skipped by pointer and keyboard.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// The label (empty for a separator).
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The accelerator label (empty when none).
    pub fn accelerator_label(&self) -> &str {
        &self.accelerator
    }

    /// True if the entry can be selected: enabled and not a separator.
    pub fn is_enabled(&self) -> bool {
        self.selectable()
    }

    /// True for a separator.
    pub fn is_separator(&self) -> bool {
        matches!(self.kind, Kind::Separator)
    }

    /// The submenu's entries; empty for actions and separators.
    pub fn children(&self) -> &[Self] {
        match &self.kind {
            Kind::Submenu(items) => items,
            _ => &[],
        }
    }

    /// The action's message; `None` for submenus and separators.
    pub fn message(&self) -> Option<&Message> {
        match &self.kind {
            Kind::Action(message) => Some(message),
            _ => None,
        }
    }

    fn selectable(&self) -> bool {
        self.enabled && !matches!(self.kind, Kind::Separator)
    }
}

/// Renderer-independent colours and logical-pixel metrics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MenuStyle {
    /// Bar and panel fill.
    pub background: Color,
    /// Enabled label and accelerator colour.
    pub text: Color,
    /// Disabled label colour.
    pub disabled: Color,
    /// Highlight fill of the selected or hovered row.
    pub selected: Color,
    /// Label colour on the highlight.
    pub selected_text: Color,
    /// Panel outline and separator colour.
    pub border: Color,
    /// Corner radius of panels and highlights.
    pub radius: f32,
    /// Label size in logical pixels.
    pub text_size: f32,
    /// Height of the bar and of each non-separator row.
    pub row_height: f32,
    /// Horizontal padding around labels.
    pub padding: f32,
}

impl Default for MenuStyle {
    fn default() -> Self {
        crate::tokens::default_menu_style()
    }
}

/// The style a menu draws with: the explicit one, else the theme's colours
/// over the default row metrics (layout has no theme, so the metrics of a
/// theme-styled menu are always the defaults).
pub(crate) fn resolve<Theme: Catalog>(style: Option<MenuStyle>, theme: &Theme) -> MenuStyle {
    style.unwrap_or_else(|| {
        let metrics = MenuStyle::default();
        MenuStyle {
            text_size: metrics.text_size,
            row_height: metrics.row_height,
            padding: metrics.padding,
            ..theme.menu_style()
        }
    })
}

type OnState<'a, Message> = Box<dyn Fn(MenuState) -> Message + 'a>;

/// A horizontal menu bar, or a context-menu wrapper around arbitrary content.
pub struct Menu<'a, Message, Theme, Renderer>
where
    Renderer: text::Renderer,
{
    items: Vec<Item<Message>>,
    content: Option<Element<'a, Message, Theme, Renderer>>,
    style: Option<MenuStyle>,
    /// Optional prepared text style; overrides the style's text size and the
    /// renderer's default font, regardless of builder order.
    text_style: Option<TextStyle<Renderer::Font>>,
    external: Option<OnState<'a, Message>>,
    host_state: Option<MenuState>,
    id: Option<iced_core::widget::Id>,
}

impl<'a, Message, Theme, Renderer> Menu<'a, Message, Theme, Renderer>
where
    Renderer: text::Renderer,
{
    /// A full-width bar whose top-level `items` are usually submenus. A
    /// top-level action publishes directly. F10 activates the bar.
    pub fn bar(items: Vec<Item<Message>>) -> Self {
        Self {
            items,
            content: None,
            style: None,
            text_style: None,
            external: None,
            host_state: None,
            id: None,
        }
    }

    /// Wraps `content`; right-click on it, or Shift+F10 while it (or a
    /// focusable child) has focus, opens `items` as a popup.
    pub fn context(
        content: impl Into<Element<'a, Message, Theme, Renderer>>,
        items: Vec<Item<Message>>,
    ) -> Self {
        Self {
            items,
            content: Some(content.into()),
            style: None,
            text_style: None,
            external: None,
            host_state: None,
            id: None,
        }
    }

    /// Names the widget so [`open_operation`] can find it.
    pub fn id(mut self, id: impl Into<iced_core::widget::Id>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// An explicit style. Without one the colours come from the theme
    /// (`theme::Catalog::menu_style`) over the default row metrics.
    pub fn style(mut self, style: MenuStyle) -> Self {
        self.style = Some(style);
        self
    }

    /// A prepared text style for labels, accelerators and submenu arrows.
    /// It overrides the style's `text_size` and the renderer's default font
    /// no matter the builder order, and rows are never shorter than its
    /// line box.
    pub fn text_style(mut self, text: TextStyle<Renderer::Font>) -> Self {
        self.text_style = Some(text);
        self
    }

    /// The metrics layout uses: the explicit style, else the defaults.
    fn metrics(&self) -> MenuStyle {
        self.style.unwrap_or_default()
    }

    /// The text layout, drawing and anchors use: the supplied style, else
    /// the menu style's text size with the renderer's default font.
    fn text(&self, renderer: &Renderer, style: MenuStyle) -> TextStyle<Renderer::Font> {
        resolve_text(renderer, style, self.text_style)
    }

    /// Height of the bar: the style's row height, grown to the content
    /// height (the text size and the requested line height, whichever is
    /// larger) of a supplied text style.
    fn bar_height(&self) -> f32 {
        self.text_style.map_or(self.metrics().row_height, |text| {
            self.metrics()
                .row_height
                .max(text.size.max(text.line_box()))
        })
    }

    /// Draws no popup. Every change to the open state (including its
    /// anchors) is published as a `MenuState` for the host to show on its
    /// own surfaces. While open, key presses reaching this widget drive the
    /// navigator and are captured. Losing window focus does not close the
    /// menu in this mode: a popup surface takes focus, and the host closes
    /// the menu when the compositor dismisses the popup.
    pub fn external_popups(mut self, on_change: impl Fn(MenuState) -> Message + 'a) -> Self {
        self.external = Some(Box::new(on_change));
        self
    }

    /// The host's current state, with `external_popups` (ignored without it).
    /// A host that changes the state (popup surfaces, compositor dismissal)
    /// must pass it: store every published value exactly as received, apply
    /// your own changes to that copy, and pass it here every view. Anchors
    /// that differ from the widget's are recomputed and republished, so a
    /// host that drops or edits them makes the widget republish on every
    /// event.
    pub fn state(mut self, state: &MenuState) -> Self {
        self.host_state = Some(state.clone());
        self
    }

    // Host state counts only in external mode: an overlay menu never
    // publishes, so a host copy would keep closing it.
    fn host_state(&self) -> Option<&MenuState> {
        self.host_state.as_ref().filter(|_| self.external.is_some())
    }

    fn navigator(&self) -> Navigator<'_, Message> {
        Navigator {
            items: &self.items,
            bar: self.content.is_none(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct State {
    nav: MenuState,
    focused: bool,
    // Bar entry under the pointer while closed.
    hovered: Option<usize>,
    // Context menu origin, in widget coordinates.
    position: Point,
    // Set even by the inert closed overlay, before iced dispatches a batch.
    overlay_bounds: Option<Size>,
    translation: Vector,
    // External mode: the state last published, compared before publishing.
    reported: MenuState,
    // Bar entry an `open_operation` asked for, consumed in `operate`.
    requested: Option<usize>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            nav: MenuState::default(),
            focused: false,
            hovered: None,
            position: Point::ORIGIN,
            overlay_bounds: None,
            translation: Vector::ZERO,
            reported: MenuState::default(),
            requested: None,
        }
    }
}

/// Opens entry `index` of the menu bar named `id` (see [`Menu::id`]) as if
/// by keyboard: the first selectable row is highlighted, and arrows, Enter
/// and Escape drive it from there. This is how a host binds Alt+letter
/// mnemonics, since an overlay menu ignores host state. It does nothing to
/// a context menu, to a menu in `external_popups` mode (the host owns the
/// state there), or when the entry is missing or disabled.
///
/// iced does not redraw after an operation by itself: chain a message after
/// the task (`widget::operate(op).chain(Task::done(msg))`) so the frame that
/// shows the open menu is drawn.
pub fn open_operation(
    id: impl Into<iced_core::widget::Id>,
    index: usize,
) -> impl Operation + 'static {
    OpenBar {
        id: id.into(),
        index,
    }
}

struct OpenBar {
    id: iced_core::widget::Id,
    index: usize,
}

impl Operation for OpenBar {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn custom(
        &mut self,
        id: Option<&iced_core::widget::Id>,
        _bounds: Rectangle,
        state: &mut dyn std::any::Any,
    ) {
        if id == Some(&self.id)
            && let Some(state) = state.downcast_mut::<State>()
        {
            state.requested = Some(self.index);
        }
    }
}

#[derive(Default)]
struct ChildFocus {
    present: bool,
    focused: bool,
}

impl Operation for ChildFocus {
    fn focusable(
        &mut self,
        _id: Option<&iced_core::widget::Id>,
        _bounds: Rectangle,
        state: &mut dyn iced_core::widget::operation::Focusable,
    ) {
        self.present = true;
        self.focused |= state.is_focused();
    }

    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }
}

// State-only events that content must see even while a menu is open. IME
// preedit text and commits are input, so the open menu blocks them like key
// presses; an empty preedit only clears a composition in flight, so it passes.
fn housekeeping(event: &Event) -> bool {
    match event {
        Event::Window(_)
        | Event::InputMethod(input_method::Event::Opened | input_method::Event::Closed)
        | Event::Keyboard(keyboard::Event::ModifiersChanged(_)) => true,
        Event::InputMethod(input_method::Event::Preedit(content, _)) => content.is_empty(),
        _ => false,
    }
}

// Input an open menu keeps from everything behind it.
fn modal_input(event: &Event) -> bool {
    match event {
        Event::Keyboard(
            keyboard::Event::KeyPressed { .. } | keyboard::Event::KeyReleased { .. },
        )
        | Event::InputMethod(input_method::Event::Commit(_)) => true,
        Event::InputMethod(input_method::Event::Preedit(..)) => !housekeeping(event),
        _ => false,
    }
}

/// The text a menu or panel uses: the supplied text style when given, else
/// the menu style's text size with the renderer's default font.
pub(crate) fn resolve_text<Renderer: text::Renderer>(
    renderer: &Renderer,
    style: MenuStyle,
    supplied: Option<TextStyle<Renderer::Font>>,
) -> TextStyle<Renderer::Font> {
    supplied.unwrap_or(TextStyle {
        font: renderer.default_font(),
        size: style.text_size,
        line_height: None,
    })
}

fn text_width<Renderer: text::Renderer>(
    _renderer: &Renderer,
    value: &str,
    text: TextStyle<Renderer::Font>,
) -> f32 {
    use text::Paragraph;
    Renderer::Paragraph::with_text(text::Text {
        content: value,
        bounds: Size::INFINITE,
        size: text.size.into(),
        line_height: text.line_height_or_default(),
        font: text.font,
        align_x: text::Alignment::Left,
        align_y: iced_core::alignment::Vertical::Top,
        shaping: text::Shaping::Advanced,
        wrapping: text::Wrapping::None,
        ellipsis: text::Ellipsis::None,
        hint_factor: None,
    })
    .min_width()
}

fn bar_rects<Message, Renderer: text::Renderer>(
    renderer: &Renderer,
    items: &[Item<Message>],
    bounds: Rectangle,
    style: MenuStyle,
    text: TextStyle<Renderer::Font>,
) -> Vec<Rectangle> {
    let mut x = bounds.x;
    items
        .iter()
        .map(|item| {
            let width = text_width(renderer, &item.label, text) + style.padding * 2.0;
            let rect = Rectangle {
                x,
                y: bounds.y,
                width,
                height: bounds.height,
            };
            x += width;
            rect
        })
        .collect()
}

fn quad<Renderer: renderer::Renderer>(
    renderer: &mut Renderer,
    bounds: Rectangle,
    background: Color,
    style: MenuStyle,
) {
    renderer.fill_quad(
        renderer::Quad {
            bounds,
            border: Border {
                color: style.border,
                width: 1.0,
                radius: style.radius.into(),
            },
            ..Default::default()
        },
        background,
    );
}

fn label<Renderer: text::Renderer>(
    renderer: &mut Renderer,
    value: &str,
    bounds: Rectangle,
    color: Color,
    text: TextStyle<Renderer::Font>,
    right: bool,
) {
    renderer.fill_text(
        text::Text {
            content: value.to_owned(),
            bounds: bounds.size(),
            size: text.size.into(),
            line_height: text.line_height_or_default(),
            font: text.font,
            align_x: if right {
                text::Alignment::Right
            } else {
                text::Alignment::Left
            },
            align_y: iced_core::alignment::Vertical::Center,
            shaping: text::Shaping::Advanced,
            wrapping: text::Wrapping::None,
            ellipsis: text::Ellipsis::None,
            hint_factor: None,
        },
        Point::new(
            if right {
                bounds.x + bounds.width
            } else {
                bounds.x
            },
            bounds.center_y(),
        ),
        color,
        bounds,
    );
}

impl<Message: Clone, Theme, Renderer: text::Renderer> Menu<'_, Message, Theme, Renderer> {
    /// Anchors for the open panels (see `MenuState::anchors`); `bounds` is
    /// this widget's layout bounds.
    fn anchors(&self, renderer: &Renderer, state: &State, bounds: Rectangle) -> Vec<Rectangle> {
        let nav = self.navigator();
        let Some(root) = state.nav.root else {
            return Vec::new();
        };
        let style = self.metrics();
        let text = self.text(renderer, style);
        let mut anchors = Vec::with_capacity(state.nav.path.len());
        anchors.push(if nav.is_bar() {
            bar_rects(
                renderer,
                &self.items,
                bounds + state.translation,
                style,
                text,
            )
            .get(root)
            .copied()
            .unwrap_or(bounds + state.translation)
        } else {
            Rectangle::new(state.position + state.translation, Size::new(1.0, 1.0))
        });
        for level in 1..state.nav.path.len() {
            let parent = nav.panel(&state.nav, level - 1);
            let width = panel_width(renderer, parent, style, text);
            anchors.push(
                state.nav.path[level - 1]
                    .and_then(|row| row_bounds_with(parent, row, width, style, self.text_style))
                    .unwrap_or_default(),
            );
        }
        anchors
    }

    /// External mode: publishes the state when it differs from what the host
    /// holds (or was last told).
    fn report(
        &self,
        renderer: &Renderer,
        state: &mut State,
        bounds: Rectangle,
        shell: &mut Shell<'_, Message>,
    ) {
        let Some(on_change) = &self.external else {
            return;
        };
        state.nav.anchors = self.anchors(renderer, state, bounds);
        let believed = self.host_state().unwrap_or(&state.reported);
        if *believed != state.nav {
            state.reported = state.nav.clone();
            shell.publish(on_change(state.nav.clone()));
        }
    }

    // External mode, menu open: this surface still gets keys (unless a
    // popup took focus) and presses outside the popups.
    fn external_open_event(
        &self,
        renderer: &Renderer,
        state: &mut State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
        shell: &mut Shell<'_, Message>,
    ) {
        let touch_event;
        let (event, cursor) =
            if let Event::Touch(iced_core::touch::Event::FingerPressed { position, .. }) = event {
                touch_event = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
                (
                    &touch_event,
                    mouse::Cursor::Available(*position - state.translation),
                )
            } else {
                (event, cursor)
            };
        let nav = self.navigator();
        let outcome = match event {
            Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) => {
                nav.key(&mut state.nav, key)
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if nav.is_bar() => {
                match self.title_at(renderer, bounds, cursor) {
                    Some(index) => nav.hover_root(&mut state.nav, index),
                    None => NavOutcome::None,
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(_)) => {
                match self.title_at(renderer, bounds, cursor) {
                    Some(index) if nav.is_bar() => nav.click_root(&mut state.nav, index),
                    _ => nav.close(&mut state.nav),
                }
            }
            _ => NavOutcome::None,
        };
        let pressed = matches!(event, Event::Mouse(mouse::Event::ButtonPressed(_)));
        if !state.nav.is_open() {
            state.hovered = None;
        }
        self.finish(outcome, shell);
        if pressed || modal_input(event) {
            shell.capture_event();
        }
    }

    fn title_at(
        &self,
        renderer: &Renderer,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<usize> {
        if !self.navigator().is_bar() {
            return None;
        }
        let style = self.metrics();
        bar_rects(
            renderer,
            &self.items,
            bounds,
            style,
            self.text(renderer, style),
        )
        .iter()
        .position(|rect| cursor.is_over(*rect))
    }

    fn finish(&self, outcome: NavOutcome<Message>, shell: &mut Shell<'_, Message>) {
        match outcome {
            NavOutcome::None => {}
            NavOutcome::Activated(message) => {
                shell.publish(message);
                shell.invalidate_layout();
                shell.request_redraw();
            }
            NavOutcome::Changed | NavOutcome::Closed => {
                shell.invalidate_layout();
                shell.request_redraw();
            }
        }
    }
}

impl<Message: Clone, Theme: Catalog, Renderer: text::Renderer> Widget<Message, Theme, Renderer>
    for Menu<'_, Message, Theme, Renderer>
{
    fn size(&self) -> Size<Length> {
        self.content
            .as_ref()
            .map(|content| content.as_widget().size())
            .unwrap_or(Size::new(Length::Fill, Length::Fixed(self.bar_height())))
    }
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }
    fn state(&self) -> tree::State {
        let mut nav = self.host_state().cloned().unwrap_or_default();
        self.navigator().validate(&mut nav);
        tree::State::new(State {
            // A rebuilt context menu keeps its origin (translation is not
            // known yet, so this is exact outside scrollables).
            position: nav
                .anchor(0)
                .map_or(Point::ORIGIN, |anchor| anchor.position()),
            nav,
            ..State::default()
        })
    }
    fn diff(&mut self, tree: &mut Tree) {
        if let Some(content) = &mut self.content {
            tree.diff_children(std::slice::from_mut(content));
        } else {
            tree.children.clear();
        }
        let state = tree.state.downcast_mut::<State>();
        let was_open = state.nav.is_open();
        if let Some(host) = self.host_state()
            && (host.root != state.nav.root || host.path != state.nav.path)
        {
            state.nav = host.clone();
        }
        // Rebuilt/dynamic menu models must never retain an invalid navigation
        // path. External mode republishes the closed state on its next event.
        self.navigator().validate(&mut state.nav);
        if was_open && !state.nav.is_open() {
            state.hovered = None;
        }
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        if let Some(content) = &mut self.content {
            let node = content
                .as_widget_mut()
                .layout(&mut tree.children[0], renderer, limits);
            layout::Node::with_children(node.size(), vec![node])
        } else {
            layout::Node::new(limits.resolve(
                Length::Fill,
                Length::Fixed(self.bar_height()),
                Size::ZERO,
            ))
        }
    }
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        if let Some(content) = &self.content {
            content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                layout.children().next().unwrap(),
                cursor,
                viewport,
            );
            return;
        }
        let state = tree.state.downcast_ref::<State>();
        let style = resolve(self.style, theme);
        let text = resolve_text(renderer, style, self.text_style);
        quad(renderer, layout.bounds(), style.background, style);
        for (index, (item, rect)) in self
            .items
            .iter()
            .zip(bar_rects(
                renderer,
                &self.items,
                layout.bounds(),
                style,
                text,
            ))
            .enumerate()
        {
            let selected = if state.nav.is_open() {
                state.nav.root == Some(index)
            } else {
                state.hovered == Some(index)
            };
            if selected {
                quad(renderer, rect, style.selected, style);
            }
            let color = if !item.selectable() {
                style.disabled
            } else if selected {
                style.selected_text
            } else {
                style.text
            };
            label(
                renderer,
                &item.label,
                Rectangle {
                    x: rect.x + style.padding,
                    width: (rect.width - style.padding * 2.0).max(0.0),
                    ..rect
                },
                color,
                text,
                false,
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
        if let Some(content) = &mut self.content {
            content.as_widget_mut().operate(
                &mut tree.children[0],
                layout.children().next().unwrap(),
                renderer,
                operation,
            );
            return;
        }
        let state = tree.state.downcast_mut::<State>();
        operation.custom(self.id.as_ref(), layout.bounds(), state);
        if let Some(index) = state.requested.take()
            && self.external.is_none()
            && self.items.get(index).is_some_and(Item::selectable)
        {
            state.focused = true;
            state.hovered = None;
            state.position = layout.bounds().position();
            let _ = self.navigator().open(&mut state.nav, index, true);
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
        let previously_captured = shell.is_event_captured();
        let external = self.external.is_some();
        let bar = self.content.is_none();
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<State>();
        if state.nav.is_open() && !housekeeping(event) {
            if previously_captured {
                return;
            }
            if external {
                self.external_open_event(renderer, state, event, bounds, cursor, shell);
                self.report(renderer, state, bounds, shell);
                return;
            }
            // iced obtains overlays before processing a batch. If this batch
            // opened the menu, subsequent events still arrive at the base
            // widget. Dispatch through the same popup path until iced rebuilds
            // the overlay. Events captured by an existing overlay never reach
            // the base widget (and the guard above also protects composition).
            let translation = state.translation;
            let overlay_bounds = state.overlay_bounds.unwrap_or(viewport.size());
            let mut popup: overlay::Element<'_, Message, Theme, Renderer> =
                overlay::Element::new(Box::new(Popup {
                    nav: self.navigator(),
                    state,
                    anchor: bounds + translation,
                    translation,
                    style: self.style,
                    text: self.text_style,
                }));
            let node = popup.as_overlay_mut().layout(renderer, overlay_bounds);
            popup.as_overlay_mut().update(
                event,
                Layout::new(&node),
                cursor + translation,
                renderer,
                shell,
            );
            drop(popup);
            if state.nav.is_open() && shell.is_event_captured() {
                // A capture in iced's base pass clears its stored overlay,
                // even for a key release that does not change our state.
                shell.invalidate_layout();
            }
            return;
        }
        if let Event::Touch(iced_core::touch::Event::FingerPressed { position, .. }) = event {
            state.focused = !previously_captured && bounds.contains(*position - state.translation);
        } else if matches!(event, Event::Mouse(mouse::Event::ButtonPressed(_))) {
            state.focused = !previously_captured && cursor.is_over(bounds);
        } else if matches!(event, Event::Window(iced_core::window::Event::Unfocused)) {
            state.focused = false;
            // A popup surface taking focus must not close an external menu.
            if state.nav.is_open() && !external {
                self.navigator().close(&mut state.nav);
                state.hovered = None;
                shell.request_redraw();
            }
        }
        // Like iced containers, always forward lifecycle and focus-changing
        // events, even when an earlier sibling captured the event. Children
        // get first refusal on context triggers, so nested menus prefer inner.
        if let Some(content) = &mut self.content {
            content.as_widget_mut().update(
                &mut tree.children[0],
                event,
                layout.children().next().unwrap(),
                cursor,
                renderer,
                shell,
                viewport,
            );
            if matches!(
                event,
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::F10),
                    ..
                })
            ) {
                let mut focus = ChildFocus::default();
                content.as_widget_mut().operate(
                    &mut tree.children[0],
                    layout.children().next().unwrap(),
                    renderer,
                    &mut focus,
                );
                if focus.present {
                    state.focused = focus.focused;
                }
            }
        }
        if shell.is_event_captured() || state.nav.is_open() {
            self.report(renderer, state, bounds, shell);
            return;
        }
        let touch_event;
        let (event, cursor) =
            if let Event::Touch(iced_core::touch::Event::FingerPressed { position, .. }) = event {
                touch_event = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
                (
                    &touch_event,
                    mouse::Cursor::Available(*position - state.translation),
                )
            } else {
                (event, cursor)
            };
        let nav = self.navigator();
        let mut outcome = NavOutcome::None;
        match event {
            Event::Mouse(mouse::Event::CursorMoved { .. } | mouse::Event::CursorLeft) if bar => {
                let hovered = self
                    .title_at(renderer, bounds, cursor)
                    .filter(|index| self.items[*index].selectable());
                if state.hovered != hovered {
                    state.hovered = hovered;
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(button)) if state.focused => {
                if bar && *button == mouse::Button::Left {
                    if let Some(index) = self.title_at(renderer, bounds, cursor) {
                        outcome = nav.click_root(&mut state.nav, index);
                    }
                } else if !bar && *button == mouse::Button::Right {
                    state.position = cursor.position().unwrap_or(bounds.position());
                    outcome = nav.open(&mut state.nav, 0, false);
                }
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::F10),
                modifiers,
                ..
            }) if (bar && !modifiers.shift()) || (!bar && state.focused && modifiers.shift()) => {
                if let Some(index) = next(&self.items, None, true) {
                    state.position = bounds.position();
                    outcome = nav.open(&mut state.nav, index, true);
                }
            }
            _ => {}
        }
        if !matches!(outcome, NavOutcome::None) {
            shell.capture_event();
        }
        self.finish(outcome, shell);
        self.report(renderer, state, bounds, shell);
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if let Some(content) = &self.content {
            content.as_widget().mouse_interaction(
                &tree.children[0],
                layout.children().next().unwrap(),
                cursor,
                viewport,
                renderer,
            )
        } else if self
            .title_at(renderer, layout.bounds(), cursor)
            .is_some_and(|index| self.items[index].selectable())
        {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::None
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
        let state = tree.state.downcast_mut::<State>();
        state.translation = translation;
        let open = state.nav.is_open();
        if self.external.is_some() {
            // No popup of our own; child overlays behave as in a container.
            return self.content.as_mut().and_then(|content| {
                content.as_widget_mut().overlay(
                    &mut tree.children[0],
                    layout.children().next().unwrap(),
                    renderer,
                    viewport,
                    translation,
                )
            });
        }
        let nav = Navigator {
            items: &self.items,
            bar: self.content.is_none(),
        };
        let popup = overlay::Element::new(Box::new(Popup {
            nav,
            state,
            anchor: layout.bounds() + translation,
            translation,
            style: self.style,
            text: self.text_style,
        }));
        let mut overlays = vec![popup];
        if !open
            && let Some(content) = &mut self.content
            && let Some(overlay) = content.as_widget_mut().overlay(
                &mut tree.children[0],
                layout.children().next().unwrap(),
                renderer,
                viewport,
                translation,
            )
        {
            overlays.push(overlay);
        }
        // The closed popup is inert. Keeping it present lets iced finish its
        // complete event batch on dismissal and supplies true window bounds
        // for a menu opened during the subsequent base-widget pass.
        Some(overlay::Group::with_children(overlays).overlay())
    }
}

impl<'a, Message: Clone + 'a, Theme: Catalog + 'a, Renderer: text::Renderer + 'a>
    From<Menu<'a, Message, Theme, Renderer>> for Element<'a, Message, Theme, Renderer>
{
    fn from(menu: Menu<'a, Message, Theme, Renderer>) -> Self {
        Self::new(menu)
    }
}

struct Popup<'a, Message, Renderer>
where
    Renderer: text::Renderer,
{
    nav: Navigator<'a, Message>,
    state: &'a mut State,
    anchor: Rectangle,
    translation: Vector,
    style: Option<MenuStyle>,
    text: Option<TextStyle<Renderer::Font>>,
}

impl<Message: Clone, Renderer: text::Renderer> Popup<'_, Message, Renderer> {
    fn metrics(&self) -> MenuStyle {
        self.style.unwrap_or_default()
    }

    /// The text layout and drawing use: the supplied style, else the menu
    /// style's text size with the renderer's default font.
    fn text(&self, renderer: &Renderer) -> TextStyle<Renderer::Font> {
        resolve_text(renderer, self.metrics(), self.text)
    }

    fn hit(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
    ) -> Option<(usize, Option<usize>)> {
        let panels: Vec<_> = layout.children().collect();
        for (depth, panel) in panels.iter().enumerate().rev() {
            if let Some(position) = cursor.position_over(panel.bounds()) {
                let items = self.nav.panel(&self.state.nav, depth);
                return Some((
                    depth,
                    row_at_with(
                        items,
                        position.y - panel.bounds().y,
                        self.metrics(),
                        self.text,
                    ),
                ));
            }
        }
        None
    }
}

impl<Message: Clone, Theme: Catalog, Renderer: text::Renderer>
    overlay::Overlay<Message, Theme, Renderer> for Popup<'_, Message, Renderer>
{
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        self.state.overlay_bounds = Some(bounds);
        let Some(root) = self.state.nav.root else {
            return layout::Node::new(bounds);
        };
        let style = self.metrics();
        let text = self.text(renderer);
        let mut panels = Vec::new();
        let mut position = if self.nav.is_bar() {
            bar_rects(renderer, self.nav.items, self.anchor, style, text)
                .get(root)
                .map(|rect| Point::new(rect.x, rect.y + rect.height))
                .unwrap_or(self.anchor.position())
        } else {
            self.state.position + self.translation
        };
        for depth in 0..self.state.nav.path.len() {
            let items = self.nav.panel(&self.state.nav, depth);
            let width = panel_width(renderer, items, style, text).min(bounds.width);
            let height = items
                .iter()
                .map(|item| row_height(item, style, self.text))
                .sum::<f32>();
            if position.x + width > bounds.width && depth > 0 {
                let previous: &layout::Node = &panels[depth - 1];
                position.x = previous.bounds().x - width;
            }
            position.x = position.x.clamp(0.0, (bounds.width - width).max(0.0));
            position.y = position.y.clamp(0.0, (bounds.height - height).max(0.0));
            panels.push(layout::Node::new(Size::new(width, height)).move_to(position));
            let selected = self.state.nav.path[depth].unwrap_or(0);
            let offset = items
                .iter()
                .take(selected)
                .map(|item| row_height(item, style, self.text))
                .sum::<f32>();
            position = Point::new(position.x + width, position.y + offset);
        }
        // Full-window bounds make dismissal modal, including outside clicks.
        layout::Node::with_children(bounds, panels)
    }
    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
    ) {
        if !self.state.nav.is_open() {
            return;
        }
        let style = resolve(self.style, theme);
        for (depth, panel) in layout.children().enumerate() {
            // A bar entry without children (an action opened by F10) has an
            // empty panel; draw_panel draws nothing for it.
            draw_panel(
                renderer,
                panel.bounds(),
                self.nav.panel(&self.state.nav, depth),
                self.state.nav.path[depth],
                style,
                self.text,
            );
        }
    }
    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
    ) {
        if !self.state.nav.is_open() || shell.is_event_captured() {
            return;
        }
        let touch_event;
        let (event, cursor) = match event {
            Event::Touch(iced_core::touch::Event::FingerPressed { position, .. }) => {
                touch_event = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
                (&touch_event, mouse::Cursor::Available(*position))
            }
            Event::Touch(iced_core::touch::Event::FingerMoved { position, .. }) => {
                touch_event = Event::Mouse(mouse::Event::CursorMoved {
                    position: *position,
                });
                (&touch_event, mouse::Cursor::Available(*position))
            }
            Event::Touch(
                iced_core::touch::Event::FingerLifted { position, .. }
                | iced_core::touch::Event::FingerLost { position, .. },
            ) => {
                touch_event = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
                (&touch_event, mouse::Cursor::Available(*position))
            }
            _ => (event, cursor),
        };
        let nav = self.nav;
        let pointer = matches!(
            event,
            Event::Mouse(
                mouse::Event::CursorMoved { .. } | mouse::Event::ButtonPressed(mouse::Button::Left)
            )
        );
        let (hit, title) = if pointer {
            let title = if nav.is_bar() {
                let style = self.metrics();
                bar_rects(renderer, nav.items, self.anchor, style, self.text(renderer))
                    .iter()
                    .position(|rect| cursor.is_over(*rect))
            } else {
                None
            };
            (self.hit(layout, cursor, renderer), title)
        } else {
            (None, None)
        };
        let state = &mut self.state.nav;
        let outcome = match event {
            Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) => nav.key(state, key),
            _ if pointer => {
                let clicked = matches!(event, Event::Mouse(mouse::Event::ButtonPressed(_)));
                match (hit, title) {
                    (Some((level, row)), _) if clicked => nav.click(state, level, row),
                    (Some((level, row)), _) => nav.hover(state, level, row),
                    (None, Some(index)) if clicked => nav.click_root(state, index),
                    (None, Some(index)) => nav.hover_root(state, index),
                    (None, None) if clicked => nav.close(state),
                    (None, None) => NavOutcome::None,
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(_)) => nav.close(state),
            Event::Window(iced_core::window::Event::Unfocused) => nav.close(state),
            _ => NavOutcome::None,
        };
        if modal_input(event) || matches!(event, Event::Mouse(_)) {
            shell.capture_event();
        }
        if !self.state.nav.is_open() {
            // As before the navigator refactor: a closed bar shows no stale
            // title highlight.
            self.state.hovered = None;
        }
        match outcome {
            NavOutcome::None => {}
            NavOutcome::Activated(message) => {
                shell.publish(message);
                shell.request_redraw();
            }
            NavOutcome::Closed => shell.request_redraw(),
            NavOutcome::Changed => {
                shell.invalidate_layout();
                shell.request_redraw();
            }
        }
    }
    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if !self.state.nav.is_open() {
            return mouse::Interaction::None;
        }
        let selectable = self
            .hit(layout, cursor, renderer)
            .is_some_and(|(depth, row)| {
                row.and_then(|row| self.nav.panel(&self.state.nav, depth).get(row))
                    .is_some_and(Item::selectable)
            });
        if selectable {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::Idle
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(debug_assertions)]
    fn update_ui<Message, Renderer: iced_core::Renderer>(
        ui: &mut iced_runtime::UserInterface<'_, Message, iced_core::Theme, Renderer>,
        events: &[Event],
        cursor: mouse::Cursor,
        renderer: &mut Renderer,
        messages: &mut Vec<Message>,
    ) -> (
        iced_runtime::user_interface::State,
        Vec<iced_core::event::Status>,
    ) {
        let mut bus = iced_core::shell::Bus::new();
        let result = ui.update(
            &iced_core::window::Headless,
            &iced_core::shell::Waker::noop(),
            events,
            cursor,
            renderer,
            &mut bus,
        );
        messages.extend(bus);
        result
    }
    use super::*;
    use keyboard::key::Named;

    #[cfg(debug_assertions)]
    #[derive(Default)]
    struct Recorder {
        quads: Vec<Rectangle>,
    }

    #[cfg(debug_assertions)]
    impl renderer::Renderer for Recorder {
        fn start_layer(&mut self, _: Rectangle) {}
        fn end_layer(&mut self) {}
        fn start_transformation(&mut self, _: iced_core::Transformation) {}
        fn end_transformation(&mut self) {}
        fn hint(&mut self, _: renderer::Scale) {}
        fn scale(&self) -> Option<renderer::Scale> {
            None
        }
        fn settings(&self) -> renderer::Settings {
            renderer::Settings::default()
        }
        fn fill_quad(&mut self, quad: renderer::Quad, _: impl Into<iced_core::Background>) {
            self.quads.push(quad.bounds);
        }
        fn reset(&mut self, _: Rectangle) {
            self.quads.clear();
        }
        fn allocate_image(
            &mut self,
            handle: &iced_core::image::Handle,
            callback: impl FnOnce(Result<iced_core::image::Allocation, iced_core::image::Error>)
            + Send
            + 'static,
        ) {
            renderer::Renderer::allocate_image(&mut (), handle, callback);
        }
    }

    #[cfg(debug_assertions)]
    impl text::Renderer for Recorder {
        type Font = iced_core::Font;
        type Paragraph = ();
        type Editor = ();
        const ICON_FONT: iced_core::Font = iced_core::Font::DEFAULT;
        const CHECKMARK_ICON: char = 'x';
        const ARROW_DOWN_ICON: char = 'v';
        const SCROLL_UP_ICON: char = '^';
        const SCROLL_DOWN_ICON: char = 'v';
        const SCROLL_LEFT_ICON: char = '<';
        const SCROLL_RIGHT_ICON: char = '>';
        const ICED_LOGO: char = 'i';
        fn default_font(&self) -> iced_core::Font {
            iced_core::Font::DEFAULT
        }
        fn default_size(&self) -> iced_core::Pixels {
            iced_core::Pixels(14.0)
        }
        fn fill_paragraph(&mut self, _: &(), _: Point, _: Color, _: Rectangle) {}
        fn fill_editor(&mut self, _: &(), _: Point, _: Color, _: Rectangle) {}
        fn fill_text(&mut self, _: text::Text<String>, _: Point, _: Color, _: Rectangle) {}
    }

    // A text renderer whose font is not iced's `Font`: proves the generic
    // `TextStyle<Renderer::Font>` contract end to end. The measured width of
    // a label depends on the face, so the assertions can tell the supplied
    // face from the renderer's default one.
    #[cfg(debug_assertions)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    struct Face(u8);

    #[cfg(debug_assertions)]
    struct FaceParagraph {
        font: Face,
        size: iced_core::Pixels,
        line_height: text::LineHeight,
        content: String,
    }

    #[cfg(debug_assertions)]
    impl Default for FaceParagraph {
        fn default() -> Self {
            Self {
                font: Face(0),
                size: iced_core::Pixels(0.0),
                line_height: text::LineHeight::default(),
                content: String::new(),
            }
        }
    }

    #[cfg(debug_assertions)]
    impl text::Paragraph for FaceParagraph {
        type Font = Face;

        fn with_text(text: text::Text<&str, Face>) -> Self {
            Self {
                font: text.font,
                size: text.size,
                line_height: text.line_height,
                content: text.content.to_owned(),
            }
        }

        fn with_spans<Link>(_text: text::Text<&[text::Span<'_, Link, Face>], Face>) -> Self {
            Self::default()
        }

        fn resize(&mut self, _new_bounds: Size) {}

        fn compare(&self, _text: text::Text<(), Face>) -> text::Difference {
            text::Difference::None
        }

        fn size(&self) -> iced_core::Pixels {
            self.size
        }

        fn hint_factor(&self) -> Option<f32> {
            None
        }

        fn font(&self) -> Face {
            self.font
        }

        fn line_height(&self) -> text::LineHeight {
            self.line_height
        }

        fn align_x(&self) -> text::Alignment {
            text::Alignment::Left
        }

        fn align_y(&self) -> iced_core::alignment::Vertical {
            iced_core::alignment::Vertical::Top
        }

        fn wrapping(&self) -> text::Wrapping {
            text::Wrapping::None
        }

        fn ellipsis(&self) -> text::Ellipsis {
            text::Ellipsis::None
        }

        fn shaping(&self) -> text::Shaping {
            text::Shaping::Advanced
        }

        fn bounds(&self) -> Size {
            Size::INFINITE
        }

        fn min_bounds(&self) -> Size {
            // The face changes the measured width, so the measurement and
            // the drawing provably use the same supplied font.
            Size::new(
                self.content.len() as f32 * 6.0 + self.font.0 as f32 * 100.0,
                self.size.0,
            )
        }

        fn hit_test(&self, _point: Point) -> Option<text::Hit> {
            None
        }

        fn hit_span(&self, _point: Point) -> Option<usize> {
            None
        }

        fn span_bounds(&self, _index: usize) -> Vec<Rectangle> {
            Vec::new()
        }
    }

    // A minimal `Face` editor, so widgets that edit text (like
    // `TextInput`) work with the non-`Font` renderer.
    #[cfg(debug_assertions)]
    #[derive(Default)]
    struct FaceEditor {
        text: String,
        font: Face,
        size: iced_core::Pixels,
        line_height: text::LineHeight,
    }

    #[cfg(debug_assertions)]
    impl text::Editor for FaceEditor {
        type Font = Face;

        fn with_text(text: &str) -> Self {
            Self {
                text: text.to_owned(),
                ..Self::default()
            }
        }

        fn is_empty(&self) -> bool {
            self.text.is_empty()
        }

        fn cursor(&self) -> text::editor::Cursor {
            text::editor::Cursor {
                position: text::Position { line: 0, index: 0 },
                selection: None,
            }
        }

        fn selection(&self) -> text::editor::Selection {
            text::editor::Selection::Caret(Point::ORIGIN)
        }

        fn copy(&self) -> Option<String> {
            None
        }

        fn line(&self, index: usize) -> Option<text::editor::Line<'_>> {
            (index == 0).then(|| text::editor::Line {
                text: self.text.as_str().into(),
                ending: text::editor::LineEnding::None,
            })
        }

        fn line_count(&self) -> usize {
            usize::from(!self.text.is_empty())
        }

        fn perform(&mut self, action: text::editor::Action) {
            let text::editor::Action::Edit(text::editor::Edit::Insert(c)) = action else {
                return;
            };
            self.text.push(c);
        }

        fn move_to(&mut self, _cursor: text::editor::Cursor) {}

        fn bounds(&self) -> Size {
            Size::ZERO
        }

        fn min_bounds(&self) -> Size {
            Size::ZERO
        }

        fn hint_factor(&self) -> Option<f32> {
            None
        }

        fn update(
            &mut self,
            _new_bounds: Size,
            new_font: Self::Font,
            new_size: iced_core::Pixels,
            new_line_height: text::LineHeight,
            _new_wrapping: text::Wrapping,
            _new_alignment: text::Alignment,
            _new_hint_factor: Option<f32>,
            _new_highlighter: &mut impl text::highlighter::Highlighter,
        ) {
            self.font = new_font;
            self.size = new_size;
            self.line_height = new_line_height;
        }

        fn overwrite(&mut self, new_text: &str) {
            self.text.clear();
            self.text.push_str(new_text);
        }

        fn highlight<H: text::highlighter::Highlighter>(
            &mut self,
            _font: Self::Font,
            _highlighter: &mut H,
            _format_highlight: impl Fn(&H::Highlight) -> text::highlighter::Format<Self::Font>,
        ) {
        }

        fn font(&self) -> Self::Font {
            self.font
        }

        fn text_size(&self) -> iced_core::Pixels {
            self.size
        }

        fn line_height(&self) -> text::LineHeight {
            self.line_height
        }
    }

    #[cfg(debug_assertions)]
    #[derive(Default)]
    struct FaceRenderer {
        quads: Vec<Rectangle>,
        texts: Vec<(Face, iced_core::Pixels, text::LineHeight)>,
        paragraphs: Vec<(Face, iced_core::Pixels, text::LineHeight)>,
        editors: Vec<(Face, iced_core::Pixels)>,
    }

    #[cfg(debug_assertions)]
    impl renderer::Renderer for FaceRenderer {
        fn start_layer(&mut self, _: Rectangle) {}
        fn end_layer(&mut self) {}
        fn start_transformation(&mut self, _: iced_core::Transformation) {}
        fn end_transformation(&mut self) {}
        fn hint(&mut self, _: renderer::Scale) {}
        fn scale(&self) -> Option<renderer::Scale> {
            None
        }
        fn settings(&self) -> renderer::Settings {
            renderer::Settings::default()
        }
        fn fill_quad(&mut self, quad: renderer::Quad, _: impl Into<iced_core::Background>) {
            self.quads.push(quad.bounds);
        }
        fn reset(&mut self, _: Rectangle) {
            self.quads.clear();
            self.texts.clear();
            self.paragraphs.clear();
            self.editors.clear();
        }
        fn allocate_image(
            &mut self,
            handle: &iced_core::image::Handle,
            callback: impl FnOnce(Result<iced_core::image::Allocation, iced_core::image::Error>)
            + Send
            + 'static,
        ) {
            renderer::Renderer::allocate_image(&mut (), handle, callback);
        }
    }

    #[cfg(debug_assertions)]
    impl text::Renderer for FaceRenderer {
        type Font = Face;
        type Paragraph = FaceParagraph;
        type Editor = FaceEditor;
        const ICON_FONT: Face = Face(0);
        const CHECKMARK_ICON: char = 'x';
        const ARROW_DOWN_ICON: char = 'v';
        const SCROLL_UP_ICON: char = '^';
        const SCROLL_DOWN_ICON: char = 'v';
        const SCROLL_LEFT_ICON: char = '<';
        const SCROLL_RIGHT_ICON: char = '>';
        const ICED_LOGO: char = 'i';
        fn default_font(&self) -> Face {
            Face(0)
        }
        fn default_size(&self) -> iced_core::Pixels {
            iced_core::Pixels(14.0)
        }
        fn fill_paragraph(&mut self, paragraph: &FaceParagraph, _: Point, _: Color, _: Rectangle) {
            self.paragraphs
                .push((paragraph.font, paragraph.size, paragraph.line_height));
        }
        fn fill_editor(&mut self, editor: &FaceEditor, _: Point, _: Color, _: Rectangle) {
            self.editors.push((editor.font, editor.size));
        }
        fn fill_text(&mut self, text: text::Text<String, Face>, _: Point, _: Color, _: Rectangle) {
            self.texts.push((text.font, text.size, text.line_height));
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    fn runtime_open_and_release_batch_keeps_drawn_overlay_and_close_keeps_tail() {
        let mut renderer = Recorder::default();
        let menu: Menu<'_, u8, iced_core::Theme, Recorder> =
            Menu::bar(vec![Item::submenu("file", items())]);
        let mut ui = iced_runtime::UserInterface::build(
            menu,
            Size::new(400.0, 300.0),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let release = Event::Keyboard(keyboard::Event::KeyReleased {
            key: keyboard::Key::Named(Named::F10),
            modified_key: keyboard::Key::Named(Named::F10),
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
        });
        let mut messages = vec![];
        let (_, statuses) = update_ui(
            &mut ui,
            &[key_event(Named::F10, keyboard::Modifiers::empty()), release],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut messages,
        );
        assert_eq!(statuses.len(), 2);
        ui.draw(
            &mut renderer,
            &iced_core::Theme::Dark,
            &renderer::Style::default(),
            mouse::Cursor::Unavailable,
        );
        assert!(
            renderer
                .quads
                .iter()
                .any(|rect| rect.y == 28.0 && rect.width == 160.0),
            "popup must actually draw after captured key release"
        );
        let (_, statuses) = update_ui(
            &mut ui,
            &[
                key_event(Named::Escape, keyboard::Modifiers::empty()),
                Event::Keyboard(keyboard::Event::ModifiersChanged(
                    keyboard::Modifiers::empty(),
                )),
                Event::Window(iced_core::window::Event::Unfocused),
            ],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut messages,
        );
        assert_eq!(
            statuses.len(),
            3,
            "iced must not drop events following menu dismissal"
        );
        assert_eq!(statuses[1], iced_core::event::Status::Ignored);
        ui.draw(
            &mut renderer,
            &iced_core::Theme::Dark,
            &renderer::Style::default(),
            mouse::Cursor::Unavailable,
        );
        assert!(
            !renderer
                .quads
                .iter()
                .any(|rect| rect.y == 28.0 && rect.width == 160.0)
        );
    }

    #[cfg(debug_assertions)]
    fn character(value: &'static str) -> Event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Character(value.into()),
            modified_key: keyboard::Key::Character(value.into()),
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: Some(value.into()),
            repeat: false,
        })
    }

    #[test]
    #[cfg(debug_assertions)]
    fn runtime_focused_child_opens_context_and_receives_modifiers_and_ime() {
        let id = iced_core::widget::Id::new("field");
        let field = iced_widget::text_input("", "")
            .id(id.clone())
            .on_input(|value| value);
        let menu: Menu<'_, String, iced_core::Theme, crate::test_renderer::LayoutRenderer> =
            Menu::context(field, vec![Item::action("action", "action".to_owned())]);
        let mut ui = iced_runtime::UserInterface::build(
            menu,
            Size::new(400.0, 300.0),
            iced_runtime::user_interface::Cache::new(),
            &mut crate::test_renderer::LayoutRenderer::new(),
        );
        ui.operate(
            &crate::test_renderer::LayoutRenderer::new(),
            &mut iced_core::widget::operation::focusable::focus::<()>(id),
        );
        let mut messages = vec![];
        let (_, statuses) = update_ui(
            &mut ui,
            &[
                Event::Keyboard(keyboard::Event::ModifiersChanged(keyboard::Modifiers::CTRL)),
                key_event(Named::F10, keyboard::Modifiers::SHIFT),
            ],
            mouse::Cursor::Unavailable,
            &mut crate::test_renderer::LayoutRenderer::new(),
            &mut messages,
        );
        assert_eq!(
            statuses[1],
            iced_core::event::Status::Captured,
            "keyboard-focused child enables context shortcut without click"
        );
        let (_, statuses) = update_ui(
            &mut ui,
            &[
                character("q"),
                Event::InputMethod(iced_core::input_method::Event::Preedit(
                    "界".into(),
                    Some(0..3),
                )),
                Event::InputMethod(iced_core::input_method::Event::Commit("界".into())),
            ],
            mouse::Cursor::Unavailable,
            &mut crate::test_renderer::LayoutRenderer::new(),
            &mut messages,
        );
        assert_eq!(
            statuses,
            [iced_core::event::Status::Captured; 3],
            "an open menu blocks key presses and IME text from the app behind it"
        );
        assert!(messages.is_empty());
        // A clearing preedit is state, not input: it reaches the field.
        let (_, statuses) = update_ui(
            &mut ui,
            &[Event::InputMethod(iced_core::input_method::Event::Preedit(
                String::new(),
                None,
            ))],
            mouse::Cursor::Unavailable,
            &mut crate::test_renderer::LayoutRenderer::new(),
            &mut messages,
        );
        assert_eq!(statuses, [iced_core::event::Status::Captured]);
        let (_, statuses) = update_ui(
            &mut ui,
            &[
                Event::Keyboard(keyboard::Event::ModifiersChanged(
                    keyboard::Modifiers::empty(),
                )),
                Event::InputMethod(iced_core::input_method::Event::Closed),
                key_event(Named::Escape, keyboard::Modifiers::empty()),
                character("c"),
            ],
            mouse::Cursor::Unavailable,
            &mut crate::test_renderer::LayoutRenderer::new(),
            &mut messages,
        );
        assert_eq!(statuses.len(), 4);
        assert_eq!(statuses[0], iced_core::event::Status::Ignored);
        assert_eq!(
            messages,
            ["c"],
            "released Control must not turn typing into Copy, nor may Escape drop the tail"
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    fn runtime_captured_sibling_click_unfocuses_context_child() {
        let id = iced_core::widget::Id::new("field");
        let menu: Menu<'_, String, iced_core::Theme, crate::test_renderer::LayoutRenderer> =
            Menu::context(
                iced_widget::text_input("", "")
                    .id(id.clone())
                    .on_input(|value| value),
                vec![Item::action("action", "action".to_owned())],
            );
        let root = iced_widget::column![
            iced_widget::button("button").on_press("button".to_owned()),
            menu
        ];
        let mut ui = iced_runtime::UserInterface::build(
            root,
            Size::new(400.0, 300.0),
            iced_runtime::user_interface::Cache::new(),
            &mut crate::test_renderer::LayoutRenderer::new(),
        );
        ui.operate(
            &crate::test_renderer::LayoutRenderer::new(),
            &mut iced_core::widget::operation::focusable::focus::<()>(id),
        );
        let mut messages = vec![];
        update_ui(
            &mut ui,
            &[
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ],
            mouse::Cursor::Available(Point::new(5.0, 5.0)),
            &mut crate::test_renderer::LayoutRenderer::new(),
            &mut messages,
        );
        let (_, statuses) = update_ui(
            &mut ui,
            &[
                key_event(Named::F10, keyboard::Modifiers::SHIFT),
                character("x"),
            ],
            mouse::Cursor::Unavailable,
            &mut crate::test_renderer::LayoutRenderer::new(),
            &mut messages,
        );
        assert_eq!(
            statuses,
            [
                iced_core::event::Status::Ignored,
                iced_core::event::Status::Ignored
            ]
        );
        assert_eq!(messages, ["button"]);
    }

    #[test]
    #[cfg(debug_assertions)]
    fn runtime_nested_context_prefers_inner_and_touch_can_activate_it() {
        let inner = Menu::context(
            iced_widget::Space::new().width(200).height(100),
            vec![Item::action("inner", 1)],
        );
        let outer: Menu<'_, u8, iced_core::Theme, ()> =
            Menu::context(inner, vec![Item::action("outer", 2)]);
        let mut ui = iced_runtime::UserInterface::build(
            outer,
            Size::new(400.0, 300.0),
            iced_runtime::user_interface::Cache::new(),
            &mut (),
        );
        let mut messages = vec![];
        update_ui(
            &mut ui,
            &[Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Right,
            ))],
            mouse::Cursor::Available(Point::new(20.0, 20.0)),
            &mut (),
            &mut messages,
        );
        update_ui(
            &mut ui,
            &[Event::Touch(iced_core::touch::Event::FingerPressed {
                id: iced_core::touch::Finger(0),
                position: Point::new(25.0, 25.0),
            })],
            mouse::Cursor::Unavailable,
            &mut (),
            &mut messages,
        );
        assert_eq!(messages, [1]);
    }

    #[test]
    #[cfg(debug_assertions)]
    fn runtime_same_batch_popup_hit_testing_uses_scrolled_window_coordinates() {
        let id = iced_core::widget::Id::new("scroll");
        let menu: Menu<'_, u8, iced_core::Theme, ()> =
            Menu::bar(vec![Item::submenu("file", vec![Item::action("run", 9)])]);
        let root = iced_widget::scrollable(iced_widget::column![
            iced_widget::Space::new().height(100),
            menu,
            iced_widget::Space::new().height(400)
        ])
        .id(id.clone())
        .width(200)
        .height(100);
        let mut ui = iced_runtime::UserInterface::build(
            root,
            Size::new(400.0, 300.0),
            iced_runtime::user_interface::Cache::new(),
            &mut (),
        );
        ui.operate(
            &(),
            &mut iced_core::widget::operation::scrollable::scroll_to::<()>(
                id,
                iced_core::widget::operation::scrollable::AbsoluteOffset {
                    x: None,
                    y: Some(80.0),
                },
            ),
        );
        let mut messages = vec![];
        let (_, statuses) = update_ui(
            &mut ui,
            &[
                key_event(Named::F10, keyboard::Modifiers::empty()),
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            ],
            mouse::Cursor::Available(Point::new(10.0, 50.0)),
            &mut (),
            &mut messages,
        );
        assert_eq!(statuses.len(), 2);
        assert_eq!(
            messages,
            [9],
            "bar at content y=100, scroll=80 has popup at window y=48"
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    fn runtime_touch_opens_bar_and_dismisses_outside_without_hover_leak() {
        let menu: Menu<'_, u8, iced_core::Theme, ()> =
            Menu::bar(vec![Item::submenu("file", items())]);
        let mut ui = iced_runtime::UserInterface::build(
            menu,
            Size::new(400.0, 300.0),
            iced_runtime::user_interface::Cache::new(),
            &mut (),
        );
        let mut messages = vec![];
        update_ui(
            &mut ui,
            &[Event::Touch(iced_core::touch::Event::FingerPressed {
                id: iced_core::touch::Finger(0),
                position: Point::new(5.0, 5.0),
            })],
            mouse::Cursor::Unavailable,
            &mut (),
            &mut messages,
        );
        let (state, _) = update_ui(
            &mut ui,
            &[],
            mouse::Cursor::Available(Point::new(5.0, 35.0)),
            &mut (),
            &mut messages,
        );
        assert!(
            matches!(
                state,
                iced_runtime::user_interface::State::Updated {
                    mouse_interaction: mouse::Interaction::Idle,
                    ..
                }
            ),
            "disabled row must block hover through to underlying widgets"
        );
        let (_, statuses) = update_ui(
            &mut ui,
            &[
                Event::Touch(iced_core::touch::Event::FingerPressed {
                    id: iced_core::touch::Finger(1),
                    position: Point::new(390.0, 290.0),
                }),
                character("x"),
            ],
            mouse::Cursor::Unavailable,
            &mut (),
            &mut messages,
        );
        assert_eq!(
            statuses,
            [
                iced_core::event::Status::Captured,
                iced_core::event::Status::Ignored
            ]
        );
        assert!(messages.is_empty());
    }

    #[cfg(debug_assertions)]
    struct Harness {
        menu: Menu<'static, u8, iced_core::Theme, ()>,
        tree: Tree,
        node: layout::Node,
    }

    #[cfg(debug_assertions)]
    impl Harness {
        fn new(mut menu: Menu<'static, u8, iced_core::Theme, ()>) -> Self {
            let mut tree = Tree::new(&menu as &dyn Widget<u8, iced_core::Theme, ()>);
            menu.diff(&mut tree);
            let node = menu.layout(
                &mut tree,
                &(),
                &layout::Limits::new(Size::ZERO, Size::new(400.0, 300.0)),
            );
            Self { menu, tree, node }
        }

        fn event(
            &mut self,
            event: Event,
            cursor: mouse::Cursor,
            popup: bool,
        ) -> (Vec<u8>, bool, iced_core::window::RedrawRequest) {
            let mut messages = iced_core::shell::Bus::new();
            let mut shell = Shell::new(
                &iced_core::window::Headless,
                iced_core::shell::Waker::noop(),
                &mut messages,
            );
            let viewport = Rectangle::with_size(Size::new(400.0, 300.0));
            if popup {
                let mut overlay = self
                    .menu
                    .overlay(
                        &mut self.tree,
                        Layout::new(&self.node),
                        &(),
                        &viewport,
                        Vector::ZERO,
                    )
                    .expect("menu is open");
                let node = overlay.as_overlay_mut().layout(&(), viewport.size());
                overlay.as_overlay_mut().update(
                    &event,
                    Layout::new(&node),
                    cursor,
                    &(),
                    &mut shell,
                );
            } else {
                self.menu.update(
                    &mut self.tree,
                    &event,
                    Layout::new(&self.node),
                    cursor,
                    &(),
                    &mut shell,
                    &viewport,
                );
            }
            let captured = shell.is_event_captured();
            let redraw = shell.redraw_request();
            (messages.into_iter().collect(), captured, redraw)
        }
    }

    #[cfg(debug_assertions)]
    fn key_event(key: Named, modifiers: keyboard::Modifiers) -> Event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(key),
            modified_key: keyboard::Key::Named(key),
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        })
    }

    #[test]
    #[cfg(debug_assertions)]
    fn widget_hover_redraws_only_when_highlight_changes() {
        let mut harness = Harness::new(Menu::bar(vec![Item::submenu("menu", items())]));
        let position = Point::new(5.0, 5.0);
        let event = Event::Mouse(mouse::Event::CursorMoved { position });
        let cursor = mouse::Cursor::Available(position);
        assert_eq!(
            harness.event(event.clone(), cursor, false).2,
            iced_core::window::RedrawRequest::NextFrame
        );
        assert_eq!(
            harness.event(event, cursor, false).2,
            iced_core::window::RedrawRequest::Wait
        );
        assert_eq!(
            harness
                .event(
                    Event::Mouse(mouse::Event::CursorLeft),
                    mouse::Cursor::Unavailable,
                    false
                )
                .2,
            iced_core::window::RedrawRequest::NextFrame
        );
        assert_eq!(harness.tree.state.downcast_ref::<State>().hovered, None);
    }

    #[test]
    #[cfg(debug_assertions)]
    fn closing_clears_the_title_highlight_and_host_state_needs_external_mode() {
        let mut harness = Harness::new(Menu::bar(vec![Item::submenu("menu", items())]));
        let at = Point::new(5.0, 5.0);
        harness.event(
            Event::Mouse(mouse::Event::CursorMoved { position: at }),
            mouse::Cursor::Available(at),
            false,
        );
        harness.event(
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            mouse::Cursor::Available(at),
            false,
        );
        let state = harness.tree.state.downcast_ref::<State>();
        assert!(state.nav.is_open());
        assert_eq!(state.hovered, Some(0));
        harness.event(
            key_event(Named::Escape, keyboard::Modifiers::empty()),
            mouse::Cursor::Unavailable,
            true,
        );
        let state = harness.tree.state.downcast_ref::<State>();
        assert!(!state.nav.is_open());
        assert_eq!(state.hovered, None);

        let open = MenuState {
            root: Some(0),
            path: vec![None],
            anchors: Vec::new(),
        };
        let overlay_menu = Harness::new(
            Menu::context(iced_widget::Space::new().width(200).height(100), items()).state(&open),
        );
        assert!(
            !overlay_menu
                .tree
                .state
                .downcast_ref::<State>()
                .nav
                .is_open()
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    fn widget_bar_keyboard_and_overlay_pointer_emit_actions() {
        let mut harness = Harness::new(Menu::bar(vec![Item::submenu("menu", items())]));
        let (_, captured, _) = harness.event(
            key_event(Named::F10, keyboard::Modifiers::empty()),
            mouse::Cursor::Unavailable,
            false,
        );
        assert!(captured);
        assert_eq!(
            harness.tree.state.downcast_ref::<State>().nav.path,
            [Some(2)]
        );
        // Popup starts below the 28px bar: disabled (28), separator (8), action (28).
        let (messages, captured, _) = harness.event(
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            mouse::Cursor::Available(Point::new(10.0, 70.0)),
            true,
        );
        assert!(captured);
        assert_eq!(messages, [1]);
        assert!(!harness.tree.state.downcast_ref::<State>().nav.is_open());
    }

    #[test]
    #[cfg(debug_assertions)]
    fn newly_opened_menu_handles_rest_of_base_event_batch() {
        let mut harness = Harness::new(Menu::bar(vec![Item::submenu("menu", items())]));
        let cursor = mouse::Cursor::Unavailable;
        harness.event(
            key_event(Named::F10, keyboard::Modifiers::empty()),
            cursor,
            false,
        );
        // No overlay recreation between these base events, as in iced's batch.
        let (messages, captured, _) = harness.event(
            key_event(Named::Enter, keyboard::Modifiers::empty()),
            cursor,
            false,
        );
        assert_eq!(messages, [1]);
        assert!(captured);
        assert!(!harness.tree.state.downcast_ref::<State>().nav.is_open());

        harness.event(
            key_event(Named::F10, keyboard::Modifiers::empty()),
            cursor,
            false,
        );
        harness.event(
            key_event(Named::ArrowDown, keyboard::Modifiers::empty()),
            cursor,
            false,
        );
        harness.event(
            key_event(Named::Enter, keyboard::Modifiers::empty()),
            cursor,
            false,
        );
        let (messages, captured, _) = harness.event(
            key_event(Named::Enter, keyboard::Modifiers::empty()),
            cursor,
            false,
        );
        assert_eq!(messages, [2]);
        assert!(captured);
        assert!(!harness.tree.state.downcast_ref::<State>().nav.is_open());
    }

    #[test]
    #[cfg(debug_assertions)]
    fn context_overlay_skips_disabled_and_captures_outside_dismissal() {
        let mut harness = Harness::new(Menu::context(
            iced_widget::Space::new().width(200).height(100),
            items(),
        ));
        let cursor = mouse::Cursor::Available(Point::new(20.0, 20.0));
        let (_, captured, _) = harness.event(
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)),
            cursor,
            false,
        );
        assert!(captured);
        let (messages, captured, _) = harness.event(
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            cursor,
            true,
        );
        assert!(captured);
        assert!(messages.is_empty());
        assert!(harness.tree.state.downcast_ref::<State>().nav.is_open());
        harness.event(
            key_event(Named::ArrowDown, keyboard::Modifiers::empty()),
            cursor,
            true,
        );
        assert_eq!(
            harness.tree.state.downcast_ref::<State>().nav.path,
            [Some(2)]
        );
        let (messages, captured, _) = harness.event(
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            mouse::Cursor::Available(Point::new(390.0, 290.0)),
            true,
        );
        assert!(messages.is_empty());
        assert!(captured);
        assert!(!harness.tree.state.downcast_ref::<State>().nav.is_open());
        // Context keyboard activation uses the focus retained by its target.
        harness.event(
            key_event(Named::F10, keyboard::Modifiers::SHIFT),
            cursor,
            false,
        );
        assert!(harness.tree.state.downcast_ref::<State>().nav.is_open());
    }

    #[cfg(debug_assertions)]
    #[derive(Debug, Clone, PartialEq)]
    enum Host {
        State(MenuState),
        Act(u8),
    }

    #[cfg(debug_assertions)]
    fn host_items() -> Vec<Item<Host>> {
        vec![
            Item::submenu(
                "file",
                vec![
                    Item::action("one", Host::Act(1)),
                    Item::submenu("more", vec![Item::action("two", Host::Act(2))]),
                ],
            ),
            Item::action("go", Host::Act(9)),
        ]
    }

    #[cfg(debug_assertions)]
    fn host_ui(
        menu: Menu<'_, Host, iced_core::Theme, ()>,
    ) -> iced_runtime::UserInterface<'_, Host, iced_core::Theme, ()> {
        iced_runtime::UserInterface::build(
            menu,
            Size::new(400.0, 300.0),
            iced_runtime::user_interface::Cache::new(),
            &mut (),
        )
    }

    #[cfg(debug_assertions)]
    fn host_ui_with_cache(
        menu: Menu<'_, Host, iced_core::Theme, ()>,
        cache: iced_runtime::user_interface::Cache,
    ) -> iced_runtime::UserInterface<'_, Host, iced_core::Theme, ()> {
        iced_runtime::UserInterface::build(menu, Size::new(400.0, 300.0), cache, &mut ())
    }

    #[cfg(debug_assertions)]
    fn draw_face_menu(
        renderer: &mut FaceRenderer,
        menu: Menu<'_, u8, iced_core::Theme, FaceRenderer>,
    ) {
        let mut ui = iced_runtime::UserInterface::build(
            menu,
            Size::new(400.0, 300.0),
            iced_runtime::user_interface::Cache::new(),
            renderer,
        );
        let mut messages = vec![];
        update_ui(
            &mut ui,
            &[key_event(Named::F10, keyboard::Modifiers::empty())],
            mouse::Cursor::Unavailable,
            renderer,
            &mut messages,
        );
        ui.draw(
            renderer,
            &iced_core::Theme::Dark,
            &renderer::Style::default(),
            mouse::Cursor::Unavailable,
        );
    }

    #[cfg(debug_assertions)]
    fn open_submenu(
        ui: &mut iced_runtime::UserInterface<
            '_,
            u8,
            iced_core::Theme,
            crate::test_renderer::LayoutRenderer,
        >,
        renderer: &mut crate::test_renderer::LayoutRenderer,
        messages: &mut Vec<u8>,
    ) {
        update_ui(
            ui,
            &[
                key_event(Named::F10, keyboard::Modifiers::empty()),
                key_event(Named::ArrowDown, keyboard::Modifiers::empty()),
                key_event(Named::ArrowRight, keyboard::Modifiers::empty()),
            ],
            mouse::Cursor::Unavailable,
            renderer,
            messages,
        );
    }

    #[cfg(debug_assertions)]
    fn send(
        ui: &mut iced_runtime::UserInterface<'_, Host, iced_core::Theme, ()>,
        events: &[Event],
    ) -> (Vec<iced_core::event::Status>, Vec<Host>) {
        let mut messages = Vec::new();
        let (_, statuses) = update_ui(
            ui,
            events,
            mouse::Cursor::Unavailable,
            &mut (),
            &mut messages,
        );
        (statuses, messages)
    }

    // With the null renderer text has no width: bar titles are 20 px
    // (padding only) and panels take the minimum width.
    #[cfg(debug_assertions)]
    fn title(index: usize) -> Rectangle {
        Rectangle::new(Point::new(20.0 * index as f32, 0.0), Size::new(20.0, 28.0))
    }

    #[test]
    #[cfg(debug_assertions)]
    fn external_bar_reports_state_with_anchors_and_draws_no_popup() {
        let mut ui = host_ui(Menu::bar(host_items()).external_popups(Host::State));
        let (statuses, messages) = send(
            &mut ui,
            &[key_event(Named::F10, keyboard::Modifiers::empty())],
        );
        assert_eq!(statuses, [iced_core::event::Status::Captured]);
        assert_eq!(
            messages,
            [Host::State(MenuState {
                root: Some(0),
                path: vec![Some(0)],
                anchors: vec![title(0)],
            })]
        );
        let (statuses, messages) = send(
            &mut ui,
            &[
                key_event(Named::ArrowDown, keyboard::Modifiers::empty()),
                key_event(Named::ArrowRight, keyboard::Modifiers::empty()),
            ],
        );
        assert_eq!(statuses, [iced_core::event::Status::Captured; 2]);
        let submenu = MenuState {
            root: Some(0),
            path: vec![Some(1), Some(0)],
            anchors: vec![
                title(0),
                Rectangle::new(Point::new(0.0, 28.0), Size::new(MIN_PANEL_WIDTH, 28.0)),
            ],
        };
        assert_eq!(messages.last(), Some(&Host::State(submenu)));
        // Losing window focus (a popup surface took it) keeps the menu open;
        // other keys are still modal.
        let (statuses, messages) = send(
            &mut ui,
            &[
                Event::Window(iced_core::window::Event::Unfocused),
                key_event(Named::Enter, keyboard::Modifiers::empty()),
            ],
        );
        assert_eq!(statuses[1], iced_core::event::Status::Captured);
        assert_eq!(messages, [Host::Act(2), Host::State(MenuState::default()),]);

        let mut menu: Menu<'_, Host, iced_core::Theme, ()> =
            Menu::bar(host_items()).external_popups(Host::State);
        let mut tree = Tree::new(&menu as &dyn Widget<Host, iced_core::Theme, ()>);
        let node = layout::Node::new(Size::new(400.0, 28.0));
        assert!(
            menu.overlay(
                &mut tree,
                Layout::new(&node),
                &(),
                &Rectangle::with_size(Size::new(400.0, 300.0)),
                Vector::ZERO,
            )
            .is_none()
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    fn external_bar_follows_host_state_and_fills_its_anchors() {
        let host = MenuState {
            root: Some(0),
            path: vec![Some(1), None],
            anchors: Vec::new(),
        };
        let mut ui = host_ui(
            Menu::bar(host_items())
                .external_popups(Host::State)
                .state(&host),
        );
        let (_, messages) = send(
            &mut ui,
            &[Event::Window(iced_core::window::Event::RedrawRequested(
                std::time::Instant::now(),
            ))],
        );
        assert_eq!(
            messages,
            [Host::State(MenuState {
                anchors: vec![
                    title(0),
                    Rectangle::new(Point::new(0.0, 28.0), Size::new(MIN_PANEL_WIDTH, 28.0)),
                ],
                ..host.clone()
            })]
        );
        // A host state that no longer fits the items is closed and reported.
        let stale = MenuState {
            root: Some(0),
            path: vec![Some(7)],
            anchors: Vec::new(),
        };
        let mut ui = host_ui(
            Menu::bar(host_items())
                .external_popups(Host::State)
                .state(&stale),
        );
        let (_, messages) = send(
            &mut ui,
            &[Event::Window(iced_core::window::Event::RedrawRequested(
                std::time::Instant::now(),
            ))],
        );
        assert_eq!(messages, [Host::State(MenuState::default())]);
        // A closed external bar still activates top-level actions.
        let mut ui = host_ui(Menu::bar(host_items()).external_popups(Host::State));
        let mut messages = Vec::new();
        update_ui(
            &mut ui,
            &[Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Left,
            ))],
            mouse::Cursor::Available(Point::new(25.0, 5.0)),
            &mut (),
            &mut messages,
        );
        assert_eq!(messages, [Host::Act(9)]);
    }

    // iced_core implements `Renderer` for `()` only with debug assertions,
    // like the other UserInterface tests here: run without --release.
    #[test]
    #[cfg(debug_assertions)]
    fn open_operation_opens_the_named_bar_entry_by_keyboard() {
        let bar = || {
            Menu::<'_, u8, iced_core::Theme, ()>::bar(vec![
                Item::submenu("file", items()),
                Item::submenu("edit", vec![Item::action("seven", 7)]),
                Item::submenu("off", vec![Item::action("nine", 9)]).enabled(false),
            ])
            .id("bar")
        };
        let mut ui = iced_runtime::UserInterface::build(
            bar(),
            Size::new(400.0, 300.0),
            iced_runtime::user_interface::Cache::new(),
            &mut (),
        );
        // A different id and a disabled entry are no-ops.
        ui.operate(&(), &mut open_operation("other", 1));
        ui.operate(&(), &mut open_operation("bar", 2));
        let mut messages = vec![];
        update_ui(
            &mut ui,
            &[key_event(Named::Enter, keyboard::Modifiers::empty())],
            mouse::Cursor::Unavailable,
            &mut (),
            &mut messages,
        );
        assert!(messages.is_empty(), "nothing was opened");
        ui.operate(&(), &mut open_operation("bar", 1));
        update_ui(
            &mut ui,
            &[key_event(Named::Enter, keyboard::Modifiers::empty())],
            mouse::Cursor::Unavailable,
            &mut (),
            &mut messages,
        );
        assert_eq!(messages, [7], "Enter activates the first row of entry 1");
        // Rebuilt from a fresh view, the menu is closed again.
        let cache = ui.into_cache();
        let mut ui =
            iced_runtime::UserInterface::build(bar(), Size::new(400.0, 300.0), cache, &mut ());
        messages.clear();
        update_ui(
            &mut ui,
            &[key_event(Named::Enter, keyboard::Modifiers::empty())],
            mouse::Cursor::Unavailable,
            &mut (),
            &mut messages,
        );
        assert!(messages.is_empty());
    }

    #[test]
    #[cfg(debug_assertions)]
    fn supplied_text_style_agrees_across_measure_draw_and_a_non_font_renderer() {
        let mut renderer = FaceRenderer::default();
        let text_style = TextStyle {
            font: Face(1),
            size: 18.0,
            line_height: Some(40.0),
        };
        // Measurement uses the supplied font: the face changes the measured
        // width of the label, so Face(1) is wider than the default Face(0).
        assert_eq!(text_width(&renderer, "file", text_style), 124.0);
        assert_eq!(
            text_width(
                &renderer,
                "file",
                TextStyle {
                    font: Face(0),
                    ..text_style
                }
            ),
            24.0,
            "the default face must measure differently"
        );
        let menu: Menu<'_, u8, iced_core::Theme, FaceRenderer> =
            Menu::bar(vec![Item::submenu("file", items())]).text_style(text_style);
        let mut ui = iced_runtime::UserInterface::build(
            menu,
            Size::new(400.0, 300.0),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut messages = vec![];
        update_ui(
            &mut ui,
            &[key_event(Named::F10, keyboard::Modifiers::empty())],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut messages,
        );
        ui.draw(
            &mut renderer,
            &iced_core::Theme::Dark,
            &renderer::Style::default(),
            mouse::Cursor::Unavailable,
        );
        // Every drawn label uses the supplied font, size and line height,
        // not the defaults.
        assert!(!renderer.texts.is_empty());
        assert!(
            renderer.texts.iter().all(|(font, size, line_height)| {
                *font == Face(1)
                    && size.0 == 18.0
                    && *line_height == text::LineHeight::Absolute(iced_core::Pixels(40.0))
            }),
            "drawing must use the supplied text style"
        );
        // The open bar entry measured with Face(1) is 144px wide ("file" is
        // 4 * 6 + 100) plus padding, and 40px tall: the bar grew to the line
        // box.
        assert!(
            renderer
                .quads
                .iter()
                .any(|rect| *rect == Rectangle::new(Point::ORIGIN, Size::new(144.0, 40.0))),
            "the open bar entry must be measured with the supplied font: {:#?}",
            renderer.quads
        );
        // The panel starts below the 40px bar and its four rows take the
        // line box: 40 + 8 (separator) + 40 + 40.
        assert!(
            renderer
                .quads
                .iter()
                .any(|rect| rect.y == 40.0 && rect.height == 128.0),
            "the open panel must lay out with line-box row heights: {:#?}",
            renderer.quads
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    fn external_bar_anchors_use_the_supplied_text_line_box() {
        let mut ui = host_ui(
            Menu::bar(host_items())
                .external_popups(Host::State)
                .text_style(TextStyle {
                    font: iced_core::Font::DEFAULT,
                    size: 14.0,
                    line_height: Some(40.0),
                }),
        );
        let (statuses, messages) = send(
            &mut ui,
            &[key_event(Named::F10, keyboard::Modifiers::empty())],
        );
        assert_eq!(statuses, [iced_core::event::Status::Captured]);
        // The bar row grew to the line box (the null renderer measures no
        // text, so titles are padding-only).
        assert_eq!(
            messages,
            [Host::State(MenuState {
                root: Some(0),
                path: vec![Some(0)],
                anchors: vec![Rectangle::new(Point::ORIGIN, Size::new(20.0, 40.0))],
            })]
        );
        // ArrowDown selects the submenu, ArrowRight opens it; the submenu
        // panel anchors below the 40px parent row.
        let (_, messages) = send(
            &mut ui,
            &[
                key_event(Named::ArrowDown, keyboard::Modifiers::empty()),
                key_event(Named::ArrowRight, keyboard::Modifiers::empty()),
            ],
        );
        assert_eq!(
            messages.last(),
            Some(&Host::State(MenuState {
                root: Some(0),
                path: vec![Some(1), Some(0)],
                anchors: vec![
                    Rectangle::new(Point::ORIGIN, Size::new(20.0, 40.0)),
                    Rectangle::new(Point::new(0.0, 40.0), Size::new(MIN_PANEL_WIDTH, 40.0)),
                ],
            }))
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    fn text_style_overrides_the_menu_style_in_either_builder_order() {
        let text = TextStyle {
            font: Face(1),
            size: 18.0,
            line_height: Some(40.0),
        };
        let style = MenuStyle {
            text_size: 50.0,
            row_height: 20.0,
            ..MenuStyle::default()
        };
        for menu in [
            Menu::bar(vec![Item::submenu("file", items())])
                .style(style)
                .text_style(text),
            Menu::bar(vec![Item::submenu("file", items())])
                .text_style(text)
                .style(style),
        ] {
            let mut renderer = FaceRenderer::default();
            draw_face_menu(&mut renderer, menu);
            assert!(
                renderer.texts.iter().all(|(font, size, line_height)| {
                    *font == Face(1)
                        && size.0 == 18.0
                        && *line_height == text::LineHeight::Absolute(iced_core::Pixels(40.0))
                }),
                "the prepared text style must win over the menu style"
            );
            // Rows grew to the 40px content height, not the style's 20px
            // rows nor its 50px text size.
            assert!(
                renderer
                    .quads
                    .iter()
                    .any(|rect| rect.y == 40.0 && rect.height == 128.0),
                "the open panel must use the prepared content height"
            );
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    fn rows_never_clip_text_taller_than_the_requested_line_height() {
        let mut renderer = FaceRenderer::default();
        // The requested line height (10) is smaller than the text size (40):
        // rows still allocate the full text size, and the renderer still
        // receives the requested line height.
        let menu: Menu<'_, u8, iced_core::Theme, FaceRenderer> =
            Menu::bar(vec![Item::submenu("file", items())]).text_style(TextStyle {
                font: Face(1),
                size: 40.0,
                line_height: Some(10.0),
            });
        draw_face_menu(&mut renderer, menu);
        assert!(
            renderer.texts.iter().all(|(font, size, line_height)| {
                *font == Face(1)
                    && size.0 == 40.0
                    && *line_height == text::LineHeight::Absolute(iced_core::Pixels(10.0))
            }),
            "the renderer keeps the requested line height"
        );
        // The bar entry is 40px tall (the text size), never clipped to 10.
        assert!(
            renderer
                .quads
                .iter()
                .any(|rect| *rect == Rectangle::new(Point::ORIGIN, Size::new(144.0, 40.0))),
            "rows must allocate the full text size: {:#?}",
            renderer.quads
        );
        // Panel rows are 40 too: 40 + 8 (separator) + 40 + 40.
        assert!(
            renderer
                .quads
                .iter()
                .any(|rect| rect.y == 40.0 && rect.height == 128.0),
            "the open panel must not clip rows to the requested line height"
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    fn a_prepared_style_without_a_line_height_grows_rows_to_the_default_factor() {
        let style = MenuStyle {
            row_height: 20.0,
            ..MenuStyle::default()
        };
        // Legacy: the bar keeps the style's own 20px row height, drawn with
        // the renderer's default face.
        let mut renderer = FaceRenderer::default();
        draw_face_menu(
            &mut renderer,
            Menu::bar(vec![Item::submenu("file", items())]).style(style),
        );
        assert!(
            renderer
                .quads
                .iter()
                .any(|rect| *rect == Rectangle::new(Point::ORIGIN, Size::new(44.0, 20.0))),
            "the legacy bar keeps the style's own row height: {:#?}",
            renderer.quads
        );
        // Prepared without a line height: rows grow to the 1.3 default
        // factor of the 20px text size (26), and the drawn text keeps the
        // default relative line height.
        let mut renderer = FaceRenderer::default();
        draw_face_menu(
            &mut renderer,
            Menu::bar(vec![Item::submenu("file", items())])
                .style(style)
                .text_style(TextStyle {
                    font: Face(1),
                    size: 20.0,
                    line_height: None,
                }),
        );
        assert!(
            renderer
                .quads
                .iter()
                .any(|rect| *rect == Rectangle::new(Point::ORIGIN, Size::new(144.0, 26.0))),
            "prepared rows use the 1.3 default factor: {:#?}",
            renderer.quads
        );
        assert!(
            renderer.texts.iter().all(|(font, size, line_height)| {
                *font == Face(1) && size.0 == 20.0 && *line_height == text::LineHeight::default()
            }),
            "a prepared style without a line height keeps the default relative factor"
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    fn text_style_text_and_input_builders_accept_a_non_font_renderer() {
        let mut renderer = FaceRenderer::default();
        let style = TextStyle {
            font: Face(1),
            size: 18.0,
            line_height: Some(40.0),
        };
        let root: Element<'_, u8, iced_core::Theme, FaceRenderer> =
            iced_widget::column![style.text("hello"), style.input("placeholder", "value")].into();
        let mut ui = iced_runtime::UserInterface::build(
            root,
            Size::new(400.0, 300.0),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        ui.draw(
            &mut renderer,
            &iced_core::Theme::Dark,
            &renderer::Style::default(),
            mouse::Cursor::Unavailable,
        );
        // The label draws through the paragraph recorder with the supplied
        // face, size and line height.
        assert!(
            renderer.paragraphs.iter().any(|(font, size, line_height)| {
                *font == Face(1)
                    && size.0 == 18.0
                    && *line_height == text::LineHeight::Absolute(iced_core::Pixels(40.0))
            }),
            "the label must draw with the supplied text style: {:#?}",
            renderer.paragraphs
        );
        // The input's editor was prepared with the same face and size.
        assert!(
            renderer
                .editors
                .iter()
                .all(|(font, size)| *font == Face(1) && size.0 == 18.0),
            "the input editor must use the supplied text style: {:#?}",
            renderer.editors
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    fn panels_work_with_a_non_font_renderer_styled_or_not() {
        let items = items();
        let renderer = FaceRenderer::default();
        // The legacy panel is renderer-neutral: no font parameter.
        let mut panel: Panel<'_, u8> = Panel::new(&items, Some(0));
        let mut tree = Tree::new(&panel as &dyn Widget<u8, iced_core::Theme, FaceRenderer>);
        Widget::<u8, iced_core::Theme, FaceRenderer>::diff(&mut panel, &mut tree);
        let node = Widget::<u8, iced_core::Theme, FaceRenderer>::layout(
            &mut panel,
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(200.0, 200.0)),
        );
        // Legacy geometry: every entry is a row, at the default 28px row
        // height and the 8px separator: 28 + 8 + 28 + 28.
        assert_eq!(node.size().height, 92.0);
        // The styled panel takes the prepared text: rows 30/8/30/30.
        let mut panel = Panel::new(&items, Some(0)).text_style(TextStyle {
            font: Face(1),
            size: 14.0,
            line_height: Some(30.0),
        });
        let mut tree = Tree::new(&panel as &dyn Widget<u8, iced_core::Theme, FaceRenderer>);
        Widget::<u8, iced_core::Theme, FaceRenderer>::diff(&mut panel, &mut tree);
        let node = Widget::<u8, iced_core::Theme, FaceRenderer>::layout(
            &mut panel,
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(200.0, 200.0)),
        );
        assert_eq!(node.size().height, 98.0);
    }

    #[test]
    #[cfg(debug_assertions)]
    fn retained_cache_keeps_navigation_and_relays_styled_geometry() {
        let small = TextStyle {
            font: iced_core::Font::DEFAULT,
            size: 14.0,
            line_height: None,
        };
        let big = TextStyle {
            font: iced_core::Font::DEFAULT,
            size: 20.0,
            line_height: Some(30.0),
        };
        let menu =
            |text: TextStyle| Menu::bar(vec![Item::submenu("file", items())]).text_style(text);
        // Open the bar and its submenu with the small style.
        let mut renderer = crate::test_renderer::LayoutRenderer::new();
        let mut ui = iced_runtime::UserInterface::build(
            menu(small),
            Size::new(400.0, 300.0),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut messages = vec![];
        open_submenu(&mut ui, &mut renderer, &mut messages);
        let cache = ui.into_cache();
        // Rebuild with the bigger prepared style: the retained cache keeps
        // the navigation, and the popup relays the new extents (a 30px bar
        // and 30px rows).
        let mut ui = iced_runtime::UserInterface::build(
            menu(big),
            Size::new(400.0, 300.0),
            cache,
            &mut renderer,
        );
        // iced establishes overlays during the update pass, not the build:
        // the rebuilt interface draws them after the next frame's event
        // batch (a redraw request in a real loop). The cache keeps the open
        // submenu and its selection, so no state is reset here.
        update_ui(
            &mut ui,
            &[Event::Window(iced_core::window::Event::RedrawRequested(
                std::time::Instant::now(),
            ))],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut messages,
        );
        let mut draw = crate::test_renderer::LayoutRenderer::new();
        ui.draw(
            &mut draw,
            &iced_core::Theme::Dark,
            &renderer::Style::default(),
            mouse::Cursor::Unavailable,
        );
        assert!(
            draw.quads
                .iter()
                .any(|(rect, _)| rect.y == 30.0 && rect.height == 98.0),
            "the retained submenu must relay the bigger rows: {:#?}",
            draw.quads
        );
        // Clicking "two" in the open submenu uses the new boundaries.
        messages.clear();
        update_ui(
            &mut ui,
            &[Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Left,
            ))],
            mouse::Cursor::Available(Point::new(170.0, 120.0)),
            &mut renderer,
            &mut messages,
        );
        assert_eq!(messages, [2]);
        // Reopen and rebuild with the small style again: the retained cache
        // relays the smaller geometry and the click boundaries move back.
        open_submenu(&mut ui, &mut renderer, &mut messages);
        let cache = ui.into_cache();
        let mut ui = iced_runtime::UserInterface::build(
            menu(small),
            Size::new(400.0, 300.0),
            cache,
            &mut renderer,
        );
        // The rebuilt interface draws the overlay after its update pass,
        // exactly as a real frame does.
        update_ui(
            &mut ui,
            &[Event::Window(iced_core::window::Event::RedrawRequested(
                std::time::Instant::now(),
            ))],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut messages,
        );
        let mut draw = crate::test_renderer::LayoutRenderer::new();
        ui.draw(
            &mut draw,
            &iced_core::Theme::Dark,
            &renderer::Style::default(),
            mouse::Cursor::Unavailable,
        );
        assert!(
            draw.quads
                .iter()
                .any(|(rect, _)| rect.y == 28.0 && rect.height == 92.0),
            "the retained submenu must relay the smaller rows: {:#?}",
            draw.quads
        );
        // A press that lands on the separator row (the small rows put it at
        // window y 56..64) publishes nothing.
        messages.clear();
        update_ui(
            &mut ui,
            &[Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Left,
            ))],
            mouse::Cursor::Available(Point::new(10.0, 60.0)),
            &mut renderer,
            &mut messages,
        );
        assert!(messages.is_empty());
    }

    #[test]
    #[cfg(debug_assertions)]
    fn external_anchors_republish_after_a_styled_rebuild_without_pointer_movement() {
        let small = TextStyle {
            font: iced_core::Font::DEFAULT,
            size: 14.0,
            line_height: None,
        };
        let mut ui = host_ui(
            Menu::bar(host_items())
                .external_popups(Host::State)
                .text_style(small),
        );
        let (_, messages) = send(
            &mut ui,
            &[key_event(Named::F10, keyboard::Modifiers::empty())],
        );
        // Prepared without a line height: the default metrics keep the 28px
        // bar (max(28, max(14, 18.2)) = 28).
        assert_eq!(
            messages,
            [Host::State(MenuState {
                root: Some(0),
                path: vec![Some(0)],
                anchors: vec![Rectangle::new(Point::ORIGIN, Size::new(20.0, 28.0))],
            })]
        );
        let cache = ui.into_cache();
        let big = TextStyle {
            font: iced_core::Font::DEFAULT,
            size: 14.0,
            line_height: Some(40.0),
        };
        let mut ui = host_ui_with_cache(
            Menu::bar(host_items())
                .external_popups(Host::State)
                .text_style(big),
            cache,
        );
        // An ordinary redraw republishes the changed anchors, without any
        // pointer movement.
        let (_, messages) = send(
            &mut ui,
            &[Event::Window(iced_core::window::Event::RedrawRequested(
                std::time::Instant::now(),
            ))],
        );
        assert_eq!(
            messages,
            [Host::State(MenuState {
                root: Some(0),
                path: vec![Some(0)],
                anchors: vec![Rectangle::new(Point::ORIGIN, Size::new(20.0, 40.0))],
            })]
        );
        // The host rebuilds its popup surface with the same prepared text:
        // the styled panel's layout matches the typed panel size exactly.
        let host_items = host_items();
        let panel_items = host_items[0].children();
        let mut panel = Panel::new(panel_items, Some(0)).text_style(big);
        let mut tree = Tree::new(&panel as &dyn Widget<Host, iced_core::Theme, ()>);
        Widget::<Host, iced_core::Theme, ()>::diff(&mut panel, &mut tree);
        let node = Widget::<Host, iced_core::Theme, ()>::layout(
            &mut panel,
            &mut tree,
            &(),
            &layout::Limits::new(Size::ZERO, Size::new(400.0, 300.0)),
        );
        assert_eq!(
            node.size(),
            panel_size_text(&(), panel_items, MenuStyle::default(), big)
        );
    }

    fn items() -> Vec<Item<u8>> {
        vec![
            Item::action("disabled", 0).enabled(false),
            Item::separator(),
            Item::action("one", 1),
            Item::submenu("more", vec![Item::separator(), Item::action("two", 2)]),
        ]
    }
}
