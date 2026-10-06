// SPDX-License-Identifier: MIT OR Apache-2.0
//! Adapter-side mapping of resolved design colours; no iced dependency leaks
//! into `cosmix-design`. These are provisional widget mappings, not additions
//! to the design compiler's closed family registry.

use ::design::{LinearRgba, ResolvedColours, ResolvedDictionary, ResolvedMetricKind};
use iced_core::Color;
#[cfg(test)]
use iced_widget::text_input;

use toolkit::{Metrics, Palette, Tokens};

/// Missing or invalid resolved dictionary entry. Never silently substitutes a
/// fallback for a partially compiled design.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenError(pub &'static str);

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "missing or invalid design token: {}", self.0)
    }
}

impl std::error::Error for TokenError {}

/// Converts linear-light design colours to iced's encoded sRGB components.
pub fn colour(value: LinearRgba) -> Color {
    // Use the model's canonical transfer function and quantisation, including
    // alpha. Passing linear channels directly makes middle greys too dark.
    let [r, g, b, a] = value.to_srgba8();
    Color::from_rgba8(r, g, b, f32::from(a) / 255.0)
}

/// Maps the `base`, `popover`, `elevated`, `card`, `primary`,
/// `destructive`, `muted` and `accent` pairs plus the
/// `border`, `input` and `ring` colours. Radius is 6 px.
pub fn from_colours(colours: &ResolvedColours) -> Result<Tokens, TokenError> {
    let pair = |name| colours.pairs.get(name).ok_or(TokenError(name));
    let non_text = |name| {
        colours
            .non_text
            .get(name)
            .map(|v| colour(v.value))
            .ok_or(TokenError(name))
    };
    let base = pair("base")?;
    let popover = pair("popover")?;
    let elevated = pair("elevated")?;
    let muted = pair("muted")?;
    let accent = pair("accent")?;
    let card = pair("card")?;
    let primary = pair("primary")?;
    let destructive = pair("destructive")?;
    Ok(Tokens {
        palette: Palette {
            surface: colour(base.rendered_surface),
            text: colour(base.rendered_foreground),
            popover: colour(popover.rendered_surface),
            popover_text: colour(popover.rendered_foreground),
            elevated: colour(elevated.rendered_surface),
            elevated_text: colour(elevated.rendered_foreground),
            card: colour(card.rendered_surface),
            card_text: colour(card.rendered_foreground),
            primary: colour(primary.rendered_surface),
            primary_text: colour(primary.rendered_foreground),
            destructive: colour(destructive.rendered_surface),
            destructive_text: colour(destructive.rendered_foreground),
            muted_surface: colour(muted.rendered_surface),
            muted_text: colour(muted.rendered_foreground),
            selection: colour(accent.rendered_surface),
            selection_text: colour(accent.rendered_foreground),
            border: non_text("border")?,
            input: non_text("input")?,
            ring: non_text("ring")?,
        },
        metrics: Metrics::DEFAULT,
    })
}

/// As `from_colours`, with the radius from the `radius.md` px metric.
pub fn from_dictionary(dictionary: &ResolvedDictionary) -> Result<Tokens, TokenError> {
    let mut tokens = from_colours(&dictionary.colours)?;
    let radius = dictionary
        .metrics
        .get("radius.md")
        .ok_or(TokenError("radius.md"))?;
    if radius.kind != ResolvedMetricKind::Px
        || !radius.value.is_finite()
        || radius.value < 0.0
        || radius.value > f32::MAX as f64
    {
        return Err(TokenError("radius.md"));
    }
    tokens.metrics.radius.md = radius.value as f32;
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::design::{ResolvedMetric, ResolvedNonTextColour, ResolvedPair};

    fn dictionary() -> ResolvedDictionary {
        let mut colours = ResolvedColours::default();
        for name in [
            "base",
            "popover",
            "elevated",
            "muted",
            "accent",
            "card",
            "primary",
            "destructive",
        ] {
            colours.pairs.insert(
                name.into(),
                ResolvedPair {
                    surface_name: name.into(),
                    surface: LinearRgba::WHITE,
                    foreground_name: name.into(),
                    foreground: LinearRgba::BLACK,
                    backdrop_name: None,
                    backdrop: None,
                    rendered_surface: LinearRgba::BLACK,
                    rendered_foreground: LinearRgba::WHITE,
                    contrast_ratio: 21.0,
                    recipe: None,
                },
            );
        }
        for name in ["border", "input", "ring"] {
            colours.non_text.insert(
                name.into(),
                ResolvedNonTextColour {
                    value_name: name.into(),
                    value: LinearRgba::WHITE,
                    adjacent: Default::default(),
                },
            );
        }
        ResolvedDictionary {
            colours,
            metrics: [(
                "radius.md".into(),
                ResolvedMetric {
                    kind: ResolvedMetricKind::Px,
                    value: 9.0,
                },
            )]
            .into(),
            scales: Default::default(),
        }
    }

    #[test]
    fn linear_conversion_preserves_alpha_and_encodes_grey() {
        let mapped = colour(LinearRgba {
            red: 0.5,
            green: 0.5,
            blue: 0.5,
            alpha: 0.25,
        });
        assert_eq!(mapped, Color::from_rgba8(188, 188, 188, 64.0 / 255.0));
    }

    #[test]
    fn mapping_uses_rendered_pairs_and_focus_token() {
        let tokens = from_dictionary(&dictionary()).unwrap();
        assert_eq!(tokens.palette.surface, Color::BLACK);
        assert_eq!(tokens.palette.text, Color::WHITE);
        assert_eq!(tokens.metrics.radius.md, 9.0);
        assert_eq!(
            tokens
                .text_input(text_input::Status::Focused { is_hovered: false })
                .border
                .color,
            tokens.palette.ring
        );
        assert_eq!(
            tokens.text_input(text_input::Status::Disabled).value,
            tokens.palette.muted_text
        );
        assert_eq!(
            tokens.menu_style().selected_text,
            tokens.palette.selection_text
        );
        let audio = tokens.audio_style();
        assert_eq!(audio.meter_clip, tokens.palette.destructive);
        assert_eq!(audio.background, tokens.palette.card);
        assert_eq!(audio.radius, 4.0);
    }

    #[test]
    fn tooltip_style_is_pure_and_uses_the_elevated_pair() {
        for tokens in [Tokens::default(), from_dictionary(&dictionary()).unwrap()] {
            let style = crate::tooltip_style(tokens, 1.0);
            assert_eq!(style, crate::tooltip_style(tokens, 1.0));
            assert_eq!(style.background, Some(tokens.palette.elevated.into()));
            assert_eq!(style.text_color, Some(tokens.palette.elevated_text));
            assert_eq!(tokens.palette.elevated.a, 1.0);
            assert_eq!(style.border.color, tokens.palette.border);
            assert_eq!(style.border.width, 1.0);
            assert_eq!(style.border.radius, tokens.metrics.radius.md.into());
            assert_eq!(crate::tooltip_style(tokens, 2.0).border.width, 2.0);
        }
    }

    #[test]
    fn incomplete_or_wrong_unit_dictionary_is_rejected() {
        assert_eq!(
            from_colours(&ResolvedColours::default()),
            Err(TokenError("base"))
        );
        let mut dictionary = dictionary();
        dictionary.metrics.get_mut("radius.md").unwrap().kind = ResolvedMetricKind::Ratio;
        assert_eq!(from_dictionary(&dictionary), Err(TokenError("radius.md")));
    }
}
