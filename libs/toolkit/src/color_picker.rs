// SPDX-License-Identifier: MIT OR Apache-2.0
//! A colour picker over HSV: [`ColorPicker`] draws a [`Spectrum`] — a
//! 1-D strip (hue ramp) or a 2-D matrix (saturation × value) — picks on
//! press, drag and touch, and reports the picked [`Hsv`]. Right-click
//! picking has its own callback ([`ColorPicker::on_select_alt`]).
//!
//! The spectrum is drawn with iced's geometry API (`Frame`, cached per
//! state), so it works under both renderers with no extra feature; the
//! colours on screen are computed from the model, never literals.
//!
//! Compose it: a `saturation_value()` matrix beside a
//! `hue_vertical()` strip is the classic picker; three strips make a
//! full HSV editor.

use iced_core::layout::{self, Limits, Node};
use iced_core::mouse::{self, Cursor};
use iced_core::renderer;
use iced_core::touch;
use iced_core::widget::tree::{State as TreeState, Tag};
use iced_core::widget::{Tree, Widget};
use iced_core::{
    Color, Element, Event, Layout, Length, Point, Rectangle, Shell, Size,
};
use iced_graphics::geometry::{self, Frame, Path};

/// Hue, Saturation, Value (with alpha).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hsv {
    /// The hue component, in degrees `0.0..=360.0`.
    pub h: f32,
    /// The saturation component, `0.0..=1.0`.
    pub s: f32,
    /// The value component, `0.0..=1.0`.
    pub v: f32,
    /// The alpha component, `0.0..=1.0`.
    pub a: f32,
}

impl Default for Hsv {
    fn default() -> Self {
        Self {
            h: 0.0,
            s: 0.0,
            v: 0.0,
            a: 1.0,
        }
    }
}

/// Builds an opaque [`Hsv`].
#[must_use]
pub fn hsv(hue: f32, saturation: f32, value: f32) -> Hsv {
    hsva(hue, saturation, value, 1.0)
}

/// Builds an [`Hsv`] with alpha.
#[must_use]
pub fn hsva(hue: f32, saturation: f32, value: f32, alpha: f32) -> Hsv {
    Hsv {
        h: hue,
        s: saturation,
        v: value,
        a: alpha,
    }
}

impl From<Hsv> for Color {
    fn from(hsv: Hsv) -> Self {
        // https://en.wikipedia.org/wiki/HSL_and_HSV#Color_conversion_formulae
        let h = (hsv.h / 60.0).floor();
        let f = (hsv.h / 60.0) - h;

        let p = hsv.v * (1.0 - hsv.s);
        let q = hsv.v * (1.0 - hsv.s * f);
        let t = hsv.v * (1.0 - hsv.s * (1.0 - f));

        let h = h as u8;
        let (red, green, blue) = match h {
            1 => (q, hsv.v, p),
            2 => (p, hsv.v, t),
            3 => (p, q, hsv.v),
            4 => (t, p, hsv.v),
            5 => (hsv.v, p, q),
            _ => (hsv.v, t, p),
        };

        Self::from_rgba(
            red.clamp(0.0, 1.0),
            green.clamp(0.0, 1.0),
            blue.clamp(0.0, 1.0),
            hsv.a.clamp(0.0, 1.0),
        )
    }
}

impl From<Color> for Hsv {
    // https://en.wikipedia.org/wiki/HSL_and_HSV#Color_conversion_formulae
    fn from(Color { r, g, b, a }: Color) -> Self {
        let max = r.max(g.max(b));
        let min = r.min(g.min(b));

        let h = if (max - min).abs() < f32::EPSILON {
            0.0
        } else if (max - r).abs() < f32::EPSILON {
            60.0 * (0.0 + (g - b) / (max - min))
        } else if (max - g).abs() < f32::EPSILON {
            60.0 * (2.0 + (b - r) / (max - min))
        } else {
            60.0 * (4.0 + (r - g) / (max - min))
        };

        let h = if h < 0.0 { h + 360.0 } else { h } % 360.0;

        let s = if max == 0.0 { 0.0 } else { (max - min) / max };

        let v = max;

        Self { h, s, v, a }
    }
}

impl Hsv {
    /// From an `[r, g, b, a]` byte array.
    #[must_use]
    pub fn from_rgba8(rgba: impl Into<[u8; 4]>) -> Self {
        let [r, g, b, a] = rgba.into();

        Self::from(Color::from_rgba8(r, g, b, a as f32 / 255.0))
    }

    /// From an `[r, g, b]` byte array.
    #[must_use]
    pub fn from_rgb8(rgb: impl Into<[u8; 3]>) -> Self {
        let [r, g, b] = rgb.into();

        Self::from(Color::from_rgb8(r, g, b))
    }

    /// From an `[r, g, b, a]` float array.
    #[must_use]
    pub fn from_rgba(rgba: impl Into<[f32; 4]>) -> Self {
        Self::from(Color::from(rgba.into()))
    }

    /// From an `[r, g, b]` float array.
    #[must_use]
    pub fn from_rgb(rgb: impl Into<[f32; 3]>) -> Self {
        Self::from(Color::from(rgb.into()))
    }

    /// As an `[r, g, b, a]` float array.
    #[must_use]
    pub fn to_rgba(self) -> [f32; 4] {
        let Color { r, g, b, a } = Color::from(self);
        [r, g, b, a]
    }

    /// As an `[r, g, b]` float array.
    #[must_use]
    pub fn to_rgb(self) -> [f32; 3] {
        let Color { r, g, b, .. } = Color::from(self);
        [r, g, b]
    }

    /// As an `[r, g, b, a]` byte array.
    #[must_use]
    pub fn to_rgba8(self) -> [u8; 4] {
        let Color { r, g, b, a } = Color::from(self);
        [to_u8(r), to_u8(g), to_u8(b), to_u8(a)]
    }

    /// As an `[r, g, b]` byte array.
    #[must_use]
    pub fn to_rgb8(self) -> [u8; 3] {
        let Color { r, g, b, .. } = Color::from(self);
        [to_u8(r), to_u8(g), to_u8(b)]
    }
}

fn to_u8(v: f32) -> u8 {
    (v * u8::MAX as f32).round() as u8
}

impl<'a> From<&'a Hsv> for Hsv {
    fn from(hsv: &'a Hsv) -> Self {
        *hsv
    }
}

impl<'a> From<&'a Color> for Hsv {
    fn from(color: &'a Color) -> Self {
        Self::from(*color)
    }
}

/// One axis of the [`Hsv`] space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    /// The hue axis.
    Hue,
    /// The saturation axis.
    Saturation,
    /// The value axis.
    Value,
}

impl Component {
    /// This component of `hsv`.
    #[must_use]
    pub fn get(self, hsv: Hsv) -> f32 {
        match self {
            Component::Hue => hsv.h,
            Component::Saturation => hsv.s,
            Component::Value => hsv.v,
        }
    }

    /// A new [`Hsv`] with this component set from a `0.0..=1.0`
    /// percentage.
    #[must_use]
    pub fn update_percentage(self, hsv: Hsv, percentage: f32) -> Hsv {
        match self {
            Component::Hue => Hsv {
                h: percentage * 360.0,
                ..hsv
            },
            Component::Saturation => Hsv {
                s: percentage,
                ..hsv
            },
            Component::Value => Hsv {
                v: 1.0 - percentage,
                ..hsv
            },
        }
    }

    /// This component of `hsv` as a `0.0..=1.0` percentage.
    #[must_use]
    pub fn get_percentage(self, hsv: Hsv) -> f32 {
        match self {
            Component::Hue => hsv.h / 360.0,
            Component::Saturation => hsv.s,
            Component::Value => 1.0 - hsv.v,
        }
    }

    /// The same hue at full saturation and value, so a hue ramp shows
    /// the hue itself rather than a tint of the current colour.
    #[must_use]
    pub fn preserve_hue(self, hsv: Hsv) -> Hsv {
        match self {
            Component::Hue => Hsv {
                s: 1.0,
                v: 1.0,
                ..hsv
            },
            _ => hsv,
        }
    }
}

/// What a [`ColorPicker`] draws: a component along one axis, or two as a
/// matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spectrum {
    /// The component varies left to right.
    Horizontal(Component),
    /// The component varies top to bottom.
    Vertical(Component),
    /// `x` varies left to right, `y` top to bottom.
    Matrix {
        /// The horizontal component.
        x: Component,
        /// The vertical component.
        y: Component,
    },
}

impl Default for Spectrum {
    /// Saturation over value: the classic picker field.
    fn default() -> Self {
        Spectrum::Matrix {
            x: Component::Saturation,
            y: Component::Value,
        }
    }
}

impl Spectrum {
    /// A vertical strip of `component`.
    #[must_use]
    pub fn vertical(component: Component) -> Self {
        Spectrum::Vertical(component)
    }

    /// A horizontal strip of `component`.
    #[must_use]
    pub fn horizontal(component: Component) -> Self {
        Spectrum::Horizontal(component)
    }

    /// A matrix of two components.
    #[must_use]
    pub fn matrix(x: Component, y: Component) -> Self {
        Spectrum::Matrix { x, y }
    }

    /// Draws the spectrum into `frame` around `color`.
    pub fn draw<Renderer>(self, frame: &mut Frame<Renderer>, color: Hsv)
    where
        Renderer: geometry::Renderer,
    {
        let cols = frame.width() as usize;
        let rows = frame.height() as usize;

        match self {
            Spectrum::Horizontal(component) => {
                for col in 0..cols {
                    let percentage = col as f32 / frame.width();
                    let new_color = component.update_percentage(color, percentage);

                    frame.fill_rectangle(
                        Point::new(col as f32, 0.0),
                        Size::new(1.0, frame.height()),
                        Color::from(component.preserve_hue(new_color)),
                    );
                }
            }
            Spectrum::Vertical(component) => {
                for row in 0..rows {
                    let percentage = row as f32 / frame.height();
                    let new_color = component.update_percentage(color, percentage);

                    frame.fill_rectangle(
                        Point::new(0.0, row as f32),
                        Size::new(frame.width(), 1.0),
                        Color::from(component.preserve_hue(new_color)),
                    );
                }
            }

            Spectrum::Matrix { x, y } => {
                // One rectangle per 2×2 quantum: high enough resolution
                // for a smooth gradient, a quarter of the fills.
                const QUANTIZATION: usize = 2;

                for col in 0..(cols / QUANTIZATION) {
                    for row in 0..(rows / QUANTIZATION) {
                        let c = col as f32 * QUANTIZATION as f32;
                        let r = row as f32 * QUANTIZATION as f32;

                        let col_percent = c / frame.width();
                        let row_percent = r / frame.height();

                        let step_1 = x.update_percentage(color, col_percent);
                        let new_color = y.update_percentage(step_1, row_percent);

                        frame.fill_rectangle(
                            Point::new(c, r),
                            Size::new(QUANTIZATION as f32, QUANTIZATION as f32),
                            Color::from(new_color),
                        );
                    }
                }
            }
        }
    }

    /// Where the marker sits for `color` inside `bounds`.
    #[must_use]
    pub fn get_marker_position(self, color: Hsv, bounds: Size) -> Point {
        let Point { x, y } = match self {
            Spectrum::Horizontal(component) => Point {
                x: component.get_percentage(color),
                y: 0.5,
            },
            Spectrum::Vertical(component) => Point {
                x: 0.5,
                y: component.get_percentage(color),
            },
            Spectrum::Matrix { x, y } => Point {
                x: x.get_percentage(color),
                y: y.get_percentage(color),
            },
        };

        Point::new(x * bounds.width, y * bounds.height)
    }

    /// Whether moving from `old_color` to `new_color` changes what this
    /// spectrum draws (its own components only).
    #[must_use]
    pub fn requires_redraw(self, old_color: Hsv, new_color: Hsv) -> bool {
        match self {
            Spectrum::Horizontal(component) | Spectrum::Vertical(component) => {
                component.get(old_color) != component.get(new_color)
            }
            Spectrum::Matrix { x, y } => {
                x.get(old_color) != x.get(new_color) || y.get(old_color) != y.get(new_color)
            }
        }
    }

    /// The hue preserved at full saturation and value — applied only to
    /// 1-D spectra, so a hue ramp shows true hues.
    #[must_use]
    pub fn preserve_hue(self, color: Hsv) -> Hsv {
        match self {
            Spectrum::Horizontal(component) | Spectrum::Vertical(component) => {
                component.preserve_hue(color)
            }
            _ => color,
        }
    }

    /// The [`Hsv`] at `cursor` inside `bounds`, starting from `color`.
    #[must_use]
    pub fn fetch_hsv(self, color: Hsv, bounds: Rectangle, cursor: Point) -> Hsv {
        let iced_core::Vector { x, y } = cursor - bounds.position();

        let col_percent = (x / bounds.width).clamp(0.0, 1.0);
        let row_percent = (y / bounds.height).clamp(0.0, 1.0);

        match self {
            Spectrum::Horizontal(component) => component.update_percentage(color, col_percent),
            Spectrum::Vertical(component) => component.update_percentage(color, row_percent),
            Spectrum::Matrix { x, y } => {
                let step_1 = x.update_percentage(color, col_percent);
                y.update_percentage(step_1, row_percent)
            }
        }
    }
}

/// A matrix of saturation (x) over value (y): the classic picker field.
#[must_use]
pub fn saturation_value() -> Spectrum {
    Spectrum::matrix(Component::Saturation, Component::Value)
}

/// A vertical hue ramp.
#[must_use]
pub fn hue_vertical() -> Spectrum {
    Spectrum::vertical(Component::Hue)
}

/// A horizontal hue ramp.
#[must_use]
pub fn hue_horizontal() -> Spectrum {
    Spectrum::horizontal(Component::Hue)
}

/// The shape of the picker's marker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MarkerShape {
    /// A square marker.
    Square {
        /// The square's side.
        size: f32,
        /// The outline's width.
        border_width: f32,
    },
    /// A circular marker.
    Circle {
        /// The circle's radius.
        radius: f32,
        /// The outline's width.
        border_width: f32,
    },
}

/// The style of a [`ColorPicker`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// The marker's shape.
    pub marker_shape: MarkerShape,
    /// Preserve the hue at full saturation/value on 1-D spectra, so a
    /// hue ramp shows true hues.
    pub preserve_hue: bool,
}

/// The theme catalog of a [`ColorPicker`].
pub trait Catalog {
    /// The item class of [`Catalog`].
    type Class<'a>;
    /// The default class produced by the [`Catalog`].
    fn default<'a>() -> Self::Class<'a>;
    /// The [`Style`] of a class.
    fn style(&self, class: &Self::Class<'_>) -> Style;
}

/// A styling function for a [`ColorPicker`].
pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme) -> Style + 'a>;

/// Creates a [`ColorPicker`] over the current colour, producing a
/// message on every pick.
#[must_use]
pub fn color_picker<'a, Message, Theme, FromColor>(
    color: impl Into<Hsv>,
    on_select: impl Fn(FromColor) -> Message + 'a,
) -> ColorPicker<'a, Message, Theme>
where
    Message: 'a,
    Theme: Catalog + 'a,
    FromColor: From<Hsv> + 'a,
{
    ColorPicker::new(color, move |color| on_select(color.into()))
}

/// A widget that picks colours from a [`Spectrum`].
///
/// ```no_run
/// # use toolkit::color_picker::{self, Hsv};
/// #[derive(Clone)]
/// enum Message { Picked(Hsv) }
///
/// fn view<'a, Theme, Renderer>(color: Hsv) -> iced_core::Element<'a, Message, Theme, Renderer>
/// where
///     Theme: toolkit::color_picker::Catalog + 'a,
///     Renderer: iced_graphics::geometry::Renderer + 'static,
/// {
///     color_picker::color_picker(color, Message::Picked)
///         .spectrum(color_picker::saturation_value())
///         .into()
/// }
/// ```
#[allow(missing_debug_implementations)]
pub struct ColorPicker<'a, Message, Theme>
where
    Message: 'a,
    Theme: Catalog,
{
    color: Hsv,
    width: Length,
    height: Length,
    on_select: Box<dyn Fn(Hsv) -> Message + 'a>,
    on_select_alt: Option<Box<dyn Fn(Hsv) -> Message + 'a>>,
    spectrum: Spectrum,
    class: Theme::Class<'a>,
}

impl<'a, Message, Theme> ColorPicker<'a, Message, Theme>
where
    Theme: Catalog,
{
    /// Creates a new [`ColorPicker`] over the current colour, producing
    /// a message on every pick (press, drag, or touch).
    #[must_use]
    pub fn new(color: impl Into<Hsv>, on_select: impl Fn(Hsv) -> Message + 'a) -> Self {
        Self {
            color: color.into(),
            width: Length::Fill,
            height: Length::Fill,
            on_select: Box::new(on_select),
            on_select_alt: None,
            spectrum: Spectrum::default(),
            class: Theme::default(),
        }
    }

    /// Sets the [`Spectrum`] displayed.
    #[must_use]
    pub fn spectrum(mut self, spectrum: Spectrum) -> Self {
        self.spectrum = spectrum;
        self
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

    /// Sets the callback for picks with the right mouse button.
    #[must_use]
    pub fn on_select_alt<FromColor: From<Hsv>>(
        mut self,
        on_select_alt: impl Fn(FromColor) -> Message + 'a,
    ) -> Self {
        self.on_select_alt = Some(Box::new(move |color| on_select_alt(color.into())));
        self
    }

    /// Sets the [`Style`].
    #[must_use]
    pub fn style(mut self, style: impl Fn(&Theme) -> Style + 'a) -> Self
    where
        Theme::Class<'a>: From<StyleFn<'a, Theme>>,
    {
        self.class = (Box::new(style) as StyleFn<'a, Theme>).into();
        self
    }

    /// Sets the style class.
    #[must_use]
    pub fn class(mut self, class: impl Into<Theme::Class<'a>>) -> Self {
        self.class = class.into();
        self
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for ColorPicker<'_, Message, Theme>
where
    Theme: Catalog,
    Renderer: geometry::Renderer + 'static,
{
    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn tag(&self) -> Tag {
        Tag::of::<PickerState<Renderer>>()
    }

    fn state(&self) -> State {
        TreeState::new(PickerState::<Renderer>::default())
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &Limits,
    ) -> Node {
        layout::atomic(limits, self.width, self.height)
    }

    fn mouse_interaction(
        &self,
        _state: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::default()
        }
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
        let State {
            spectrum_cache,
            pressed,
            current_color,
            marker_cache,
        }: &mut PickerState<Renderer> = tree.state.downcast_mut();

        let cursor_in_bounds = cursor.is_over(layout.bounds());
        let bounds = layout.bounds();

        if diff(
            self.spectrum,
            spectrum_cache,
            marker_cache,
            current_color,
            self.color,
        ) {
            shell.request_redraw();
        }

        match event {
            Event::Mouse(mouse_event) => match mouse_event {
                mouse::Event::ButtonReleased(mouse_button) => match (mouse_button, *pressed) {
                    (mouse::Button::Left, Some(Pressed::Primary)) => *pressed = None,
                    (mouse::Button::Right, Some(Pressed::Secondary)) => *pressed = None,
                    _ => (),
                },
                mouse::Event::ButtonPressed(mouse_button)
                    if cursor_in_bounds && pressed.is_none() =>
                {
                    let Some(cursor) = cursor.position() else {
                        return;
                    };

                    let (new_pressed, on_select) = match mouse_button {
                        mouse::Button::Left => {
                            (Pressed::Primary, Some(self.on_select.as_ref()))
                        }
                        mouse::Button::Right => (Pressed::Secondary, self.on_select_alt.as_deref()),
                        _ => return,
                    };

                    if let Some(on_select) = on_select {
                        *pressed = Some(new_pressed);

                        let new_color = self.spectrum.fetch_hsv(*current_color, bounds, cursor);
                        shell.publish(on_select(new_color));
                    }
                }
                mouse::Event::CursorMoved { .. } => {
                    if let Some(cursor) = cursor.position()
                        && let Some(cursor_down) = pressed
                    {
                        let new_color = self.spectrum.fetch_hsv(*current_color, bounds, cursor);

                        match cursor_down {
                            Pressed::Primary => shell.publish((self.on_select)(new_color)),
                            Pressed::Secondary => {
                                if let Some(on_select_alt) = &self.on_select_alt {
                                    shell.publish(on_select_alt(new_color));
                                }
                            }
                            _ => (),
                        }
                    }
                }
                _ => (),
            },
            Event::Touch(touch_event) => match touch_event {
                touch::Event::FingerPressed { id, position } => {
                    if bounds.contains(*position) && pressed.is_none() {
                        *pressed = Some(Pressed::Finger(id.0));

                        let new_color = self.spectrum.fetch_hsv(*current_color, bounds, *position);
                        shell.publish((self.on_select)(new_color));
                    }
                }
                touch::Event::FingerMoved { id, position } => {
                    if let Some(Pressed::Finger(finger_id)) = *pressed
                        && id.0 == finger_id
                    {
                        let new_color = self.spectrum.fetch_hsv(*current_color, bounds, *position);
                        shell.publish((self.on_select)(new_color));
                    }
                }
                touch::Event::FingerLifted { id, .. } => {
                    if let Some(Pressed::Finger(finger_id)) = *pressed
                        && id.0 == finger_id
                    {
                        *pressed = None;
                    }
                }
                _ => (),
            },

            _ => (),
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: Cursor,
        _viewport: &Rectangle,
    ) {
        use iced_core::Renderer as _;

        let State {
            spectrum_cache,
            marker_cache,
            current_color,
            ..
        }: &PickerState<Renderer> = tree.state.downcast_ref();

        let Style {
            marker_shape,
            preserve_hue,
        } = theme.style(&self.class);

        let bounds = layout.bounds();
        let size = layout.bounds().size();

        renderer.with_layer(bounds, |renderer| {
            renderer.with_translation(bounds.position() - Point::ORIGIN, |renderer| {
                let spectrum = spectrum_cache.draw(renderer, size, |frame| {
                    self.spectrum.draw(frame, *current_color)
                });

                let marker = marker_cache.draw(renderer, size, |frame| {
                    draw_marker(frame, self.spectrum, *current_color, size, preserve_hue, marker_shape);
                });

                renderer.draw_geometry(spectrum);
                renderer.draw_geometry(marker);
            });
        });
    }
}

impl<'a, Message, Theme, Renderer> From<ColorPicker<'a, Message, Theme>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: Catalog + 'a,
    Renderer: geometry::Renderer + 'static,
{
    fn from(picker: ColorPicker<'a, Message, Theme>) -> Self {
        Element::new(picker)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Pressed {
    Primary,
    Secondary,
    Finger(u64),
}

/// The picker's cached state: geometry caches, the pointer, and the
/// colour the caches were built around.
struct PickerState<Renderer: geometry::Renderer> {
    spectrum_cache: geometry::Cache<Renderer>,
    marker_cache: geometry::Cache<Renderer>,
    pressed: Option<Pressed>,
    current_color: Hsv,
}

impl<Renderer: geometry::Renderer> Default for PickerState<Renderer> {
    fn default() -> Self {
        Self {
            spectrum_cache: geometry::Cache::default(),
            marker_cache: geometry::Cache::default(),
            pressed: None,
            current_color: Hsv::default(),
        }
    }
}

/// Draws the marker into `frame`: the picked colour (hue preserved on
/// 1-D spectra) with an outline from the opposite achromatic pole of
/// the picked colour's own space — a function of the colour, not a
/// theme literal.
fn draw_marker<Renderer>(
    frame: &mut Frame<Renderer>,
    spectrum: Spectrum,
    current_color: Hsv,
    bounds: Size,
    preserve_hue: bool,
    shape: MarkerShape,
) where
    Renderer: geometry::Renderer,
{
    let color = Color::from(if preserve_hue {
        spectrum.preserve_hue(current_color)
    } else {
        current_color
    });
    let position = spectrum.get_marker_position(current_color, bounds);

    // s = 0 walks Hsv's achromatic axis: v = 0 is black, v = 1 is
    // white. The outline takes the pole away from the colour's own
    // luminance, so it always contrasts.
    let outline = Color::from(Hsv {
        s: 0.0,
        v: if color.relative_luminance() > 0.5 {
            0.0
        } else {
            1.0
        },
        h: 0.0,
        a: 1.0,
    });

    match shape {
        MarkerShape::Square { size, border_width } => {
            let size = size.max(0.0);
            let border_width = border_width.max(0.0);

            frame.fill_rectangle(
                Point::new(
                    position.x - (size / 2.0) - border_width,
                    position.y - (size / 2.0) - border_width,
                ),
                Size::new(size + (border_width * 2.0), size + (border_width * 2.0)),
                outline,
            );

            frame.fill_rectangle(
                Point::new(position.x - (size / 2.0), position.y - (size / 2.0)),
                Size::new(size, size),
                color,
            );
        }
        MarkerShape::Circle {
            radius,
            border_width,
        } => {
            let radius = radius.max(0.0);
            let border_width = border_width.max(0.0);

            frame.fill(&Path::circle(position, radius + border_width), outline);
            frame.fill(&Path::circle(position, radius), color);
        }
    }
}

/// Moves `current_color` to `new_color`, clearing the caches it changes;
/// returns whether a redraw is needed.
fn diff<Renderer>(
    spectrum: Spectrum,
    canvas_cache: &geometry::Cache<Renderer>,
    cursor_cache: &geometry::Cache<Renderer>,
    current_color: &mut Hsv,
    new_color: Hsv,
) -> bool
where
    Renderer: geometry::Renderer,
{
    let redraw = spectrum.requires_redraw(*current_color, new_color);

    if new_color != *current_color {
        *current_color = new_color;
        canvas_cache.clear();
        cursor_cache.clear();
    }

    redraw
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn hsv_and_color_round_trip() {
        for hsv in [
            hsv(0.0, 0.0, 0.0),
            hsv(0.0, 0.0, 1.0),
            hsv(120.0, 1.0, 1.0),
            hsv(240.0, 0.5, 0.5),
            hsv(359.0, 0.8, 0.9),
        ] {
            let color = Color::from(hsv);
            let back = Hsv::from(color);
            let back = Hsv { a: 1.0, ..back };
            assert!(close(hsv.h, back.h), "hue {hsv:?} -> {color:?} -> {back:?}");
            assert!(close(hsv.s, back.s), "sat {hsv:?} -> {color:?} -> {back:?}");
            assert!(close(hsv.v, back.v), "val {hsv:?} -> {color:?} -> {back:?}");
        }
    }

    #[test]
    fn hue_ramp_pure_ends() {
        assert_eq!(Color::from(hsv(0.0, 1.0, 1.0)), Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 });
        assert_eq!(Color::from(hsv(120.0, 1.0, 1.0)), Color { r: 0.0, g: 1.0, b: 0.0, a: 1.0 });
        assert_eq!(Color::from(hsv(240.0, 1.0, 1.0)), Color { r: 0.0, g: 0.0, b: 1.0, a: 1.0 });
    }

    #[test]
    fn fetch_reads_the_matrix_both_ways() {
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        let spectrum = saturation_value();
        let base = hsv(210.0, 0.5, 0.5);

        let top_left = spectrum.fetch_hsv(base, bounds, Point::new(0.0, 0.0));
        assert!(close(top_left.s, 0.0));
        assert!(close(top_left.v, 1.0), "y=0 is full value");

        let bottom_right = spectrum.fetch_hsv(base, bounds, Point::new(100.0, 100.0));
        assert!(close(bottom_right.s, 1.0));
        assert!(close(bottom_right.v, 0.0), "y=1 is zero value");
        assert!(close(bottom_right.h, 210.0), "hue untouched");

        // Out-of-bounds cursors clamp.
        let beyond = spectrum.fetch_hsv(base, bounds, Point::new(150.0, -20.0));
        assert!(close(beyond.s, 1.0));
        assert!(close(beyond.v, 1.0));
    }

    #[test]
    fn marker_position_matches_percentages() {
        let bounds = Size::new(200.0, 100.0);
        let color = hsv(72.0, 0.25, 0.8);
        let position = hue_horizontal().get_marker_position(color, bounds);
        assert!(close(position.x, 72.0 / 360.0 * 200.0));
        assert!(close(position.y, 50.0), "a strip centres on its cross axis");
    }

    #[test]
    fn redraw_only_for_the_spectrums_own_components() {
        let matrix = saturation_value();
        let base = hsv(100.0, 0.4, 0.6);
        assert!(!matrix.requires_redraw(base, hsv(101.0, 0.4, 0.6)), "hue is not drawn");
        assert!(matrix.requires_redraw(base, hsv(100.0, 0.5, 0.6)), "saturation is");
        assert!(matrix.requires_redraw(base, hsv(100.0, 0.4, 0.7)), "value is");

        let ramp = hue_vertical();
        assert!(ramp.requires_redraw(base, hsv(102.0, 0.4, 0.6)));
        assert!(!ramp.requires_redraw(base, hsv(100.0, 0.9, 0.1)));
    }

    #[test]
    fn to_rgba8_round_trips_bytes() {
        let color = hsv(300.0, 0.5, 1.0);
        let rgba8 = color.to_rgba8();
        let back = Hsv::from_rgba8(rgba8);
        assert!(close(Color::from(color).r, Color::from(back).r));
        assert!(close(Color::from(color).g, Color::from(back).g));
        assert!(close(Color::from(color).b, Color::from(back).b));
    }
}
