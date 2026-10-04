// SPDX-License-Identifier: MIT OR Apache-2.0
//! A compiled MixOS theme: the resolved design for one scheme, mode and
//! contrast, together with the context it was compiled for.
//!
//! The `design` crate's resolved artifact does not record its context, so a
//! consumer that compiled it would otherwise lose the scheme and mode it
//! asked for. [`Theme`] keeps the two together. It is built from the
//! embedded default design, from a full `theme.conf.mix` document, or from
//! the shared selection-only file (`scheme:` and `mode:` alone), which is
//! resolved against the embedded design exactly as compd's chrome does.

use std::fmt;
use std::path::{Path, PathBuf};

use design::{
    Contrast, DesignCompileResult, DesignContext, DesignDiagnostic, DesignSourceDocument,
    DesignSourceError, LegacyV0Source, Mode, ResolvedDictionary, ResolvedTypography, Scheme,
    SourceIdentity, UnstampedResolvedDesign,
};

/// The shared theme file, `theme.conf.mix`, in the MixOS etc directory.
pub const THEME_FILE: &str = "theme.conf.mix";

/// Where [`Theme::load`] reads the shared theme: `theme.conf.mix` under
/// `config::path(Dir::Etc)` (`$MIXOS_ETC`, else `$MIXOS/etc`, else
/// `/etc/mixos` for root, else `$XDG_CONFIG_HOME/mixos`).
pub fn theme_path() -> PathBuf {
    config::path(config::Dir::Etc).join(THEME_FILE)
}

/// Why a theme could not be built.
#[derive(Debug)]
pub enum Error {
    /// The text is neither a design document nor a selection-only file.
    Source(DesignSourceError),
    /// A selection names a scheme or mode the design does not have.
    Selection {
        field: &'static str,
        value: String,
    },
    /// The design did not compile for the requested context.
    Compile(Vec<DesignDiagnostic>),
    /// The theme file could not be read.
    Read {
        path: PathBuf,
        error: std::io::Error,
    },
    /// The fonts did not install (see `toolkit::fonts::install`).
    Fonts(toolkit::fonts::FontError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(error) => write!(f, "theme source: {error}"),
            Self::Selection { field, value } => write!(f, "unknown {field} {value:?}"),
            Self::Compile(diagnostics) => {
                write!(f, "theme did not compile:")?;
                for diagnostic in diagnostics {
                    write!(
                        f,
                        " [{} {}: {}]",
                        diagnostic.code, diagnostic.path, diagnostic.message
                    )?;
                }
                Ok(())
            }
            Self::Read { path, error } => write!(f, "read {}: {error}", path.display()),
            Self::Fonts(error) => write!(f, "fonts: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Source(error) => Some(error),
            Self::Read { error, .. } => Some(error),
            Self::Fonts(error) => Some(error),
            Self::Selection { .. } | Self::Compile(_) => None,
        }
    }
}

impl From<toolkit::fonts::FontError> for Error {
    fn from(error: toolkit::fonts::FontError) -> Self {
        Self::Fonts(error)
    }
}

/// A resolved design: the two parts the token mapping reads. Implemented for
/// the compiler's unstamped candidate, the live stamped artifact and
/// [`Theme`], so `tokens` takes any of them.
pub trait Resolved {
    fn dictionary(&self) -> &ResolvedDictionary;
    fn typography(&self) -> &ResolvedTypography;
}

impl Resolved for UnstampedResolvedDesign {
    fn dictionary(&self) -> &ResolvedDictionary {
        UnstampedResolvedDesign::dictionary(self)
    }

    fn typography(&self) -> &ResolvedTypography {
        UnstampedResolvedDesign::typography(self)
    }
}

impl Resolved for design::ResolvedDesign {
    fn dictionary(&self) -> &ResolvedDictionary {
        design::ResolvedDesign::dictionary(self)
    }

    fn typography(&self) -> &ResolvedTypography {
        design::ResolvedDesign::typography(self)
    }
}

/// The resolved design for one context.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    context: DesignContext,
    design: UnstampedResolvedDesign,
}

impl Resolved for Theme {
    fn dictionary(&self) -> &ResolvedDictionary {
        self.design.dictionary()
    }

    fn typography(&self) -> &ResolvedTypography {
        self.design.typography()
    }
}

impl Theme {
    /// The embedded default design in its own selection: ocean, light,
    /// normal contrast.
    pub fn embedded() -> Self {
        Self::for_context(DesignContext::default())
    }

    /// The embedded default design compiled for `context`.
    ///
    /// # Panics
    /// The embedded design claims every scheme, mode and contrast and the
    /// compiler compiles each claimed context before it returns one, so a
    /// failure here is a broken build, not a runtime condition.
    pub fn for_context(context: DesignContext) -> Self {
        let document = embedded_document();
        Self::compile(&document, context).expect("the embedded default design compiles")
    }

    /// A theme from `source`: a full design document (its own `scheme:` and
    /// `mode:` select the context), or the shared selection-only file
    /// holding nothing but `scheme:` and `mode:`, resolved against the
    /// embedded design. `identity` names the source in diagnostics.
    pub fn from_source(identity: &str, source: &str) -> Result<Self, Error> {
        let (document, selection) =
            match design::parse_design_source(SourceIdentity::new(identity), source) {
                Ok(document) => {
                    let selection = document.legacy.clone();
                    (document, selection)
                }
                Err(error) => {
                    let Some(selection) = selection_only(source) else {
                        return Err(Error::Source(error));
                    };
                    (embedded_document(), selection)
                }
            };
        let context = DesignContext {
            scheme: axis(selection.scheme.as_deref(), "scheme", Scheme::from_name)?,
            mode: axis(selection.mode.as_deref(), "mode", Mode::from_name)?,
            contrast: Contrast::default(),
            app: None,
        };
        Self::compile(&document, context)
    }

    /// The theme in the file at `path`: `None` when there is no file, an
    /// error when there is one that does not read, parse or compile.
    pub fn read(path: &Path) -> Result<Option<Self>, Error> {
        let source = match std::fs::read_to_string(path) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(Error::Read {
                    path: path.to_path_buf(),
                    error,
                });
            }
        };
        Self::from_source(&path.display().to_string(), &source).map(Some)
    }

    /// The shared theme file ([`theme_path`]), or the embedded default when
    /// there is none or it is unusable. A caller that wants the reason uses
    /// [`Theme::read`].
    pub fn load() -> Self {
        match Self::read(&theme_path()) {
            Ok(Some(theme)) => theme,
            Ok(None) | Err(_) => Self::embedded(),
        }
    }

    fn compile(document: &DesignSourceDocument, context: DesignContext) -> Result<Self, Error> {
        match design::compile_design(document, context.clone()) {
            DesignCompileResult::Success(success) => Ok(Self {
                context,
                design: success.candidate,
            }),
            DesignCompileResult::Fatal(failure) => Err(Error::Compile(failure.diagnostics)),
        }
    }

    pub fn context(&self) -> &DesignContext {
        &self.context
    }

    pub fn scheme(&self) -> Scheme {
        self.context.scheme
    }

    pub fn mode(&self) -> Mode {
        self.context.mode
    }

    pub fn contrast(&self) -> Contrast {
        self.context.contrast
    }

    pub fn design(&self) -> &UnstampedResolvedDesign {
        &self.design
    }

    /// The theme, consumed: for a caller that goes on to stamp the design
    /// with `design::apply_compiled_design`.
    pub fn into_design(self) -> UnstampedResolvedDesign {
        self.design
    }
}

fn embedded_document() -> DesignSourceDocument {
    design::parse_design_source(
        SourceIdentity::new("embedded"),
        design::EMBEDDED_DEFAULT_SOURCE,
    )
    .expect("the embedded default design parses")
}

/// The selection in a file that holds only `scheme:` and `mode:`.
fn selection_only(source: &str) -> Option<LegacyV0Source> {
    let value = config::parse(source).ok()?;
    let config::Value::Map(fields) = &value else {
        return None;
    };
    if fields.keys().any(|key| key != "scheme" && key != "mode") {
        return None;
    }
    let selection = design::parse_legacy_v0_source(source).ok()?;
    selection.is_selection_only().then_some(selection)
}

fn axis<T: Default>(
    name: Option<&str>,
    field: &'static str,
    from_name: fn(&str) -> Option<T>,
) -> Result<T, Error> {
    match name {
        None => Ok(T::default()),
        Some(name) => from_name(name).ok_or_else(|| Error::Selection {
            field,
            value: name.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_theme_is_ocean_light() {
        let theme = Theme::embedded();
        assert_eq!(theme.scheme(), Scheme::Ocean);
        assert_eq!(theme.mode(), Mode::Light);
        assert_eq!(theme.contrast(), Contrast::Normal);
        assert_eq!(theme.context().app, None);
        assert!(theme.dictionary().colours.pairs.contains_key("base"));
    }

    #[test]
    fn every_shipped_context_compiles() {
        for scheme in Scheme::ALL {
            for mode in Mode::ALL {
                let theme = Theme::for_context(DesignContext {
                    scheme,
                    mode,
                    ..DesignContext::default()
                });
                assert_eq!((theme.scheme(), theme.mode()), (scheme, mode));
            }
        }
    }

    #[test]
    fn selection_only_file_selects_against_the_embedded_design() {
        let theme = Theme::from_source("test", "scheme: \"forest\"\nmode: \"dark\"\n").unwrap();
        assert_eq!(theme.scheme(), Scheme::Forest);
        assert_eq!(theme.mode(), Mode::Dark);
        assert_eq!(
            theme.design(),
            Theme::for_context(DesignContext {
                scheme: Scheme::Forest,
                mode: Mode::Dark,
                ..DesignContext::default()
            })
            .design()
        );
        let partial = Theme::from_source("test", "mode: \"dark\"\n").unwrap();
        assert_eq!((partial.scheme(), partial.mode()), (Scheme::Ocean, Mode::Dark));
    }

    #[test]
    fn the_full_document_selects_its_own_context() {
        let theme = Theme::from_source("embedded", design::EMBEDDED_DEFAULT_SOURCE).unwrap();
        assert_eq!((theme.scheme(), theme.mode()), (Scheme::Ocean, Mode::Light));
        assert_eq!(theme.design(), Theme::embedded().design());
    }

    #[test]
    fn bad_sources_are_reported() {
        let unknown = Theme::from_source("test", "scheme: \"neon\"\n").unwrap_err();
        assert!(
            matches!(&unknown, Error::Selection { field: "scheme", value } if value == "neon"),
            "{unknown}"
        );
        assert_eq!(unknown.to_string(), "unknown scheme \"neon\"");
        let extra = Theme::from_source("test", "scheme: \"ocean\"\nsurface: \"#ffffff\"\n")
            .unwrap_err();
        assert!(matches!(extra, Error::Source(_)), "{extra}");
        let garbage = Theme::from_source("test", "{{{").unwrap_err();
        assert!(matches!(garbage, Error::Source(_)), "{garbage}");
    }

    #[test]
    fn read_distinguishes_missing_from_broken() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(THEME_FILE);
        assert!(Theme::read(&path).unwrap().is_none());
        std::fs::write(&path, "scheme: \"stone\"\nmode: \"light\"\n").unwrap();
        assert_eq!(Theme::read(&path).unwrap().unwrap().scheme(), Scheme::Stone);
        std::fs::write(&path, "mode: \"dusk\"\n").unwrap();
        assert!(matches!(
            Theme::read(&path).unwrap_err(),
            Error::Selection { field: "mode", .. }
        ));
        assert!(theme_path().ends_with(THEME_FILE));
    }
}
