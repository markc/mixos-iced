// SPDX-License-Identifier: MIT OR Apache-2.0
//! Widget-level drag and drop: [`DragArea`] starts a drag, [`DropArea`]
//! claims it while the pointer is over it, and [`Layer`] — wrapped
//! around the whole window content — draws the preview card, routes the
//! events and finishes the gesture with a [`Choice`] (Move / Copy /
//! Cancel) that the application turns into a transfer.
//!
//! The payload is generic (`P: Clone + Send`) and the state is shared
//! ([`Shared`]), so the source, the targets and the layer all see the
//! same gesture. Everything is in-window; cross-window DnD belongs to
//! the compositor, not a widget toolkit.
//!
//! [`find_zones`] is the geometry query: a widget operation that
//! reports the bounds of every id'd container passing a filter — the
//! drop zones — for hit-testing outside this module's own widgets.

use std::sync::{Arc, Mutex, MutexGuard};

use iced_core::layout::{Layout, Limits, Node};
use iced_core::mouse::{self, Cursor};
use iced_core::overlay;
use iced_core::renderer;
use iced_core::widget::tree::{State as TreeState, Tag};
use iced_core::widget::operation::{Outcome, Scrollable};
use iced_core::widget::{Id, Operation, Tree, Widget};
use iced_core::window;
use iced_core::{
    Border, Element, Event, Length, Point, Rectangle, Shell, Size, Vector, keyboard,
};
use crate::tokens::Tokens;

/// How far the pointer must move while held before a press becomes a
/// drag.
const DRAG_THRESHOLD: f32 = 4.0;

/// What the user chose on release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Move the payload into the target.
    Move,
    /// Copy the payload into the target.
    Copy,
}

/// One claimed drop target.
#[derive(Debug, Clone, Copy)]
pub struct Target {
    /// The drop area's bounds as drawn.
    pub bounds: Rectangle,
    /// The strip inside `bounds` that should be highlighted.
    pub highlight: Rectangle,
}

/// An in-flight drag.
#[derive(Debug, Clone)]
pub struct Gesture<P> {
    /// What is being dragged, app-defined.
    pub payload: P,
    /// The drag's label, shown on the preview card.
    pub label: String,
    /// The pointer.
    pub pointer: Point,
    /// The claimed target, if any.
    pub target: Option<Target>,
}

impl<P> Gesture<P> {
    /// A new gesture at `pointer`.
    #[must_use]
    pub fn new(payload: P, label: String, pointer: Point) -> Self {
        Self {
            payload,
            label,
            pointer,
            target: None,
        }
    }
}

/// The shared drag state.
#[derive(Debug, Default)]
pub struct State<P> {
    /// The gesture being dragged.
    pub active: Option<Gesture<P>>,
    /// The released gesture awaiting its choice (the choice card is up).
    pub pending: Option<Gesture<P>>,
    /// Bumped on every cancel, so areas holding stale highlights clear.
    pub cancel_epoch: u64,
}

impl<P> State<P> {
    /// Cancels any gesture and bumps the epoch.
    pub fn cancel(&mut self) {
        self.active = None;
        self.pending = None;
        self.cancel_epoch = self.cancel_epoch.wrapping_add(1);
    }

    fn release(&mut self) {
        self.pending = self.active.take().filter(|drag| drag.target.is_some());
    }
}

/// The shared-state handle.
pub type Shared<P> = Arc<Mutex<State<P>>>;

/// Locks the shared state (recovering from poison).
pub fn lock<P>(shared: &Shared<P>) -> MutexGuard<'_, State<P>> {
    shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What the layer tells the application when the gesture finishes.
#[derive(Debug, Clone)]
pub struct Finished<P> {
    /// The payload that was dragged.
    pub payload: P,
    /// The bounds of the target it was dropped on.
    pub target: Rectangle,
    /// The user's choice.
    pub choice: Choice,
}

/// The layer's localisable card strings.
#[derive(Debug, Clone, Default)]
pub struct Labels {
    /// The Move row.
    pub r#move: String,
    /// The Copy row.
    pub copy: String,
    /// The Cancel row.
    pub cancel: String,
}

impl Labels {
    /// The English card strings.
    #[must_use]
    pub fn english() -> Self {
        Self {
            r#move: "Move here".into(),
            copy: "Copy here".into(),
            cancel: "Cancel".into(),
        }
    }
}

/// The drag layer: wrap it around the whole window content. It draws
/// the preview card and the choice card, cancels on Escape / focus
/// loss / leaving the window, and publishes `on_finished` when a choice
/// is made.
#[allow(missing_debug_implementations)]
pub struct Layer<'a, Message, Theme, Renderer, P>
where
    P: Clone + Send,
{
    content: Element<'a, Message, Theme, Renderer>,
    shared: Shared<P>,
    tokens: Tokens,
    labels: Labels,
    on_finished: Box<dyn Fn(Finished<P>) -> Message + 'a>,
}

impl<'a, Message, Theme, Renderer, P> Layer<'a, Message, Theme, Renderer, P>
where
    P: Clone + Send,
{
    /// Wraps `content` in the drag layer over `shared`.
    pub fn new(
        content: impl Into<Element<'a, Message, Theme, Renderer>>,
        shared: Shared<P>,
        tokens: Tokens,
        on_finished: impl Fn(Finished<P>) -> Message + 'a,
    ) -> Self {
        Self {
            content: content.into(),
            shared,
            tokens,
            labels: Labels::english(),
            on_finished: Box::new(on_finished),
        }
    }

    /// Overrides the choice-card strings.
    #[must_use]
    pub fn labels(mut self, labels: Labels) -> Self {
        self.labels = labels;
        self
    }
}

/// The choice at `point` over the four-row card: `None` above the
/// rows, `Some(None)` on Cancel, a choice on Move/Copy.
fn choice_at(point: Point, bounds: Rectangle) -> Option<Option<Choice>> {
    if !bounds.contains(point) || point.y < bounds.y + bounds.height / 4.0 {
        return None;
    }
    match (((point.y - bounds.y) / bounds.height) * 4.0) as usize {
        1 => Some(Some(Choice::Move)),
        2 => Some(Some(Choice::Copy)),
        _ => Some(None),
    }
}

/// The choice card's bounds: clamped into the target and the viewport.
fn card_bounds(drag: &Gesture<impl Clone + Send>, viewport: Rectangle, tokens: &Tokens) -> Option<Rectangle> {
    let target = drag.target.as_ref()?.bounds.intersection(&viewport)?;
    let m = &tokens.metrics;
    let width = (m.text.md * 20.0 + 2.0 * m.spacing.sm).min(target.width);
    let height = (m.text.md * 1.5 * 4.0 + 5.0 * m.spacing.xs).min(target.height);
    Some(Rectangle {
        x: drag.pointer.x.clamp(target.x, target.x + target.width - width),
        y: drag.pointer.y.clamp(target.y, target.y + target.height - height),
        width,
        height,
    })
}

impl<Message, Theme, Renderer, P> Widget<Message, Theme, Renderer>
    for Layer<'_, Message, Theme, Renderer, P>
where
    P: Clone + Send,
    Theme: iced_core::widget::text::Catalog + iced_widget::container::Catalog,
    Renderer: renderer::Renderer + iced_core::text::Renderer,
{
    fn tag(&self) -> Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> TreeState {
        self.content.as_widget().state()
    }

    fn diff(&mut self, tree: &mut Tree) {
        self.content.as_widget_mut().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
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
        {
            let mut state = lock(&self.shared);
            let engaged = state.active.is_some() || state.pending.is_some();
            if engaged
                && matches!(
                    event,
                    Event::Window(window::Event::Unfocused | window::Event::Resized(_))
                        | Event::Mouse(mouse::Event::CursorLeft)
                        | Event::Keyboard(keyboard::Event::KeyPressed {
                            key: keyboard::Key::Named(keyboard::key::Named::Escape),
                            ..
                        })
                )
            {
                state.cancel();
                shell.request_redraw();
                shell.capture_event();
                return;
            }
            if let Some(pending) = state.pending.as_ref() {
                if let Event::Mouse(mouse::Event::ButtonPressed(button)) = event {
                    let choice = if *button == mouse::Button::Left {
                        cursor
                            .position()
                            .and_then(|point| {
                                card_bounds(pending, *viewport, &self.tokens)
                                    .and_then(|bounds| choice_at(point, bounds))
                            })
                            .flatten()
                    } else {
                        None
                    };
                    if let Some(choice) = choice {
                        let pending = state.pending.take().expect("pending choice");
                        let target = pending.target.expect("validated drop target");
                        shell.publish((self.on_finished)(Finished {
                            payload: pending.payload,
                            target: target.bounds,
                            choice,
                        }));
                    } else {
                        state.cancel();
                    }
                    shell.request_redraw();
                }
                // While the card is up, the content underneath gets
                // nothing but modifier changes.
                if matches!(event, Event::Mouse(_) | Event::Keyboard(_))
                    && !matches!(
                        event,
                        Event::Keyboard(keyboard::Event::ModifiersChanged(_))
                    )
                {
                    if matches!(event, Event::Mouse(mouse::Event::CursorMoved { .. })) {
                        shell.request_redraw();
                    }
                    shell.capture_event();
                    return;
                }
            }
            if state.active.is_some()
                && matches!(event, Event::Keyboard(_))
                && !matches!(
                    event,
                    Event::Keyboard(keyboard::Event::ModifiersChanged(_))
                )
            {
                shell.capture_event();
                return;
            }
            if matches!(
                event,
                Event::Mouse(
                    mouse::Event::CursorMoved { .. }
                        | mouse::Event::ButtonReleased(mouse::Button::Left)
                )
            ) && let Some(active) = state.active.as_mut()
            {
                if let Some(position) = cursor.position() {
                    active.pointer = position;
                }
                // Targets re-claim on their own update pass, which runs
                // under the content below.
                active.target = None;
            }
        }
        self.content.as_widget_mut().update(
            tree, event, layout, cursor, renderer, shell, viewport,
        );
        let mut state = lock(&self.shared);
        if state.active.is_some() {
            if matches!(
                event,
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            ) {
                state.release();
            }
            if matches!(event, Event::Mouse(_)) {
                shell.capture_event();
                shell.request_redraw();
            }
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
        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);

        let t = self.tokens.palette;
        let m = &self.tokens.metrics;
        let state = lock(&self.shared);

        // The preview card under the pointer.
        if let Some(active) = &state.active {
            let bounds = Rectangle {
                x: active.pointer.x + m.spacing.sm,
                y: active.pointer.y + m.spacing.sm,
                width: (m.text.md * 20.0).min(viewport.width),
                height: m.text.md * 1.5 + 2.0 * m.spacing.sm,
            };
            renderer.with_layer(*viewport, |renderer| {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds,
                        border: Border {
                            color: t.border,
                            width: m.border.width,
                            radius: m.radius.sm.into(),
                        },
                        ..renderer::Quad::default()
                    },
                    iced_core::Background::Color(t.popover),
                );
                renderer.fill_text(
                    iced_core::text::Text {
                        content: active.label.clone(),
                        bounds: Size::new(bounds.width - 2.0 * m.spacing.sm, bounds.height),
                        size: iced_core::Pixels(m.text.sm),
                        font: renderer.default_font(),
                        align_x: iced_core::text::Alignment::Left,
                        align_y: iced_core::alignment::Vertical::Center,
                        line_height: iced_core::text::LineHeight::Absolute((m.text.sm * 1.4).into()),
                        shaping: iced_core::text::Shaping::Advanced,
                        wrapping: iced_core::text::Wrapping::None,
                        ellipsis: iced_core::text::Ellipsis::None,
                        hint_factor: renderer.hint_factor(),
                    },
                    Point::new(bounds.x + m.spacing.sm, bounds.center_y()),
                    t.popover_text,
                    bounds,
                );
            });
        }

        // The choice card over the target.
        if let Some(pending) = &state.pending
            && let Some(bounds) = card_bounds(pending, *viewport, &self.tokens)
        {
            renderer.with_layer(bounds, |renderer| {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds,
                        border: Border {
                            color: t.border,
                            width: m.border.width,
                            radius: m.radius.sm.into(),
                        },
                        ..renderer::Quad::default()
                    },
                    iced_core::Background::Color(t.popover),
                );
                let rows = [
                    pending.label.as_str(),
                    self.labels.r#move.as_str(),
                    self.labels.copy.as_str(),
                    self.labels.cancel.as_str(),
                ];
                for (index, label) in rows.iter().enumerate() {
                    let row = Rectangle {
                        y: bounds.y + bounds.height * index as f32 / 4.0,
                        height: bounds.height / 4.0,
                        ..bounds
                    };
                    if index > 0 && cursor.is_over(row) {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: row,
                                ..renderer::Quad::default()
                            },
                            iced_core::Background::Color(t.muted_surface),
                        );
                    }
                    renderer.fill_text(
                        iced_core::text::Text {
                            content: (*label).to_owned(),
                            bounds: Size::new(row.width - 2.0 * m.spacing.sm, row.height),
                            size: iced_core::Pixels(m.text.sm),
                            font: renderer.default_font(),
                            align_x: iced_core::text::Alignment::Left,
                            align_y: iced_core::alignment::Vertical::Top,
                            line_height: iced_core::text::LineHeight::Absolute((m.text.sm * 1.4).into()),
                            shaping: iced_core::text::Shaping::Advanced,
                            wrapping: iced_core::text::Wrapping::None,
                            ellipsis: iced_core::text::Ellipsis::None,
                            hint_factor: renderer.hint_factor(),
                        },
                        Point::new(row.x + m.spacing.sm, row.y + m.spacing.xs),
                        t.popover_text,
                        row,
                    );
                }
            });
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let state = lock(&self.shared);
        if state.active.is_some() {
            mouse::Interaction::Grabbing
        } else if let Some(pending) = &state.pending {
            if cursor.position().is_some_and(|point| {
                card_bounds(pending, *viewport, &self.tokens)
                    .and_then(|bounds| choice_at(point, bounds))
                    .is_some()
            }) {
                mouse::Interaction::Pointer
            } else {
                mouse::Interaction::Idle
            }
        } else {
            self.content
                .as_widget()
                .mouse_interaction(tree, layout, cursor, viewport, renderer)
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
        let engaged = {
            let state = lock(&self.shared);
            state.active.is_some() || state.pending.is_some()
        };
        if engaged {
            None
        } else {
            self.content
                .as_widget_mut()
                .overlay(tree, layout, renderer, viewport, translation)
        }
    }
}

impl<'a, Message, Theme, Renderer, P> From<Layer<'a, Message, Theme, Renderer, P>>
    for Element<'a, Message, Theme, Renderer>
where
    P: Clone + Send + 'a,
    Message: 'a,
    Theme: iced_core::widget::text::Catalog + iced_widget::container::Catalog + 'a,
    Renderer: renderer::Renderer + iced_core::text::Renderer + 'a,
{
    fn from(layer: Layer<'a, Message, Theme, Renderer, P>) -> Self {
        Element::new(layer)
    }
}

/// An area that starts a drag: wrap it around draggable content. A
/// press that moves past the threshold calls `on_drag` with the payload
/// (the application stores the gesture in the [`Shared`] state, or uses
/// [`DragArea::start_directly`] to have the area do it).
#[allow(missing_debug_implementations)]
pub struct DragArea<'a, Message, Theme, Renderer, P>
where
    P: Clone + Send,
{
    content: Element<'a, Message, Theme, Renderer>,
    payload: P,
    label: Box<dyn Fn(&P) -> String + 'a>,
    shared: Option<Shared<P>>,
    on_drag: Option<Box<dyn Fn(P) -> Message + 'a>>,
}

impl<'a, Message, Theme, Renderer, P> DragArea<'a, Message, Theme, Renderer, P>
where
    P: Clone + Send,
{
    /// Wraps `content` as draggable, carrying `payload`.
    pub fn new(
        content: impl Into<Element<'a, Message, Theme, Renderer>>,
        payload: P,
    ) -> Self {
        Self {
            content: content.into(),
            payload,
            label: Box::new(|_| String::new()),
            shared: None,
            on_drag: None,
        }
    }

    /// Sets the preview label from the payload.
    #[must_use]
    pub fn label(mut self, label: impl Fn(&P) -> String + 'a) -> Self {
        self.label = Box::new(label);
        self
    }

    /// Publishes `on_drag(payload)` when the drag starts; the
    /// application puts the gesture into the shared state.
    #[must_use]
    pub fn on_drag(mut self, on_drag: impl Fn(P) -> Message + 'a) -> Self {
        self.on_drag = Some(Box::new(on_drag));
        self
    }

    /// Starts the drag directly into `shared` (no message needed).
    #[must_use]
    pub fn start_directly(mut self, shared: Shared<P>) -> Self {
        self.shared = Some(shared);
        self
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct DragAreaState {
    origin: Option<Point>,
    dragging: bool,
}

impl<Message, Theme, Renderer, P> Widget<Message, Theme, Renderer>
    for DragArea<'_, Message, Theme, Renderer, P>
where
    P: Clone + Send,
    Theme: iced_core::widget::text::Catalog + iced_widget::container::Catalog,
    Renderer: renderer::Renderer,
{
    fn tag(&self) -> Tag {
        Tag::of::<DragAreaState>()
    }

    fn state(&self) -> TreeState {
        TreeState::new(DragAreaState::default())
    }

    fn diff(&mut self, tree: &mut Tree) {
        self.content.as_widget_mut().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
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
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<DragAreaState>();

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                if cursor.is_over(bounds) && !state.dragging =>
            {
                state.origin = cursor.position();
                // Do not capture yet: a click is not a drag.
            }
            Event::Mouse(mouse::Event::CursorMoved { position })
                if state.origin.is_some() && !state.dragging =>
            {
                let origin = state.origin.expect("set above");
                let moved = (position.x - origin.x).hypot(position.y - origin.y);
                if moved >= DRAG_THRESHOLD {
                    state.dragging = true;
                    let payload = self.payload.clone();
                    let label = (self.label)(&self.payload);
                    if let Some(shared) = &self.shared {
                        let mut drag = lock(shared);
                        if drag.active.is_none() && drag.pending.is_none() {
                            drag.active = Some(Gesture::new(payload.clone(), label, *position));
                            shell.capture_event();
                            shell.request_redraw();
                        }
                    } else if let Some(on_drag) = &self.on_drag {
                        shell.publish(on_drag(payload));
                        shell.capture_event();
                        shell.request_redraw();
                    }
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.origin = None;
                state.dragging = false;
            }
            _ => {}
        }

        self.content.as_widget_mut().update(
            tree, event, layout, cursor, renderer, shell, viewport,
        );
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
        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
    }
}

impl<'a, Message, Theme, Renderer, P> From<DragArea<'a, Message, Theme, Renderer, P>>
    for Element<'a, Message, Theme, Renderer>
where
    P: Clone + Send + 'a,
    Message: 'a,
    Theme: iced_core::widget::text::Catalog + iced_widget::container::Catalog + 'a,
    Renderer: renderer::Renderer + 'a,
{
    fn from(area: DragArea<'a, Message, Theme, Renderer, P>) -> Self {
        Element::new(area)
    }
}

/// An area that claims a drag while the pointer is over it: wrap it
/// around droppable content. While a gesture is active, its bounds
/// become the gesture's target (the last claim wins — innermost areas
/// should draw last).
#[allow(missing_debug_implementations)]
pub struct DropArea<'a, Message, Theme, Renderer, P>
where
    P: Clone + Send,
{
    content: Element<'a, Message, Theme, Renderer>,
    shared: Shared<P>,
    tokens: Option<Tokens>,
    highlight: bool,
}

impl<'a, Message, Theme, Renderer, P> DropArea<'a, Message, Theme, Renderer, P>
where
    P: Clone + Send,
{
    /// Wraps `content` as a drop target participating in `shared`.
    /// The target highlight needs [`Tokens`]; without them the area
    /// still claims, it just draws nothing.
    pub fn new(
        content: impl Into<Element<'a, Message, Theme, Renderer>>,
        shared: Shared<P>,
    ) -> Self {
        Self {
            content: content.into(),
            shared,
            tokens: None,
            highlight: true,
        }
    }

    /// Gives the area its tokens, enabling the built-in highlight.
    #[must_use]
    pub fn tokens(mut self, tokens: Tokens) -> Self {
        self.tokens = Some(tokens);
        self
    }

    /// Turns the target highlight off (the content draws its own).
    #[must_use]
    pub fn highlight(mut self, highlight: bool) -> Self {
        self.highlight = highlight;
        self
    }
}

impl<Message, Theme, Renderer, P> Widget<Message, Theme, Renderer>
    for DropArea<'_, Message, Theme, Renderer, P>
where
    P: Clone + Send,
    Theme: iced_core::widget::text::Catalog + iced_widget::container::Catalog,
    Renderer: renderer::Renderer,
{
    fn tag(&self) -> Tag {
        Tag::stateless()
    }

    fn state(&self) -> TreeState {
        TreeState::None
    }

    fn diff(&mut self, tree: &mut Tree) {
        self.content.as_widget_mut().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
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
        // Claim: innermost (later) areas overwrite earlier claims, and
        // each area holds its claim until the gesture moves away.
        {
            let mut state = lock(&self.shared);
            if let Some(active) = state.active.as_mut() {
                let bounds = layout.bounds();
                let inner = cursor.is_over(bounds);
                if inner {
                    let highlight = Rectangle {
                        x: bounds.x,
                        y: bounds.y,
                        width: bounds.width,
                        height: bounds.height,
                    };
                    active.target = Some(Target {
                        bounds,
                        highlight,
                    });
                }
            }
        }

        self.content.as_widget_mut().update(
            tree, event, layout, cursor, renderer, shell, viewport,
        );
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
        let state = lock(&self.shared);
        let claimed = state
            .active
            .as_ref()
            .or(state.pending.as_ref())
            .and_then(|g| g.target.as_ref())
            .is_some_and(|t| t.bounds == layout.bounds());
        drop(state);

        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);

        if let Some(tokens) = self.tokens
            && claimed
            && self.highlight
        {
            let t = tokens.palette;
            renderer.fill_quad(
                renderer::Quad {
                    bounds: layout.bounds(),
                    border: Border {
                        color: t.primary,
                        width: (tokens.metrics.border.width * 2.0).max(1.0),
                        radius: tokens.metrics.radius.sm.into(),
                    },
                    ..renderer::Quad::default()
                },
                t.primary.scale_alpha(0.08),
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        self.content
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
    }
}

impl<'a, Message, Theme, Renderer, P> From<DropArea<'a, Message, Theme, Renderer, P>>
    for Element<'a, Message, Theme, Renderer>
where
    P: Clone + Send + 'a,
    Message: 'a,
    Theme: iced_core::widget::text::Catalog + iced_widget::container::Catalog + 'a,
    Renderer: renderer::Renderer + 'a,
{
    fn from(area: DropArea<'a, Message, Theme, Renderer, P>) -> Self {
        Element::new(area)
    }
}

/// The bounds of every id'd drop zone passing `filter`, in tree order.
/// `options` restricts the ids considered; `depth` bounds how deep
/// nested zones are explored. Run it as a widget operation from the
/// host runtime, or over a tree you hold.
#[must_use]
pub fn find_zones<F>(
    filter: F,
    options: Option<Vec<Id>>,
    depth: Option<usize>,
) -> FindZones<F>
where
    F: Fn(&Rectangle) -> bool + Send + 'static,
{
    FindZones {
        filter,
        options,
        zones: vec![],
        max_depth: depth,
        c_depth: 0,
        offset: Vector { x: 0.0, y: 0.0 },
        goto_next: false,
    }
}

/// The drop-zone-collecting operation [`find_zones`] builds; run it as
/// any other widget operation.
pub struct FindZones<F> {
    filter: F,
    options: Option<Vec<Id>>,
    zones: Vec<(Id, Rectangle)>,
    max_depth: Option<usize>,
    c_depth: usize,
    offset: Vector,
    goto_next: bool,
}

impl<F> Operation<Vec<(Id, Rectangle)>> for FindZones<F>
where
    F: Fn(&Rectangle) -> bool + Send + 'static,
{
    fn traverse(
        &mut self,
        operate: &mut dyn FnMut(&mut dyn Operation<Vec<(Id, Rectangle)>>),
    ) {
        if self.goto_next {
            operate(self);
        }
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if let Some(id) = id {
            let is_option = match &self.options {
                Some(options) => options.contains(id),
                None => true,
            };
            let bounds = bounds - self.offset;
            if is_option && (self.filter)(&bounds) {
                self.c_depth += 1;
                self.zones.push((id.clone(), bounds));
            }
        }
        self.goto_next = match &self.max_depth {
            Some(m_depth) => self.c_depth < *m_depth,
            None => true,
        };
    }

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        bounds: Rectangle,
        _content_bounds: Rectangle,
        translation: Vector,
        _state: &mut dyn Scrollable,
    ) {
        if (self.filter)(&bounds) {
            self.offset += translation;
        }
    }

    fn finish(&self) -> Outcome<Vec<(Id, Rectangle)>> {
        Outcome::Some(self.zones.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> Rectangle {
        Rectangle {
            x: 100.0,
            y: 100.0,
            width: 160.0,
            height: 80.0,
        }
    }

    #[test]
    fn the_choice_card_maps_rows_to_choices() {
        let b = card();
        assert_eq!(choice_at(Point::new(120.0, 100.0), b), None, "the title row");
        assert_eq!(choice_at(Point::new(120.0, 120.0), b), Some(Some(Choice::Move)));
        assert_eq!(choice_at(Point::new(120.0, 140.0), b), Some(Some(Choice::Copy)));
        assert_eq!(choice_at(Point::new(120.0, 165.0), b), Some(None), "Cancel");
        assert_eq!(choice_at(Point::new(400.0, 140.0), b), None, "outside");
    }

    #[test]
    fn release_keeps_only_targeted_gestures() {
        let mut state: State<u8> = State::default();
        let mut g = Gesture::new(7, "seven".into(), Point::new(10.0, 10.0));
        assert!(g.target.is_none());

        state.active = Some(g.clone());
        state.release();
        assert!(state.pending.is_none(), "an untargeted drag is dropped");

        g.target = Some(Target {
            bounds: card(),
            highlight: card(),
        });
        state.active = Some(g);
        state.release();
        assert!(state.pending.is_some() && state.active.is_none());
    }

    #[test]
    fn cancel_bumps_the_epoch() {
        let mut state: State<()> = State::default();
        let before = state.cancel_epoch;
        state.active = Some(Gesture::new((), String::new(), Point::ORIGIN));
        state.pending = state.active.clone();
        state.cancel();
        assert_eq!(state.cancel_epoch, before.wrapping_add(1));
        assert!(state.active.is_none() && state.pending.is_none());
    }


    #[test]
    fn labels_default_to_english() {
        let labels = Labels::english();
        assert_eq!(labels.r#move, "Move here");
        assert_eq!(labels.cancel, "Cancel");
    }

    #[allow(dead_code)]
    fn layer_and_areas_compose() {
        use iced_widget::container;
        // A compile check of the intended composition.
        fn build<'a>(
            shared: Shared<u8>,
            tokens: Tokens,
        ) -> Element<'a, u8, crate::theme::Theme, iced_widget::Renderer> {
            let source: Element<'a, u8, crate::theme::Theme, iced_widget::Renderer> =
                DragArea::new(iced_widget::text("drag me"), 1u8)
                    .label(|p| format!("item {p}"))
                    .start_directly(shared.clone())
                    .into();
            let target: Element<'a, u8, crate::theme::Theme, iced_widget::Renderer> = DropArea::new(
                container(iced_widget::text("drop here")).padding(8),
                shared.clone(),
            )
            .tokens(tokens)
            .into();
            let _ = (source, target);
            let layer: Element<'a, u8, crate::theme::Theme, iced_widget::Renderer> =
                Layer::new(iced_widget::Column::with_children(vec![iced_widget::text("hi").into()]), shared, tokens, |f| {
                    u8::from(f.choice == Choice::Move)
                })
                .into();
            layer
        }
        let _ = build;
    }
}
