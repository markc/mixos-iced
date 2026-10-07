// SPDX-License-Identifier: MIT OR Apache-2.0
//! Theme: the compiled appearance of one prepared settings generation.
//! The live window converts shared authority data through
//! [`from_settings`] on the settings worker (checked fonts, tokens,
//! typography, density and line heights); it never reads the legacy
//! `theme.conf.mix` files as an authority. The legacy selection/compilation
//! helpers remain only for standalone callers and fixtures.
//!
//! Desktop-wide theming is mandatory: there is no colour literal anywhere in
//! dopus. Every colour is a compiled design token or a mix of two of them. A
//! design that fails to compile falls back to the shared preview palette of
//! `mixos-iced-widgets` and says so, once, in [`Theme::notes`].
//!
//! Unlike ced there is no editor palette: rows, headers and the status bar
//! draw from the chrome [`Tokens`] plus a few extra token colours in
//! [`Chrome`].

use std::path::{Path, PathBuf};

use appearance::conversion::colour;
use application::iced::Color;
use design::{
    DesignCompileResult, DesignContext, Mode, ResolvedDictionary, ResolvedTypeRecord, Scheme,
    SourceIdentity, TypographyRole,
};
use toolkit::Tokens;

/// The app identity the design compiler selects a per-app overlay by.
const APP: &str = "dopus";

pub struct Theme {
    /// Shared sidebar typography: the original Places size, derived from Ui.
    pub sidebar_px: f32,
    /// Small role size for dense secondary columns, retaining the mono family.
    pub small_px: f32,
    /// Chrome colours (rows, headers, status bar, stock widgets).
    pub tokens: Tokens,
    /// Extra token colours the `Tokens` set does not carry.
    pub chrome: Chrome,
    /// Mono role: family name and size in px (row secondary columns).
    pub mono: (String, f32),
    /// Ui role: family name and size in px.
    pub ui: (String, f32),
    /// The fonts to hand iced, resolved to installed families.
    pub mono_font: application::iced::Font,
    pub ui_font: application::iced::Font,
    /// The resolved selection (for `dopus.state` and theme actions).
    pub scheme: Scheme,
    pub mode: Mode,
    /// The prepared ui.density: chrome spacing is scaled by it, and the
    /// measurement caches key on it so a density change reshapes.
    pub density: f32,
    /// The prepared role line heights (the measurement caches key on them).
    pub ui_line_height: Option<f32>,
    pub mono_line_height: Option<f32>,
    /// Something went wrong resolving (shown once in the status bar).
    pub notes: Option<String>,
}

/// Extra chrome colours, all tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chrome {
    pub gap: f32,
    pub pad: f32,
    pub small: f32,
    pub icon: f32,
    pub edge: f32,
    /// `secondary` surface: the header strip, status bar, inactive pane.
    pub secondary: Color,
    pub secondary_text: Color,
    /// `palette.accent.default`: the active-pane border.
    pub accent: Color,
    /// `status.success`.
    pub success: Color,
    /// `status.warning`.
    pub warning: Color,
}

impl Chrome {
    /// Density-scaled chrome: the spacing-derived fields shrink as the
    /// interface gets denser. `edge` is a border width and stays an unscaled
    /// logical pixel.
    pub fn scaled(self, density: f32) -> Self {
        Self {
            gap: self.gap * density,
            pad: self.pad * density,
            small: self.small * density,
            icon: self.icon * density,
            ..self
        }
    }
}

/// The effective `(scheme, mode)` and the design source to compile.
#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    pub scheme: Scheme,
    pub mode: Mode,
    /// A user design (`design:` in the shared theme file) replaces the
    /// embedded one; `None` = embedded.
    pub design_source: Option<(PathBuf, String)>,
}

#[derive(serde::Deserialize, Default)]
struct ThemeFileSelection {
    scheme: Option<String>,
    mode: Option<String>,
    design: Option<serde::de::IgnoredAny>,
}

/// Read `shared ← app` selections for legacy fixtures. Missing files are
/// skipped; malformed ones are skipped with a note, exactly as ctk does — a
/// broken theme never bricks the app. The live window does not call this: the
/// shared settings authority is the only live selection source.
pub fn read_selection(
    shared: Option<&Path>,
    app: Option<&Path>,
    notes: &mut Vec<String>,
) -> Selection {
    let mut selection = Selection {
        scheme: Scheme::default(),
        mode: Mode::default(),
        design_source: None,
    };
    for (layer, path) in [("shared", shared), ("app", app)] {
        let Some(path) = path.filter(|p| p.exists()) else {
            continue;
        };
        match ::config::store::load_conf_mix_path::<ThemeFileSelection>(path) {
            Ok(file) => {
                if let Some(name) = file.scheme {
                    match Scheme::from_name(&name) {
                        Some(scheme) => selection.scheme = scheme,
                        None => notes.push(format!("{layer} theme: unknown scheme {name:?}")),
                    }
                }
                if let Some(name) = file.mode {
                    match Mode::from_name(&name) {
                        Some(mode) => selection.mode = mode,
                        None => notes.push(format!("{layer} theme: unknown mode {name:?}")),
                    }
                }
                if layer == "shared"
                    && file.design.is_some()
                    && let Ok(text) = std::fs::read_to_string(path)
                {
                    selection.design_source = Some((path.to_path_buf(), text));
                }
            }
            Err(error) => notes.push(format!("{layer} theme skipped: {error:#}")),
        }
    }
    selection
}

/// Convert one prepared settings projection into a [`Theme`]. Called on the
/// settings worker BEFORE the UI activates the stage: no authored source
/// compilation, theme-file read or font discovery happens here.
pub fn from_prepared(
    look: &appearance::settings::Prepared,
) -> Result<Theme, settings::Diagnostic> {
    let missing = |name| {
        settings::Diagnostic::new(
            "unsupported_content",
            name,
            "Required appearance input missing",
        )
    };
    let chrome = build_chrome(look.dictionary())
        .map_err(|name| {
            settings::Diagnostic::new(
                "unsupported_content",
                &name,
                "Chrome colour or metric missing",
            )
        })?
        .scaled(look.density());
    let ui = look.typography().get("ui").ok_or_else(|| missing("ui"))?;
    let mono = look
        .typography()
        .get("mono")
        .ok_or_else(|| missing("mono"))?;
    let small = look
        .typography()
        .get("small")
        .ok_or_else(|| missing("small"))?;
    Ok(Theme {
        sidebar_px: ui.size * 0.9,
        small_px: small.size,
        tokens: look.tokens(),
        chrome,
        mono: (family_name(&mono.font), mono.size),
        ui: (family_name(&ui.font), ui.size),
        scheme: Scheme::default(),
        mode: Mode::default(),
        mono_font: mono.font,
        ui_font: ui.font,
        density: look.density(),
        ui_line_height: ui.line_height,
        mono_line_height: mono.line_height,
        notes: None,
    })
}

/// The windowed conversion: [`from_prepared`] plus the app-context selection
/// the accepted snapshot resolved for `app:dopus`.
pub fn from_settings(
    look: &appearance::settings::Prepared,
    snapshot: &settings::Snapshot,
) -> Result<Theme, settings::Diagnostic> {
    let mut theme = from_prepared(look)?;
    let missing = |name| {
        settings::Diagnostic::new(
            "unsupported_content",
            name,
            "Required appearance input missing",
        )
    };
    let effective = snapshot
        .effective
        .get("app:dopus")
        .ok_or_else(|| missing("app:dopus"))?;
    theme.scheme =
        Scheme::from_name(&effective.scheme).ok_or_else(|| missing("scheme"))?;
    theme.mode = Mode::from_name(&effective.mode).ok_or_else(|| missing("mode"))?;
    Ok(theme)
}

/// Compile `selection` into a [`Theme`] (legacy fixture path only; the live
/// window builds its theme from prepared settings data).
pub fn resolve_selection(selection: &Selection, mut notes: Vec<String>) -> Theme {
    if let Err(error) = appearance::fonts::register_installed() {
        notes.push(format!("static assets: {error}"));
    }
    let compiled = compile(selection).or_else(|error| {
        if selection.design_source.is_some() {
            notes.push(format!("{error}; using the embedded design"));
            compile(&Selection {
                design_source: None,
                ..selection.clone()
            })
        } else {
            Err(error)
        }
    });
    let (tokens, chrome, typography) = match compiled {
        Ok(Compiled {
            dictionary,
            typography,
        }) => match (
            appearance::conversion::from_dictionary(&dictionary),
            build_chrome(&dictionary),
        ) {
            (Ok(tokens), Ok(chrome)) => (tokens, chrome, Some(typography)),
            (Err(error), _) => {
                notes.push(format!("design dictionary: {error}"));
                fallback()
            }
            (_, Err(error)) => {
                notes.push(format!("design dictionary: {error}"));
                fallback()
            }
        },
        Err(error) => {
            notes.push(error);
            fallback()
        }
    };
    let role = |r: TypographyRole| -> ResolvedTypeRecord {
        typography
            .as_ref()
            .and_then(|t: &design::ResolvedTypography| t.role(r).cloned())
            .unwrap_or_else(|| design::default_typography(r).clone())
    };
    let mono = role(TypographyRole::Mono);
    let ui = role(TypographyRole::Ui);
    let mono_font = font_for(&mono, true, selection.design_source.is_none());
    let ui_font = font_for(&ui, false, selection.design_source.is_none());
    Theme {
        sidebar_px: ui.font_size as f32 * 0.9,
        small_px: role(TypographyRole::Small).font_size as f32,
        tokens,
        chrome,
        mono: (family_name(&mono_font), mono.font_size as f32),
        ui: (family_name(&ui_font), ui.font_size as f32),
        scheme: selection.scheme,
        mode: selection.mode,
        mono_font,
        ui_font,
        density: 1.0,
        ui_line_height: ui.line_height.map(|v| v as f32),
        mono_line_height: mono.line_height.map(|v| v as f32),
        notes: (!notes.is_empty()).then(|| notes.join("; ")),
    }
}

struct Compiled {
    dictionary: ResolvedDictionary,
    typography: design::ResolvedTypography,
}

fn compile(selection: &Selection) -> Result<Compiled, String> {
    let (identity, source) = match &selection.design_source {
        Some((path, text)) => (format!("file:{}", path.display()), text.as_str()),
        None => (
            "embedded:mixos-design-default".to_owned(),
            design::EMBEDDED_DEFAULT_SOURCE,
        ),
    };
    let document = design::parse_design_source(SourceIdentity::new(identity.clone()), source)
        .map_err(|error| format!("design source {identity}: {error}"))?;
    let context = DesignContext {
        scheme: selection.scheme,
        mode: selection.mode,
        app: Some(APP.to_owned()),
        ..DesignContext::default()
    };
    match design::compile_design(&document, context) {
        DesignCompileResult::Success(success) => Ok(Compiled {
            dictionary: success.candidate.dictionary().clone(),
            typography: success.candidate.typography().clone(),
        }),
        DesignCompileResult::Fatal(_) => Err(format!("design {identity} does not compile")),
    }
}

/// The shared preview palette, used only when the design cannot be compiled.
fn fallback() -> (Tokens, Chrome, Option<design::ResolvedTypography>) {
    let t = Tokens::default();
    let chrome = Chrome {
        secondary: t.palette.muted_surface,
        secondary_text: t.palette.text,
        accent: t.palette.ring,
        success: t.palette.primary,
        warning: t.palette.primary,
        ..build_chrome(
            &compile(&Selection {
                scheme: Scheme::default(),
                mode: Mode::default(),
                design_source: None,
            })
            .expect("embedded design")
            .dictionary,
        )
        .expect("embedded metrics")
    };
    (t, chrome, None)
}

/// The extra chrome colours; `Err(name)` names the first missing token.
pub fn build_chrome(d: &ResolvedDictionary) -> Result<Chrome, String> {
    let prim = |name: &str| {
        d.colours
            .primitives
            .get(name)
            .copied()
            .ok_or_else(|| name.to_owned())
    };
    let pair = |name: &str| {
        d.colours
            .pairs
            .get(name)
            .ok_or_else(|| format!("pair {name}"))
    };
    let secondary = pair("secondary")?;
    let spacing = |index: usize| {
        d.scales
            .get("spacing")
            .and_then(|s| s.get(index))
            .copied()
            .filter(|v| v.is_finite() && *v >= 0.0)
            .map(|v| v as f32)
            .ok_or_else(|| format!("spacing[{index}]"))
    };
    let edge = d
        .metrics
        .get("button.border_width")
        .filter(|m| {
            m.kind == design::ResolvedMetricKind::Px && m.value.is_finite() && m.value >= 0.0
        })
        .ok_or_else(|| "button.border_width".to_owned())?
        .value as f32;
    Ok(Chrome {
        gap: spacing(6)?,
        pad: spacing(4)?,
        small: spacing(2)?,
        icon: spacing(7)?,
        edge,
        secondary: colour(secondary.rendered_surface),
        secondary_text: colour(secondary.rendered_foreground),
        accent: colour(prim("palette.accent.default")?),
        success: colour(prim("status.success")?),
        warning: colour(prim("status.warning")?),
    })
}

/// The first installed family of the role (named family, then its
/// fallbacks), else the generic family. The name is interned once per
/// process: iced fonts name families with `&'static str`.
fn font_for(
    record: &ResolvedTypeRecord,
    monospace: bool,
    builtin: bool,
) -> application::iced::Font {
    let default = design::default_typography(if monospace {
        TypographyRole::Mono
    } else {
        TypographyRole::Ui
    });
    let prefer_assets =
        builtin && record.family == default.family && record.fallbacks == default.fallbacks;
    // The shared UI face is independent of sans-serif's existing Inter
    // fallback. An authored design keeps its explicit family authoritative.
    if prefer_assets
        && !monospace
        && let Ok(Some(set)) = appearance::fonts::register_installed()
        && let Some(family) = set.family("ui")
    {
        return toolkit::fonts::font_for(family, &record.fallbacks, record.weight, false, false);
    }
    toolkit::fonts::font_for(
        &record.family,
        &record.fallbacks,
        record.weight,
        monospace,
        prefer_assets,
    )
}
fn family_name(font: &application::iced::Font) -> String {
    match font.family {
        application::iced::font::Family::Name(name) => name.to_owned(),
        application::iced::font::Family::Monospace => "monospace".to_owned(),
        _ => "sans-serif".to_owned(),
    }
}

impl Theme {
    /// An iced theme for the stock widgets (buttons, scrollables, containers),
    /// built from the same tokens.
    pub fn iced_theme(&self) -> application::iced::Theme {
        application::iced::Theme::custom(
            "mixos-dopus",
            application::iced::theme::palette::Seed {
                background: self.tokens.palette.surface,
                text: self.tokens.palette.text,
                primary: self.tokens.palette.primary,
                success: self.chrome.success,
                warning: self.chrome.warning,
                danger: self.tokens.palette.destructive,
            },
        )
    }

    /// The Ui role at its resolved size.
    pub fn ui_px(&self) -> f32 {
        self.ui.1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a bootstrapped static asset set"]
    fn authored_builtin_family_remains_authoritative() {
        use application::iced::advanced::graphics::text::{
            cosmic_text::fontdb::{Database, Language},
            font_system,
        };
        let set = appearance::fonts::register_installed().unwrap().unwrap();
        let requested = design::default_typography(TypographyRole::Ui)
            .family
            .clone();
        let mut source = Database::new();
        source
            .load_font_file(set.font_path("serif").unwrap())
            .unwrap();
        let mut face = source.faces().next().unwrap().clone();
        face.families = vec![(requested.clone(), Language::English_UnitedStates)];
        font_system()
            .write()
            .unwrap()
            .raw()
            .db_mut()
            .push_face_info(face);
        let theme = resolve_selection(
            &Selection {
                scheme: Scheme::default(),
                mode: Mode::default(),
                design_source: Some((
                    PathBuf::from("authored.conf.mix"),
                    design::EMBEDDED_DEFAULT_SOURCE.to_owned(),
                )),
            },
            Vec::new(),
        );
        assert_eq!(family_name(&theme.ui_font), requested);
        assert_ne!(family_name(&theme.ui_font), set.family("sans").unwrap());
    }

    #[test]
    #[ignore = "requires a bootstrapped static asset set with a UI face"]
    fn shared_ui_face_uses_true_light_weight() {
        let set = appearance::fonts::register_installed().unwrap().unwrap();
        {
            use application::iced::advanced::graphics::text::{
                cosmic_text::fontdb::Weight, font_system,
            };
            let mut system = font_system().write().unwrap();
            let faces: Vec<_> = system
                .raw()
                .db()
                .faces()
                .filter(|face| face.families.iter().any(|(name, _)| name == "Noto Sans"))
                .map(|face| face.weight)
                .collect();
            assert!(!faces.is_empty());
            assert!(
                faces.iter().all(|weight| *weight == Weight::NORMAL),
                "the variable faces are indexed at 400, not separate light faces"
            );
        }
        let theme = resolve_selection(
            &Selection {
                scheme: Scheme::default(),
                mode: Mode::default(),
                design_source: None,
            },
            Vec::new(),
        );
        assert_eq!(set.family("ui"), Some("Noto Sans"));
        assert_eq!(theme.ui.0, "Noto Sans");
        assert_eq!(theme.ui_font.weight, application::iced::font::Weight::Light);
        let generic = toolkit::fonts::font_for("Missing UI family", &[], 300, false, false);
        assert_eq!(generic.family, application::iced::font::Family::SansSerif);
        assert_eq!(generic.weight, application::iced::font::Weight::Light);
    }

    #[test]
    fn embedded_tooltips_use_opaque_contrast_checked_elevated_pairs() {
        for scheme in Scheme::ALL {
            for mode in Mode::ALL {
                let compiled = compile(&Selection {
                    scheme,
                    mode,
                    design_source: None,
                })
                .unwrap();
                let dictionary = &compiled.dictionary;
                let tokens = appearance::conversion::from_dictionary(dictionary).unwrap();
                let style = appearance::tooltip_style(
                    tokens,
                    dictionary.metrics["button.border_width"].value as f32,
                );
                assert_eq!(style.background, Some(tokens.palette.elevated.into()));
                assert_eq!(style.text_color, Some(tokens.palette.elevated_text));
                assert_eq!(tokens.palette.elevated.a, 1.0);
                assert!(dictionary.colours.pairs["elevated"].contrast_ratio >= 4.5);
                // The active-pane header pair stays on the now-real muted
                // surface, which must also clear AA.
                assert!(dictionary.colours.pairs["muted"].contrast_ratio >= 4.5);
            }
        }
    }

    #[test]
    fn the_embedded_design_resolves_for_dopus() {
        let theme = resolve_selection(
            &Selection {
                scheme: Scheme::Ocean,
                mode: Mode::Dark,
                design_source: None,
            },
            Vec::new(),
        );
        assert!(theme.notes.is_none(), "{:?}", theme.notes);
        assert_ne!(
            theme.tokens.palette.surface,
            Tokens::default().palette.surface,
            "not the fallback palette"
        );
        assert!(
            (theme.mono.1 - 16.0).abs() < 0.01,
            "Mono role is 16 px: {}",
            theme.mono.1
        );
    }

    #[test]
    fn selection_layers_shared_then_app_and_skips_bad_files() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.conf.mix");
        let app = dir.path().join("app.conf.mix");
        std::fs::write(&shared, "scheme: \"forest\"\nmode: \"dark\"\n").unwrap();
        std::fs::write(&app, "mode: \"light\"\n").unwrap();
        let mut notes = Vec::new();
        let s = read_selection(Some(&shared), Some(&app), &mut notes);
        assert_eq!((s.scheme, s.mode), (Scheme::Forest, Mode::Light));
        assert!(notes.is_empty(), "{notes:?}");
        std::fs::write(&app, "scheme: \"plaid\"\n").unwrap();
        let s = read_selection(Some(&shared), Some(&app), &mut notes);
        assert_eq!((s.scheme, s.mode), (Scheme::Forest, Mode::Dark));
        assert_eq!(notes.len(), 1, "{notes:?}");
        let s = read_selection(Some(&dir.path().join("missing")), None, &mut Vec::new());
        assert_eq!((s.scheme, s.mode), (Scheme::default(), Mode::default()));
    }

    #[test]
    fn explicit_selection_resolves_directly() {
        let theme = resolve_selection(
            &Selection {
                scheme: Scheme::Mono,
                mode: Mode::Light,
                design_source: None,
            },
            Vec::new(),
        );
        assert_eq!((theme.scheme, theme.mode), (Scheme::Mono, Mode::Light));
    }

    fn snapshot(desktop: settings::Desktop) -> settings::Snapshot {
        settings::Snapshot {
            schema: settings::SCHEMA,
            binding: settings::Binding {
                instance: "fixture".into(),
                profile: "default".into(),
            },
            incarnation: "fixture".into(),
            revision: settings::Revision(1),
            design_revision: settings::Revision(1),
            source_digest: settings::source_digest(settings::EMBEDDED_DEFAULT_SOURCE),
            effective: settings::resolve(&desktop).expect("default desktop resolves"),
            desktop,
        }
    }

    #[test]
    fn settings_conversion_preserves_chrome_palette_and_typography() {
        let look = appearance::settings::bootstrap().expect("generic bootstrap");
        let theme = from_settings(&look, &snapshot(settings::Desktop::default())).unwrap();
        assert_eq!((theme.scheme, theme.mode), (Scheme::Ocean, Mode::Light));
        assert_ne!(
            theme.tokens.palette.surface,
            Tokens::default().palette.surface,
            "not the fallback palette"
        );
        assert!(theme.chrome.gap > 0.0 && theme.chrome.icon > 0.0);
        assert_eq!(theme.density, 1.0);
        assert!(
            (theme.ui.1 - 14.666_667).abs() < 0.001,
            "Ui role is the embedded 14.667 px: {}",
            theme.ui.1
        );
        assert!((theme.mono.1 - 16.0).abs() < 0.01, "Mono role is 16 px");
        let effective = &settings::resolve(&settings::Desktop::default()).unwrap()["desktop"];
        assert_eq!(
            theme.ui_line_height,
            effective.design.typography["ui"].line_height.map(|v| v as f32)
        );
    }

    #[test]
    fn settings_conversion_takes_the_app_effective_selection_and_density() {
        let mut desktop = settings::Desktop::default();
        desktop.appearance.scheme = "crimson".into();
        desktop.appearance.mode = "dark".into();
        desktop.ui.density = 1.5;
        let look = appearance::settings::bootstrap().expect("generic bootstrap");
        let plain = from_prepared(&look).unwrap();
        let theme = from_settings(&look, &snapshot(desktop)).unwrap();
        assert_eq!((theme.scheme, theme.mode), (Scheme::Crimson, Mode::Dark));
        assert!(theme.density > 1.0);
        assert!(theme.chrome.gap > plain.chrome.gap, "density scales chrome spacing");
        assert_eq!(
            theme.chrome.gap,
            plain.chrome.gap * theme.density,
            "spacing scales linearly with density"
        );
    }
}
