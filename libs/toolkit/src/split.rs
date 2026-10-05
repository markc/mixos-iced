// SPDX-License-Identifier: MIT OR Apache-2.0
//! A two-pane split: [`Split`], two children divided by a draggable
//! handle (horizontal or vertical), with relative (`0.0..=1.0` of the
//! layout) or absolute (from the start or the end) split positions,
//! double-click reset, and minimum pane sizes read off the children's
//! own `Length` declarations.
//!
//! The handle is the file-manager grip: a slim rounded bar, the accent
//! colour while hovered or dragged, the border colour at rest.

use iced_core::Widget;
use iced_core::layout::{Layout, Limits, Node};
use iced_core::mouse::{self, Cursor};
use iced_core::renderer;
use iced_core::widget::tree::{State, Tag};
use iced_core::widget::{Operation, Tree};
use iced_core::{
    Border, Color, Element, Event, Length, Padding, Pixels, Point, Rectangle, Shell, Size,
    Vector, overlay,
};

/// The default handle width.
const DEFAULT_HANDLE_WIDTH: f32 = 4.0;
/// The extra width either side of the handle that still grabs the pointer.
const GRAB_MARGIN: f32 = 4.0;
/// The window within which two clicks count as a double click.
const DOUBLE_CLICK_MS: u64 = 500;

/// Which way the panes divide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    /// The panes sit side by side; the handle moves left and right.
    #[default]
    Horizontal,
    /// The panes stack; the handle moves up and down.
    Vertical,
}

impl Direction {
    /// `(cross, along)` for this direction from a size.
    fn select(self, width: f32, height: f32) -> (f32, f32) {
        match self {
            Direction::Horizontal => (height, width),
            Direction::Vertical => (width, height),
        }
    }

    /// `(cross, along)` for this direction from a length pair.
    fn select_length(self, width: Length, height: Length) -> (Length, Length) {
        match self {
            Direction::Horizontal => (height, width),
            Direction::Vertical => (width, height),
        }
    }
}

/// How `split_at` is measured.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Strategy {
    /// `0.0..=1.0` of the layout direction (minus the handle).
    #[default]
    Relative,
    /// Pixels from the start edge to the handle centre.
    Start,
    /// Pixels from the end edge to the handle centre.
    End,
}

/// The style of a [`Split`]'s handle.
#[derive(Clone, Copy, Debug)]
pub struct Style {
    /// The handle colour at rest.
    pub color: Color,
    /// The handle colour while hovered or dragged.
    pub active_color: Color,
    /// The handle width at rest.
    pub width: f32,
    /// The handle width while hovered or dragged.
    pub active_width: f32,
    /// The handle corner radius.
    pub radius: f32,
}

/// The theme catalog of a [`Split`].
pub trait Catalog {
    /// The item class of [`Catalog`].
    type Class<'a>;
    /// The default class produced by the [`Catalog`].
    fn default<'a>() -> Self::Class<'a>;
    /// The [`Style`] of a class.
    fn style(&self, class: &Self::Class<'_>) -> Style;
}

/// A styling function for a [`Split`].
pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme) -> Style + 'a>;

/// The interaction state of a [`Split`].
#[derive(Debug, Clone, Copy, PartialEq)]
enum Status {
    /// The handle is being dragged.
    Dragging,
    /// The handle was pressed but has not moved.
    Grabbed,
    /// A double click began on the handle.
    DoubleClicked,
    /// The pointer is over the handle.
    Hovering,
    /// Nothing.
    Idle,
}

#[derive(Debug, Clone, Copy, Default)]
struct SplitState {
    status: Status,
    last_click: Option<(iced_core::time::Instant, Point)>,
    start_layout: f32,
}

impl Default for Status {
    fn default() -> Self {
        Self::Idle
    }
}

/// A two-pane split with a draggable handle.
///
/// ```no_run
/// # use toolkit::split::Split;
/// #[derive(Clone)]
/// enum Message { Resized(f32) }
///
/// fn view<'a, Theme, Renderer>(
///     split_at: f32,
/// ) -> iced_core::Element<'a, Message, Theme, Renderer>
/// where
///     Theme: toolkit::split::Catalog + 'a,
///     Renderer: iced_core::Renderer + 'a,
/// {
///     Split::new(split_at, iced_widget::text("Start"), iced_widget::text("End"))
///         .on_drag(Message::Resized)
///         .into()
/// }
/// ```
#[allow(missing_debug_implementations)]
pub struct Split<'a, Message, Theme, Renderer>
where
    Theme: Catalog,
{
    /// The split position, per [`Strategy`].
    split_at: f32,
    children: Vec<Element<'a, Message, Theme, Renderer>>,
    on_drag: Option<Box<dyn Fn(f32) -> Message + 'a>>,
    on_drag_start: Option<Box<dyn Fn() -> Message + 'a>>,
    on_drag_end: Option<Box<dyn Fn() -> Message + 'a>>,
    on_double_click: Option<Box<dyn Fn() -> Message + 'a>>,
    direction: Direction,
    strategy: Strategy,
    handle_width: f32,
    spacing: f32,
    class: <Theme as Catalog>::Class<'a>,
}

impl<'a, Message, Theme, Renderer> Split<'a, Message, Theme, Renderer>
where
    Theme: Catalog,
{
    /// Creates a new [`Split`] from the start pane, the end pane and the
    /// split position.
    #[must_use]
    pub fn new(
        split_at: f32,
        start: impl Into<Element<'a, Message, Theme, Renderer>>,
        end: impl Into<Element<'a, Message, Theme, Renderer>>,
    ) -> Self {
        Self {
            split_at,
            children: vec![start.into(), end.into()],
            on_drag: None,
            on_drag_start: None,
            on_drag_end: None,
            on_double_click: None,
            direction: Direction::default(),
            strategy: Strategy::default(),
            handle_width: DEFAULT_HANDLE_WIDTH,
            spacing: 0.0,
            class: Theme::default(),
        }
    }

    /// Sets the message produced as the handle moves; the value is the
    /// new split position, measured per [`Strategy`]. Setting this is
    /// what makes the handle draggable.
    #[must_use]
    pub fn on_drag(mut self, on_drag: impl Fn(f32) -> Message + 'a) -> Self {
        self.on_drag = Some(Box::new(on_drag));
        self
    }

    /// Sets the message produced when a drag begins.
    #[must_use]
    pub fn on_drag_start(mut self, on_drag_start: impl Fn() -> Message + 'a) -> Self {
        self.on_drag_start = Some(Box::new(on_drag_start));
        self
    }

    /// Sets the message produced when a drag ends.
    #[must_use]
    pub fn on_drag_end(mut self, on_drag_end: impl Fn() -> Message + 'a) -> Self {
        self.on_drag_end = Some(Box::new(on_drag_end));
        self
    }

    /// Sets the message produced when the handle is double-clicked —
    /// the usual "reset the split" affordance.
    #[must_use]
    pub fn on_double_click(mut self, on_double_click: impl Fn() -> Message + 'a) -> Self {
        self.on_double_click = Some(Box::new(on_double_click));
        self
    }

    /// Sets the [`Direction`] of the [`Split`].
    #[must_use]
    pub fn direction(mut self, direction: Direction) -> Self {
        self.direction = direction;
        self
    }

    /// Sets the [`Strategy`] of the [`Split`].
    #[must_use]
    pub fn strategy(mut self, strategy: Strategy) -> Self {
        self.strategy = strategy;
        self
    }

    /// Sets the width of the [`Split`]'s handle.
    #[must_use]
    pub fn handle_width(mut self, handle_width: impl Into<Pixels>) -> Self {
        self.handle_width = handle_width.into().0;
        self
    }

    /// Sets the spacing between the [`Split`]'s handle and the panes.
    #[must_use]
    pub fn spacing(mut self, spacing: impl Into<Pixels>) -> Self {
        self.spacing = spacing.into().0;
        self
    }

    /// Sets the style of the [`Split`]'s handle.
    #[must_use]
    pub fn style(mut self, style: impl Fn(&Theme) -> Style + 'a) -> Self
    where
        <Theme as Catalog>::Class<'a>: From<StyleFn<'a, Theme>>,
    {
        self.class = (Box::new(style) as StyleFn<'a, Theme>).into();
        self
    }

    /// Sets the style class of the [`Split`]'s handle.
    #[must_use]
    pub fn class(mut self, class: impl Into<<Theme as Catalog>::Class<'a>>) -> Self {
        self.class = class.into();
        self
    }

    /// The grab bounds of the handle: the handle plus a margin either
    /// side, so a slim handle still catches the pointer.
    fn handle_bounds(&self, bounds: Rectangle, start_layout: f32) -> Rectangle {
        let cross = self.direction.select(bounds.width, bounds.height).0;

        let along = start_layout + self.spacing;
        let (x, y) = match self.direction {
            Direction::Horizontal => (along, 0.0),
            Direction::Vertical => (0.0, along),
        };
        let (x, y) = (x + bounds.x, y + bounds.y);
        let (width, height) = match self.direction {
            Direction::Horizontal => (
                self.handle_width + 2.0 * GRAB_MARGIN,
                cross,
            ),
            Direction::Vertical => (cross, self.handle_width + 2.0 * GRAB_MARGIN),
        };

        Rectangle { x, y, width, height }
    }

    fn focused(&self, state: &SplitState) -> bool {
        self.on_drag.is_some() && state.status != Status::Idle
    }
}

/// The minimum along-direction size a pane declares, from its `Length`.
fn min_along(length: Length) -> f32 {
    match length {
        Length::Fixed(min) | Length::Shrink => min.max(0.0),
        _ => 0.0,
    }
}

impl<'a, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Split<'a, Message, Theme, Renderer>
where
    Theme: Catalog + 'a,
    Renderer: renderer::Renderer,
{
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn tag(&self) -> Tag {
        Tag::of::<SplitState>()
    }

    fn state(&self) -> State {
        State::new(SplitState::default())
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(&mut self.children);
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &Limits,
    ) -> Node {
        // Each pane's declared minimum along its own axis bounds the split.
        let start_size = self.children[0].as_widget().size();
        let start_min = min_along(self.direction.select_length(start_size.width, start_size.height).1);

        let end_size = self.children[1].as_widget().size();
        let end_min = min_along(self.direction.select_length(end_size.width, end_size.height).1);

        let (cross, along) = self
            .direction
            .select(limits.max.width, limits.max.height);

        let separation = 2.0 * self.spacing + self.handle_width;
        let state = tree.state.downcast_mut::<SplitState>();
        state.start_layout = match self.strategy {
            Strategy::Relative => along * self.split_at - separation / 2.0,
            Strategy::Start => self.split_at,
            Strategy::End => along - self.split_at - separation,
        }
        .min(along - separation - end_min)
        .max(start_min)
        .max(0.0);

        let (start_cross, start_along) =
            self.direction.select(cross, state.start_layout);
        let start_limits = Limits::new(Size::ZERO, Size::new(start_cross, start_along));
        let start = self.children[0]
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, &start_limits)
            .move_to([0.0, 0.0]);

        let end_along = along - state.start_layout - separation;
        let (end_cross, end_along_size) = self.direction.select(cross, end_along);
        let end_limits = Limits::new(Size::ZERO, Size::new(end_cross, end_along_size));
        let (offset_cross, offset_along) =
            self.direction.select(0.0, state.start_layout + separation);
        let end = self.children[1]
            .as_widget_mut()
            .layout(&mut tree.children[1], renderer, &end_limits)
            .move_to([offset_cross, offset_along]);

        Node::with_children(limits.max, vec![start, end])
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
        for ((child, state), child_layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                state, event, child_layout, cursor, renderer, shell, viewport,
            );
        }

        let state = tree.state.downcast_mut::<SplitState>();

        if shell.is_event_captured() {
            return;
        }

        let bounds = layout.bounds();
        let handle_bounds = self.handle_bounds(bounds, state.start_layout);

        if let Event::Mouse(event) = event {
            match event {
                mouse::Event::ButtonPressed(mouse::Button::Left)
                    if self.on_drag.is_some() && cursor.is_over(handle_bounds) =>
                {
                    let now = iced_core::time::Instant::now();
                    let double = state
                        .last_click
                        .is_some_and(|(then, position)| {
                            (now - then).as_millis() as u64 <= DOUBLE_CLICK_MS
                                && cursor
                                    .position()
                                    .is_some_and(|p| p.distance(position) <= GRAB_MARGIN * 2.0)
                        });

                    state.last_click =
                        cursor.position().map(|position| (now, position));

                    state.status = if double {
                        Status::DoubleClicked
                    } else {
                        Status::Grabbed
                    };

                    shell.capture_event();
                }
                mouse::Event::CursorMoved { position } => {
                    if let Some(on_drag) = &self.on_drag
                        && matches!(
                            state.status,
                            Status::Dragging | Status::Grabbed | Status::DoubleClicked
                        )
                    {
                        let (x, y) = (position.x - bounds.x, position.y - bounds.y);
                        let (_, along) =
                            self.direction.select(bounds.width, bounds.height);
                        let split_at = self.direction.select(x, y).1;

                        let separation = 2.0 * self.spacing + self.handle_width;
                        let split_at = match self.strategy {
                            Strategy::Relative => split_at / along.max(1.0),
                            Strategy::Start => split_at - separation / 2.0,
                            Strategy::End => along - split_at - separation / 2.0,
                        };

                        if split_at != self.split_at {
                            if state.status != Status::Dragging {
                                state.status = Status::Dragging;
                                if let Some(on_drag_start) = &self.on_drag_start {
                                    shell.publish(on_drag_start());
                                }
                            }

                            shell.publish(on_drag(split_at));
                            shell.capture_event();
                        }
                    } else {
                        let focused = self.focused(state);
                        state.status = if cursor.is_over(handle_bounds) {
                            Status::Hovering
                        } else {
                            Status::Idle
                        };
                        if self.focused(state) != focused {
                            shell.request_redraw();
                        }
                    }
                }
                mouse::Event::ButtonReleased(mouse::Button::Left) => match state.status {
                    Status::Dragging => {
                        if let Some(on_drag_end) = &self.on_drag_end {
                            shell.publish(on_drag_end());
                            shell.capture_event();
                        }

                        state.status = if cursor.is_over(handle_bounds) {
                            Status::Hovering
                        } else {
                            Status::Idle
                        };
                    }
                    Status::DoubleClicked => {
                        if let Some(on_double_click) = &self.on_double_click {
                            shell.publish(on_double_click());
                            shell.capture_event();
                        }

                        state.status = Status::Hovering;
                    }
                    Status::Grabbed => state.status = Status::Hovering,
                    _ => {}
                },
                _ => {}
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
        for ((child, state), child_layout) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
        {
            child
                .as_widget()
                .draw(state, renderer, theme, style, child_layout, cursor, viewport);
        }

        let style = theme.style(&self.class);
        let state = tree.state.downcast_ref::<SplitState>();
        let active = state.status != Status::Idle;

        let (color, width) = if active {
            (style.active_color, style.active_width)
        } else {
            (style.color, style.width)
        };

        let bounds = layout.bounds();
        let along = state.start_layout + self.spacing + (self.handle_width - width) / 2.0;
        let (x, y) = match self.direction {
            Direction::Horizontal => (along, 0.0),
            Direction::Vertical => (0.0, along),
        };
        let (x, y) = (x + bounds.x, y + bounds.y);
        let cross = self.direction.select(bounds.width, bounds.height).0;
        let (width, height) = match self.direction {
            Direction::Horizontal => (width, cross),
            Direction::Vertical => (cross, width),
        };

        renderer.fill_quad(
            renderer::Quad {
                bounds: Rectangle { x, y, width, height },
                border: Border {
                    radius: style.radius.into(),
                    width: 0.0,
                    color: Color::TRANSPARENT,
                },
                ..renderer::Quad::default()
            },
            color,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<SplitState>();
        let handle_bounds = self.handle_bounds(layout.bounds(), state.start_layout);

        if cursor.is_over(handle_bounds) || matches!(state.status, Status::Dragging | Status::Grabbed)
        {
            match self.direction {
                Direction::Horizontal => mouse::Interaction::ResizingHorizontally,
                Direction::Vertical => mouse::Interaction::ResizingVertically,
            }
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
        self.children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
            .find_map(|((child, state), layout)| {
                child
                    .as_widget_mut()
                    .overlay(state, layout, renderer, viewport, translation)
            })
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        for ((child, state), child_layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child
                .as_widget_mut()
                .operate(state, child_layout, renderer, operation);
        }
    }
}

impl<'a, Message, Theme, Renderer> From<Split<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Theme: Catalog + 'a,
    Renderer: renderer::Renderer + 'a,
{
    fn from(split: Split<'a, Message, Theme, Renderer>) -> Self {
        Element::new(split)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_renderer::LayoutRenderer;
    use iced_widget::{Space, Text};

    type TestSplit<'a> = Split<'a, u8, iced_core::Theme, LayoutRenderer>;

    fn laid_out(split_at: f32, strategy: Strategy) -> (Node, SplitState) {
        let mut split: TestSplit = Split::new(
            split_at,
            Text::new("start"),
            Space::new().width(Length::Fill).height(Length::Fill),
        )
        .strategy(strategy);
        let renderer = LayoutRenderer::new();
        let mut element: Element<'_, u8, iced_core::Theme, LayoutRenderer> = split.into();
        let mut tree = Tree::new(&element);
        element.as_widget_mut().diff(&mut tree);
        let limits = Limits::new(Size::ZERO, Size::new(400.0, 300.0));
        let node = element.as_widget_mut().layout(&mut tree, &renderer, &limits);
        let state = tree.state.downcast_ref::<SplitState>().clone();
        (node, state)
    }

    #[test]
    fn a_relative_half_split_divides_the_space() {
        let (node, state) = laid_out(0.5, Strategy::Relative);
        // 400 wide: start pane ends at 200 minus half the handle.
        assert!((state.start_layout - 198.0).abs() < 1.0, "{}", state.start_layout);
        assert_eq!(node.children().len(), 2);
        let laid: Vec<_> = Layout::new(&node).children().collect();
        assert_eq!(laid[0].bounds().width, state.start_layout);
        assert!(
            laid[1].bounds().x >= state.start_layout + 2.0,
            "the end pane starts after the handle"
        );
        let _ = split_placeholder();
    }

    #[test]
    fn a_start_strategy_split_measures_from_the_start() {
        let (_, state) = laid_out(120.0, Strategy::Start);
        assert!((state.start_layout - 120.0).abs() < f32::EPSILON);
    }

    #[test]
    fn an_end_strategy_split_measures_from_the_end() {
        let (_, state) = laid_out(100.0, Strategy::End);
        // 400 - 100 - 4 (handle) = 296.
        assert!((state.start_layout - 296.0).abs() < 1.0, "{}", state.start_layout);
    }

    fn split_placeholder() -> TestSplit<'static> {
        Split::new(0.5, Text::new("a"), Text::new("b"))
    }
}
