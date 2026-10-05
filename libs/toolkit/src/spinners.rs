// SPDX-License-Identifier: MIT OR Apache-2.0
//! Indeterminate progress spinners from iced's `loading_spinners`
//! example: a [`Circular`] arc that sweeps around, and a [`Linear`] bar
//! that cycles through its track. Both drive their own redraws while
//! visible and take their colours from the inherited text colour or an
//! override.

use iced_core::layout::{Limits, Node};
use iced_core::mouse::Cursor;
use iced_core::renderer;
use iced_core::time::{Duration, Instant};
use iced_core::widget::tree::{State, Tag};
use iced_core::widget::{Tree, Widget};
use iced_core::window::{self, RedrawRequest};
use iced_core::{
    Color, Element, Event, Layout, Length, Rectangle, Shell, Size,
};

/// The default cycle time of a spinner.
const DEFAULT_PERIOD: Duration = Duration::from_millis(1200);
/// Redraw cadence while animating.
const FRAMES_PER_SECOND: u64 = 60;

/// An arc that sweeps around a circle, suggesting work in progress.
#[allow(missing_debug_implementations)]
pub struct Circular {
    width: Length,
    height: Length,
    period: Duration,
    line_width: f32,
    color: Option<Color>,
}

impl Default for Circular {
    fn default() -> Self {
        Self {
            width: Length::Fixed(24.0),
            height: Length::Fixed(24.0),
            period: DEFAULT_PERIOD,
            line_width: 2.0,
            color: None,
        }
    }
}

impl Circular {
    /// A new [`Circular`] spinner.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the width.
    #[must_use]
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Sets the height.
    #[must_use]
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// Sets the time one full revolution takes.
    #[must_use]
    pub fn period(mut self, period: Duration) -> Self {
        self.period = period;
        self
    }

    /// Sets the arc stroke width.
    #[must_use]
    pub fn line_width(mut self, line_width: f32) -> Self {
        self.line_width = line_width;
        self
    }

    /// Overrides the arc colour (the inherited text colour otherwise).
    #[must_use]
    pub fn color(mut self, color: impl Into<Color>) -> Self {
        self.color = Some(color.into());
        self
    }
}

struct SpinnerState {
    start: Instant,
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Circular
where
    Renderer: renderer::Renderer + iced_core::text::Renderer,
{
    fn tag(&self) -> Tag {
        Tag::of::<SpinnerState>()
    }

    fn state(&self) -> State {
        State::new(SpinnerState { start: Instant::now() })
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn layout(&mut self, _tree: &mut Tree, _renderer: &Renderer, limits: &Limits) -> Node {
        Node::new(
            limits
                .width(self.width)
                .height(self.height)
                .resolve(self.width, self.height, Size::new(24.0, 24.0)),
        )
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        _cursor: Cursor,
        _renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return;
        }
        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            let state = tree.state.downcast_ref::<SpinnerState>();
            // Ask for the next frame while the phase advances.
            if self.period > Duration::ZERO {
                let _ = state.start;
                shell.request_redraw_at(RedrawRequest::At(
                    *now + Duration::from_millis(1000 / FRAMES_PER_SECOND),
                ));
            }
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        if !bounds.intersects(viewport) || self.period.is_zero() {
            return;
        }

        let state = tree.state.downcast_ref::<SpinnerState>();
        let phase = state.start.elapsed().as_secs_f32() / self.period.as_secs_f32();
        let color = self.color.unwrap_or(style.text_color);

        // A dot orbiting the centre, eased in and out along the orbit —
        // the software-renderer form of iced's circular loading spinner
        // (a true stroked arc needs the canvas feature).
        let ease = (phase * std::f32::consts::PI).sin() * 0.25 + 0.75;
        let center = bounds.center();
        let radius = bounds.width.min(bounds.height) / 2.0 - self.line_width;
        let angle = phase * std::f32::consts::TAU;
        let (y, x) = angle.sin_cos();
        let dot = Rectangle {
            x: center.x + x * radius - self.line_width / 2.0,
            y: center.y + y * radius - self.line_width / 2.0,
            width: self.line_width * ease,
            height: self.line_width * ease,
        };

        renderer.fill_quad(
            renderer::Quad {
                bounds: dot,
                border: iced_core::Border {
                    radius: (self.line_width * ease / 2.0).into(),
                    width: 0.0,
                    color: Color::TRANSPARENT,
                },
                ..renderer::Quad::default()
            },
            color,
        );
    }
}

impl<'a, Message, Theme, Renderer> From<Circular> for Element<'a, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer + iced_core::text::Renderer + 'a,
{
    fn from(spinner: Circular) -> Self {
        Element::new(spinner)
    }
}

/// A bar that cycles through its track, suggesting a load in progress.
#[allow(missing_debug_implementations)]
pub struct Linear {
    width: Length,
    height: Length,
    period: Duration,
    color: Option<Color>,
    track: Option<Color>,
}

impl Default for Linear {
    fn default() -> Self {
        Self {
            width: Length::Fixed(120.0),
            height: Length::Fixed(4.0),
            period: DEFAULT_PERIOD,
            color: None,
            track: None,
        }
    }
}

impl Linear {
    /// A new [`Linear`] spinner.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the width.
    #[must_use]
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Sets the height.
    #[must_use]
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// Sets the time one traverse takes.
    #[must_use]
    pub fn period(mut self, period: Duration) -> Self {
        self.period = period;
        self
    }

    /// Overrides the bar colour (the inherited text colour otherwise).
    #[must_use]
    pub fn color(mut self, color: impl Into<Color>) -> Self {
        self.color = Some(color.into());
        self
    }

    /// Overrides the track colour.
    #[must_use]
    pub fn track(mut self, color: impl Into<Color>) -> Self {
        self.track = Some(color.into());
        self
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Linear
where
    Renderer: renderer::Renderer,
{
    fn tag(&self) -> Tag {
        Tag::of::<SpinnerState>()
    }

    fn state(&self) -> State {
        State::new(SpinnerState { start: Instant::now() })
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn layout(&mut self, _tree: &mut Tree, _renderer: &Renderer, limits: &Limits) -> Node {
        Node::new(
            limits
                .width(self.width)
                .height(self.height)
                .resolve(self.width, self.height, Size::new(120.0, 4.0)),
        )
    }

    fn update(
        &mut self,
        _tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        _cursor: Cursor,
        _renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        if layout.bounds().width <= 0.0 {
            return;
        }
        if let Event::Window(window::Event::RedrawRequested(now)) = event
            && self.period > Duration::ZERO
        {
            shell.request_redraw_at(RedrawRequest::At(
                *now + Duration::from_millis(1000 / FRAMES_PER_SECOND),
            ));
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        if !bounds.intersects(viewport) || self.period.is_zero() {
            return;
        }

        let color = self.color.unwrap_or(style.text_color);

        // The track.
        renderer.fill_quad(
            renderer::Quad {
                bounds,
                ..renderer::Quad::default()
            },
            self.track.unwrap_or(color.scale_alpha(0.25)),
        );

        // The bar: a segment that grows, then shrinks, while crossing
        // the track — the classic material linear indeterminate.
        let state = tree.state.downcast_ref::<SpinnerState>();
        let phase = state.start.elapsed().as_secs_f32() / self.period.as_secs_f32();
        // Two crossings per cycle (there and back).
        let t = (phase * 2.0).fract();
        let (head, tail) = if t < 0.5 {
            // Growing while moving right.
            (t * 1.4, t * 0.4)
        } else {
            // Shrinking while continuing right.
            let t = t - 0.5;
            (0.7 + t * 0.3, 0.4 + t * 0.6)
        };
        let (head, tail) = (head.min(1.0), tail.min(head));

        let bar = Rectangle {
            x: bounds.x + tail * bounds.width,
            width: (head - tail).max(0.05) * bounds.width,
            ..bounds
        };

        renderer.fill_quad(
            renderer::Quad {
                bounds: bar,
                ..renderer::Quad::default()
            },
            color,
        );
    }
}

impl<'a, Message, Theme, Renderer> From<Linear> for Element<'a, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer + 'a,
{
    fn from(spinner: Linear) -> Self {
        Element::new(spinner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_compact_and_animated() {
        let circular = Circular::new();
        assert_eq!(circular.width, Length::Fixed(24.0));
        assert_eq!(circular.period, DEFAULT_PERIOD);
        assert!(circular.color.is_none());

        let linear = Linear::new();
        assert_eq!(linear.height, Length::Fixed(4.0));
        assert_eq!(linear.period, DEFAULT_PERIOD);
    }
}
