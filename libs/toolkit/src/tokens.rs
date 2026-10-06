// SPDX-License-Identifier: MIT OR Apache-2.0
//! Theme tokens: a [`Palette`] of colours and [`Metrics`] (spacing, radii,
//! border widths, type scale and weights), combined as [`Tokens`].
//!
//! The widgets take their colours through `Tokens::text_input`,
//! `Tokens::menu_style`, `Tokens::tooltip_style` and `Tokens::audio_style`.
//! The built-in [`Palette::dark`] and [`Palette::light`] sets are complete
//! on their own; an application with its own theme fills the same plain
//! fields. Nothing here parses a theme file.

use iced_core::{Border, Color};
use iced_widget::{container, text_input};

use crate::{AudioStyle, MenuStyle};

#[cfg(feature = "design")]
mod design;
#[cfg(feature = "design")]
pub use design::{TokenError, colour};

/// Track colours for the standalone gallery; retained from the 0.1.7 preview.
pub const PREVIEW_TRACK_COLOURS: [Color; 4] = [
    Color::from_rgb(0.35, 0.72, 0.55),
    Color::from_rgb(0.40, 0.62, 0.86),
    Color::from_rgb(0.85, 0.63, 0.35),
    Color::from_rgb(0.76, 0.45, 0.72),
];

pub(crate) const TRANSPARENT: Color = Color::TRANSPARENT;

#[cfg(test)]
pub(crate) const TEST_NOTE_COLOUR: Color = Color::from_rgb8(10, 20, 30);

pub(crate) fn default_menu_style() -> MenuStyle {
    MenuStyle {
        background: Color::from_rgb8(35, 37, 42),
        text: Color::WHITE,
        disabled: Color::from_rgb8(130, 132, 140),
        selected: Color::from_rgb8(51, 91, 145),
        selected_text: Color::WHITE,
        border: Color::from_rgb8(70, 74, 82),
        radius: 4.0,
        text_size: 14.0,
        row_height: 28.0,
        padding: 10.0,
    }
}

/// Widget colours in iced terms. Each `*_text` field is the foreground that
/// reads on the surface of the same name; `border`, `input` and `ring` are
/// outline colours (resting outline, input outline, focus ring).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// The window background.
    pub surface: Color,
    /// Text on `surface`.
    pub text: Color,
    /// Menus and other popovers.
    pub popover: Color,
    /// Text on `popover`.
    pub popover_text: Color,
    /// Tooltips and other content that floats over the surface; opaque and
    /// distinct from `surface`.
    pub elevated: Color,
    /// Text on `elevated`.
    pub elevated_text: Color,
    /// Grouped content such as a channel strip.
    pub card: Color,
    /// Text on `card`.
    pub card_text: Color,
    /// The main action and fill colour.
    pub primary: Color,
    /// Text on `primary`.
    pub primary_text: Color,
    /// Dangerous actions and clipping indicators.
    pub destructive: Color,
    /// Text on `destructive`.
    pub destructive_text: Color,
    /// De-emphasised surfaces (disabled inputs, tracks).
    pub muted_surface: Color,
    /// Placeholder and disabled text.
    pub muted_text: Color,
    /// Selection highlight in inputs and menus.
    pub selection: Color,
    /// Text on `selection`.
    pub selection_text: Color,
    /// Resting outlines and separators.
    pub border: Color,
    /// Outline of an input at rest.
    pub input: Color,
    /// Focus ring.
    pub ring: Color,
}

impl Palette {
    /// A dark palette: near-black surfaces, light text, green primary.
    pub const fn dark() -> Self {
        Self {
            surface: Color::from_rgb8(27, 29, 35),
            text: Color::from_rgb8(230, 234, 241),
            popover: Color::from_rgb8(32, 36, 45),
            popover_text: Color::from_rgb8(230, 234, 241),
            elevated: Color::from_rgb8(44, 50, 61),
            elevated_text: Color::from_rgb8(230, 234, 241),
            card: Color::from_rgb8(30, 33, 40),
            card_text: Color::from_rgb8(230, 234, 241),
            primary: Color::from_rgb8(64, 160, 110),
            primary_text: Color::WHITE,
            destructive: Color::from_rgb8(205, 64, 64),
            destructive_text: Color::WHITE,
            muted_surface: Color::from_rgb8(38, 43, 53),
            muted_text: Color::from_rgb8(155, 163, 177),
            selection: Color::from_rgb8(47, 85, 130),
            selection_text: Color::WHITE,
            border: Color::from_rgb8(80, 91, 109),
            input: Color::from_rgb8(66, 77, 95),
            ring: Color::from_rgb8(143, 184, 232),
        }
    }

    /// A light palette: white surfaces, near-black text, the same hues.
    pub const fn light() -> Self {
        Self {
            surface: Color::WHITE,
            text: Color::from_rgb8(28, 30, 36),
            popover: Color::from_rgb8(250, 250, 252),
            popover_text: Color::from_rgb8(28, 30, 36),
            elevated: Color::from_rgb8(240, 242, 246),
            elevated_text: Color::from_rgb8(28, 30, 36),
            card: Color::from_rgb8(247, 248, 250),
            card_text: Color::from_rgb8(28, 30, 36),
            primary: Color::from_rgb8(36, 122, 80),
            primary_text: Color::WHITE,
            destructive: Color::from_rgb8(190, 40, 40),
            destructive_text: Color::WHITE,
            muted_surface: Color::from_rgb8(236, 238, 242),
            muted_text: Color::from_rgb8(92, 100, 114),
            selection: Color::from_rgb8(184, 208, 240),
            selection_text: Color::from_rgb8(20, 28, 44),
            border: Color::from_rgb8(214, 218, 226),
            input: Color::from_rgb8(226, 229, 235),
            ring: Color::from_rgb8(66, 133, 244),
        }
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::dark()
    }
}

/// Semantic foregrounds on ordinary surfaces. Kept separate from [`Palette`]
/// so existing caller palette literals stay source compatible.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Semantic {
    pub success: Color,
    pub warning: Color,
}

impl Semantic {
    pub const fn dark() -> Self {
        Self {
            success: Color::from_rgb8(111, 211, 155),
            warning: Color::from_rgb8(244, 193, 84),
        }
    }

    pub const fn light() -> Self {
        Self {
            success: Color::from_rgb8(24, 109, 65),
            warning: Color::from_rgb8(139, 84, 0),
        }
    }
}

/// The spacing scale, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spacing {
    pub xs: f32,
    pub sm: f32,
    pub md: f32,
    pub lg: f32,
    pub xl: f32,
}

/// Corner radii, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Radii {
    /// Small controls: thumbs, toggles, meters.
    pub sm: f32,
    /// Inputs, menus, tooltips.
    pub md: f32,
    /// Cards and panels.
    pub lg: f32,
}

/// Border widths, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Borders {
    /// Outlines and separators.
    pub width: f32,
    /// The focus ring.
    pub focus_width: f32,
}

/// Text sizes, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TypeScale {
    pub xs: f32,
    pub sm: f32,
    /// Body text and menu labels.
    pub md: f32,
    pub lg: f32,
    pub xl: f32,
    pub xxl: f32,
}

/// Font weights on the CSS 100–900 scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Weights {
    pub light: u16,
    pub regular: u16,
    pub medium: u16,
    pub bold: u16,
}

/// Sizes that do not depend on the palette.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub spacing: Spacing,
    pub radius: Radii,
    pub border: Borders,
    pub text: TypeScale,
    pub weight: Weights,
}

impl Metrics {
    /// The built-in scale: 2/4/8/16/24 spacing, 4/6/12 radii, 1 px borders
    /// with a 2 px focus ring, 11–24 px text, 300/400/500/700 weights.
    pub const DEFAULT: Self = Self {
        spacing: Spacing {
            xs: 2.0,
            sm: 4.0,
            md: 8.0,
            lg: 16.0,
            xl: 24.0,
        },
        radius: Radii {
            sm: 4.0,
            md: 6.0,
            lg: 12.0,
        },
        border: Borders {
            width: 1.0,
            focus_width: 2.0,
        },
        text: TypeScale {
            xs: 11.0,
            sm: 12.0,
            md: 14.0,
            lg: 16.0,
            xl: 20.0,
            xxl: 24.0,
        },
        weight: Weights {
            light: 300,
            regular: 400,
            medium: 500,
            bold: 700,
        },
    };
}

impl Default for Metrics {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A palette and its metrics: everything the widget styles are derived from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tokens {
    pub palette: Palette,
    pub metrics: Metrics,
}

impl Tokens {
    pub const fn new(palette: Palette, metrics: Metrics) -> Self {
        Self { palette, metrics }
    }

    /// `Palette::dark` with the default metrics.
    pub const fn dark() -> Self {
        Self::new(Palette::dark(), Metrics::DEFAULT)
    }

    /// `Palette::light` with the default metrics.
    pub const fn light() -> Self {
        Self::new(Palette::light(), Metrics::DEFAULT)
    }

    /// Style for `TextField::style` (or a plain iced `text_input`).
    pub fn text_input(self, status: text_input::Status) -> text_input::Style {
        let palette = self.palette;
        let disabled = matches!(status, text_input::Status::Disabled);
        text_input::Style {
            background: if disabled {
                palette.muted_surface
            } else {
                palette.surface
            }
            .into(),
            border: Border {
                color: match status {
                    text_input::Status::Focused { .. } => palette.ring,
                    text_input::Status::Hovered => palette.border,
                    _ => palette.input,
                },
                width: self.metrics.border.width,
                radius: self.metrics.radius.md.into(),
            },
            placeholder: palette.muted_text,
            value: if disabled {
                palette.muted_text
            } else {
                palette.text
            },
            selection: palette.selection,
        }
    }

    /// Style for `Menu::style`, keeping the default row metrics.
    pub fn menu_style(self) -> MenuStyle {
        let palette = self.palette;
        MenuStyle {
            background: palette.popover,
            text: palette.popover_text,
            disabled: palette.muted_text,
            selected: palette.selection,
            selected_text: palette.selection_text,
            border: palette.border,
            radius: self.metrics.radius.md,
            text_size: self.metrics.text.md,
            ..MenuStyle::default()
        }
    }

    /// Tooltip chrome on the `elevated` pair, so tooltip text never sits on
    /// the surface it covers. Padding belongs to the tooltip's own spacing.
    pub fn tooltip_style(self) -> container::Style {
        container::Style {
            background: Some(self.palette.elevated.into()),
            text_color: Some(self.palette.elevated_text),
            border: Border {
                color: self.palette.border,
                width: self.metrics.border.width,
                radius: self.metrics.radius.md.into(),
            },
            ..Default::default()
        }
    }

    /// Style for the pro-audio controls and canvases. Meter zones run
    /// primary (below -12 dB), selection (to -3 dB), destructive (above).
    pub fn audio_style(self) -> AudioStyle {
        let palette = self.palette;
        AudioStyle {
            background: palette.card,
            track: palette.muted_surface,
            fill: palette.primary,
            thumb: palette.card_text,
            text: palette.card_text,
            muted_text: palette.muted_text,
            border: palette.border,
            meter_low: palette.primary,
            meter_high: palette.selection,
            meter_clip: palette.destructive,
            peak: palette.card_text,
            active: palette.selection,
            active_text: palette.selection_text,
            alert: palette.destructive,
            alert_text: palette.destructive_text,
            grid: palette.border,
            lane: palette.muted_surface,
            note: palette.primary,
            waveform: palette.primary,
            playhead: palette.ring,
            radius: self.metrics.radius.sm,
        }
    }
}

/// The dark palette with the default metrics.
impl Default for Tokens {
    fn default() -> Self {
        Self::dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG relative luminance of an encoded sRGB colour.
    fn luminance(colour: Color) -> f32 {
        let channel = |c: f32| {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(colour.r) + 0.7152 * channel(colour.g) + 0.0722 * channel(colour.b)
    }

    fn contrast(a: Color, b: Color) -> f32 {
        let (l1, l2) = (luminance(a), luminance(b));
        (l1.max(l2) + 0.05) / (l1.min(l2) + 0.05)
    }

    /// Text pairs read at AA (4.5:1); filled controls at the UI threshold (3:1).
    #[test]
    fn built_in_palettes_are_readable() {
        for palette in [Palette::dark(), Palette::light()] {
            for (surface, text) in [
                (palette.surface, palette.text),
                (palette.popover, palette.popover_text),
                (palette.elevated, palette.elevated_text),
                (palette.card, palette.card_text),
                (palette.muted_surface, palette.muted_text),
                (palette.surface, palette.muted_text),
            ] {
                assert!(contrast(surface, text) >= 4.5, "{surface:?} / {text:?}");
            }
            for (surface, text) in [
                (palette.primary, palette.primary_text),
                (palette.destructive, palette.destructive_text),
                (palette.selection, palette.selection_text),
                (palette.surface, palette.ring),
            ] {
                assert!(contrast(surface, text) >= 3.0, "{surface:?} / {text:?}");
            }
            assert_eq!(palette.elevated.a, 1.0);
            assert_ne!(palette.elevated, palette.surface);
        }
        assert!(luminance(Palette::light().surface) > luminance(Palette::dark().surface));
    }

    #[test]
    fn text_input_uses_ring_on_focus_and_muted_when_disabled() {
        let tokens = Tokens::default();
        let focused = tokens.text_input(text_input::Status::Focused { is_hovered: false });
        assert_eq!(focused.border.color, tokens.palette.ring);
        assert_eq!(focused.border.width, tokens.metrics.border.width);
        assert_eq!(focused.border.radius, tokens.metrics.radius.md.into());
        assert_eq!(
            tokens.text_input(text_input::Status::Hovered).border.color,
            tokens.palette.border
        );
        let disabled = tokens.text_input(text_input::Status::Disabled);
        assert_eq!(disabled.value, tokens.palette.muted_text);
        assert_eq!(disabled.background, tokens.palette.muted_surface.into());
    }

    #[test]
    fn menu_and_audio_styles_follow_the_palette_and_metrics() {
        let tokens = Tokens::light();
        let menu = tokens.menu_style();
        assert_eq!(menu.selected_text, tokens.palette.selection_text);
        assert_eq!(menu.background, tokens.palette.popover);
        assert_eq!(menu.radius, tokens.metrics.radius.md);
        assert_eq!(menu.text_size, tokens.metrics.text.md);
        assert_eq!(menu.row_height, MenuStyle::default().row_height);
        let audio = tokens.audio_style();
        assert_eq!(audio.meter_clip, tokens.palette.destructive);
        assert_eq!(audio.background, tokens.palette.card);
        assert_eq!(audio.radius, tokens.metrics.radius.sm);
        assert_eq!(AudioStyle::default(), Tokens::default().audio_style());
    }

    #[test]
    fn tooltip_style_is_pure_and_uses_the_elevated_pair() {
        for tokens in [Tokens::dark(), Tokens::light()] {
            let style = tokens.tooltip_style();
            assert_eq!(style, tokens.tooltip_style());
            assert_eq!(style.background, Some(tokens.palette.elevated.into()));
            assert_eq!(style.text_color, Some(tokens.palette.elevated_text));
            assert_eq!(style.border.color, tokens.palette.border);
            assert_eq!(style.border.width, 1.0);
            assert_eq!(style.border.radius, tokens.metrics.radius.md.into());
        }
        let mut thick = Tokens::dark();
        thick.metrics.border.width = 2.0;
        assert_eq!(thick.tooltip_style().border.width, 2.0);
    }

    #[test]
    fn default_metrics_scale_upwards() {
        let m = Metrics::default();
        assert!(m.spacing.xs < m.spacing.sm && m.spacing.sm < m.spacing.md);
        assert!(m.spacing.md < m.spacing.lg && m.spacing.lg < m.spacing.xl);
        assert!(m.radius.sm < m.radius.md && m.radius.md < m.radius.lg);
        assert!(m.border.width < m.border.focus_width);
        assert!(m.text.xs < m.text.sm && m.text.sm < m.text.md && m.text.md < m.text.lg);
        assert!(m.text.lg < m.text.xl && m.text.xl < m.text.xxl);
        assert!(m.weight.light < m.weight.regular && m.weight.regular < m.weight.medium);
        assert!(m.weight.medium < m.weight.bold);
        assert_eq!(Tokens::default(), Tokens::dark());
    }
}
