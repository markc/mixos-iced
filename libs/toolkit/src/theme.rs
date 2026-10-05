// SPDX-License-Identifier: MIT OR Apache-2.0
//! A complete iced theme derived from [`Tokens`].
//!
//! [`Theme`] wraps a [`Tokens`] value and implements iced's `theme::Base`
//! plus the `Catalog` of every built-in widget a desktop application uses
//! (button, text input, checkbox, radio, toggler, slider, pick list and its
//! menu, combo box, scrollable, container, progress bar, rule, pane grid,
//! text editor, text, svg, table, float) and the [`Catalog`] of this crate's
//! own widgets (menu, audio controls). An application that uses `Theme` as
//! its iced theme type gets the whole look from one `Tokens` value; every
//! widget still takes a per-instance style closure or class.
//!
//! The styles are pure functions of the tokens, computed on every call, so
//! replacing the tokens restyles every widget on the next frame. The theme's
//! name carries a fingerprint of the tokens, which is how widgets that cache
//! by theme name (iced's text editor highlighter) notice the change.
//!
//! Every colour here comes from the palette: a derived shade is a mix of two
//! palette colours, never a literal. `tests/colours.rs` enforces it.

use std::hash::{Hash, Hasher};

use iced_core::border::{self, Border};
use iced_core::theme::{self, Mode, palette::Seed};
use iced_core::{Color, Shadow, Vector};

use crate::{AudioStyle, MenuStyle, Metrics, Palette, Tokens};

/// An iced theme made of [`Tokens`].
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    tokens: Tokens,
    name: String,
    labelled: bool,
}

impl Theme {
    /// A theme named by a fingerprint of `tokens`, so two themes with
    /// different tokens never share a name.
    pub fn new(tokens: Tokens) -> Self {
        Self {
            tokens,
            name: fingerprint_name(&tokens),
            labelled: false,
        }
    }

    /// A theme with a label of the application's choosing (for a theme
    /// picker). The label is the theme's name as iced sees it, so give
    /// distinct tokens distinct labels.
    pub fn named(tokens: Tokens, label: impl Into<String>) -> Self {
        Self {
            tokens,
            name: label.into(),
            labelled: true,
        }
    }

    /// `Tokens::dark()`.
    pub fn dark() -> Self {
        Self::new(Tokens::dark())
    }

    /// `Tokens::light()`.
    pub fn light() -> Self {
        Self::new(Tokens::light())
    }

    pub fn tokens(&self) -> Tokens {
        self.tokens
    }

    pub fn palette(&self) -> Palette {
        self.tokens.palette
    }

    pub fn metrics(&self) -> Metrics {
        self.tokens.metrics
    }

    /// Replaces the tokens. A fingerprint name is recomputed; a label is
    /// kept.
    pub fn set_tokens(&mut self, tokens: Tokens) {
        self.tokens = tokens;
        if !self.labelled {
            self.name = fingerprint_name(&tokens);
        }
    }

    /// Whether the surface is darker than the text on it.
    pub fn is_dark(&self) -> bool {
        let palette = self.tokens.palette;
        palette.surface.relative_luminance() < palette.text.relative_luminance()
    }

    /// The seed of an equivalent iced built-in theme: surface, text and
    /// primary as they are; success from primary, warning from the
    /// selection colour, danger from destructive.
    pub fn iced_seed(&self) -> Seed {
        let palette = self.tokens.palette;
        Seed {
            background: palette.surface,
            text: palette.text,
            primary: palette.primary,
            success: palette.primary,
            warning: palette.selection,
            danger: palette.destructive,
        }
    }

    /// An `iced_core::Theme` generated from [`Theme::iced_seed`], for
    /// third-party widgets that style themselves from iced's theme type
    /// (wrap them in iced's `themer`).
    pub fn to_iced(&self) -> iced_core::Theme {
        iced_core::Theme::custom(self.name.clone(), self.iced_seed())
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl From<Tokens> for Theme {
    fn from(tokens: Tokens) -> Self {
        Self::new(tokens)
    }
}

impl std::fmt::Display for Theme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

impl theme::Base for Theme {
    fn default(preference: Mode) -> Self {
        match preference {
            Mode::Light => Self::light(),
            Mode::None | Mode::Dark => Self::dark(),
        }
    }

    fn mode(&self) -> Mode {
        if self.is_dark() {
            Mode::Dark
        } else {
            Mode::Light
        }
    }

    fn base(&self) -> theme::Style {
        theme::Style {
            background_color: self.tokens.palette.surface,
            text_color: self.tokens.palette.text,
        }
    }

    fn seed(&self) -> Option<Seed> {
        Some(self.iced_seed())
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Every colour channel and metric of the tokens, as bits, so equal tokens
/// hash equal and any change moves the name.
pub fn fingerprint(tokens: &Tokens) -> u64 {
    let p = tokens.palette;
    let m = tokens.metrics;
    let colours = [
        p.surface,
        p.text,
        p.popover,
        p.popover_text,
        p.elevated,
        p.elevated_text,
        p.card,
        p.card_text,
        p.primary,
        p.primary_text,
        p.destructive,
        p.destructive_text,
        p.muted_surface,
        p.muted_text,
        p.selection,
        p.selection_text,
        p.border,
        p.input,
        p.ring,
    ];
    let sizes = [
        m.spacing.xs,
        m.spacing.sm,
        m.spacing.md,
        m.spacing.lg,
        m.spacing.xl,
        m.radius.sm,
        m.radius.md,
        m.radius.lg,
        m.border.width,
        m.border.focus_width,
        m.text.xs,
        m.text.sm,
        m.text.md,
        m.text.lg,
        m.text.xl,
        m.text.xxl,
    ];
    let mut hasher = std::hash::DefaultHasher::new();
    for colour in colours {
        for channel in [colour.r, colour.g, colour.b, colour.a] {
            channel.to_bits().hash(&mut hasher);
        }
    }
    for size in sizes {
        size.to_bits().hash(&mut hasher);
    }
    [
        m.weight.light,
        m.weight.regular,
        m.weight.medium,
        m.weight.bold,
    ]
    .hash(&mut hasher);
    hasher.finish()
}

fn fingerprint_name(tokens: &Tokens) -> String {
    format!("toolkit-{:016x}", fingerprint(tokens))
}

/// The styles of this crate's own widgets, for any theme type. `Menu`,
/// `Panel`, `Fader`, `Knob`, `LevelMeter`, `Toggle`, `Waveform` and
/// `PianoRoll` take these from the theme unless given a style explicitly.
/// Implemented for [`Theme`] (from its tokens) and for `iced_core::Theme`
/// (from its palette).
pub trait Catalog {
    fn menu_style(&self) -> MenuStyle;
    fn audio_style(&self) -> AudioStyle;
}

impl Catalog for Theme {
    fn menu_style(&self) -> MenuStyle {
        self.tokens.menu_style()
    }

    fn audio_style(&self) -> AudioStyle {
        self.tokens.audio_style()
    }
}

impl Catalog for iced_core::Theme {
    fn menu_style(&self) -> MenuStyle {
        let p = self.palette();
        MenuStyle {
            background: p.background.weak.color,
            text: p.background.weak.text,
            disabled: p.secondary.base.color,
            selected: p.primary.strong.color,
            selected_text: p.primary.strong.text,
            border: p.background.strong.color,
            ..MenuStyle::default()
        }
    }

    fn audio_style(&self) -> AudioStyle {
        let p = self.palette();
        AudioStyle {
            background: p.background.weakest.color,
            track: p.background.strong.color,
            fill: p.primary.base.color,
            thumb: p.background.base.text,
            text: p.background.base.text,
            muted_text: p.secondary.base.color,
            border: p.background.strongest.color,
            meter_low: p.success.base.color,
            meter_high: p.warning.base.color,
            meter_clip: p.danger.base.color,
            peak: p.background.base.text,
            active: p.primary.base.color,
            active_text: p.primary.base.text,
            alert: p.danger.base.color,
            alert_text: p.danger.base.text,
            grid: p.background.strong.color,
            lane: p.background.weak.color,
            note: p.primary.base.color,
            waveform: p.primary.base.color,
            playhead: p.primary.strong.color,
            radius: AudioStyle::default().radius,
        }
    }
}

/// `a` moved `amount` of the way to `b` (0 keeps `a`, 1 gives `b`).
fn towards(a: Color, b: Color, amount: f32) -> Color {
    a.mix(b, amount)
}

fn outline(colour: Color, metrics: Metrics, radius: f32) -> Border {
    Border {
        color: colour,
        width: metrics.border.width,
        radius: radius.into(),
    }
}

fn no_border(radius: f32) -> Border {
    Border {
        color: Color::TRANSPARENT,
        width: 0.0,
        radius: radius.into(),
    }
}

/// Each `iced_widget` catalog: `Class<'a>` is iced's own boxed style
/// closure, so `.style(|theme, status| ...)` works as it does with iced's
/// theme, and the default class is the toolkit style of that widget.
macro_rules! catalog {
    ($($module:ident)::+, $default:expr) => {
        impl $($module)::+::Catalog for Theme {
            type Class<'a> = $($module)::+::StyleFn<'a, Self>;

            fn default<'a>() -> Self::Class<'a> {
                Box::new($default)
            }

            fn style(&self, class: &Self::Class<'_>, status: $($module)::+::Status) -> $($module)::+::Style {
                class(self, status)
            }
        }
    };
    ($($module:ident)::+, $default:expr, stateless) => {
        impl $($module)::+::Catalog for Theme {
            type Class<'a> = $($module)::+::StyleFn<'a, Self>;

            fn default<'a>() -> Self::Class<'a> {
                Box::new($default)
            }

            fn style(&self, class: &Self::Class<'_>) -> $($module)::+::Style {
                class(self)
            }
        }
    };
}

catalog!(iced_widget::button, button::primary);
catalog!(iced_widget::text_input, text_input::default);
catalog!(iced_widget::checkbox, checkbox::default);
catalog!(iced_widget::radio, radio::default);
catalog!(iced_widget::toggler, toggler::default);
catalog!(iced_widget::slider, slider::default);
catalog!(iced_widget::scrollable, scrollable::default);
catalog!(iced_widget::text_editor, text_editor::default);
#[cfg(feature = "svg")]
catalog!(iced_widget::svg, svg::default);
catalog!(iced_widget::container, container::transparent, stateless);
catalog!(iced_widget::progress_bar, progress_bar::default, stateless);
catalog!(iced_widget::rule, rule::default, stateless);
catalog!(iced_widget::table, table::default, stateless);
catalog!(iced_widget::float, float::default, stateless);
catalog!(iced_core::widget::text, text::default, stateless);

impl iced_widget::overlay::menu::Catalog for Theme {
    type Class<'a> = iced_widget::overlay::menu::StyleFn<'a, Self>;

    fn default<'a>() -> <Self as iced_widget::overlay::menu::Catalog>::Class<'a> {
        Box::new(menu::default)
    }

    fn style(
        &self,
        class: &<Self as iced_widget::overlay::menu::Catalog>::Class<'_>,
    ) -> iced_widget::overlay::menu::Style {
        class(self)
    }
}

impl iced_widget::pick_list::Catalog for Theme {
    type Class<'a> = iced_widget::pick_list::StyleFn<'a, Self>;

    fn default<'a>() -> <Self as iced_widget::pick_list::Catalog>::Class<'a> {
        Box::new(pick_list::default)
    }

    fn style(
        &self,
        class: &<Self as iced_widget::pick_list::Catalog>::Class<'_>,
        status: iced_widget::pick_list::Status,
    ) -> iced_widget::pick_list::Style {
        class(self, status)
    }
}

impl iced_widget::pane_grid::Catalog for Theme {
    type Class<'a> = iced_widget::pane_grid::StyleFn<'a, Self>;

    fn default<'a>() -> <Self as iced_widget::pane_grid::Catalog>::Class<'a> {
        Box::new(pane_grid::default)
    }

    fn style(
        &self,
        class: &<Self as iced_widget::pane_grid::Catalog>::Class<'_>,
    ) -> iced_widget::pane_grid::Style {
        class(self)
    }
}

impl iced_widget::combo_box::Catalog for Theme {}

/// Button variants. `primary` is the default class.
pub mod button {
    use super::*;
    use iced_widget::button::{Status, Style};

    fn filled(theme: &Theme, status: Status, fill: Color, text: Color) -> Style {
        let p = theme.tokens.palette;
        let m = theme.tokens.metrics;
        let (background, text_color) = match status {
            Status::Active => (fill, text),
            Status::Hovered => (towards(fill, text, 0.12), text),
            Status::Pressed => (towards(fill, p.surface, 0.2), text),
            Status::Disabled => (p.muted_surface, p.muted_text),
        };
        Style {
            background: Some(background.into()),
            text_color,
            border: no_border(m.radius.md),
            shadow: Shadow::default(),
            snap: true,
        }
    }

    /// The main action: the `primary` pair.
    pub fn primary(theme: &Theme, status: Status) -> Style {
        let p = theme.tokens.palette;
        filled(theme, status, p.primary, p.primary_text)
    }

    /// A dangerous action: the `destructive` pair.
    pub fn destructive(theme: &Theme, status: Status) -> Style {
        let p = theme.tokens.palette;
        filled(theme, status, p.destructive, p.destructive_text)
    }

    /// An ordinary action: the `card` pair with an outline.
    pub fn secondary(theme: &Theme, status: Status) -> Style {
        let p = theme.tokens.palette;
        let m = theme.tokens.metrics;
        let (background, text_color, edge) = match status {
            Status::Active => (p.card, p.card_text, p.border),
            Status::Hovered => (p.elevated, p.elevated_text, p.border),
            Status::Pressed => (p.muted_surface, p.text, p.ring),
            Status::Disabled => (p.muted_surface, p.muted_text, p.input),
        };
        Style {
            background: Some(background.into()),
            text_color,
            border: outline(edge, m, m.radius.md),
            shadow: Shadow::default(),
            snap: true,
        }
    }

    /// A ghost button: no fill at rest, the muted surface under the pointer.
    pub fn text(theme: &Theme, status: Status) -> Style {
        let p = theme.tokens.palette;
        let m = theme.tokens.metrics;
        let (background, text_color) = match status {
            Status::Active => (None, p.text),
            Status::Hovered => (Some(p.muted_surface), p.text),
            Status::Pressed => (Some(p.selection), p.selection_text),
            Status::Disabled => (None, p.muted_text),
        };
        Style {
            background: background.map(Into::into),
            text_color,
            border: no_border(m.radius.md),
            shadow: Shadow::default(),
            snap: true,
        }
    }
}

/// Text input: `Tokens::text_input`.
pub mod text_input {
    use super::*;
    use iced_widget::text_input::{Status, Style};

    pub fn default(theme: &Theme, status: Status) -> Style {
        theme.tokens.text_input(status)
    }
}

/// Text editor: the text-input look on the same surfaces.
pub mod text_editor {
    use super::*;
    use iced_widget::text_editor::{Status, Style};

    pub fn default(theme: &Theme, status: Status) -> Style {
        use iced_widget::text_input::Status as Input;
        let input = theme.tokens.text_input(match status {
            Status::Active => Input::Active,
            Status::Hovered => Input::Hovered,
            Status::Focused { is_hovered } => Input::Focused { is_hovered },
            Status::Disabled => Input::Disabled,
        });
        Style {
            background: input.background,
            border: input.border,
            placeholder: input.placeholder,
            value: input.value,
            selection: input.selection,
        }
    }
}

/// Checkbox: an outlined box that fills with `primary` when checked.
pub mod checkbox {
    use super::*;
    use iced_widget::checkbox::{Status, Style};

    pub fn default(theme: &Theme, status: Status) -> Style {
        let p = theme.tokens.palette;
        let m = theme.tokens.metrics;
        let (is_checked, hovered, disabled) = match status {
            Status::Active { is_checked } => (is_checked, false, false),
            Status::Hovered { is_checked } => (is_checked, true, false),
            Status::Disabled { is_checked } => (is_checked, false, true),
        };
        let (background, icon_color, edge) = match (is_checked, disabled) {
            (_, true) => (p.muted_surface, p.muted_text, p.input),
            (true, false) => (
                if hovered {
                    towards(p.primary, p.primary_text, 0.12)
                } else {
                    p.primary
                },
                p.primary_text,
                p.primary,
            ),
            (false, false) => (
                p.surface,
                p.surface,
                if hovered { p.border } else { p.input },
            ),
        };
        Style {
            background: background.into(),
            icon_color,
            border: outline(edge, m, m.radius.sm),
            text_color: disabled.then_some(p.muted_text),
        }
    }
}

/// Radio: a `primary` dot in an outlined circle.
pub mod radio {
    use super::*;
    use iced_widget::radio::{Status, Style};

    pub fn default(theme: &Theme, status: Status) -> Style {
        let p = theme.tokens.palette;
        let m = theme.tokens.metrics;
        let (is_selected, hovered) = match status {
            Status::Active { is_selected } => (is_selected, false),
            Status::Hovered { is_selected } => (is_selected, true),
        };
        Style {
            background: if hovered { p.muted_surface } else { p.surface }.into(),
            dot_color: p.primary,
            border_width: m.border.width,
            border_color: if is_selected {
                p.primary
            } else if hovered {
                p.border
            } else {
                p.input
            },
            text_color: None,
        }
    }
}

/// Toggler: a muted track that turns `primary` when on.
pub mod toggler {
    use super::*;
    use iced_widget::toggler::{Status, Style};

    pub fn default(theme: &Theme, status: Status) -> Style {
        let p = theme.tokens.palette;
        let m = theme.tokens.metrics;
        let (on, hovered, disabled) = match status {
            Status::Active { is_toggled } => (is_toggled, false, false),
            Status::Hovered { is_toggled } => (is_toggled, true, false),
            Status::Disabled { is_toggled } => (is_toggled, false, true),
        };
        let (background, foreground, outline) = match (on, disabled) {
            (true, false) => (
                if hovered {
                    towards(p.primary, p.primary_text, 0.12)
                } else {
                    p.primary
                },
                p.primary_text,
                Color::TRANSPARENT,
            ),
            (false, false) => (
                p.muted_surface,
                if hovered { p.text } else { p.muted_text },
                if hovered { p.border } else { p.input },
            ),
            (true, true) => (p.muted_text, p.muted_surface, Color::TRANSPARENT),
            (false, true) => (p.muted_surface, p.muted_text, p.input),
        };
        Style {
            background: background.into(),
            background_border_width: if outline == Color::TRANSPARENT {
                0.0
            } else {
                m.border.width
            },
            background_border_color: outline,
            foreground: foreground.into(),
            foreground_border_width: 0.0,
            foreground_border_color: Color::TRANSPARENT,
            text_color: disabled.then_some(p.muted_text),
            border_radius: None,
            padding_ratio: 0.1,
        }
    }
}

/// Slider: a `primary` fill on a muted rail, round handle, focus ring when
/// dragged.
pub mod slider {
    use super::*;
    use iced_widget::slider::{Handle, HandleShape, Rail, Status, Style};

    pub fn default(theme: &Theme, status: Status) -> Style {
        let p = theme.tokens.palette;
        let m = theme.tokens.metrics;
        let (fill, outline) = match status {
            Status::Active => (p.primary, Color::TRANSPARENT),
            Status::Hovered => (towards(p.primary, p.primary_text, 0.12), p.border),
            Status::Dragged => (p.primary, p.ring),
        };
        Style {
            rail: Rail {
                backgrounds: (fill.into(), p.muted_surface.into()),
                width: m.radius.sm,
                border: no_border(m.radius.sm / 2.0),
            },
            handle: Handle {
                shape: HandleShape::Circle {
                    radius: m.spacing.md,
                },
                background: fill.into(),
                border_width: if outline == Color::TRANSPARENT {
                    0.0
                } else {
                    m.border.focus_width
                },
                border_color: outline,
            },
        }
    }
}

/// Pick list: the text-input surfaces, the ring while open.
pub mod pick_list {
    use super::*;
    use iced_widget::pick_list::{Status, Style};

    pub fn default(theme: &Theme, status: Status) -> Style {
        let p = theme.tokens.palette;
        let m = theme.tokens.metrics;
        let disabled = matches!(status, Status::Disabled);
        let edge = match status {
            Status::Active => p.input,
            Status::Hovered => p.border,
            Status::Opened { .. } => p.ring,
            Status::Disabled => p.input,
        };
        Style {
            text_color: if disabled { p.muted_text } else { p.text },
            placeholder_color: p.muted_text,
            handle_color: if disabled { p.muted_text } else { p.text },
            background: if disabled { p.muted_surface } else { p.surface }.into(),
            border: outline(edge, m, m.radius.md),
        }
    }
}

/// The drop-down menu of a pick list or combo box: the `popover` pair with
/// the `selection` highlight, as the toolkit menu draws itself.
pub mod menu {
    use super::*;
    use iced_widget::overlay::menu::Style;

    pub fn default(theme: &Theme) -> Style {
        let p = theme.tokens.palette;
        let m = theme.tokens.metrics;
        Style {
            background: p.popover.into(),
            border: outline(p.border, m, m.radius.md),
            text_color: p.popover_text,
            selected_text_color: p.selection_text,
            selected_background: p.selection.into(),
            shadow: Shadow::default(),
        }
    }
}

/// Scrollable: bare rails, a `border`-coloured scroller that strengthens
/// under the pointer and turns `primary` while dragged.
pub mod scrollable {
    use super::*;
    use iced_widget::scrollable::{AutoScroll, Rail, Scroller, Status, Style};

    pub fn default(theme: &Theme, status: Status) -> Style {
        let p = theme.tokens.palette;
        let m = theme.tokens.metrics;
        let rail = |colour: Color| Rail {
            background: None,
            border: no_border(m.radius.sm),
            scroller: Scroller {
                background: colour.into(),
                border: no_border(m.radius.sm),
            },
        };
        let (vertical, horizontal) = match status {
            Status::Active { .. } => (p.border, p.border),
            Status::Hovered {
                is_horizontal_scrollbar_hovered,
                is_vertical_scrollbar_hovered,
                ..
            } => (
                if is_vertical_scrollbar_hovered {
                    p.muted_text
                } else {
                    p.border
                },
                if is_horizontal_scrollbar_hovered {
                    p.muted_text
                } else {
                    p.border
                },
            ),
            Status::Dragged {
                is_horizontal_scrollbar_dragged,
                is_vertical_scrollbar_dragged,
                ..
            } => (
                if is_vertical_scrollbar_dragged {
                    p.primary
                } else {
                    p.border
                },
                if is_horizontal_scrollbar_dragged {
                    p.primary
                } else {
                    p.border
                },
            ),
        };
        Style {
            container: iced_widget::container::Style::default(),
            vertical_rail: rail(vertical),
            horizontal_rail: rail(horizontal),
            gap: None,
            auto_scroll: AutoScroll {
                background: p.elevated.into(),
                border: outline(p.border, m, m.radius.lg),
                shadow: Shadow {
                    color: p.border,
                    offset: Vector::ZERO,
                    blur_radius: m.spacing.xs,
                },
                icon: p.elevated_text,
            },
        }
    }
}

/// Container surfaces. `transparent` is the default class (inherits the
/// background and text colour of whatever is behind it).
pub mod container {
    use super::*;
    use iced_widget::container::Style;

    pub fn transparent(_theme: &Theme) -> Style {
        Style::default()
    }

    /// The window surface.
    pub fn surface(theme: &Theme) -> Style {
        let p = theme.tokens.palette;
        Style {
            background: Some(p.surface.into()),
            text_color: Some(p.text),
            ..Style::default()
        }
    }

    /// Grouped content: the `card` pair, outlined, large radius.
    pub fn card(theme: &Theme) -> Style {
        let p = theme.tokens.palette;
        let m = theme.tokens.metrics;
        Style {
            background: Some(p.card.into()),
            text_color: Some(p.card_text),
            border: outline(p.border, m, m.radius.lg),
            ..Style::default()
        }
    }

    /// Menus and other popovers.
    pub fn popover(theme: &Theme) -> Style {
        let p = theme.tokens.palette;
        let m = theme.tokens.metrics;
        Style {
            background: Some(p.popover.into()),
            text_color: Some(p.popover_text),
            border: outline(p.border, m, m.radius.md),
            ..Style::default()
        }
    }

    /// Content floating over the surface: the opaque `elevated` pair.
    pub fn elevated(theme: &Theme) -> Style {
        let p = theme.tokens.palette;
        let m = theme.tokens.metrics;
        Style {
            background: Some(p.elevated.into()),
            text_color: Some(p.elevated_text),
            border: outline(p.border, m, m.radius.md),
            shadow: Shadow {
                color: p.border,
                offset: Vector::new(0.0, m.spacing.xs),
                blur_radius: m.spacing.md,
            },
            ..Style::default()
        }
    }

    /// `Tokens::tooltip_style`.
    pub fn tooltip(theme: &Theme) -> Style {
        theme.tokens.tooltip_style()
    }
}

/// Progress bar: `primary` on the muted surface.
pub mod progress_bar {
    use super::*;
    use iced_widget::progress_bar::Style;

    pub fn default(theme: &Theme) -> Style {
        let p = theme.tokens.palette;
        let m = theme.tokens.metrics;
        Style {
            background: p.muted_surface.into(),
            bar: p.primary.into(),
            border: no_border(m.radius.sm),
        }
    }
}

/// Rule: a full-width separator in the `border` colour.
pub mod rule {
    use super::*;
    use iced_widget::rule::{FillMode, Style};

    pub fn default(theme: &Theme) -> Style {
        Style {
            color: theme.tokens.palette.border,
            radius: border::Radius::default(),
            fill_mode: FillMode::Full,
            snap: true,
        }
    }
}

/// Pane grid: the `selection` tint over a hovered region, `primary` and
/// `ring` split lines.
pub mod pane_grid {
    use super::*;
    use iced_widget::pane_grid::{Highlight, Line, Style};

    pub fn default(theme: &Theme) -> Style {
        let p = theme.tokens.palette;
        let m = theme.tokens.metrics;
        Style {
            hovered_region: Highlight {
                background: p.selection.scale_alpha(0.5).into(),
                border: Border {
                    color: p.ring,
                    width: m.border.focus_width,
                    radius: m.radius.sm.into(),
                },
            },
            hovered_split: Line {
                color: p.primary,
                width: m.border.focus_width,
            },
            picked_split: Line {
                color: p.ring,
                width: m.border.focus_width,
            },
        }
    }
}

/// Text colours. The default inherits; the others name a palette role.
pub mod text {
    use super::*;
    use iced_core::widget::text::Style;

    pub fn default(_theme: &Theme) -> Style {
        Style { color: None }
    }

    pub fn muted(theme: &Theme) -> Style {
        Style {
            color: Some(theme.tokens.palette.muted_text),
        }
    }

    pub fn primary(theme: &Theme) -> Style {
        Style {
            color: Some(theme.tokens.palette.primary),
        }
    }

    pub fn destructive(theme: &Theme) -> Style {
        Style {
            color: Some(theme.tokens.palette.destructive),
        }
    }
}

/// Svg (feature `svg`): the default keeps the image's own colours;
/// `symbolic` tints it with the text colour, as symbolic icons expect.
#[cfg(feature = "svg")]
pub mod svg {
    use super::*;
    use iced_widget::svg::{Status, Style};

    pub fn default(_theme: &Theme, _status: Status) -> Style {
        Style { color: None }
    }

    pub fn symbolic(theme: &Theme, _status: Status) -> Style {
        Style {
            color: Some(theme.tokens.palette.text),
        }
    }
}

/// Table: `border`-coloured separators.
pub mod table {
    use super::*;
    use iced_widget::table::Style;

    pub fn default(theme: &Theme) -> Style {
        let separator = theme.tokens.palette.border.into();
        Style {
            separator_x: separator,
            separator_y: separator,
        }
    }
}

/// Float: no shadow.
pub mod float {
    use super::*;
    use iced_widget::float::Style;

    pub fn default(_theme: &Theme) -> Style {
        Style::default()
    }
}

/// Badge variants. `primary` is the default class.
pub mod badge {
    use super::*;
    use crate::badge::{Style, Status};

    fn filled(status: Status, fill: Color, text: Color) -> Style {
        let background = match status {
            Status::Active => fill,
            Status::Hovered => towards(fill, text, 0.12),
        };
        Style {
            background: background.into(),
            border_radius: None,
            border_width: 0.0,
            border_color: None,
            text_color: text,
        }
    }

    /// The `primary` pair.
    pub fn primary(theme: &Theme, status: Status) -> Style {
        let p = theme.tokens.palette;
        filled(status, p.primary, p.primary_text)
    }

    /// The `muted` pair: an understated chip.
    pub fn neutral(theme: &Theme, status: Status) -> Style {
        let p = theme.tokens.palette;
        filled(status, p.muted_surface, p.muted_text)
    }

    /// The `destructive` pair.
    pub fn destructive(theme: &Theme, status: Status) -> Style {
        let p = theme.tokens.palette;
        filled(status, p.destructive, p.destructive_text)
    }
}

/// The card frame: a `card` panel with an `elevated` head.
pub mod card {
    use super::*;
    use crate::card::Style;

    pub fn default(theme: &Theme) -> Style {
        let p = theme.tokens.palette;
        let m = theme.tokens.metrics;
        Style {
            background: p.card.into(),
            border_radius: m.radius.md,
            border_width: m.border.width,
            border_color: p.border,
            head_background: p.elevated.into(),
            head_text_color: p.elevated_text,
            body_background: Color::TRANSPARENT.into(),
            body_text_color: p.card_text,
            foot_background: Color::TRANSPARENT.into(),
            foot_text_color: p.card_text,
            close_color: p.muted_text,
        }
    }
}

/// A labelled frame drawn in the border colour.
pub mod labeled_frame {
    use super::*;
    use crate::labeled_frame::Style;

    pub fn default(theme: &Theme) -> Style {
        let p = theme.tokens.palette;
        let m = theme.tokens.metrics;
        Style {
            color: p.border.into(),
            radius: m.radius.sm.into(),
        }
    }
}

/// Selection list rows: the `selection` pair on the chosen row, the
/// `muted` pair under the pointer.
pub mod selection_list {
    use super::*;
    use crate::selection_list::{Style, Status};

    pub fn default(theme: &Theme, status: Status) -> Style {
        let p = theme.tokens.palette;
        let m = theme.tokens.metrics;
        let (background, text_color) = match status {
            Status::Active => (p.card, p.card_text),
            Status::Hovered => (p.muted_surface, p.text),
            Status::Selected => (p.selection, p.selection_text),
        };
        Style {
            background: background.into(),
            text_color,
            border: outline(p.border, m, m.radius.md),
        }
    }
}

/// A slider bar: the `primary` fill on the `muted` track.
pub mod slide_bar {
    use super::*;
    use crate::slide_bar::Style;

    pub fn default(theme: &Theme) -> Style {
        let p = theme.tokens.palette;
        let m = theme.tokens.metrics;
        Style {
            background: p.muted_surface.into(),
            bar: p.primary,
            border: outline(p.border, m, m.radius.md),
            radius: m.radius.md,
        }
    }
}

/// Number input modifier buttons: the `primary` pair.
pub mod number_input {
    use super::*;
    use crate::number_input::{Style, Status};

    pub fn primary(theme: &Theme, status: Status) -> Style {
        let p = theme.tokens.palette;
        let (background, icon_color) = match status {
            Status::Active => (Some(p.primary.into()), p.primary_text),
            Status::Pressed => (Some(towards(p.primary, p.primary_text, 0.12).into()), p.primary_text),
            Status::Disabled => (None, p.muted_text),
        };
        Style {
            button_background: background,
            icon_color,
        }
    }
}

/// Tab bars and sidebars: the active tab is the `surface` sheet with an
/// edge, the hovered one `elevated`, the inactive ones `muted`.
pub mod tab_bar {
    use super::*;
    use crate::tab_bar::{Style, Status};

    pub fn default(theme: &Theme, status: Status) -> Style {
        let p = theme.tokens.palette;
        let m = theme.tokens.metrics;
        let (tab_label_background, text) = match status {
            Status::Active => (p.surface, p.text),
            Status::Hovered => (p.elevated, p.elevated_text),
            Status::Disabled => (p.muted_surface, p.muted_text),
        };
        Style {
            background: None,
            border_color: None,
            border_width: 0.0,
            tab_border_radius: iced_core::border::Radius::default()
                .top_left(m.radius.sm)
                .top_right(m.radius.sm),
            tab_label_background: tab_label_background.into(),
            tab_label_border_color: if status == Status::Active { p.border } else { p.muted_surface },
            tab_label_border_width: if status == Status::Active { m.border.width } else { 0.0 },
            icon_color: text,
            icon_background: Some(p.muted_surface.into()),
            icon_border_radius: m.radius.sm.into(),
            text_color: text,
        }
    }
}

/// Sidebar tabs: the active tab is the `surface` sheet, rounded on its
/// leading corners.
pub mod sidebar {
    use super::*;
    use crate::sidebar::{Status, Style};

    pub fn default(theme: &Theme, status: Status) -> Style {
        let style = super::tab_bar::default(theme, status.into());
        let m = theme.tokens.metrics;
        Style {
            tab_border_radius: iced_core::border::Radius::default()
                .top_left(m.radius.sm)
                .bottom_left(m.radius.sm),
            ..style
        }
    }
}

catalog!(crate::badge, badge::primary);
catalog!(crate::card, card::default, stateless);
catalog!(crate::labeled_frame, labeled_frame::default, stateless);
catalog!(crate::number_input, number_input::primary);
catalog!(crate::selection_list, selection_list::default);
catalog!(crate::sidebar, sidebar::default);
/// The split grip: the border colour at rest, the `primary` pair while
/// active.
pub mod split {
    use super::*;
    use crate::split::Style;

    pub fn default(theme: &Theme) -> Style {
        let p = theme.tokens.palette;
        let m = theme.tokens.metrics;
        Style {
            color: p.border,
            active_color: p.primary,
            width: m.border.width.max(2.0),
            active_width: m.border.width.max(2.0) + 1.0,
            radius: m.radius.sm,
        }
    }
}

/// The colour picker: a circular marker at the small text size.
pub mod color_picker {
    use super::*;
    use crate::color_picker::{MarkerShape, Style};

    pub fn default(theme: &Theme) -> Style {
        let m = theme.tokens.metrics;
        let size = m.text.sm;
        Style {
            marker_shape: MarkerShape::Circle {
                radius: size / 2.0,
                border_width: m.border.width.max(1.0),
            },
            preserve_hue: true,
        }
    }
}

catalog!(crate::color_picker, color_picker::default, stateless);
catalog!(crate::slide_bar, slide_bar::default, stateless);
catalog!(crate::split, split::default, stateless);
catalog!(crate::tab_bar, tab_bar::default);

impl crate::badge::Catalog for iced_core::Theme {
    type Class<'a> = crate::badge::StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(|theme, status| {
            let p = theme.palette();
            let fill = match status {
                crate::badge::Status::Active => p.primary.base.color,
                crate::badge::Status::Hovered => towards(p.primary.base.color, p.primary.base.text, 0.12),
            };
            crate::badge::Style {
                background: fill.into(),
                border_radius: None,
                border_width: 0.0,
                border_color: None,
                text_color: p.primary.base.text,
            }
        })
    }

    fn style(&self, class: &Self::Class<'_>, status: crate::badge::Status) -> crate::badge::Style {
        class(self, status)
    }
}

impl crate::card::Catalog for iced_core::Theme {
    type Class<'a> = crate::card::StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(|theme| {
            let p = theme.palette();
            crate::card::Style {
                background: p.background.weak.color.into(),
                border_radius: 0.0,
                border_width: 1.0,
                border_color: p.background.strong.color,
                head_background: p.background.strong.color.into(),
                head_text_color: p.background.strong.text,
                body_background: Color::TRANSPARENT.into(),
                body_text_color: p.background.weak.text,
                foot_background: Color::TRANSPARENT.into(),
                foot_text_color: p.background.weak.text,
                close_color: p.background.base.text,
            }
        })
    }

    fn style(&self, class: &Self::Class<'_>) -> crate::card::Style {
        class(self)
    }
}

impl crate::labeled_frame::Catalog for iced_core::Theme {
    type Class<'a> = crate::labeled_frame::StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(|theme| {
            let p = theme.palette();
            crate::labeled_frame::Style {
                color: p.background.strong.color.into(),
                radius: iced_core::border::Radius::default(),
            }
        })
    }

    fn style(&self, class: &Self::Class<'_>) -> crate::labeled_frame::Style {
        class(self)
    }
}

impl crate::selection_list::Catalog for iced_core::Theme {
    type Class<'a> = crate::selection_list::StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(|theme, status| {
            let p = theme.palette();
            let (background, text_color) = match status {
                crate::selection_list::Status::Active => {
                    (p.background.weak.color, p.background.weak.text)
                }
                crate::selection_list::Status::Hovered => {
                    (p.background.strong.color, p.background.strong.text)
                }
                crate::selection_list::Status::Selected => {
                    (p.primary.strong.color, p.primary.strong.text)
                }
            };
            crate::selection_list::Style {
                background: background.into(),
                text_color,
                border: iced_core::Border {
                    color: p.background.strong.color,
                    width: 1.0,
                    radius: 0.0.into(),
                },
            }
        })
    }

    fn style(
        &self,
        class: &Self::Class<'_>,
        status: crate::selection_list::Status,
    ) -> crate::selection_list::Style {
        class(self, status)
    }
}

impl crate::slide_bar::Catalog for iced_core::Theme {
    type Class<'a> = crate::slide_bar::StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(|theme| {
            let p = theme.palette();
            crate::slide_bar::Style {
                background: p.background.strong.color.into(),
                bar: p.primary.base.color,
                border: iced_core::Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 0.0.into(),
                },
                radius: 0.0,
            }
        })
    }

    fn style(&self, class: &Self::Class<'_>) -> crate::slide_bar::Style {
        class(self)
    }
}

impl crate::number_input::Catalog for iced_core::Theme {
    type Class<'a> = crate::number_input::StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(|theme, status| {
            let p = theme.palette();
            let (background, icon_color) = match status {
                crate::number_input::Status::Active => {
                    (Some(p.primary.base.color.into()), p.primary.base.text)
                }
                crate::number_input::Status::Pressed => (
                    Some(towards(p.primary.base.color, p.primary.base.text, 0.12).into()),
                    p.primary.base.text,
                ),
                crate::number_input::Status::Disabled => (None, p.secondary.base.color),
            };
            crate::number_input::Style {
                button_background: background,
                icon_color,
            }
        })
    }

    fn style(
        &self,
        class: &Self::Class<'_>,
        status: crate::number_input::Status,
    ) -> crate::number_input::Style {
        class(self, status)
    }
}

fn iced_tab_style(theme: &iced_core::Theme, status: crate::tab_bar::Status) -> crate::tab_bar::Style {
    let p = theme.palette();
    let (tab_label_background, text) = match status {
        crate::tab_bar::Status::Active => (p.background.base.color, p.background.base.text),
        crate::tab_bar::Status::Hovered => (p.background.strong.color, p.background.strong.text),
        crate::tab_bar::Status::Disabled => (p.secondary.weak.color, p.secondary.weak.text),
    };
    crate::tab_bar::Style {
        background: None,
        border_color: None,
        border_width: 0.0,
        tab_border_radius: iced_core::border::Radius::default(),
        tab_label_background: tab_label_background.into(),
        tab_label_border_color: Color::TRANSPARENT,
        tab_label_border_width: 0.0,
        icon_color: text,
        icon_background: Some(p.background.strong.color.into()),
        icon_border_radius: 0.0.into(),
        text_color: text,
    }
}

impl crate::tab_bar::Catalog for iced_core::Theme {
    type Class<'a> = crate::tab_bar::StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(iced_tab_style)
    }

    fn style(
        &self,
        class: &Self::Class<'_>,
        status: crate::tab_bar::Status,
    ) -> crate::tab_bar::Style {
        class(self, status)
    }
}

impl crate::split::Catalog for iced_core::Theme {
    type Class<'a> = crate::split::StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(|theme| {
            let p = theme.palette();
            crate::split::Style {
                color: p.background.strong.color,
                active_color: p.primary.base.color,
                width: 2.0,
                active_width: 3.0,
                radius: 0.0,
            }
        })
    }

    fn style(&self, class: &Self::Class<'_>) -> crate::split::Style {
        class(self)
    }
}

impl crate::color_picker::Catalog for iced_core::Theme {
    type Class<'a> = crate::color_picker::StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(|_| crate::color_picker::Style {
            marker_shape: crate::color_picker::MarkerShape::Circle {
                radius: 6.0,
                border_width: 2.0,
            },
            preserve_hue: true,
        })
    }

    fn style(&self, class: &Self::Class<'_>) -> crate::color_picker::Style {
        class(self)
    }
}

impl crate::sidebar::Catalog for iced_core::Theme {
    type Class<'a> = crate::sidebar::StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(|theme, status| iced_tab_style(theme, crate::tab_bar::Status::from(status)))
    }

    fn style(&self, class: &Self::Class<'_>, status: crate::sidebar::Status) -> crate::sidebar::Style {
        class(self, status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_core::theme::Base;
    use iced_widget::button::Status as ButtonStatus;

    #[test]
    fn base_follows_the_tokens() {
        let dark = Theme::dark();
        let light = Theme::light();
        assert_eq!(dark.mode(), Mode::Dark);
        assert_eq!(light.mode(), Mode::Light);
        assert_eq!(dark.base().background_color, Palette::dark().surface);
        assert_eq!(light.base().text_color, Palette::light().text);
        assert_eq!(<Theme as Default>::default(), dark);
        assert_eq!(<Theme as Base>::default(Mode::Light), light);
        assert_eq!(<Theme as Base>::default(Mode::None), dark);
        assert_eq!(dark.seed().unwrap().primary, Palette::dark().primary);
        assert_eq!(
            dark.to_iced().palette().background.base.color,
            Palette::dark().surface
        );
        assert_eq!(Theme::from(Tokens::light()), light);
    }

    #[test]
    fn names_fingerprint_the_tokens() {
        let dark = Theme::dark();
        let light = Theme::light();
        assert_ne!(dark.name(), light.name());
        assert_eq!(dark.name(), Theme::new(Tokens::dark()).name());
        assert!(dark.name().starts_with("toolkit-"));
        let mut thick = Tokens::dark();
        thick.metrics.border.width = 2.0;
        assert_ne!(Theme::new(thick).name(), dark.name());
        let mut tinted = Tokens::dark();
        tinted.palette.ring = Palette::light().ring;
        assert_ne!(Theme::new(tinted).name(), dark.name());
        assert_eq!(fingerprint(&Tokens::dark()), fingerprint(&Tokens::dark()));

        let mut swapped = Theme::dark();
        swapped.set_tokens(Tokens::light());
        assert_eq!(swapped, light);
        assert_eq!(swapped.to_string(), light.name());

        let mut labelled = Theme::named(Tokens::dark(), "Night");
        assert_eq!(labelled.name(), "Night");
        labelled.set_tokens(Tokens::light());
        assert_eq!(labelled.name(), "Night");
        assert_eq!(labelled.tokens(), Tokens::light());
    }

    #[test]
    fn default_classes_are_the_toolkit_styles() {
        use iced_widget::{button as b, container as c, progress_bar as pb, text_input as ti};
        let theme = Theme::light();
        let p = theme.palette();
        let primary = <Theme as b::Catalog>::style(
            &theme,
            &<Theme as b::Catalog>::default(),
            ButtonStatus::Active,
        );
        assert_eq!(primary.background, Some(p.primary.into()));
        assert_eq!(primary.text_color, p.primary_text);
        assert_eq!(primary.border.radius, theme.metrics().radius.md.into());
        let disabled = button::primary(&theme, ButtonStatus::Disabled);
        assert_eq!(disabled.text_color, p.muted_text);
        let hovered = button::primary(&theme, ButtonStatus::Hovered);
        assert_ne!(hovered.background, primary.background);
        assert_eq!(
            button::secondary(&theme, ButtonStatus::Active).border.color,
            p.border
        );
        assert_eq!(button::text(&theme, ButtonStatus::Active).background, None);
        assert_eq!(
            button::destructive(&theme, ButtonStatus::Active).background,
            Some(p.destructive.into())
        );

        let input = <Theme as ti::Catalog>::style(
            &theme,
            &<Theme as ti::Catalog>::default(),
            ti::Status::Focused { is_hovered: false },
        );
        assert_eq!(
            input,
            theme
                .tokens()
                .text_input(ti::Status::Focused { is_hovered: false })
        );
        assert_eq!(input.border.color, p.ring);

        let transparent = <Theme as c::Catalog>::style(&theme, &<Theme as c::Catalog>::default());
        assert_eq!(transparent, c::Style::default());
        assert_eq!(container::tooltip(&theme), theme.tokens().tooltip_style());
        assert_eq!(container::card(&theme).background, Some(p.card.into()));
        assert_eq!(container::popover(&theme).text_color, Some(p.popover_text));
        assert_eq!(
            container::elevated(&theme).background,
            Some(p.elevated.into())
        );
        assert_eq!(
            container::surface(&theme).background,
            Some(p.surface.into())
        );

        let bar = <Theme as pb::Catalog>::style(&theme, &<Theme as pb::Catalog>::default());
        assert_eq!(bar.bar, p.primary.into());
        assert_eq!(rule::default(&theme).color, p.border);
        assert_eq!(
            menu::default(&theme).selected_background,
            p.selection.into()
        );
        assert_eq!(text::muted(&theme).color, Some(p.muted_text));
        assert_eq!(text::default(&theme).color, None);
        assert_eq!(table::default(&theme).separator_x, p.border.into());
        assert_eq!(pane_grid::default(&theme).hovered_split.color, p.primary);
    }

    #[test]
    fn stateful_styles_follow_status() {
        use iced_widget::{checkbox as cb, pick_list as pl, radio as r, slider as s, toggler as t};
        let theme = Theme::dark();
        let p = theme.palette();
        assert_eq!(
            checkbox::default(&theme, cb::Status::Active { is_checked: true }).background,
            p.primary.into()
        );
        assert_eq!(
            checkbox::default(&theme, cb::Status::Active { is_checked: false })
                .border
                .color,
            p.input
        );
        assert_eq!(
            checkbox::default(&theme, cb::Status::Disabled { is_checked: true }).text_color,
            Some(p.muted_text)
        );
        assert_eq!(
            radio::default(&theme, r::Status::Active { is_selected: true }).border_color,
            p.primary
        );
        assert_eq!(
            radio::default(&theme, r::Status::Hovered { is_selected: false }).background,
            p.muted_surface.into()
        );
        assert_eq!(
            toggler::default(&theme, t::Status::Active { is_toggled: true }).background,
            p.primary.into()
        );
        assert_eq!(
            toggler::default(&theme, t::Status::Active { is_toggled: false })
                .background_border_color,
            p.input
        );
        assert_eq!(
            slider::default(&theme, s::Status::Dragged)
                .handle
                .border_color,
            p.ring
        );
        assert_eq!(
            slider::default(&theme, s::Status::Active)
                .handle
                .border_width,
            0.0
        );
        assert_eq!(
            pick_list::default(&theme, pl::Status::Opened { is_hovered: true })
                .border
                .color,
            p.ring
        );
        assert_eq!(
            pick_list::default(&theme, pl::Status::Disabled).text_color,
            p.muted_text
        );
        let editor = text_editor::default(&theme, iced_widget::text_editor::Status::Disabled);
        assert_eq!(editor.value, p.muted_text);
        let dragged = scrollable::default(
            &theme,
            iced_widget::scrollable::Status::Dragged {
                is_horizontal_scrollbar_dragged: false,
                is_vertical_scrollbar_dragged: true,
                is_horizontal_scrollbar_disabled: false,
                is_vertical_scrollbar_disabled: false,
            },
        );
        assert_eq!(dragged.vertical_rail.scroller.background, p.primary.into());
        assert_eq!(dragged.horizontal_rail.scroller.background, p.border.into());
    }

    #[test]
    fn toolkit_widget_styles_come_from_either_theme() {
        let theme = Theme::light();
        assert_eq!(theme.menu_style(), Tokens::light().menu_style());
        assert_eq!(theme.audio_style(), Tokens::light().audio_style());
        let iced = iced_core::Theme::Dark;
        let menu = iced.menu_style();
        assert_eq!(menu.selected, iced.palette().primary.strong.color);
        assert_eq!(menu.row_height, MenuStyle::default().row_height);
        let audio = iced.audio_style();
        assert_eq!(audio.meter_clip, iced.palette().danger.base.color);
        assert_eq!(audio.radius, AudioStyle::default().radius);
    }

    /// Every style is a pure function of the tokens: the same tokens give
    /// the same style, and a palette change reaches every widget.
    #[test]
    fn styles_change_with_the_tokens() {
        let dark = Theme::dark();
        let light = Theme::light();
        assert_eq!(
            button::primary(&dark, ButtonStatus::Active),
            button::primary(&Theme::dark(), ButtonStatus::Active)
        );
        assert_ne!(
            button::primary(&dark, ButtonStatus::Active),
            button::primary(&light, ButtonStatus::Active)
        );
        assert_ne!(container::card(&dark), container::card(&light));
        assert_ne!(dark.base().background_color, light.base().background_color);
    }
}
