//! The UI fonts for iced surfaces: the pinned asset set, or the embedded Inter.
//!
//! [`install`] registers the activated MixOS asset set (`assets::mixos`) into
//! iced's global font system: every font role the set locks (sans, serif,
//! mono, their italics, display, icons and the colour emoji), with the
//! generic families — which `iced_core::Font::DEFAULT` and `Font::MONOSPACE`
//! resolve through — mapped to the set's sans, mono and serif families.
//! Without a set (no `current` link under any asset root, or a set that
//! fails its checks) the embedded Inter (vendor/font/) is registered as the
//! sans-serif default instead, with a warning, so text renders on systems
//! with no fonts installed. System fonts still load alongside either way, so
//! per-glyph fallback keeps working for scripts the set lacks.
//!
//! Weight and optical size: cosmic-text drives the `wght` axis of a variable
//! font to the requested weight and leaves every other axis at the font's
//! `fvar` default. Inter's default `opsz` is 14 (its text cut), so [`BODY`]
//! — Light, 300 — renders the text cut at Light; nothing here needs to set
//! `opsz`. The display cut (`opsz` 32) is not reachable through cosmic-text.
//!
//! [`install`] must run before ANY iced text is created or measured: iced's
//! font system is a lazy global, and whoever touches it first freezes what
//! the early shaping caches see. `main()` calls this at startup, ahead of
//! every engine/surface construction.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use assets::AssetSet;
use iced_core::Font;
use iced_core::font::Weight;
use iced_graphics::text::cosmic_text::fontdb::{Database, Family, ID, Query, Source};

/// Inter roman variable font, the no-set fallback. cosmic-text drives the
/// `wght` axis, so every requested weight renders true from this one file.
/// It has no italic axis: italic styles render upright unless an italic VF
/// is added alongside.
pub const INTER_FONT: &[u8] =
    include_bytes!("../../../../../../../vendor/font/Inter-VariableFont_opsz,wght.ttf");

/// Family name in the embedded font's `name` table (what fontdb indexes it
/// under). The asset set's Inter declares `Inter Variable`, so the two never
/// shadow each other.
pub const FAMILY: &str = "Inter";

/// Body text: the sans-serif default at Light (300), the measured closest
/// free match to SF Pro Text Light with Inter's text cut (`opsz` 14, the
/// font's default). Every iced surface starts from this (`EngineSettings`),
/// and explicit weights (bold labels, titles) still override it per text.
pub const BODY: Font = Font {
    weight: Weight::Light,
    ..Font::DEFAULT
};

/// The set roles the generic families map to, in report order.
const GENERIC: [(&str, Family<'static>, &str); 3] = [
    ("sans", Family::SansSerif, "sans-serif"),
    ("mono", Family::Monospace, "monospace"),
    ("serif", Family::Serif, "serif"),
];

/// Where the installed fonts came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// The asset set with this ID.
    Set(String),
    /// The embedded Inter: no set was found.
    Embedded,
}

/// One font role loaded from a set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub role: String,
    /// The family the loaded file declares (what fontdb matches on).
    pub family: String,
    /// The file, relative to the set.
    pub path: String,
}

/// What [`install_in`] did, for the one startup log line and the tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub origin: Origin,
    /// The set's roles that loaded, in role order.
    pub fonts: Vec<Loaded>,
    /// The set's roles whose file failed to load, with the error.
    pub failed: Vec<(String, String)>,
    /// Host-installed faces removed because they shadowed a loaded family.
    pub pruned: usize,
    /// Each generic family and the family it resolves to (`None`: no face;
    /// text in that family would render as tofu).
    pub generic: Vec<(&'static str, Option<String>)>,
    /// Roles whose declared family (manifest) differs from the file's.
    pub mismatched: Vec<(String, String, String)>,
}

impl Report {
    /// The generic families that resolve to nothing.
    pub fn unresolved(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.generic
            .iter()
            .filter(|(_, family)| family.is_none())
            .map(|(generic, _)| *generic)
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.origin {
            Origin::Set(id) => {
                write!(f, "set {id}:")?;
                for font in &self.fonts {
                    write!(f, " {}={} ({})", font.role, font.family, font.path)?;
                }
            }
            Origin::Embedded => {
                write!(f, "embedded {FAMILY} ({} KiB)", INTER_FONT.len() / 1024)?;
            }
        }
        if self.pruned > 0 {
            write!(f, "; pruned {} shadowing host face(s)", self.pruned)?;
        }
        write!(f, ";")?;
        for (generic, family) in &self.generic {
            match family {
                Some(family) => write!(f, " {generic}->{family}")?,
                None => write!(f, " {generic}->NONE")?,
            }
        }
        Ok(())
    }
}

/// Discover the asset set and register the UI fonts into iced's global font
/// system. Never panics: on any failure the compositor keeps running and
/// shaping falls back to whatever fonts the system provides.
pub fn install() {
    let started = Instant::now();
    let set = match assets::mixos::discover() {
        Ok(Some(set)) => Some(set),
        Ok(None) => {
            let roots: Vec<String> = assets::mixos::lookup()
                .roots()
                .iter()
                .map(|root| root.join(assets::CURRENT_LINK).display().to_string())
                .collect();
            warn!(
                "UI fonts: no asset set activated (none of {}); using the embedded {FAMILY}",
                roots.join(", ")
            );
            None
        }
        Err(err) => {
            warn!("UI fonts: the asset set is unusable ({err}); using the embedded {FAMILY}");
            None
        }
    };

    let Ok(mut system) = iced_graphics::text::font_system().write() else {
        warn!("UI fonts: iced font system lock poisoned; skipping the install");
        return;
    };
    let report = install_in(system.raw().db_mut(), set.as_ref());
    drop(system);

    info!("UI fonts: {report} ({} ms)", started.elapsed().as_millis());
    for (role, error) in &report.failed {
        warn!("UI fonts: role {role} did not load: {error}");
    }
    for (role, declared, actual) in &report.mismatched {
        warn!("UI fonts: role {role} declares family {declared:?} but its file is {actual:?}");
    }
    for generic in report.unresolved() {
        warn!("UI fonts: {generic} resolves to NO face; text in it would render as tofu");
    }
}

/// Register the fonts into `db`: the set's roles when `set` is given, else
/// the embedded Inter. Pure over the database, so a test can run it on a
/// fresh `Database` without iced's global.
pub fn install_in(db: &mut Database, set: Option<&AssetSet>) -> Report {
    match set {
        Some(set) => install_set(db, set),
        None => install_embedded(db),
    }
}

fn install_set(db: &mut Database, set: &AssetSet) -> Report {
    let mut fonts = Vec::new();
    let mut failed = Vec::new();
    let mut mismatched = Vec::new();
    let mut paths: Vec<PathBuf> = Vec::new();
    for role in set.roles() {
        let Some(path) = set.font_path(role) else {
            continue;
        };
        let relative = path
            .strip_prefix(set.root())
            .unwrap_or(&path)
            .display()
            .to_string();
        // Variable fonts are indexed once; a collection would give several
        // faces, all under the same family.
        let loaded_before: Vec<ID> = db.faces().map(|face| face.id).collect();
        if let Err(err) = db.load_font_file(&path) {
            failed.push((role.to_owned(), err.to_string()));
            continue;
        }
        let Some(face) = db
            .faces()
            .find(|face| !loaded_before.contains(&face.id) && source_path(&face.source) == Some(path.as_path()))
        else {
            failed.push((role.to_owned(), "no face indexed".to_owned()));
            continue;
        };
        let family = face
            .families
            .first()
            .map(|(name, _)| name.clone())
            .unwrap_or_default();
        if let Some(declared) = set.family(role)
            && !face.families.iter().any(|(name, _)| name.as_str() == declared)
        {
            mismatched.push((role.to_owned(), declared.to_owned(), family.clone()));
        }
        paths.push(path.clone());
        fonts.push(Loaded {
            role: role.to_owned(),
            family,
            path: relative,
        });
    }

    // Host-installed faces of a family the set provides would shadow it
    // (system fonts load first and same-family faces resolve first-come),
    // making the UI render whatever version the host ships. Prune them so
    // every machine renders the exact pinned files; hosts without those
    // families are unaffected. The set's own files and embedded faces stay.
    let families: Vec<&str> = fonts.iter().map(|font| font.family.as_str()).collect();
    let pruned = prune_shadowing(db, &families, |source| {
        source_path(source).is_some_and(|path| !paths.iter().any(|own| own == path))
    });

    for (role, _, _) in GENERIC {
        if let Some(font) = fonts.iter().find(|font| font.role == role) {
            set_generic(db, role, font.family.clone());
        }
    }

    Report {
        origin: Origin::Set(set.set_id().to_owned()),
        fonts,
        failed,
        pruned,
        generic: resolve_generic(db),
        mismatched,
    }
}

fn install_embedded(db: &mut Database) -> Report {
    db.load_font_data(INTER_FONT.to_vec());
    db.set_sans_serif_family(FAMILY);
    let pruned = prune_shadowing(db, &[FAMILY], |source| !matches!(source, Source::Binary(_)));
    Report {
        origin: Origin::Embedded,
        fonts: Vec::new(),
        failed: Vec::new(),
        pruned,
        generic: resolve_generic(db),
        mismatched: Vec::new(),
    }
}

/// Remove every face of one of `families` whose source `is_foreign`.
fn prune_shadowing(db: &mut Database, families: &[&str], is_foreign: impl Fn(&Source) -> bool) -> usize {
    let shadowing: Vec<ID> = db
        .faces()
        .filter(|face| {
            is_foreign(&face.source)
                && face
                    .families
                    .iter()
                    .any(|(name, _)| families.contains(&name.as_str()))
        })
        .map(|face| face.id)
        .collect();
    for id in &shadowing {
        db.remove_face(*id);
    }
    shadowing.len()
}

fn set_generic(db: &mut Database, role: &str, family: String) {
    match role {
        "sans" => db.set_sans_serif_family(family),
        "mono" => db.set_monospace_family(family),
        "serif" => db.set_serif_family(family),
        _ => {}
    }
}

/// Resolve each generic family exactly like shaping will, to the family of
/// the face that wins.
fn resolve_generic(db: &Database) -> Vec<(&'static str, Option<String>)> {
    GENERIC
        .iter()
        .map(|(_, family, name)| {
            let winner = db
                .query(&Query {
                    families: &[*family],
                    ..Query::default()
                })
                .and_then(|id| db.face(id))
                .and_then(|face| face.families.first().map(|(name, _)| name.clone()));
            (*name, winner)
        })
        .collect()
}

/// The file a face was loaded from, for a file-backed source.
fn source_path(source: &Source) -> Option<&Path> {
    match source {
        Source::File(path) | Source::SharedFile(path, _) => Some(path.as_path()),
        Source::Binary(_) => None,
    }
}
