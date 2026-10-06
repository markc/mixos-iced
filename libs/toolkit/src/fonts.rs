// SPDX-License-Identifier: MIT OR Apache-2.0
//! Caller-supplied fonts and icon fonts, registered once per process with
//! iced's shared font system.
//!
//! A [`FontSet`] names one face per role (sans, mono, serif, display, emoji)
//! from bytes or a path. An [`IconFont`] is a glyph font with a name →
//! codepoint table in the Material Symbols `.codepoints` format (`name hex`
//! per line). [`install`] reads them, replaces any preloaded face of the same
//! family, registers them and binds iced's generic sans-serif, serif and
//! monospace families to the supplied roles. Nothing here reads a
//! configuration, an environment variable or a fixed path: the application
//! decides where its fonts come from.

use std::{
    borrow::Cow,
    collections::{BTreeMap, HashSet},
    fmt, io,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use iced_core::{Font, font};
use iced_graphics::text::{
    cosmic_text::{self, fontdb},
    font_system,
};

/// Font bytes, or a file to read them from at [`install`] time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontSource {
    Bytes(Cow<'static, [u8]>),
    Path(PathBuf),
}

impl From<&'static [u8]> for FontSource {
    fn from(bytes: &'static [u8]) -> Self {
        Self::Bytes(Cow::Borrowed(bytes))
    }
}

impl From<Vec<u8>> for FontSource {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Bytes(Cow::Owned(bytes))
    }
}

impl From<PathBuf> for FontSource {
    fn from(path: PathBuf) -> Self {
        Self::Path(path)
    }
}

impl From<&Path> for FontSource {
    fn from(path: &Path) -> Self {
        Self::Path(path.to_path_buf())
    }
}

impl FontSource {
    fn load(self, role: &'static str) -> Result<Cow<'static, [u8]>, FontError> {
        match self {
            Self::Bytes(bytes) => Ok(bytes),
            Self::Path(path) => std::fs::read(&path)
                .map(Cow::Owned)
                .map_err(|error| FontError::Read { role, path, error }),
        }
    }
}

/// The roles a [`FontSet`] can fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    /// Body and UI text; bound to iced's generic sans-serif family.
    Sans,
    /// Code and technical fields; bound to the generic monospace family.
    Mono,
    /// Bound to the generic serif family.
    Serif,
    /// Headings; selected explicitly with [`Fonts::font`].
    Display,
    /// Emoji; registered for fallback, selected explicitly if wanted.
    Emoji,
}

impl Role {
    pub const ALL: [Role; 5] = [
        Role::Sans,
        Role::Mono,
        Role::Serif,
        Role::Display,
        Role::Emoji,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Role::Sans => "sans",
            Role::Mono => "mono",
            Role::Serif => "serif",
            Role::Display => "display",
            Role::Emoji => "emoji",
        }
    }
}

/// One optional face per role. Build it with the role methods or fill the
/// fields directly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FontSet {
    /// Extra faces registered without changing any generic role binding.
    pub additional: Vec<FontSource>,
    pub sans: Option<FontSource>,
    pub mono: Option<FontSource>,
    pub serif: Option<FontSource>,
    pub display: Option<FontSource>,
    pub emoji: Option<FontSource>,
}

impl FontSet {
    pub fn additional(mut self, source: impl Into<FontSource>) -> Self {
        self.additional.push(source.into());
        self
    }
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sans(mut self, source: impl Into<FontSource>) -> Self {
        self.sans = Some(source.into());
        self
    }

    pub fn mono(mut self, source: impl Into<FontSource>) -> Self {
        self.mono = Some(source.into());
        self
    }

    pub fn serif(mut self, source: impl Into<FontSource>) -> Self {
        self.serif = Some(source.into());
        self
    }

    pub fn display(mut self, source: impl Into<FontSource>) -> Self {
        self.display = Some(source.into());
        self
    }

    pub fn emoji(mut self, source: impl Into<FontSource>) -> Self {
        self.emoji = Some(source.into());
        self
    }

    pub fn get(&self, role: Role) -> Option<&FontSource> {
        match role {
            Role::Sans => self.sans.as_ref(),
            Role::Mono => self.mono.as_ref(),
            Role::Serif => self.serif.as_ref(),
            Role::Display => self.display.as_ref(),
            Role::Emoji => self.emoji.as_ref(),
        }
    }

    fn take(&mut self, role: Role) -> Option<FontSource> {
        match role {
            Role::Sans => self.sans.take(),
            Role::Mono => self.mono.take(),
            Role::Serif => self.serif.take(),
            Role::Display => self.display.take(),
            Role::Emoji => self.emoji.take(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.additional.is_empty() && Role::ALL.iter().all(|role| self.get(*role).is_none())
    }
}

/// A line of a `.codepoints` table that could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodepointsError {
    /// 1-based line number.
    pub line: usize,
    pub reason: &'static str,
}

impl fmt::Display for CodepointsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "codepoints line {}: {}", self.line, self.reason)
    }
}

impl std::error::Error for CodepointsError {}

/// A glyph font with a name → codepoint table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconFont {
    pub font: FontSource,
    pub codepoints: BTreeMap<String, char>,
}

impl IconFont {
    pub fn new(font: impl Into<FontSource>, codepoints: BTreeMap<String, char>) -> Self {
        Self {
            font: font.into(),
            codepoints,
        }
    }

    /// From a font and the text of its `.codepoints` table.
    pub fn from_codepoints(
        font: impl Into<FontSource>,
        text: &str,
    ) -> Result<Self, CodepointsError> {
        Ok(Self::new(font, Self::parse_codepoints(text)?))
    }

    /// Parse the Material Symbols `.codepoints` format: one `name hex` pair
    /// per line, blank lines ignored, names unique.
    pub fn parse_codepoints(text: &str) -> Result<BTreeMap<String, char>, CodepointsError> {
        let mut icons = BTreeMap::new();
        for (index, line) in text.lines().enumerate() {
            let fail = |reason| CodepointsError {
                line: index + 1,
                reason,
            };
            let mut parts = line.split_whitespace();
            let Some(name) = parts.next() else {
                continue;
            };
            let hex = parts.next().ok_or(fail("missing codepoint"))?;
            if parts.next().is_some() {
                return Err(fail("more than two fields"));
            }
            let scalar = u32::from_str_radix(hex, 16).map_err(|_| fail("codepoint is not hex"))?;
            let glyph = char::from_u32(scalar).ok_or(fail("codepoint is not a Unicode scalar"))?;
            if icons.insert(name.to_owned(), glyph).is_some() {
                return Err(fail("duplicate name"));
            }
        }
        if icons.is_empty() {
            return Err(CodepointsError {
                line: 0,
                reason: "empty table",
            });
        }
        Ok(icons)
    }

    pub fn glyph(&self, name: &str) -> Option<char> {
        self.codepoints.get(name).copied()
    }
}

/// Why [`install`] failed. The font system is left as it was.
#[derive(Debug)]
pub enum FontError {
    /// A `FontSource::Path` could not be read.
    Read {
        role: &'static str,
        path: PathBuf,
        error: io::Error,
    },
    /// The bytes hold no font face.
    NoFace { role: &'static str },
    /// The face did not register with iced's font system.
    NotRegistered { role: &'static str, family: String },
    /// Fonts are installed once per process.
    AlreadyInstalled,
    /// iced's font system lock is poisoned.
    Poisoned,
}

impl fmt::Display for FontError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { role, path, error } => {
                write!(f, "{role} font {}: {error}", path.display())
            }
            Self::NoFace { role } => write!(f, "{role} font holds no face"),
            Self::NotRegistered { role, family } => {
                write!(f, "{role} font family {family:?} could not be registered")
            }
            Self::AlreadyInstalled => write!(f, "fonts are already installed"),
            Self::Poisoned => write!(f, "font system lock poisoned"),
        }
    }
}

impl std::error::Error for FontError {}

/// The registered fonts: a family name per supplied role and the icon table.
#[derive(Debug)]
pub struct Fonts {
    families: BTreeMap<Role, &'static str>,
    icon_family: Option<&'static str>,
    icons: BTreeMap<String, char>,
}

impl Fonts {
    /// The registered family name of a role.
    pub fn family(&self, role: Role) -> Option<&'static str> {
        self.families.get(&role).copied()
    }

    /// A regular-weight font naming the role's family.
    pub fn font(&self, role: Role) -> Option<Font> {
        self.family(role).map(named)
    }

    /// The icon font, without a glyph.
    pub fn icon_font(&self) -> Option<Font> {
        self.icon_family.map(named)
    }

    /// The glyph and font of a named icon, ready for a text widget.
    pub fn icon(&self, name: &str) -> Option<(char, Font)> {
        self.icons.get(name).copied().zip(self.icon_font())
    }

    /// Every icon name, in order.
    pub fn icon_names(&self) -> impl Iterator<Item = &str> {
        self.icons.keys().map(String::as_str)
    }
}

static INSTALLED: OnceLock<Fonts> = OnceLock::new();
static INSTALLING: Mutex<()> = Mutex::new(());

/// Register the set and the icon font, once per process. Faces already in
/// the font system under one of the supplied families are replaced, so the
/// application's bytes win over a system font of the same name. The
/// generic sans-serif, serif and monospace families are bound to the sans,
/// serif and mono roles when supplied. Sources are read and checked before
/// the installed-once rule applies, so a bad source is always reported.
pub fn install(mut set: FontSet, icon: Option<IconFont>) -> Result<&'static Fonts, FontError> {
    // Read and identify every face before touching the renderer's collection.
    let mut faces: Vec<(&'static str, Cow<'static, [u8]>, String)> = Vec::new();
    let mut roles = Vec::new();
    for role in Role::ALL {
        if let Some(source) = set.take(role) {
            let bytes = source.load(role.name())?;
            let family = first_family(&bytes).ok_or(FontError::NoFace { role: role.name() })?;
            roles.push((role, family.clone()));
            faces.push((role.name(), bytes, family));
        }
    }
    let icons = icon
        .map(|icon| {
            let bytes = icon.font.load("icon")?;
            let family = first_family(&bytes).ok_or(FontError::NoFace { role: "icon" })?;
            faces.push(("icon", bytes, family.clone()));
            Ok::<_, FontError>((family, icon.codepoints))
        })
        .transpose()?;
    for source in set.additional {
        let bytes = source.load("additional")?;
        let family = first_family(&bytes).ok_or(FontError::NoFace { role: "additional" })?;
        faces.push(("additional", bytes, family));
    }
    let families: HashSet<String> = faces
        .iter()
        .map(|(_, _, family)| family.to_ascii_lowercase())
        .collect();
    let _installing = INSTALLING.lock().map_err(|_| FontError::Poisoned)?;
    if INSTALLED.get().is_some() {
        return Err(FontError::AlreadyInstalled);
    }
    {
        let mut system = font_system().write().map_err(|_| FontError::Poisoned)?;
        let conflicts: Vec<_> = system
            .raw()
            .db()
            .faces()
            .filter(|face| {
                face.families
                    .iter()
                    .any(|(name, _)| families.contains(&name.to_ascii_lowercase()))
            })
            .map(|face| face.id)
            .collect();
        // db_mut invalidates cosmic-text's family-match cache; load_font also
        // increments iced's version so existing paragraphs refresh.
        for id in conflicts {
            system.raw().db_mut().remove_face(id);
        }
        for (_, bytes, _) in &faces {
            system.load_font(bytes.clone());
        }
        for (role, _, family) in &faces {
            if !has_family(system.raw().db(), family) {
                return Err(FontError::NotRegistered {
                    role,
                    family: family.clone(),
                });
            }
        }
        // Generic widget fonts share the supplied roles. These bindings do
        // not rewrite an explicit `Family::Name` choice.
        let db = system.raw().db_mut();
        for (role, family) in &roles {
            match role {
                Role::Sans => db.set_sans_serif_family(family.clone()),
                Role::Serif => db.set_serif_family(family.clone()),
                Role::Mono => db.set_monospace_family(family.clone()),
                Role::Display | Role::Emoji => {}
            }
        }
    }
    let fonts = Fonts {
        families: roles
            .into_iter()
            .map(|(role, family)| (role, intern(&family)))
            .collect(),
        icon_family: icons.as_ref().map(|(family, _)| intern(family)),
        icons: icons.map(|(_, table)| table).unwrap_or_default(),
    };
    INSTALLED
        .set(fonts)
        .map_err(|_| FontError::AlreadyInstalled)?;
    Ok(INSTALLED.get().expect("just set"))
}

/// The fonts registered by [`install`], if any.
pub fn installed() -> Option<&'static Fonts> {
    INSTALLED.get()
}

/// The glyph and font of a named icon from the installed [`IconFont`].
pub fn icon(name: &str) -> Option<(char, Font)> {
    installed().and_then(|fonts| fonts.icon(name))
}

/// Shared UI default for an iced application's `.default_font(...)`: the
/// installed sans face, else the generic sans-serif family.
pub fn default_ui_font() -> Font {
    font_for("sans-serif", &[], 400, false, true)
}

/// Shared mono default for code, technical fields and other mono widgets.
pub fn default_mono_font() -> Font {
    font_for("monospace", &[], 400, true, true)
}

/// Resolve a family chain to a registered face. With `prefer_installed`, the
/// installed sans (or mono) role is tried first; an explicit family that is
/// registered keeps precedence when `prefer_installed` is false. The weight
/// is bucketed to iced's scale, and a Light request falls back to Normal in
/// a family with neither a light face nor a variable weight axis covering 300.
pub fn font_for(
    family: &str,
    fallbacks: &[String],
    requested_weight: u16,
    monospace: bool,
    prefer_installed: bool,
) -> Font {
    let preferred = prefer_installed
        .then(|| {
            installed()
                .and_then(|fonts| fonts.family(if monospace { Role::Mono } else { Role::Sans }))
        })
        .flatten();
    let names: Vec<_> = preferred
        .into_iter()
        .chain(std::iter::once(family))
        .chain(fallbacks.iter().map(String::as_str))
        .collect();
    let (found, has_light) = {
        let mut system = font_system().write().expect("font system");
        let raw = system.raw();
        let found = names
            .iter()
            .find(|name| has_family(raw.db(), name))
            .map(|name| (*name).to_owned());
        let generic = raw
            .db()
            .family_name(&if monospace {
                fontdb::Family::Monospace
            } else {
                fontdb::Family::SansSerif
            })
            .to_owned();
        let light = family_has_light(raw, found.as_deref().unwrap_or(&generic));
        (found, light)
    };
    let family = match found {
        Some(name) => font::Family::Name(intern(&name)),
        None if monospace => font::Family::Monospace,
        None => font::Family::SansSerif,
    };
    Font {
        family,
        weight: weight(effective_weight(requested_weight, has_light)),
        ..Font::DEFAULT
    }
}

/// fontdb indexes a variable face at its default weight, usually 400.
/// Read its actual `wght` range before deciding that Light is unavailable.
fn family_has_light(system: &mut cosmic_text::FontSystem, family: &str) -> bool {
    let faces: Vec<_> = system
        .db()
        .faces()
        .filter(|face| {
            face.families
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(family))
        })
        .map(|face| (face.id, face.weight))
        .collect();
    faces.into_iter().any(|(id, weight)| {
        weight == fontdb::Weight::LIGHT
            || system
                .get_font(id, fontdb::Weight::LIGHT)
                .is_some_and(|font| {
                    font.as_swash().variations().any(|axis| {
                        axis.tag() == u32::from_be_bytes(*b"wght")
                            && axis.min_value() <= 300.0
                            && axis.max_value() >= 300.0
                    })
                })
    })
}

/// A Light (300) request in a family with no light face selects Normal, so
/// that a fallback family never renders ExtraLight.
pub const fn effective_weight(requested: u16, has_light: bool) -> u16 {
    if requested == 300 && !has_light {
        400
    } else {
        requested
    }
}

/// Bucket a CSS weight to iced's named weights.
pub const fn weight(value: u16) -> font::Weight {
    match value {
        0..=150 => font::Weight::Thin,
        151..=250 => font::Weight::ExtraLight,
        251..=350 => font::Weight::Light,
        351..=450 => font::Weight::Normal,
        451..=550 => font::Weight::Medium,
        551..=650 => font::Weight::Semibold,
        651..=750 => font::Weight::Bold,
        751..=850 => font::Weight::ExtraBold,
        _ => font::Weight::Black,
    }
}

fn named(family: &'static str) -> Font {
    Font {
        family: font::Family::Name(family),
        ..Font::DEFAULT
    }
}

/// The first family name of the first face in `bytes`.
fn first_family(bytes: &[u8]) -> Option<String> {
    let mut scratch = fontdb::Database::new();
    scratch.load_font_data(bytes.to_vec());
    scratch
        .faces()
        .next()
        .and_then(|face| face.families.first())
        .map(|(name, _)| name.clone())
}

fn has_family(db: &fontdb::Database, family: &str) -> bool {
    db.faces().any(|face| {
        face.families
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(family))
    })
}

fn intern(name: &str) -> &'static str {
    static NAMES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut names = NAMES
        .get_or_init(Default::default)
        .lock()
        .expect("font names");
    if let Some(existing) = names.get(name) {
        return existing;
    }
    let leaked: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.insert(leaked);
    leaked
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_graphics::text::FIRA_SANS_REGULAR;

    #[test]
    fn codepoints_parse_and_reject_bad_rows() {
        let table = IconFont::parse_codepoints("delete e872\n\nfolder e2c7\n").unwrap();
        assert_eq!(table.get("delete"), Some(&'\u{e872}'));
        assert_eq!(table.get("folder"), Some(&'\u{e2c7}'));
        assert_eq!(table.len(), 2);
        let icon = IconFont::from_codepoints(FIRA_SANS_REGULAR, "a 61").unwrap();
        assert_eq!(icon.glyph("a"), Some('a'));
        assert_eq!(icon.glyph("b"), None);
        for (text, line, reason) in [
            ("delete", 1, "missing codepoint"),
            ("delete e872 extra", 1, "more than two fields"),
            ("delete zz", 1, "codepoint is not hex"),
            ("delete d800", 1, "codepoint is not a Unicode scalar"),
            ("a 61\na 62", 2, "duplicate name"),
            ("\n\n", 0, "empty table"),
        ] {
            assert_eq!(
                IconFont::parse_codepoints(text),
                Err(CodepointsError { line, reason }),
                "{text:?}"
            );
        }
    }

    #[test]
    fn weights_bucket_and_light_falls_back() {
        assert_eq!(effective_weight(300, false), 400);
        assert_eq!(effective_weight(300, true), 300);
        assert_eq!(effective_weight(700, false), 700);
        assert_eq!(weight(100), font::Weight::Thin);
        assert_eq!(weight(200), font::Weight::ExtraLight);
        assert_eq!(weight(300), font::Weight::Light);
        assert_eq!(weight(400), font::Weight::Normal);
        assert_eq!(weight(600), font::Weight::Semibold);
        assert_eq!(weight(900), font::Weight::Black);
    }

    #[test]
    fn font_set_builders_fill_roles() {
        let set = FontSet::new()
            .sans(FIRA_SANS_REGULAR)
            .mono(Path::new("mono.ttf"))
            .emoji(vec![1, 2, 3]);
        assert!(!set.is_empty());
        assert!(FontSet::new().is_empty());
        assert_eq!(
            set.get(Role::Sans),
            Some(&FontSource::from(FIRA_SANS_REGULAR))
        );
        assert_eq!(
            set.get(Role::Mono),
            Some(&FontSource::Path(PathBuf::from("mono.ttf")))
        );
        assert_eq!(set.get(Role::Serif), None);
        assert_eq!(Role::Display.name(), "display");
        assert_eq!(
            first_family(FIRA_SANS_REGULAR).as_deref(),
            Some("Fira Sans")
        );
        assert_eq!(first_family(b"not a font"), None);
    }

    #[test]
    fn missing_file_and_bad_bytes_are_reported() {
        let missing = install(
            FontSet::new().sans(Path::new("/nonexistent/toolkit-test.ttf")),
            None,
        );
        assert!(matches!(missing, Err(FontError::Read { role: "sans", .. })));
        let bad = install(
            FontSet::new(),
            Some(IconFont::new(vec![0u8; 4], BTreeMap::new())),
        );
        assert!(matches!(bad, Err(FontError::NoFace { role: "icon" })));
    }

    /// The one test that installs: it uses the Fira Sans bytes iced embeds
    /// under its `fira-sans` feature, so no font file is needed.
    #[test]
    fn install_registers_roles_and_icons_once() {
        use fontdb::{Family, Query};
        let icon_font = IconFont::from_codepoints(FIRA_SANS_REGULAR, "a 61\nb 62").unwrap();
        let fonts = install(
            FontSet::new()
                .sans(FIRA_SANS_REGULAR)
                .mono(FIRA_SANS_REGULAR.to_vec()),
            Some(icon_font),
        )
        .unwrap();
        assert_eq!(fonts.family(Role::Sans), Some("Fira Sans"));
        assert_eq!(fonts.family(Role::Mono), Some("Fira Sans"));
        assert_eq!(fonts.family(Role::Serif), None);
        assert_eq!(fonts.icon("a"), Some(('a', named("Fira Sans"))));
        assert_eq!(fonts.icon("c"), None);
        assert_eq!(fonts.icon_names().collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(icon("b").map(|(glyph, _)| glyph), Some('b'));
        assert!(std::ptr::eq(installed().unwrap(), fonts));
        {
            let mut system = font_system().write().unwrap();
            let db = system.raw().db();
            assert_eq!(db.family_name(&Family::SansSerif), "Fira Sans");
            assert_eq!(db.family_name(&Family::Monospace), "Fira Sans");
            let id = db
                .query(&Query {
                    families: &[Family::Name("Fira Sans")],
                    ..Default::default()
                })
                .unwrap();
            assert_eq!(
                db.with_face_data(id, |bytes, _| bytes.to_vec()).unwrap(),
                FIRA_SANS_REGULAR
            );
        }
        let sans = font_for("Missing family", &[], 300, false, true);
        assert_eq!(sans.family, font::Family::Name("Fira Sans"));
        assert_eq!(sans.weight, font::Weight::Normal, "no light face");
        assert_eq!(
            default_ui_font(),
            font_for("sans-serif", &[], 400, false, true)
        );
        assert_eq!(default_mono_font().family, font::Family::Name("Fira Sans"));
        assert_eq!(
            font_for("Missing family", &[], 400, true, false).family,
            font::Family::Monospace
        );
        assert!(matches!(
            install(FontSet::new(), None),
            Err(FontError::AlreadyInstalled)
        ));
    }
}
