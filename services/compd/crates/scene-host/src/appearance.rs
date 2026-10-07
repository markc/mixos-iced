// SPDX-License-Identifier: MIT OR Apache-2.0
//! Renderer-ready shell content, prepared on the existing Bus worker. The
//! compositor activates the whole value only after the shared session fence.
use ::appearance::settings::Prepared;
use decor::{
    ChromeTheme, Palette,
    layout::{ChromeStyle, DecoFontFamily, DecoFontWeight},
};
use iced_core::{
    Theme,
    font::Family,
};
use settings::{Diagnostic, Snapshot};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(crate) struct Look {
    pub prepared: Arc<Prepared>,
    pub preferences: crate::preferences::Preferences,
    chrome: Vec<ChromeTheme>,
}

pub(crate) fn build(prepared: &Prepared, snapshot: &Snapshot) -> Result<Look, Diagnostic> {
    let preferences = crate::preferences::Preferences::prepare(&snapshot.desktop.shell)?;
    let title = prepared.typography().get("ui_display").ok_or_else(|| {
        Diagnostic::new(
            "unsupported_presentation",
            "ui_display",
            "Missing prepared title font",
        )
    })?;
    let family = match title.font.family {
        Family::Name(name) => DecoFontFamily::Named(name.to_owned()),
        Family::SansSerif => DecoFontFamily::SystemUi,
        Family::Monospace => DecoFontFamily::Monospace,
        _ => {
            return Err(Diagnostic::new(
                "unsupported_presentation",
                "ui_display",
                "Unsupported title font family",
            ));
        }
    };
    let weight = title.font.weight.value();
    Ok(Look {
        prepared: Arc::new(prepared.clone()),
        preferences,
        chrome: ChromeStyle::ALL
            .into_iter()
            .map(|style| {
                ChromeTheme::from_read(
                    style,
                    prepared.dictionary(),
                    family.clone(),
                    DecoFontWeight(weight),
                    title.size,
                )
            })
            .collect(),
    })
}
impl Look {
    pub fn chrome(&self, style: ChromeStyle) -> ChromeTheme {
        self.chrome
            .iter()
            .find(|theme| theme.deco.style == style)
            .expect("every chrome style was prepared")
            .clone()
    }
}

/// Edge pages deliberately use the secondary pair; all other defaults and
/// semantic colours still come from the same prepared authority projection.
pub(crate) fn page(prepared: &Prepared, dialog: bool) -> (Palette, Theme) {
    let mut palette = Palette::from_dictionary(prepared.dictionary());
    let mut theme = prepared.theme();
    if !dialog {
        palette.base = palette.secondary;
        let mut tokens = theme.tokens();
        tokens.palette.surface = iced_core::Color::from_rgba(
            palette.base.surface.r,
            palette.base.surface.g,
            palette.base.surface.b,
            palette.base.surface.a,
        );
        tokens.palette.text = iced_core::Color::from_rgba(
            palette.base.foreground.r,
            palette.base.foreground.g,
            palette.base.foreground.b,
            palette.base.foreground.a,
        );
        theme.set_tokens(tokens);
    }
    (palette, theme.to_iced())
}

#[cfg(test)]
pub(crate) fn fixture(mode: &str, scale: f64) -> Arc<Prepared> {
    let mut desktop = settings::Desktop::default();
    desktop.appearance.mode = mode.into();
    desktop.ui.text_scale = scale;
    let effective = settings::resolve(&desktop)
        .unwrap()
        .remove("desktop")
        .unwrap();
    Arc::new(
        ::appearance::settings::Projection::new(&effective)
            .unwrap()
            .prepare(|_, record| {
                Ok(toolkit::fonts::FontSelection {
                    font: iced_core::Font {
                        weight: toolkit::fonts::weight(record.weight),
                        ..iced_core::Font::DEFAULT
                    },
                    choice: toolkit::fonts::FontChoice::Declared,
                })
            })
            .unwrap(),
    )
}
