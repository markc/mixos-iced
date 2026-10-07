// SPDX-License-Identifier: MIT OR Apache-2.0
//! An indeterminate progress indicator: [`Spinner`], a dot orbiting the
//! center of the widget at a fixed rate, driving its own redraws while
//! visible.
//!
//! Use it for a wait with no known duration. For progress that can be
//! measured, iced's `progress_bar` (or a [`crate::slide_bar::SlideBar`])
//! says how far along the work is.

use iced_core::layout::{Limits, Node};
use iced_core::mouse::Cursor;
use iced_core::renderer;
use iced_core::time::{Duration, Instant};
use iced_core::widget::tree::{State, Tag};
use iced_core::widget::Tree;
use iced_core::window::{self, RedrawRequest};
use iced_core::{
    Border, Color, Element, Event, Layout, Length, Rectangle, Shell, Size, Vector, Widget,
};

/// A spinner: a circle spinning around the center of the widget.
#[allow(missing_debug_implementations)]
pub struct Spinner {
    /// The width of the [`Spinner`].
    width: Length,
    /// The height of the [`Spinner`].
    height: Length,
    /// The rate of the [`Spinner`].
    rate: Duration,
    /// The radius of the spinning circle.
    circle_radius: f32,
    /// Whether the spinner animates. Effective animation also needs a
    /// nonzero rate, nonempty bounds and viewport intersection. Hosts are
    /// expected to set this from `Prepared::reduced_motion` on each view;
    /// the widget itself never applies that preference.
    animated: bool,
}

impl Default for Spinner {
    fn default() -> Self {
        Self {
            width: Length::Fixed(20.0),
            height: Length::Fixed(20.0),
            rate: Duration::from_secs_f32(1.0),
            circle_radius: 2.0,
            animated: true,
        }
    }
}

impl Spinner {
    /// Creates a new [`Spinner`] widget.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the width of the [`Spinner`].
    #[must_use]
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Sets the height of the [`Spinner`].
    #[must_use]
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// Sets the time one orbit takes (`Duration::ZERO` freezes the dot).
    #[must_use]
    pub fn rate(mut self, rate: Duration) -> Self {
        self.rate = rate;
        self
    }

    /// Sets the radius of the orbiting circle.
    #[must_use]
    pub fn circle_radius(mut self, radius: f32) -> Self {
        self.circle_radius = radius;
        self
    }

    /// Whether the spinner drives its own animation. When false (or when
    /// the rate is zero or the bounds are empty or clipped) it draws its
    /// retained phase but schedules no future frame.
    #[must_use]
    pub fn animated(mut self, animated: bool) -> Self {
        self.animated = animated;
        self
    }
}

struct SpinnerState {
    /// The timestamp of the last advancing frame; `None` until the first
    /// active frame after creation, resume, disable or a rate change.
    last_update: Option<Instant>,
    /// The retained orbit phase, in turns (0.0..1.0).
    t: f32,
    /// The animation and rate the state last saw, so `diff` can clear the
    /// time origin when either changes.
    animated: bool,
    rate: Duration,
}

fn is_visible(bounds: &Rectangle, viewport: &Rectangle) -> bool {
    bounds.width > 0.0 && bounds.height > 0.0 && bounds.intersects(viewport)
}

fn fill_circle(renderer: &mut impl renderer::Renderer, position: Vector, radius: f32, color: Color) {
    if radius > 0. {
        renderer.fill_quad(
            renderer::Quad {
                bounds: Rectangle {
                    x: position.x,
                    y: position.y,
                    width: radius * 2.0,
                    height: radius * 2.0,
                },
                border: Border {
                    radius: radius.into(),
                    width: 0.0,
                    color: Color::TRANSPARENT,
                },
                ..renderer::Quad::default()
            },
            color,
        );
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Spinner
where
    Renderer: renderer::Renderer,
{
    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn layout(&mut self, _tree: &mut Tree, _renderer: &Renderer, limits: &Limits) -> Node {
        Node::new(
            limits
                .width(self.width)
                .height(self.height)
                .resolve(self.width, self.height, Size::new(f32::INFINITY, f32::INFINITY)),
        )
    }

    fn draw(
        &self,
        state: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();

        if !is_visible(&bounds, viewport) {
            return;
        }

        let size = if bounds.width < bounds.height {
            bounds.width
        } else {
            bounds.height
        } / 2.0;
        let state = state.state.downcast_ref::<SpinnerState>();
        let center = bounds.center();
        let distance_from_center = size - self.circle_radius;
        let (y, x) = (state.t * std::f32::consts::TAU).sin_cos();
        let position = Vector::new(
            center.x + x * distance_from_center - self.circle_radius,
            center.y + y * distance_from_center - self.circle_radius,
        );

        fill_circle(renderer, position, self.circle_radius, style.text_color);
    }

    fn tag(&self) -> Tag {
        Tag::of::<SpinnerState>()
    }

    fn state(&self) -> State {
        State::new(SpinnerState {
            last_update: None,
            t: 0.0,
            animated: self.animated,
            rate: self.rate,
        })
    }

    fn diff(&mut self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<SpinnerState>();
        if state.animated != self.animated || state.rate != self.rate {
            // Disable, resume or rate change: clear the time origin so no
            // paused, clipped or zero-rate time is integrated. The phase
            // stays for the resumed first frame.
            state.last_update = None;
            state.animated = self.animated;
            state.rate = self.rate;
        }
    }

    fn update(
        &mut self,
        state: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        _cursor: Cursor,
        _renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        const FRAMES_PER_SECOND: u64 = 60;

        let bounds = layout.bounds();

        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            let state = state.state.downcast_mut::<SpinnerState>();
            // Inactive (disabled, zero rate, or empty/clipped bounds):
            // clear the time origin and schedule nothing. A host redraw
            // queued before the change may still arrive once; no new
            // future frame comes from this widget.
            if !self.animated || self.rate == Duration::ZERO || !is_visible(&bounds, viewport) {
                state.last_update = None;
                return;
            }
            if let Some(last) = state.last_update {
                // A subsequent frame: advance by the elapsed event time.
                let duration = (*now - last).as_secs_f32();
                let increment = duration / self.rate.as_secs_f32();
                state.t = (state.t + increment) % 1.0;
            }
            // The first active frame after creation, resume or disable:
            // adopt this event's timestamp as the origin, advance zero
            // and schedule the next frame.
            state.last_update = Some(*now);
            shell.request_redraw_at(RedrawRequest::At(
                *now + Duration::from_millis(1000 / FRAMES_PER_SECOND),
            ));
        }
    }
}

impl<'a, Message, Theme, Renderer> From<Spinner> for Element<'a, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer + 'a,
{
    fn from(spinner: Spinner) -> Self {
        Self::new(spinner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_core::shell::{Bus, Waker};

    fn frame<Message: 'static>(
        spinner: &mut Spinner,
        tree: &mut Tree,
        now: Instant,
        shell: &mut Shell<'_, Message>,
    ) {
        let node = Node::new(Size::new(20.0, 20.0));
        let layout = Layout::new(&node);
        Widget::<Message, iced_core::Theme, ()>::update(
            spinner,
            tree,
            &Event::Window(window::Event::RedrawRequested(now)),
            layout,
            mouse::Cursor::Unavailable,
            &(),
            shell,
            &Rectangle::with_size(Size::new(20.0, 20.0)),
        );
    }

    fn spinner<Message: 'static>() -> (Spinner, Tree) {
        let spinner = Spinner::new();
        let mut tree = Tree::new(&spinner as &dyn Widget<Message, iced_core::Theme, ()>);
        spinner.diff(&mut tree);
        (spinner, tree)
    }

    fn shell<Message: 'static>() -> Shell<'static, Message> {
        let bus: &'static mut iced_core::shell::Bus<Message> =
            Box::leak(Box::new(iced_core::shell::Bus::new()));
        Shell::new(
            &iced_core::window::Headless,
            iced_core::shell::Waker::noop(),
            bus,
        )
    }

    #[test]
    fn defaults_are_a_small_one_second_spinner() {
        let spinner = Spinner::new();
        assert_eq!(spinner.width, Length::Fixed(20.0));
        assert_eq!(spinner.height, Length::Fixed(20.0));
        assert_eq!(spinner.rate, Duration::from_secs(1));
        assert_eq!(spinner.circle_radius, 2.0);
        assert!(spinner.animated);
    }

    #[test]
    fn the_first_active_frame_adopts_the_event_time_and_advances_zero() {
        let (mut spinner, mut tree) = spinner::<()>();
        let mut shell = shell::<()>();
        let origin = Instant::now();
        frame(&mut spinner, &mut tree, origin, &mut shell);
        assert_eq!(
            tree.state.downcast_ref::<SpinnerState>().t, 0.0,
            "the first frame advances zero"
        );
        let next = origin + Duration::from_millis(1000 / 60);
        assert!(matches!(
            shell.redraw_request(),
            RedrawRequest::At(at) if at == next
        ));
        // A later frame advances by the elapsed event time only.
        frame(&mut spinner, &mut tree, next, &mut shell);
        let t = tree.state.downcast_ref::<SpinnerState>().t;
        let expected = (next - origin).as_secs_f32() / 1.0;
        assert!((t - expected).abs() < f32::EPSILON, "{t} vs {expected}");
    }

    #[test]
    fn disabled_zero_rate_and_clipped_bounds_schedule_no_future_frame() {
        let now = Instant::now();
        // Disabled.
        let (mut spinner, mut tree) = spinner::<()>();
        let mut shell = shell::<()>();
        spinner = spinner.animated(false);
        spinner.diff(&mut tree);
        frame(&mut spinner, &mut tree, now, &mut shell);
        assert!(matches!(shell.redraw_request(), RedrawRequest::None));
        assert!(tree.state.downcast_ref::<SpinnerState>().last_update.is_none());
        // Zero rate.
        let (mut spinner, mut tree) = spinner::<()>();
        let mut shell = shell::<()>();
        spinner = spinner.rate(Duration::ZERO);
        spinner.diff(&mut tree);
        frame(&mut spinner, &mut tree, now, &mut shell);
        assert!(matches!(shell.redraw_request(), RedrawRequest::None));
        // Bounds clipped by the viewport.
        let (mut spinner, mut tree) = spinner::<()>();
        let mut shell = shell::<()>();
        let node = Node::new(Size::new(20.0, 20.0));
        Widget::<(), iced_core::Theme, ()>::update(
            &mut spinner,
            &mut tree,
            &Event::Window(window::Event::RedrawRequested(now)),
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &(),
            &mut shell,
            &Rectangle::new(
                iced_core::Point::new(100.0, 100.0),
                Size::new(20.0, 20.0),
            ),
        );
        assert!(matches!(shell.redraw_request(), RedrawRequest::None));
    }

    #[test]
    fn an_inactive_update_clears_the_origin_and_keeps_an_earlier_deadline() {
        let now = Instant::now();
        let (mut spinner, mut tree) = spinner::<()>();
        let mut shell = shell::<()>();
        // An independent earlier deadline exists.
        let deadline = now + Duration::from_millis(250);
        shell.request_redraw_at(RedrawRequest::At(deadline));
        // Advance once, then disable.
        frame(&mut spinner, &mut tree, now, &mut shell);
        let t = tree.state.downcast_ref::<SpinnerState>().t;
        spinner = spinner.animated(false);
        spinner.diff(&mut tree);
        frame(&mut spinner, &mut tree, now + Duration::from_secs(5), &mut shell);
        // The phase survives; the widget schedules nothing of its own and
        // never replaces the independently supplied earlier deadline.
        assert_eq!(tree.state.downcast_ref::<SpinnerState>().t, t);
        assert!(tree.state.downcast_ref::<SpinnerState>().last_update.is_none());
        assert!(matches!(
            shell.redraw_request(),
            RedrawRequest::At(at) if at == deadline
        ));
    }

    #[test]
    fn a_pause_never_integrates_and_resume_preserves_phase_on_its_first_frame() {
        let (mut spinner, mut tree) = spinner::<()>();
        let mut shell = shell::<()>();
        let origin = Instant::now();
        frame(&mut spinner, &mut tree, origin, &mut shell);
        let next = origin + Duration::from_millis(500);
        frame(&mut spinner, &mut tree, next, &mut shell);
        let phase = tree.state.downcast_ref::<SpinnerState>().t;
        assert!(phase > 0.0);
        // A long pause without animation: the origin clears.
        spinner = spinner.animated(false);
        spinner.diff(&mut tree);
        frame(&mut spinner, &mut tree, next + Duration::from_secs(60), &mut shell);
        // Resume: the first active frame advances zero and keeps the phase.
        spinner = spinner.animated(true);
        spinner.diff(&mut tree);
        let resumed = next + Duration::from_secs(61);
        frame(&mut spinner, &mut tree, resumed, &mut shell);
        assert_eq!(
            tree.state.downcast_ref::<SpinnerState>().t,
            phase,
            "the paused minute must not be integrated"
        );
        // Only the next frame advances, by its own elapsed time.
        frame(
            &mut spinner,
            &mut tree,
            resumed + Duration::from_millis(250),
            &mut shell,
        );
        let t = tree.state.downcast_ref::<SpinnerState>().t;
        assert!((t - (phase + 0.25) % 1.0).abs() < 1e-6, "{t}");
    }

    #[test]
    fn the_state_tag_survives_a_retained_rebuild() {
        let (mut spinner, mut tree) = spinner::<()>();
        let mut shell = shell::<()>();
        let origin = Instant::now();
        frame(&mut spinner, &mut tree, origin, &mut shell);
        let phase = tree.state.downcast_ref::<SpinnerState>().t;
        // A fresh spinner diffed into the retained tree keeps the tag and
        // therefore the phase.
        let mut rebuilt = Spinner::new();
        assert_eq!(
            Widget::<(), iced_core::Theme, ()>::tag(&rebuilt),
            Widget::<(), iced_core::Theme, ()>::tag(&spinner)
        );
        rebuilt.diff(&mut tree);
        assert_eq!(tree.state.downcast_ref::<SpinnerState>().t, phase);
    }
}
