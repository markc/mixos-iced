//! The chrome theme: the `layout` style preset for the SHAPE, the design
//! tokens for every COLOUR, the corner radius (mixos style) and the title font.
//!
//! The preset decides what exists and how big it is: titlebar height, button
//! shape, side, order and spacing, border thickness, resize band, shadow
//! extent, and whether a slot is drawn at all (a slot the preset leaves fully
//! transparent, such as mac's border or win11's divider, stays transparent).
//! The tokens decide what colour everything that IS drawn has. A slot keeps the
//! preset's alpha and takes the token's colour, so a translucent hover overlay
//! stays an overlay in the theme's hue.
//!
//! The tokens come from `theme.conf.mix` (see [`theme_path`]) compiled by
//! the design tokens; without that file, from the design library's embedded default.
//! Either way no colour is hard-coded here. The one exception is the shadow,
//! which stays the preset's black: the design system has no shadow token.

use std::path::PathBuf;

use crate::layout::{
    ButtonColors, ChromeStyle, DecoFontFamily, DecoFontWeight, DecoTheme, Srgba, presets,
};
use design::{
    Contrast, DesignCompileResult, DesignContext, LinearRgba, ResolvedMetricKind, SourceIdentity,
    TypographyRole, UnstampedResolvedDesign,
};

/// A theme colour as 8-bit straight-alpha sRGB `[r, g, b, a]`, as the truth
/// reports it and a screenshot reads it.
pub fn rgba8(c: Srgba) -> [u8; 4] {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [q(c.r), q(c.g), q(c.b), q(c.a)]
}

/// Where the tokens came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenSource {
    /// The shared `theme.conf.mix`.
    File,
    /// the design library's embedded default (no file, or it did not compile).
    Embedded,
}

/// A resolved chrome theme.
#[derive(Clone, Debug, PartialEq)]
pub struct ChromeTheme {
    pub deco: DecoTheme,
    pub tokens: TokenSource,
    /// The same design's tokens for in-compositor content (Mix Scenes): one
    /// compile serves both, so the scene host never loads the theme again.
    pub palette: Palette,
}

/// A design-token surface with the text drawn on it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pair {
    pub surface: Srgba,
    pub foreground: Srgba,
}

/// The design roles in-compositor content draws with: `base` (the
/// page), `secondary` (a normal control), `primary` and `destructive` (the
/// button tones), `muted` (secondary text), and the non-text `border` and
/// `ring` (focus).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Palette {
    pub base: Pair,
    pub secondary: Pair,
    pub primary: Pair,
    pub destructive: Pair,
    pub muted: Pair,
    pub border: Srgba,
    pub ring: Srgba,
}

impl Palette {
    /// The roles from a compiled design. A role the design does not resolve
    /// falls back to the base pair (or its foreground, for the non-text
    /// tokens), never to a literal colour.
    pub fn from_design(design: &UnstampedResolvedDesign) -> Palette {
        let colours = &design.dictionary().colours;
        let pair = |name: &str| {
            colours.pairs.get(name).map(|p| Pair {
                surface: srgba(p.rendered_surface),
                foreground: srgba(p.rendered_foreground),
            })
        };
        let non_text = |name: &str| colours.non_text.get(name).map(|c| srgba(c.value));
        let base = pair("base").unwrap_or_default();
        Palette {
            base,
            secondary: pair("secondary").unwrap_or(base),
            primary: pair("primary").unwrap_or(base),
            destructive: pair("destructive").unwrap_or(base),
            muted: pair("muted").unwrap_or(base),
            border: non_text("border").unwrap_or(base.foreground),
            ring: non_text("ring").unwrap_or(base.foreground),
        }
    }
}

impl ChromeTheme {
    /// `style`, themed from the shared `theme.conf.mix` (read once; a missing
    /// or broken file falls back to the embedded default tokens).
    pub fn load(style: ChromeStyle) -> ChromeTheme {
        let source = std::fs::read_to_string(theme_path()).ok();
        ChromeTheme::from_source(style, source.as_deref())
    }

    /// `style`, themed from `source` (a `theme.conf.mix` document), or from
    /// the embedded default when there is none or it does not compile.
    pub fn from_source(style: ChromeStyle, source: Option<&str>) -> ChromeTheme {
        let (design, tokens) = match source.and_then(compile) {
            Some(design) => (design, TokenSource::File),
            None => (
                compile(design::EMBEDDED_DEFAULT_SOURCE)
                    .expect("the design library's embedded default compiles"),
                TokenSource::Embedded,
            ),
        };
        let mut deco = presets::resolve(style, scheme_of(&design), mode_of(&design));
        apply_tokens(&mut deco, &design);
        let palette = Palette::from_design(&design);
        ChromeTheme { deco, tokens, palette }
    }
}

/// The shared theme file: `theme.conf.mix` in the MixOS etc dir
/// (`config::path(Dir::Etc)`: `$MIXOS_ETC`, else `$MIXOS/etc`, else
/// `/etc/mixos` for root, else `$XDG_CONFIG_HOME/mixos`).
pub fn theme_path() -> PathBuf {
    config::path(config::Dir::Etc).join("theme.conf.mix")
}

/// Parse and compile a `theme.conf.mix` for the scheme and mode it names.
fn compile(source: &str) -> Option<UnstampedResolvedDesign> {
    // Quoin/CTK also accepts the shared, selection-only {scheme, mode}
    // file. Resolve that selection against the embedded design, without
    // changing its legacy crosswalk (which the compiler validates separately).
    let (document, selection) = match design::parse_design_source(SourceIdentity::new("compd:chrome"), source) {
        Ok(document) => {
            let selection = document.legacy.clone();
            (document, selection)
        }
        Err(_) => {
            let value = config::parse(source).ok()?;
            let config::Value::Map(fields) = &value else { return None };
            if fields.keys().any(|key| key != "scheme" && key != "mode") {
                return None;
            }
            let selection = design::parse_legacy_v0_source(source).ok()?;
            if !selection.is_selection_only() {
                return None;
            }
            let document = design::parse_design_source(
                SourceIdentity::new("compd:chrome:selection"), design::EMBEDDED_DEFAULT_SOURCE,
            ).ok()?;
            (document, selection)
        }
    };
    let context = DesignContext {
        scheme: selection
            .scheme
            .as_deref()
            .map(design::Scheme::from_name)
            .unwrap_or(Some(design::Scheme::default()))?,
        mode: selection
            .mode
            .as_deref()
            .map(design::Mode::from_name)
            .unwrap_or(Some(design::Mode::default()))?,
        contrast: Contrast::default(),
        app: None,
    };
    match design::compile_design(&document, context) {
        DesignCompileResult::Success(success) => Some(success.candidate),
        DesignCompileResult::Fatal(_) => None,
    }
}

fn scheme_of(_design: &UnstampedResolvedDesign) -> crate::layout::Scheme {
    // The compiled colours already carry the scheme; the deco scheme only
    // seeds the mixos preset's accent, which `apply_tokens` replaces.
    crate::layout::Scheme::default()
}

fn mode_of(design: &UnstampedResolvedDesign) -> crate::layout::Mode {
    // Light or dark preset metrics are identical; pick by the base surface's
    // lightness so a preset slot the tokens do not cover still matches.
    match design.dictionary().colours.pairs.get("base") {
        Some(base) if luminance(base.rendered_surface) < 0.18 => crate::layout::Mode::Dark,
        _ => crate::layout::Mode::Light,
    }
}

fn luminance(c: LinearRgba) -> f64 {
    0.2126 * c.red + 0.7152 * c.green + 0.0722 * c.blue
}

/// A token colour as the sRGB the chrome draws with.
fn srgba(c: LinearRgba) -> Srgba {
    let [r, g, b, a] = c.to_srgba8();
    Srgba::new(
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    )
}

/// Give `slot` the token's colour, keeping the preset's alpha (scaled by the
/// token's own). A slot the preset leaves fully transparent is not drawn and
/// stays so; a missing token leaves the preset's colour.
fn recolour(slot: &mut Srgba, token: Option<Srgba>) {
    if slot.a <= 0.0 {
        return;
    }
    if let Some(token) = token {
        *slot = Srgba::new(token.r, token.g, token.b, slot.a * token.a);
    }
}

fn recolour_button(button: &mut ButtonColors, fill: Option<Srgba>, glyph: Option<Srgba>) {
    recolour(&mut button.fill_idle, fill);
    recolour(&mut button.fill_hover, fill);
    recolour(&mut button.fill_pressed, fill);
    recolour(&mut button.glyph, glyph);
    recolour(&mut button.glyph_hover, glyph);
}

/// Every colour from the tokens; the radius (mixos style) and the title font
/// from the design. The token names are the design library's resolved roles:
/// text pairs `card` (focused titlebar), `muted` (unfocused), `destructive`
/// (close), `accent`; non-text `border` and `ring` (focus); primitives
/// `status.{danger,warning,success}` (mac's three lights).
pub fn apply_tokens(theme: &mut DecoTheme, design: &UnstampedResolvedDesign) {
    let colours = &design.dictionary().colours;
    let surface = |name: &str| colours.pairs.get(name).map(|p| srgba(p.rendered_surface));
    let foreground = |name: &str| {
        colours
            .pairs
            .get(name)
            .map(|p| srgba(p.rendered_foreground))
    };
    let non_text = |name: &str| colours.non_text.get(name).map(|c| srgba(c.value));
    let primitive = |name: &str| colours.primitives.get(name).copied().map(srgba);

    let c = &mut theme.colors;
    recolour(&mut c.titlebar_focused, surface("card"));
    recolour(&mut c.titlebar_unfocused, surface("muted"));
    recolour(&mut c.title_text_focused, foreground("card"));
    recolour(&mut c.title_text_unfocused, foreground("muted"));
    recolour(&mut c.titlebar_divider, non_text("border"));
    recolour(&mut c.border_unfocused, non_text("border"));
    let focus_border = match theme.style {
        ChromeStyle::Mixos => non_text("ring"),
        ChromeStyle::Mac | ChromeStyle::Win11 => non_text("border"),
    };
    recolour(&mut c.border_focused, focus_border);

    let b = &mut theme.buttons;
    match theme.style {
        ChromeStyle::Mac => {
            // The three lights are status colours; their glyphs the base text.
            let glyph = foreground("base");
            recolour_button(&mut b.close, primitive("status.danger"), glyph);
            recolour_button(&mut b.minimize, primitive("status.warning"), glyph);
            recolour_button(&mut b.maximize, primitive("status.success"), glyph);
            for button in [&mut b.close, &mut b.minimize, &mut b.maximize] {
                recolour(&mut button.fill_unfocused, non_text("border"));
            }
        }
        ChromeStyle::Win11 | ChromeStyle::Mixos => {
            let glyph = foreground("card");
            // Idle and unfocused fills are translucent overlays of the text
            // colour; hover takes the accent (mixos) or the text overlay
            // (win11), close hover the destructive pair.
            let hover = match theme.style {
                ChromeStyle::Mixos => surface("accent"),
                _ => glyph,
            };
            for button in [&mut b.minimize, &mut b.maximize] {
                recolour(&mut button.fill_idle, glyph);
                recolour(&mut button.fill_unfocused, glyph);
                recolour(&mut button.fill_hover, hover);
                recolour(&mut button.fill_pressed, hover);
                recolour(&mut button.glyph, glyph);
                recolour(&mut button.glyph_hover, glyph);
            }
            recolour(&mut b.close.fill_idle, glyph);
            recolour(&mut b.close.fill_unfocused, glyph);
            recolour(&mut b.close.fill_hover, surface("destructive"));
            recolour(&mut b.close.fill_pressed, surface("destructive"));
            recolour(&mut b.close.glyph, glyph);
            recolour(&mut b.close.glyph_hover, foreground("destructive"));
        }
    }

    // The preset picks the shape family; the theme's radius wins for the
    // mixos style (mac and win11 keep their platform radius).
    if theme.style == ChromeStyle::Mixos
        && let Some(radius) = design.dictionary().metrics.get("radius")
        && radius.kind == ResolvedMetricKind::Px
    {
        theme.metrics.corner_radius = radius.value as f32;
    }

    let title =
        design::active_typography(Some(design.typography()), TypographyRole::UiDisplay);
    theme.metrics.title_font_family = DecoFontFamily::Named(title.family.clone());
    theme.metrics.title_size_px = title.font_size as f32;
    theme.metrics.title_font_weight = DecoFontWeight(title.weight).resolved();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn embedded() -> UnstampedResolvedDesign {
        compile(design::EMBEDDED_DEFAULT_SOURCE).expect("embedded default compiles")
    }

    fn close(a: Srgba, b: Srgba) -> bool {
        (a.r - b.r).abs() < 1e-3
            && (a.g - b.g).abs() < 1e-3
            && (a.b - b.b).abs() < 1e-3
            && (a.a - b.a).abs() < 1e-3
    }

    #[test]
    fn no_file_themes_from_the_embedded_default() {
        let theme = ChromeTheme::from_source(ChromeStyle::Mac, None);
        assert_eq!(theme.tokens, TokenSource::Embedded);
        let broken = ChromeTheme::from_source(ChromeStyle::Mac, Some("this is not mix {"));
        assert_eq!(broken.tokens, TokenSource::Embedded);
        assert_eq!(theme, broken);
    }

    #[test]
    fn a_shared_selection_resolves_the_whole_design_pair_in_that_context() {
        for scheme in design::Scheme::ALL {
            for mode in design::Mode::ALL {
                let source = format!("{{scheme: \"{}\", mode: \"{}\"}}", scheme.name(), mode.name());
                let theme = ChromeTheme::from_source(ChromeStyle::Mac, Some(&source));
                assert_eq!(theme.tokens, TokenSource::File);
                let document = design::parse_design_source(
                    SourceIdentity::new("test:selection"), design::EMBEDDED_DEFAULT_SOURCE,
                ).unwrap();
                let DesignCompileResult::Success(expected) = design::compile_design(&document, DesignContext {
                    scheme, mode, contrast: Contrast::default(), app: None,
                }) else { panic!("embedded design compiles") };
                assert_eq!(theme.palette, Palette::from_design(&expected.candidate));
                let pair = &expected.candidate.dictionary().colours.pairs["base"];
                assert_eq!(luminance(pair.rendered_surface) < luminance(pair.rendered_foreground), mode == design::Mode::Dark);
            }
        }
        assert!(compile("{scheme: \"invalid\", mode: \"dark\"}").is_none());
        assert!(compile("{scheme: \"ocean\", mode: \"invalid\"}").is_none());
        assert!(compile("{scheme: \"ocean\", mode: \"dark\", design: {schema_version: 999}}").is_none());
    }

    #[test]
    fn the_embedded_default_compiled_as_a_file_is_the_same_theme() {
        let file = ChromeTheme::from_source(
            ChromeStyle::Mixos,
            Some(design::EMBEDDED_DEFAULT_SOURCE),
        );
        assert_eq!(file.tokens, TokenSource::File);
        assert_eq!(
            file.deco,
            ChromeTheme::from_source(ChromeStyle::Mixos, None).deco
        );
    }

    /// Every drawn colour comes from a token, in every style: the titlebar and
    /// title are the card / muted pairs.
    #[test]
    fn titlebar_and_title_are_the_card_and_muted_pairs() {
        let design = embedded();
        let pairs = &design.dictionary().colours.pairs;
        for style in ChromeStyle::ALL {
            let theme = ChromeTheme::from_source(style, None).deco;
            assert!(
                close(
                    theme.colors.titlebar_focused,
                    srgba(pairs["card"].rendered_surface)
                ),
                "{style:?}"
            );
            assert!(
                close(
                    theme.colors.titlebar_unfocused,
                    srgba(pairs["muted"].rendered_surface)
                ),
                "{style:?}"
            );
            assert!(
                close(
                    theme.colors.title_text_focused,
                    srgba(pairs["card"].rendered_foreground)
                ),
                "{style:?}"
            );
            assert!(
                close(
                    theme.colors.title_text_unfocused,
                    srgba(pairs["muted"].rendered_foreground)
                ),
                "{style:?}"
            );
        }
    }

    /// The preset decides presence: mac draws no border and win11 no divider,
    /// whatever the tokens say.
    #[test]
    fn slots_a_style_does_not_draw_stay_transparent() {
        let mac = ChromeTheme::from_source(ChromeStyle::Mac, None).deco;
        assert_eq!(mac.colors.border_focused.a, 0.0);
        assert_eq!(mac.colors.border_unfocused.a, 0.0);
        let win11 = ChromeTheme::from_source(ChromeStyle::Win11, None).deco;
        assert_eq!(win11.colors.titlebar_divider.a, 0.0);
        assert_eq!(win11.buttons.minimize.fill_idle.a, 0.0);
    }

    /// A translucent overlay keeps the preset's alpha in the token's hue.
    #[test]
    fn overlays_keep_the_preset_alpha() {
        let preset = presets::resolve(
            ChromeStyle::Mixos,
            crate::layout::Scheme::Ocean,
            crate::layout::Mode::Light,
        );
        let theme = ChromeTheme::from_source(ChromeStyle::Mixos, None).deco;
        assert!(
            (theme.buttons.minimize.fill_hover.a - preset.buttons.minimize.fill_hover.a).abs()
                < 1e-6
        );
        let accent = srgba(embedded().dictionary().colours.pairs["accent"].rendered_surface);
        let hover = theme.buttons.minimize.fill_hover;
        assert!(close(
            Srgba::new(hover.r, hover.g, hover.b, 1.0),
            accent.with_alpha(1.0)
        ));
    }

    /// The scene palette is the same compile's pairs, not a second load.
    #[test]
    fn the_palette_carries_the_designs_pairs() {
        let design = embedded();
        let colours = &design.dictionary().colours;
        let palette = ChromeTheme::from_source(ChromeStyle::Mac, None).palette;
        for (pair, name) in [
            (palette.base, "base"),
            (palette.secondary, "secondary"),
            (palette.primary, "primary"),
            (palette.destructive, "destructive"),
            (palette.muted, "muted"),
        ] {
            assert!(close(pair.surface, srgba(colours.pairs[name].rendered_surface)), "{name}");
            assert!(close(pair.foreground, srgba(colours.pairs[name].rendered_foreground)), "{name}");
        }
        assert!(close(palette.border, srgba(colours.non_text["border"].value)));
        assert!(!close(palette.primary.surface, palette.base.surface), "a primary button is distinguishable");
    }

    #[test]
    fn close_hover_is_the_destructive_pair_and_mac_lights_are_status_colours() {
        let design = embedded();
        let colours = &design.dictionary().colours;
        for style in [ChromeStyle::Win11, ChromeStyle::Mixos] {
            let theme = ChromeTheme::from_source(style, None).deco;
            let hover = theme.buttons.close.fill_hover;
            assert!(close(
                Srgba::new(hover.r, hover.g, hover.b, 1.0),
                srgba(colours.pairs["destructive"].rendered_surface).with_alpha(1.0)
            ));
        }
        let mac = ChromeTheme::from_source(ChromeStyle::Mac, None).deco;
        for (button, token) in [
            (mac.buttons.close, "status.danger"),
            (mac.buttons.minimize, "status.warning"),
            (mac.buttons.maximize, "status.success"),
        ] {
            assert!(
                close(button.fill_idle, srgba(colours.primitives[token])),
                "{token}"
            );
        }
    }

    /// The theme radius wins for the mixos style only.
    #[test]
    fn only_the_mixos_style_takes_the_theme_radius() {
        let design = embedded();
        let radius = design.dictionary().metrics["radius"].value as f32;
        assert_eq!(
            ChromeTheme::from_source(ChromeStyle::Mixos, None)
                .deco
                .metrics
                .corner_radius,
            radius
        );
        for style in [ChromeStyle::Mac, ChromeStyle::Win11] {
            let preset =
                presets::resolve(style, crate::layout::Scheme::Ocean, crate::layout::Mode::Light);
            assert_eq!(
                ChromeTheme::from_source(style, None)
                    .deco
                    .metrics
                    .corner_radius,
                preset.metrics.corner_radius
            );
        }
    }

    #[test]
    fn the_title_face_is_the_ui_display_role() {
        let design = embedded();
        let role =
            design::active_typography(Some(design.typography()), TypographyRole::UiDisplay);
        let theme = ChromeTheme::from_source(ChromeStyle::Win11, None).deco;
        assert_eq!(
            theme.metrics.title_font_family,
            DecoFontFamily::Named(role.family.clone())
        );
        assert_eq!(theme.metrics.title_size_px, role.font_size as f32);
    }

    #[test]
    fn the_theme_path_is_theme_conf_in_the_etc_dir() {
        // Read-only check of the composition rule, without touching the
        // process environment (tests run in parallel).
        let path = theme_path();
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("theme.conf.mix")
        );
    }
}
