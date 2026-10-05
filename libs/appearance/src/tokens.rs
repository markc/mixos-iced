// SPDX-License-Identifier: MIT OR Apache-2.0
//! The resolved design mapped onto toolkit's [`Tokens`]: a [`Palette`] from
//! the semantic colour pairs and non-text colours, and [`Metrics`] from the
//! metrics, scales and typography roles.
//!
//! The mapping, field by field (a design value that a toolkit field has no
//! source for takes the derivation noted, never a literal colour):
//!
//! | toolkit field | design source |
//! |---|---|
//! | `surface` / `text` | pair `base`, rendered surface / foreground |
//! | `popover` / `popover_text` | pair `popover` (the compiler's alias of `elevated`) |
//! | `elevated` / `elevated_text` | pair `elevated` |
//! | `card` / `card_text` | pair `card` (composited over its backdrop) |
//! | `primary` / `primary_text` | pair `primary` |
//! | `destructive` / `destructive_text` | pair `destructive` |
//! | `muted_surface` / `muted_text` | pair `muted` |
//! | `selection` / `selection_text` | pair `accent` (the tinted control pair) |
//! | `border`, `input`, `ring` | non-text `border`, `input`, `ring` |
//! | `spacing.xs … xl` | `spacing` scale, steps [`SPACING_STEPS`] |
//! | `radius.md` | metric `radius`; `sm` = 2/3 of it, `lg` = twice it |
//! | `border.width` | metric `button.border_width`; `focus_width` has no source |
//! | `text.md` | typography role `ui`; `xs` role `small`; `sm` metric `type.compact` |
//! | `text.lg`, `xl`, `xxl` | `md` scaled by toolkit's default ratios (no source) |
//! | `weight.light` | role `ui`; `regular` the default `md` button label; `medium`, `bold` have no source |
//!
//! The design's `secondary` pair has no toolkit slot.

use design::{
    ButtonPart, ButtonSize, ButtonTypographyKey, ButtonVariant, LinearRgba, ResolvedColours,
    ResolvedDictionary, ResolvedMetricKind, ResolvedTypography, TypographyRole,
};
use iced_core::Color;
use toolkit::tokens::{Borders, Radii, Spacing, TypeScale, Weights};
use toolkit::{Metrics, Palette, Tokens};

use crate::theme::Resolved;

/// The indices into the design's `spacing` scale that fill `xs`, `sm`, `md`,
/// `lg` and `xl`: on the shipped scale (0, 2, 4, 6, 8, 10, 12, 16, 20, 24)
/// they are 2, 4, 8, 16 and 24.
pub const SPACING_STEPS: [usize; 5] = [1, 2, 4, 7, 9];

/// The tokens of a resolved design (a [`crate::Theme`], or a design
/// artifact straight from the compiler).
pub fn tokens(design: &impl Resolved) -> Tokens {
    Tokens::new(
        palette(&design.dictionary().colours),
        metrics(design.dictionary(), design.typography()),
    )
}

/// Success/warning primitives adapted to readable foregrounds on the toolkit
/// surface. Keep the authored hue where it already clears AA; otherwise blend
/// towards the design's base foreground until the status is readable.
pub fn semantic(design: &impl Resolved) -> toolkit::tokens::Semantic {
    let colours = &design.dictionary().colours;
    let palette = palette(colours);
    let fallback = toolkit::Theme::new(tokens(design)).semantic();
    let status = |name: &str, default| {
        let Some(value) = colours.primitives.get(name) else {
            return default;
        };
        let authored = colour(*value);
        let luminance = |colour: Color| {
            let channel = |c: f32| {
                if c <= 0.04045 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(colour.r) + 0.7152 * channel(colour.g) + 0.0722 * channel(colour.b)
        };
        let readable = |colour| {
            let a = luminance(colour);
            let b = luminance(palette.surface);
            (a.max(b) + 0.05) / (a.min(b) + 0.05) >= 4.5
        };
        if readable(authored) {
            return authored;
        }
        if !readable(palette.text) {
            return default;
        }
        let blend = |weight: f32| Color {
            r: authored.r + (palette.text.r - authored.r) * weight,
            g: authored.g + (palette.text.g - authored.g) * weight,
            b: authored.b + (palette.text.b - authored.b) * weight,
            a: 1.0,
        };
        let (mut lo, mut hi) = (0.0, 1.0);
        for _ in 0..24 {
            let mid = (lo + hi) / 2.0;
            if readable(blend(mid)) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        blend(hi)
    };
    toolkit::tokens::Semantic {
        success: status("status.success", fallback.success),
        warning: status("status.warning", fallback.warning),
    }
}

/// The palette: every pair's rendered (composited) halves, so a translucent
/// surface arrives as the colour it reads at. A pair the design lacks takes
/// the base pair; a missing non-text colour takes the base foreground.
pub fn palette(colours: &ResolvedColours) -> Palette {
    let fallback = Palette::light();
    let base = colours
        .pairs
        .get("base")
        .map(|pair| {
            (
                colour(pair.rendered_surface),
                colour(pair.rendered_foreground),
            )
        })
        .unwrap_or((fallback.surface, fallback.text));
    let pair = |name: &str| {
        colours
            .pairs
            .get(name)
            .map(|pair| {
                (
                    colour(pair.rendered_surface),
                    colour(pair.rendered_foreground),
                )
            })
            .unwrap_or(base)
    };
    let non_text = |name: &str| {
        colours
            .non_text
            .get(name)
            .map(|value| colour(value.value))
            .unwrap_or(base.1)
    };
    let (surface, text) = base;
    let (popover, popover_text) = pair("popover");
    let (elevated, elevated_text) = pair("elevated");
    let (card, card_text) = pair("card");
    let (primary, primary_text) = pair("primary");
    let (destructive, destructive_text) = pair("destructive");
    let (muted_surface, muted_text) = pair("muted");
    let (selection, selection_text) = pair("accent");
    Palette {
        surface,
        text,
        popover,
        popover_text,
        elevated,
        elevated_text,
        card,
        card_text,
        primary,
        primary_text,
        destructive,
        destructive_text,
        muted_surface,
        muted_text,
        selection,
        selection_text,
        border: non_text("border"),
        input: non_text("input"),
        ring: non_text("ring"),
    }
}

/// The metrics. Every field the design does not author keeps toolkit's
/// default or is derived from a field it does, as the module table says.
pub fn metrics(dictionary: &ResolvedDictionary, typography: &ResolvedTypography) -> Metrics {
    let default = Metrics::DEFAULT;
    let px = |name: &str| {
        dictionary
            .metrics
            .get(name)
            .filter(|metric| metric.kind == ResolvedMetricKind::Px)
            .map(|metric| metric.value as f32)
    };
    let step = |scale: &str, index: usize| {
        dictionary
            .scales
            .get(scale)
            .and_then(|scale| scale.get(index))
            .map(|value| *value as f32)
    };
    let [xs, sm, md, lg, xl] = SPACING_STEPS.map(|index| step("spacing", index));
    let radius = px("radius").unwrap_or(default.radius.md);
    let ui = design::active_typography(Some(typography), TypographyRole::Ui);
    let small = design::active_typography(Some(typography), TypographyRole::Small);
    let label = typography
        .button(ButtonTypographyKey {
            variant: ButtonVariant::Default,
            size: ButtonSize::Md,
            part: ButtonPart::Label,
        })
        .record;
    let body = ui.font_size as f32;
    Metrics {
        spacing: Spacing {
            xs: xs.unwrap_or(default.spacing.xs),
            sm: sm.unwrap_or(default.spacing.sm),
            md: md.unwrap_or(default.spacing.md),
            lg: lg.unwrap_or(default.spacing.lg),
            xl: xl.unwrap_or(default.spacing.xl),
        },
        radius: Radii {
            sm: radius * 2.0 / 3.0,
            md: radius,
            lg: radius * 2.0,
        },
        border: Borders {
            width: px("button.border_width").unwrap_or(default.border.width),
            focus_width: default.border.focus_width,
        },
        text: TypeScale {
            xs: small.font_size as f32,
            sm: px("type.compact").unwrap_or(default.text.sm),
            md: body,
            lg: body * default.text.lg / default.text.md,
            xl: body * default.text.xl / default.text.md,
            xxl: body * default.text.xxl / default.text.md,
        },
        weight: Weights {
            light: ui.weight,
            regular: label.weight,
            medium: default.weight.medium,
            bold: default.weight.bold,
        },
    }
}

/// A design colour (linear-light sRGB) as the iced `Color` it renders as.
pub fn colour(value: LinearRgba) -> Color {
    Color::from_linear_rgba(
        value.red as f32,
        value.green as f32,
        value.blue as f32,
        value.alpha as f32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Theme;
    use design::{DesignContext, Mode, Scheme};

    /// WCAG relative luminance of an encoded sRGB colour, as toolkit's own
    /// palette test measures it.
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

    fn shipped() -> impl Iterator<Item = Theme> {
        Scheme::ALL.into_iter().flat_map(|scheme| {
            Mode::ALL.into_iter().map(move |mode| {
                Theme::for_context(DesignContext {
                    scheme,
                    mode,
                    ..DesignContext::default()
                })
            })
        })
    }

    /// toolkit's expectations of a palette, applied to every shipped theme:
    /// text pairs at AA (4.5:1), filled controls and the ring at 3:1, an
    /// opaque elevated surface distinct from the window.
    #[test]
    fn shipped_themes_meet_the_toolkit_contrast_bar() {
        for theme in shipped() {
            let palette = tokens(&theme).palette;
            let label = format!("{} {}", theme.scheme().name(), theme.mode().name());
            let semantic = semantic(&theme);
            for colour in [semantic.success, semantic.warning] {
                assert!(
                    contrast(palette.surface, colour) >= 4.5,
                    "{label}: status {colour:?}"
                );
            }
            for (surface, text) in [
                (palette.surface, palette.text),
                (palette.popover, palette.popover_text),
                (palette.elevated, palette.elevated_text),
                (palette.card, palette.card_text),
                (palette.muted_surface, palette.muted_text),
                (palette.surface, palette.muted_text),
            ] {
                let ratio = contrast(surface, text);
                assert!(ratio >= 4.5, "{label}: {surface:?} / {text:?} = {ratio}");
            }
            for (surface, text) in [
                (palette.primary, palette.primary_text),
                (palette.destructive, palette.destructive_text),
                (palette.selection, palette.selection_text),
                (palette.surface, palette.ring),
            ] {
                let ratio = contrast(surface, text);
                assert!(ratio >= 3.0, "{label}: {surface:?} / {text:?} = {ratio}");
            }
            assert_eq!(palette.elevated.a, 1.0, "{label}");
            assert_ne!(palette.elevated, palette.surface, "{label}");
            assert_eq!(
                luminance(palette.surface) < luminance(palette.text),
                theme.mode() == Mode::Dark,
                "{label}"
            );
        }
    }

    #[test]
    fn palette_takes_the_rendered_halves_of_each_pair() {
        let theme = Theme::embedded();
        let colours = &theme.dictionary().colours;
        let mapped = palette(colours);
        for (name, surface, text) in [
            ("base", mapped.surface, mapped.text),
            ("popover", mapped.popover, mapped.popover_text),
            ("elevated", mapped.elevated, mapped.elevated_text),
            ("card", mapped.card, mapped.card_text),
            ("primary", mapped.primary, mapped.primary_text),
            ("destructive", mapped.destructive, mapped.destructive_text),
            ("muted", mapped.muted_surface, mapped.muted_text),
            ("accent", mapped.selection, mapped.selection_text),
        ] {
            let pair = &colours.pairs[name];
            assert_eq!(surface, colour(pair.rendered_surface), "{name}");
            assert_eq!(text, colour(pair.rendered_foreground), "{name}");
        }
        assert_eq!(mapped.border, colour(colours.non_text["border"].value));
        assert_eq!(mapped.input, colour(colours.non_text["input"].value));
        assert_eq!(mapped.ring, colour(colours.non_text["ring"].value));
        // The card surface is authored transparent over the base backdrop:
        // what arrives is the composite, never the transparent source.
        assert_eq!(mapped.card.a, 1.0);
        assert_eq!(mapped.popover, mapped.elevated);
    }

    #[test]
    fn missing_roles_fall_back_to_base_never_to_a_literal() {
        let theme = Theme::embedded();
        let mut colours = theme.dictionary().colours.clone();
        colours.pairs.remove("accent");
        colours.non_text.remove("ring");
        let mapped = palette(&colours);
        assert_eq!(mapped.selection, mapped.surface);
        assert_eq!(mapped.selection_text, mapped.text);
        assert_eq!(mapped.ring, mapped.text);
        let empty = palette(&ResolvedColours::default());
        assert_eq!(empty.surface, Palette::light().surface);
        assert_eq!(empty.ring, Palette::light().text);
    }

    #[test]
    fn embedded_metrics_follow_the_design() {
        let theme = Theme::embedded();
        let m = metrics(theme.dictionary(), theme.typography());
        assert_eq!(
            [
                m.spacing.xs,
                m.spacing.sm,
                m.spacing.md,
                m.spacing.lg,
                m.spacing.xl
            ],
            [2.0, 4.0, 8.0, 16.0, 24.0]
        );
        assert_eq!([m.radius.sm, m.radius.md, m.radius.lg], [4.0, 6.0, 12.0]);
        assert_eq!(m.border.width, 1.0);
        assert_eq!(m.border.focus_width, Metrics::DEFAULT.border.focus_width);
        let ui = design::active_typography(Some(theme.typography()), TypographyRole::Ui);
        assert_eq!(m.text.md, ui.font_size as f32);
        assert!((m.text.md - 14.667).abs() < 0.01, "{}", m.text.md);
        assert!((m.text.xs - 10.667).abs() < 0.01, "{}", m.text.xs);
        assert!((m.text.sm - 11.333).abs() < 0.01, "{}", m.text.sm);
        assert!(m.text.xs < m.text.sm && m.text.sm < m.text.md && m.text.md < m.text.lg);
        assert!(m.text.lg < m.text.xl && m.text.xl < m.text.xxl);
        assert_eq!(m.weight.light, 300, "UI text is Light");
        assert_eq!(m.weight.regular, 400);
        assert!(m.weight.regular < m.weight.medium && m.weight.medium < m.weight.bold);
        assert_eq!(tokens(&theme).metrics, m);
        assert_eq!(tokens(theme.design()).metrics, m);
    }

    #[test]
    fn absent_metrics_keep_the_defaults() {
        let theme = Theme::embedded();
        let m = metrics(&ResolvedDictionary::default(), theme.typography());
        assert_eq!(m.spacing, Metrics::DEFAULT.spacing);
        assert_eq!(m.radius, Metrics::DEFAULT.radius);
        assert_eq!(m.border, Metrics::DEFAULT.border);
        assert_eq!(m.text.sm, Metrics::DEFAULT.text.sm);
    }

    #[test]
    fn colours_convert_from_linear_light() {
        let close = |a: Color, b: Color| {
            (a.r - b.r).abs() < 1e-5 && (a.g - b.g).abs() < 1e-5 && (a.b - b.b).abs() < 1e-5
        };
        assert!(close(colour(LinearRgba::WHITE), Color::WHITE));
        assert!(close(colour(LinearRgba::BLACK), Color::BLACK));
        let mid = colour(LinearRgba {
            red: 0.2140,
            green: 0.2140,
            blue: 0.2140,
            alpha: 0.5,
        });
        assert!((mid.r - 0.5).abs() < 0.01, "{}", mid.r);
        assert_eq!(mid.a, 0.5);
    }
}
