// SPDX-License-Identifier: MIT OR Apache-2.0
//! A horizontal slider bar: [`SlideBar`], the plain track-and-fill of a
//! slider without iced `slider`'s handle — for volume strips and
//! seek bars where a handle would be noise.
//!
//! Dragging (mouse or touch) moves the value in `step` increments across
//! `range`; `on_change` fires per movement and `on_release` when the
//! pointer lifts. Styling comes from the theme catalog, never literals.

use std::ops::RangeInclusive;

use iced_core::layout::{Limits, Node};
use iced_core::mouse::{self, Cursor};
use iced_core::renderer;
use iced_core::touch;
use iced_core::widget::tree::{self, Tree};
use iced_core::widget::Operation;
use iced_core::{
    Background, Border, Color, Element, Event, Layout, Length, Point, Rectangle, Shell, Size,
    Widget,
};

/// Constant default height of a [`SlideBar`].
pub const DEFAULT_HEIGHT: f32 = 30.0;

/// The style of a [`SlideBar`].
#[derive(Clone, Copy, Debug)]
pub struct Style {
    /// The background (track) of the [`SlideBar`].
    pub background: Background,
    /// The fill color of the [`SlideBar`].
    pub bar: Color,
    /// The border of the [`SlideBar`].
    pub border: Border,
    /// The border radius of the [`SlideBar`].
    pub radius: f32,
}

/// The theme catalog of a [`SlideBar`].
pub trait Catalog {
    /// The item class of [`Catalog`].
    type Class<'a>;
    /// The default class produced by the [`Catalog`].
    fn default<'a>() -> Self::Class<'a>;
    /// The [`Style`] of a class.
    fn style(&self, class: &Self::Class<'_>) -> Style;
}

/// A styling function for a [`SlideBar`].
pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme) -> Style + 'a>;

/// A widget that draws a slider bar.
///
/// ```no_run
/// # use toolkit::slide_bar::SlideBar;
/// #[derive(Clone)]
/// enum Message { Seek(f32) }
///
/// fn view<'a, Theme, Renderer>() -> iced_core::Element<'a, Message, Theme, Renderer>
/// where
///     Theme: toolkit::slide_bar::Catalog + 'a,
///     Renderer: iced_core::renderer::Renderer + 'a,
/// {
///     SlideBar::new(0.0..=1.0, 0.5, Message::Seek).into()
/// }
/// ```
#[allow(missing_debug_implementations)]
pub struct SlideBar<'a, T, Message, Theme, Renderer>
where
    Message: Clone,
    Theme: Catalog,
{
    /// Width of the bar.
    pub width: Length,
    /// Height of the bar.
    pub height: Option<Length>,
    /// Value range.
    pub range: RangeInclusive<T>,
    /// Smallest value step within moveable limitations.
    step: T,
    /// Value of the bar.
    value: T,
    /// Change event of the bar when a value is modified.
    on_change: Box<dyn Fn(T) -> Message + 'a>,
    /// Release event when the mouse is released.
    on_release: Option<Message>,
    /// The style class.
    class: Theme::Class<'a>,
    #[allow(clippy::missing_docs_in_private_items)]
    _renderer: std::marker::PhantomData<Renderer>,
}

impl<'a, T, Message, Theme, Renderer> SlideBar<'a, T, Message, Theme, Renderer>
where
    T: Copy + From<u8> + PartialOrd,
    Message: Clone,
    Theme: Catalog,
{
    /// Creates a new [`SlideBar`].
    ///
    /// It expects:
    ///   * an inclusive range of possible values,
    ///   * the current value (clamped into the range),
    ///   * a function called with each new value while the bar is dragged.
    pub fn new<F>(range: RangeInclusive<T>, value: T, on_change: F) -> Self
    where
        F: 'a + Fn(T) -> Message,
    {
        let value = if value >= *range.start() {
            value
        } else {
            *range.start()
        };
        let value = if value <= *range.end() { value } else { *range.end() };

        Self {
            width: Length::Fill,
            height: None,
            step: T::from(1),
            value,
            range,
            on_change: Box::new(on_change),
            on_release: None,
            class: Theme::default(),
            _renderer: std::marker::PhantomData,
        }
    }

    /// Sets the release message of the [`SlideBar`], called when the
    /// pointer is released — the end of one interaction, for work too
    /// heavy to run per movement.
    #[must_use]
    pub fn on_release(mut self, on_release: Message) -> Self {
        self.on_release = Some(on_release);
        self
    }

    /// Sets the width of the [`SlideBar`].
    #[must_use]
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Sets the height of the [`SlideBar`].
    #[must_use]
    pub fn height(mut self, height: Option<Length>) -> Self {
        self.height = height;
        self
    }

    /// Sets the step size of the [`SlideBar`].
    #[must_use]
    pub fn step(mut self, step: impl Into<T>) -> Self {
        self.step = step.into();
        self
    }

    /// Sets the style of the [`SlideBar`].
    #[must_use]
    pub fn style(mut self, style: impl Fn(&Theme) -> Style + 'a) -> Self
    where
        Theme::Class<'a>: From<StyleFn<'a, Theme>>,
    {
        self.class = (Box::new(style) as StyleFn<'a, Theme>).into();
        self
    }

    /// Sets the style class of the [`SlideBar`].
    #[must_use]
    pub fn class(mut self, class: impl Into<Theme::Class<'a>>) -> Self {
        self.class = class.into();
        self
    }
}

impl<T, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for SlideBar<'_, T, Message, Theme, Renderer>
where
    T: Copy + Into<f64> + num_traits::FromPrimitive,
    Message: Clone,
    Renderer: renderer::Renderer,
    Theme: Catalog,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::new())
    }

    fn size(&self) -> Size<Length> {
        Size {
            width: self.width,
            height: self.height.unwrap_or(Length::Fixed(DEFAULT_HEIGHT)),
        }
    }

    fn layout(&mut self, _tree: &mut Tree, _renderer: &Renderer, limits: &Limits) -> Node {
        let limits = limits
            .width(self.width)
            .height(self.height.unwrap_or(Length::Fixed(DEFAULT_HEIGHT)));

        let size = limits.resolve(
            self.width,
            self.height.unwrap_or(Length::Fixed(DEFAULT_HEIGHT)),
            Size::ZERO,
        );

        Node::new(size)
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        _renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        update(
            event,
            layout,
            cursor,
            shell,
            tree.state.downcast_mut::<State>(),
            &mut self.value,
            &self.range,
            self.step,
            self.on_change.as_ref(),
            &self.on_release,
        );
    }

    fn operate(
        &mut self,
        _tree: &mut Tree,
        layout: Layout<'_>,
        _renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        // Register the widget's bounds for measurement.
        operation.container(None, layout.bounds());
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: Cursor,
        viewport: &Rectangle,
    ) {
        let style = theme.style(&self.class);
        draw(renderer, layout, viewport, self, &style);
    }
}

/// Processes an [`Event`] and updates the [`State`] of a [`SlideBar`]
/// accordingly.
#[allow(clippy::too_many_arguments)]
pub fn update<Message, T>(
    event: &Event,
    layout: Layout<'_>,
    cursor: Cursor,
    shell: &mut Shell<'_, Message>,
    state: &mut State,
    value: &mut T,
    range: &RangeInclusive<T>,
    step: T,
    on_change: &dyn Fn(T) -> Message,
    on_release: &Option<Message>,
) where
    T: Copy + Into<f64> + num_traits::FromPrimitive,
    Message: Clone,
{
    let is_dragging = state.is_dragging;

    let mut change = |cursor_position: Point| {
        let bounds = layout.bounds();
        let new_value = if cursor_position.x <= bounds.x {
            *range.start()
        } else if cursor_position.x >= bounds.x + bounds.width {
            *range.end()
        } else {
            let step = step.into();
            let start = (*range.start()).into();
            let end = (*range.end()).into();

            let percent = f64::from(cursor_position.x - bounds.x) / f64::from(bounds.width);

            let steps = (percent * (end - start) / step).round();
            let value = steps * step + start;

            if let Some(value) = T::from_f64(value) {
                value
            } else {
                return;
            }
        };

        if ((*value).into() - new_value.into()).abs() > f64::EPSILON {
            shell.publish((on_change)(new_value));

            *value = new_value;
        }
    };

    match event {
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
        | Event::Touch(touch::Event::FingerPressed { .. }) => {
            if let Some(cursor_position) = cursor.position_over(layout.bounds()) {
                change(cursor_position);
                state.is_dragging = true;

                shell.capture_event();
            }
        }
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
        | Event::Touch(touch::Event::FingerLifted { .. } | touch::Event::FingerLost { .. })
            if is_dragging =>
        {
            if let Some(on_release) = on_release.clone() {
                shell.publish(on_release);
            }
            state.is_dragging = false;

            shell.capture_event();
        }
        Event::Mouse(mouse::Event::CursorMoved { .. })
        | Event::Touch(touch::Event::FingerMoved { .. }) if is_dragging => {
            let _ = cursor.position().map(change);

            shell.capture_event();
        }
        _ => {}
    }
}

/// Draws a [`SlideBar`].
fn draw<T, R, Message>(
    renderer: &mut R,
    layout: Layout<'_>,
    viewport: &Rectangle,
    slider: &SlideBar<'_, T, Message>,
    style: &Style,
) where
    T: Into<f64> + Copy,
    Message: Clone,
    R: renderer::Renderer,
{
    let bounds = layout.bounds();
    let value = slider.value.into() as f32;
    let (range_start, range_end) = {
        let (start, end) = slider.range.clone().into_inner();

        (start.into() as f32, end.into() as f32)
    };

    let active_progress_bounds = if range_start >= range_end {
        Rectangle {
            width: 0.0,
            ..bounds
        }
    } else {
        Rectangle {
            width: bounds.width * (value - range_start) / (range_end - range_start),
            ..bounds
        }
    };

    if bounds.intersects(viewport) {
        renderer.fill_quad(
            renderer::Quad {
                bounds,
                border: Border {
                    radius: style.radius.into(),
                    width: style.border.width,
                    color: style.border.color,
                },
                ..Default::default()
            },
            style.background,
        );
    }

    if active_progress_bounds.intersects(viewport) {
        renderer.fill_quad(
            renderer::Quad {
                bounds: active_progress_bounds,
                border: Border {
                    radius: style.radius.into(),
                    width: 0.0,
                    color: Color::TRANSPARENT,
                },
                ..Default::default()
            },
            style.bar,
        );
    }
}

impl<'a, T, Message, Theme, Renderer> From<SlideBar<'a, T, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    T: 'a + Copy + Into<f64> + num_traits::FromPrimitive,
    Renderer: 'a + renderer::Renderer,
    Message: 'a + Clone,
    Theme: 'a + Catalog,
{
    fn from(value: SlideBar<'a, T, Message, Theme, Renderer>) -> Self {
        Self::new(value)
    }
}

/// The local state of a [`SlideBar`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct State {
    #[allow(clippy::missing_docs_in_private_items)]
    is_dragging: bool,
}

impl State {
    /// Creates a new [`State`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_renderer::LayoutRenderer;

    type Bar<'a> = SlideBar<'a, u32, Message, iced_core::Theme, LayoutRenderer>;

    #[derive(Clone, Debug)]
    enum Message {
        #[allow(dead_code)]
        Changed(u32),
    }

    #[test]
    fn the_value_is_clamped_into_the_range() {
        let slider = Bar::new(10..=100, 5, Message::Changed);
        assert_eq!(slider.value, 10);
        let slider = Bar::new(0..=50, 100, Message::Changed);
        assert_eq!(slider.value, 50);
        let slider = Bar::new(0..=100, 50, Message::Changed);
        assert_eq!(slider.value, 50);
    }

    #[test]
    fn builders_set_their_values() {
        let slider = Bar::new(0u32..=100, 50, Message::Changed)
            .step(10u32)
            .on_release(Message::Changed(0))
            .width(Length::Fixed(300.0))
            .height(Some(Length::Fixed(50.0)));
        assert_eq!(slider.step, 10);
        assert!(slider.on_release.is_some());
        assert_eq!(slider.width, Length::Fixed(300.0));
        assert_eq!(slider.height, Some(Length::Fixed(50.0)));
    }
}
