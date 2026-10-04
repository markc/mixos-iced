//! The decoration model: one `DecoTheme` fully describes how compd lays out
//! and draws a window's server-side chrome.
//!
//! Two orthogonal axes select a theme:
//!
//! * `ChromeStyle`: the shape of the chrome (button side, order and shape,
//!   titlebar height, corner radius, shadow, hover behaviour): `mac`, `win11`
//!   or `mixos`.
//! * `Scheme` x `Mode`: the palette. The mac and win11 styles are faithful to
//!   their platforms and ignore the scheme; the mixos style keys its accent
//!   off it. In compd every colour is then replaced by the design tokens
//!   (`crate::theme::apply_tokens`), so the preset palette only decides which
//!   slots are drawn at all and with what alpha.
//!
//! Everything here is plain data: no rendering, no I/O.

use super::geom::Srgba;

/// Which chrome family to draw. The names are the config values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChromeStyle {
    /// macOS-like: traffic lights top-left, tall rounded corners, big soft
    /// shadow. The default: windows come up in this style.
    #[default]
    Mac,
    /// Windows 11-like: caption buttons top-right, wide flat hover targets,
    /// red close.
    Win11,
    /// The native MixOS look: scheme-accented, symmetric.
    Mixos,
}

impl ChromeStyle {
    pub const ALL: [ChromeStyle; 3] = [ChromeStyle::Mac, ChromeStyle::Win11, ChromeStyle::Mixos];

    pub fn name(self) -> &'static str {
        match self {
            ChromeStyle::Mac => "mac",
            ChromeStyle::Win11 => "win11",
            ChromeStyle::Mixos => "mixos",
        }
    }

    pub fn from_name(s: &str) -> Option<ChromeStyle> {
        ChromeStyle::ALL.into_iter().find(|c| c.name() == s)
    }
}

/// The colour scheme the mixos preset takes its accent hue from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scheme {
    #[default]
    Ocean,
    Crimson,
    Stone,
    Forest,
    Sunset,
    Mono,
}

impl Scheme {
    pub const ALL: [Scheme; 6] = [
        Scheme::Ocean,
        Scheme::Crimson,
        Scheme::Stone,
        Scheme::Forest,
        Scheme::Sunset,
        Scheme::Mono,
    ];
}

/// Light or dark preset palette.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// The default: the mac light look, traffic lights on a near-white
    /// titlebar.
    #[default]
    Light,
    Dark,
}

/// Whether the window owns keyboard focus. Unfocused chrome is drawn muted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Focused,
    Unfocused,
}

/// Pointer interaction state of one caption button.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonState {
    #[default]
    Idle,
    Hover,
    Pressed,
}

/// The caption buttons. `Maximize` doubles as restore when the window is
/// maximised (the glyph swap happens at render time from the toplevel state).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptionButton {
    Close,
    Minimize,
    Maximize,
}

/// Which end of the titlebar the button cluster sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonSide {
    Left,
    Right,
}

/// Caption button silhouette.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ButtonShape {
    /// A circle of the given diameter, vertically centred (mac, mixos).
    Circle { diameter: f32 },
    /// A rectangle of the given width spanning the full titlebar height
    /// (win11).
    FullHeightRect { width: f32 },
}

/// When to draw the glyph inside a caption button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphPolicy {
    /// Always visible (win11, mixos).
    Always,
    /// Only while the pointer is over any button in the cluster: the mac
    /// behaviour, so the hover flag for glyphs is cluster-wide, not
    /// per-button.
    ClusterHover,
}

/// Title text placement along the titlebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitleAlign {
    /// Centred in the full titlebar width (mac).
    Center,
    /// Leading edge, after the button cluster if it is on that side (win11,
    /// mixos). Titles follow an LTR desktop-chrome policy, independent of
    /// the title text's bidi direction.
    Leading,
}

/// Font family requested for decoration titles.
///
/// A token only: font discovery, bytes and renderer handles belong to the
/// rasteriser (`crate::text`).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum DecoFontFamily {
    /// The platform's default user-interface family.
    #[default]
    SystemUi,
    /// A font family by its advertised name. A name the host does not have
    /// is not fatal: the rasteriser degrades to the platform UI family and
    /// finally to the embedded face.
    Named(String),
}

/// Font weight requested for decoration titles, on the CSS/OpenType numeric
/// scale (1 to 1000; 300 is Light, 400 Regular, 700 Bold).
///
/// A token only, like [`DecoFontFamily`]: the rasteriser matches it to the
/// nearest available weight of the family.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DecoFontWeight(pub u16);

impl DecoFontWeight {
    pub const LIGHT: Self = Self(300);
    pub const NORMAL: Self = Self(400);
    pub const DEFAULT: Self = Self::NORMAL;

    /// The weight a renderer should actually request: `0` means unset and
    /// resolves to [`DecoFontWeight::DEFAULT`]; anything above 1000
    /// saturates. Total by construction, so no consumer range-checks a theme.
    pub const fn resolved(self) -> Self {
        match self.0 {
            0 => Self::DEFAULT,
            w if w > 1000 => Self(1000),
            w => Self(w),
        }
    }
}

impl Default for DecoFontWeight {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Points to logical pixels at the CSS reference density (96 dpi).
///
/// Font preferences are stated in points; every metric in [`DecoMetrics`] is
/// logical pixels. Output scale is applied on top of the logical value, so
/// this is a unit change, not a DPI calculation.
pub const fn pt_to_px(pt: f32) -> f32 {
    pt * (96.0 / 72.0)
}

/// Fill and glyph colours for one caption button across interaction states.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ButtonColors {
    pub fill_idle: Srgba,
    pub fill_hover: Srgba,
    pub fill_pressed: Srgba,
    /// Fill when the window is unfocused (all styles mute their buttons).
    pub fill_unfocused: Srgba,
    pub glyph: Srgba,
    pub glyph_hover: Srgba,
}

impl ButtonColors {
    pub fn fill(&self, state: ButtonState, focus: Focus) -> Srgba {
        match (focus, state) {
            // A hovered or pressed button lights up even on an unfocused
            // window, as on both platforms.
            (Focus::Unfocused, ButtonState::Idle) => self.fill_unfocused,
            (_, ButtonState::Idle) => self.fill_idle,
            (_, ButtonState::Hover) => self.fill_hover,
            (_, ButtonState::Pressed) => self.fill_pressed,
        }
    }
}

/// The button cluster: geometry, order and per-button colours.
#[derive(Clone, Debug, PartialEq)]
pub struct ButtonCluster {
    pub side: ButtonSide,
    /// Drawing and layout order, outermost first (nearest the window edge).
    /// Mac: `[Close, Minimize, Maximize]` on the left.
    /// Win11: `[Close, Maximize, Minimize]` on the right.
    pub order: [CaptionButton; 3],
    pub shape: ButtonShape,
    /// Gap between buttons (ignored for `FullHeightRect`, which packs flush).
    pub gap: f32,
    /// Inset from the window edge to the first button.
    pub edge_inset: f32,
    /// Glyph size as a fraction of the button's smaller dimension.
    ///
    /// Per style because the right answer depends on how much of the button
    /// is visible at rest: a mac traffic light is a filled 12px disc whose
    /// glyph only appears on hover, while a win11 caption cell is invisible
    /// until hovered, so its glyph is the button and has to carry the whole
    /// target on its own.
    pub glyph_extent_ratio: f32,
    pub glyphs: GlyphPolicy,
    pub close: ButtonColors,
    pub minimize: ButtonColors,
    pub maximize: ButtonColors,
}

impl ButtonCluster {
    pub fn colors(&self, button: CaptionButton) -> &ButtonColors {
        match button {
            CaptionButton::Close => &self.close,
            CaptionButton::Minimize => &self.minimize,
            CaptionButton::Maximize => &self.maximize,
        }
    }
}

/// Drop shadow drawn outside the xdg-shell window geometry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowSpec {
    /// Blur radius in logical px.
    pub softness: f32,
    /// Vertical offset: light comes from above.
    pub offset_y: f32,
    /// Base shadow colour; focus selects the alpha below.
    pub color: Srgba,
    pub alpha_focused: f32,
    pub alpha_unfocused: f32,
}

/// All chrome geometry in logical pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct DecoMetrics {
    pub titlebar_height: f32,
    /// Visible frame line around the content (0.0 for none; win11 uses 1px).
    pub border_thickness: f32,
    /// Invisible band outside the window edge that still hit-tests as a
    /// resize handle. Corner zones reach `(resize_band * 2).max(12)` along
    /// each edge so corners stay grabbable even with a thin band.
    pub resize_band: f32,
    /// Corner radius of the whole window (titlebar and content mask).
    pub corner_radius: f32,
    pub title_size_px: f32,
    pub title_font_family: DecoFontFamily,
    pub title_font_weight: DecoFontWeight,
    pub title_align: TitleAlign,
    /// Minimum padding between title text and buttons or edges.
    pub title_pad: f32,
    pub shadow: ShadowSpec,
}

/// Chrome surface colours (buttons live in `ButtonCluster`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecoColors {
    pub titlebar_focused: Srgba,
    pub titlebar_unfocused: Srgba,
    /// Hairline under the titlebar separating it from content (alpha 0 =
    /// none).
    pub titlebar_divider: Srgba,
    pub title_text_focused: Srgba,
    pub title_text_unfocused: Srgba,
    pub border_focused: Srgba,
    pub border_unfocused: Srgba,
}

/// A complete decoration theme: everything needed to lay out, hit-test and
/// draw one window's chrome, resolved for a concrete (`ChromeStyle`,
/// `Scheme`, `Mode`) triple.
#[derive(Clone, Debug, PartialEq)]
pub struct DecoTheme {
    pub style: ChromeStyle,
    pub scheme: Scheme,
    pub mode: Mode,
    pub metrics: DecoMetrics,
    pub colors: DecoColors,
    pub buttons: ButtonCluster,
}

impl DecoTheme {
    pub fn titlebar_fill(&self, focus: Focus) -> Srgba {
        match focus {
            Focus::Focused => self.colors.titlebar_focused,
            Focus::Unfocused => self.colors.titlebar_unfocused,
        }
    }

    pub fn title_text(&self, focus: Focus) -> Srgba {
        match focus {
            Focus::Focused => self.colors.title_text_focused,
            Focus::Unfocused => self.colors.title_text_unfocused,
        }
    }

    pub fn border(&self, focus: Focus) -> Srgba {
        match focus {
            Focus::Focused => self.colors.border_focused,
            Focus::Unfocused => self.colors.border_unfocused,
        }
    }

    pub fn shadow_alpha(&self, focus: Focus) -> f32 {
        match focus {
            Focus::Focused => self.metrics.shadow.alpha_focused,
            Focus::Unfocused => self.metrics.shadow.alpha_unfocused,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_names_round_trip() {
        for style in ChromeStyle::ALL {
            assert_eq!(ChromeStyle::from_name(style.name()), Some(style));
        }
        assert_eq!(ChromeStyle::from_name("mixos"), Some(ChromeStyle::Mixos));
        assert_eq!(ChromeStyle::from_name("unknown"), None);
        assert_eq!(ChromeStyle::default(), ChromeStyle::Mac);
    }

    #[test]
    fn decoration_font_family_defaults_to_system_ui() {
        assert_eq!(DecoFontFamily::default(), DecoFontFamily::SystemUi);
    }

    #[test]
    fn decoration_font_weight_defaults_to_regular() {
        assert_eq!(DecoFontWeight::default(), DecoFontWeight::NORMAL);
        assert_eq!(DecoFontWeight::NORMAL.0, 400);
        assert_eq!(DecoFontWeight::LIGHT.0, 300);
    }

    #[test]
    fn unset_and_out_of_range_weights_resolve_into_the_representable_range() {
        assert_eq!(DecoFontWeight(0).resolved(), DecoFontWeight::DEFAULT);
        assert_eq!(DecoFontWeight(1).resolved(), DecoFontWeight(1));
        assert_eq!(DecoFontWeight(300).resolved(), DecoFontWeight::LIGHT);
        assert_eq!(DecoFontWeight(1000).resolved(), DecoFontWeight(1000));
        assert_eq!(DecoFontWeight(1001).resolved(), DecoFontWeight(1000));
        assert_eq!(DecoFontWeight(u16::MAX).resolved(), DecoFontWeight(1000));
    }

    #[test]
    fn points_convert_to_logical_pixels_at_the_css_reference_density() {
        assert_eq!(pt_to_px(72.0), 96.0);
        assert_eq!(pt_to_px(9.0), 12.0);
        // 11pt, the preset default, is not a whole pixel; the rasteriser
        // keeps the fraction and only the atlas rounds.
        assert!((pt_to_px(11.0) - 14.666_667).abs() < 1e-4);
    }
}
