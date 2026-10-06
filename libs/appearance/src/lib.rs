// SPDX-License-Identifier: MIT OR Apache-2.0
//! The MixOS look for an iced application, in one call.
//!
//! `toolkit` is generic: it takes plain [`toolkit::Tokens`] and a
//! [`toolkit::FontSet`] and knows nothing about where they come from. This
//! crate is the MixOS side of that line. It compiles the MixOS design
//! ([`Theme`]), maps it onto toolkit's tokens ([`tokens`]), turns the pinned
//! asset set into toolkit's font sources ([`fonts`]) and installs them once
//! ([`install`]):
//!
//! ```no_run
//! let look = appearance::install(&appearance::Theme::load())?;
//! if let Some(warning) = look.warning() {
//!     eprintln!("{warning}");
//! }
//! // iced::application(...).default_font(look.ui_font());
//! // widget.style(move |_, status| look.tokens.text_input(status));
//! # Ok::<(), appearance::Error>(())
//! ```
//!
//! A theme change at runtime recomputes the tokens ([`Appearance::retheme`]);
//! the fonts stay installed for the life of the process.

pub mod conversion;
pub mod fonts;
pub mod theme;
pub mod tokens;

use iced_core::font::{Family, Weight};
use iced_core::{Color, Font};

pub use design::{Contrast, DesignContext, Mode, Scheme};
pub use fonts::{FontOrigin, FontSources, fonts, fonts_in, fonts_of};
pub use theme::{Error, Resolved, THEME_FILE, Theme, theme_path};
pub use tokens::{colour, metrics, palette, semantic, tokens};

use design::{ResolvedTypeRecord, TypographyRole};
use toolkit::fonts::{Fonts, Role};
use toolkit::{Theme as ToolkitTheme, Tokens};

/// The tooltip style with the application's resolved border width.
pub fn tooltip_style(mut tokens: Tokens, width: f32) -> iced_widget::container::Style {
    tokens.metrics.border.width = width;
    tokens.tooltip_style()
}

/// The installed look: the tokens for the current theme, the context they
/// came from, and the fonts registered with iced.
#[derive(Debug)]
pub struct Appearance {
    /// Colours and metrics for the widgets' `style` calls.
    pub tokens: Tokens,
    semantic: toolkit::tokens::Semantic,
    /// The scheme, mode and contrast the tokens were mapped from.
    pub context: DesignContext,
    /// The registered fonts (`toolkit::fonts::install`'s result).
    pub fonts: &'static Fonts,
    /// Where the fonts came from, for the startup log.
    pub origin: FontOrigin,
    ui: ResolvedTypeRecord,
    display: ResolvedTypeRecord,
    mono: ResolvedTypeRecord,
}

/// Map `theme` and install the MixOS fonts from the activated asset set,
/// once per process. Without a set the look keeps iced's generic families
/// and [`Appearance::warning`] says so; the only errors are a font source
/// that does not install and a second call (`FontError::AlreadyInstalled`).
pub fn install(theme: &Theme) -> Result<Appearance, Error> {
    install_with(theme, fonts())
}

/// [`install`] with font sources the caller resolved (another lookup, or
/// none at all).
pub fn install_with(theme: &Theme, sources: FontSources) -> Result<Appearance, Error> {
    let fonts = toolkit::fonts::install(sources.set, sources.icons)?;
    let mut look = Appearance {
        tokens: Tokens::default(),
        semantic: semantic(theme),
        context: theme.context().clone(),
        fonts,
        origin: sources.origin,
        ui: role(theme, TypographyRole::Ui),
        display: role(theme, TypographyRole::UiDisplay),
        mono: role(theme, TypographyRole::Mono),
    };
    look.retheme(theme);
    Ok(look)
}

impl Appearance {
    /// Recompute the tokens and typography from a new theme; the fonts stay
    /// installed.
    pub fn retheme(&mut self, theme: &Theme) {
        self.tokens = tokens(theme);
        self.semantic = semantic(theme);
        self.context = theme.context().clone();
        self.ui = role(theme, TypographyRole::Ui);
        self.display = role(theme, TypographyRole::UiDisplay);
        self.mono = role(theme, TypographyRole::Mono);
    }

    pub fn scheme(&self) -> Scheme {
        self.context.scheme
    }

    pub fn mode(&self) -> Mode {
        self.context.mode
    }

    /// The line to log when the look is not the pinned one.
    pub fn warning(&self) -> Option<String> {
        FontSources::none(self.origin.clone()).warning()
    }

    /// UI text: the installed sans face at the design's `ui` weight (Light,
    /// 300), for an application's `default_font`.
    pub fn ui_font(&self) -> Font {
        self.font(Role::Sans, &self.ui, false)
    }

    /// Window titles and headings: the installed display face at the
    /// `ui_display` weight, else the UI font.
    pub fn display_font(&self) -> Font {
        self.font(Role::Display, &self.display, false)
    }

    /// Code and technical fields: the installed mono face at the `mono`
    /// weight.
    pub fn mono_font(&self) -> Font {
        self.font(Role::Mono, &self.mono, true)
    }

    /// The glyph and font of a named icon from the set's icon font.
    pub fn icon(&self, name: &str) -> Option<(char, Font)> {
        self.fonts.icon(name)
    }

    /// The window background, for `iced::application(...).style`.
    pub fn background(&self) -> Color {
        self.tokens.palette.surface
    }

    /// The current tokens as toolkit's iced theme, for
    /// `iced::application(...).theme`: every iced and toolkit widget then
    /// takes the MixOS look, and a `retheme` restyles the next frame.
    pub fn theme(&self) -> ToolkitTheme {
        ToolkitTheme::new(self.tokens).with_semantic(self.semantic)
    }

    /// The installed family of `role` at the record's weight, trusting the
    /// pinned variable fonts to render the weight requested. Without that
    /// role, the record's own family chain through toolkit's resolver, where
    /// a Light request on a family with no light face becomes Normal.
    fn font(&self, role: Role, record: &ResolvedTypeRecord, monospace: bool) -> Font {
        match self.fonts.family(role) {
            Some(family) => Font {
                family: Family::Name(family),
                weight: weight(record.weight),
                ..Font::DEFAULT
            },
            None => toolkit::fonts::font_for(
                &record.family,
                &record.fallbacks,
                record.weight,
                monospace,
                true,
            ),
        }
    }
}

fn role(theme: &Theme, role: TypographyRole) -> ResolvedTypeRecord {
    design::active_typography(Some(theme.typography()), role).clone()
}

fn weight(value: u16) -> Weight {
    toolkit::fonts::weight(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one test that installs (once per process): no set, so toolkit
    /// registers nothing and every font resolves to a generic family.
    #[test]
    fn install_without_a_set_keeps_the_generic_families() {
        let theme = Theme::embedded();
        let roots = vec![std::path::PathBuf::from("/nonexistent/appearance-install")];
        let sources = fonts_in(&roots.clone().into_iter().collect());
        let look = install_with(&theme, sources).unwrap();
        assert_eq!(look.tokens, tokens(&theme));
        assert_eq!((look.scheme(), look.mode()), (Scheme::Ocean, Mode::Light));
        assert_eq!(look.origin, FontOrigin::NoSet { roots });
        assert!(
            look.warning()
                .unwrap()
                .starts_with("no asset set activated")
        );
        assert_eq!(look.fonts.family(Role::Sans), None);
        assert_eq!(look.icon("delete"), None);
        assert_eq!(look.background(), look.tokens.palette.surface);
        assert_eq!(look.theme().tokens(), look.tokens);
        // With no installed role, each font is the design record's own
        // family chain through toolkit's resolver (whatever the host has).
        let ui = design::active_typography(Some(theme.typography()), TypographyRole::Ui);
        assert_eq!(
            look.ui_font(),
            toolkit::fonts::font_for(&ui.family, &ui.fallbacks, ui.weight, false, true)
        );
        assert!(matches!(
            look.ui_font().weight,
            Weight::Light | Weight::Normal
        ));
        let mono = design::active_typography(Some(theme.typography()), TypographyRole::Mono);
        assert_eq!(
            look.mono_font(),
            toolkit::fonts::font_for(&mono.family, &mono.fallbacks, mono.weight, true, true)
        );
        let display =
            design::active_typography(Some(theme.typography()), TypographyRole::UiDisplay);
        assert_eq!(
            look.display_font(),
            toolkit::fonts::font_for(
                &display.family,
                &display.fallbacks,
                display.weight,
                false,
                true
            )
        );

        let mut look = look;
        let dark = Theme::for_context(DesignContext {
            scheme: Scheme::Forest,
            mode: Mode::Dark,
            ..DesignContext::default()
        });
        let before = look.theme();
        look.retheme(&dark);
        assert_eq!(look.tokens, tokens(&dark));
        assert_eq!(look.theme().tokens(), tokens(&dark));
        assert_ne!(look.theme(), before);
        assert_eq!((look.scheme(), look.mode()), (Scheme::Forest, Mode::Dark));
        assert_ne!(look.tokens.palette.surface, tokens(&theme).palette.surface);

        assert!(matches!(
            install_with(
                &theme,
                FontSources::none(FontOrigin::Unusable("second".into()))
            ),
            Err(Error::Fonts(toolkit::fonts::FontError::AlreadyInstalled))
        ));
    }

    #[test]
    fn weights_follow_the_design_records() {
        let theme = Theme::embedded();
        assert_eq!(role(&theme, TypographyRole::Ui).weight, 300);
        assert_eq!(weight(300), Weight::Light);
        assert_eq!(
            weight(role(&theme, TypographyRole::Small).weight),
            Weight::Normal
        );
    }
}
