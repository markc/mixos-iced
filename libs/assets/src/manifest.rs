// SPDX-License-Identifier: MIT OR Apache-2.0

//! The locked manifest: what a published set contains and the rules every
//! manifest must satisfy before a set is opened.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result, invalid};

/// The one legacy manifest schema: the locked layout without per-style
/// icon metadata.
pub const SCHEMA: &str = "mixos.static-assets.v1";

/// The versioned manifest schema that declares icon metadata: an
/// [`IconDefault`] selection, per-style [`IconCatalogue`]s and named
/// non-font [`IconAsset`]s. Every v1 field is unchanged.
pub const SCHEMA_V2: &str = "mixos.static-assets.v2";

/// The style a v1 manifest's icons role is labelled: v1 records no
/// per-style icon metadata, so its one default catalogue is `default`
/// unless a versioned package mapping supplies the real semantics.
pub const DEFAULT_ICON_STYLE: &str = "default";

/// The most icon catalogues (family/style pairs) a v2 manifest may declare.
pub const MAX_ICON_CATALOGUES: usize = 32;

/// The most named non-font icon assets a v2 manifest may declare.
pub const MAX_ICON_ASSETS: usize = 4096;

/// The largest TTC face index a catalogue may select.
pub const MAX_FACE_INDEX: u32 = 65_535;

/// The largest icon family name, in UTF-8 bytes (the font role rule).
pub const MAX_FAMILY_BYTES: usize = 128;

/// The manifest's file name inside a published set.
pub const MANIFEST_FILE: &str = "manifest.conf.mix";

/// The generated stylesheet inside a published set; its text is locked in
/// the manifest as `web_css`.
pub const STYLESHEET_FILE: &str = "fonts.css";

/// The most files a manifest may lock.
pub const MAX_FILES: usize = 256;

/// The most font roles a manifest may name.
pub const MAX_ROLES: usize = 32;

/// The largest file a manifest may lock, in bytes.
pub const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// Locked installation data, read as strict (non-executable) data.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Always [`SCHEMA`].
    pub schema: String,
    /// The set this manifest describes; equals its `sets/<id>` directory.
    pub set_id: String,
    /// Native font roles (`sans`, `mono`, `icons`, `emoji`, `display`, …)
    /// to a locked font file.
    pub fonts: BTreeMap<String, String>,
    /// The family name each role's font declares, for callers that match
    /// by family rather than by file.
    #[serde(default)]
    pub font_families: BTreeMap<String, String>,
    /// Every file in the set, with its provenance and hashes.
    pub files: Vec<AssetFile>,
    /// The exact text of the set's `fonts.css`.
    pub web_css: String,
}

/// The versioned manifest ([`SCHEMA_V2`]): the unchanged v1 fields plus
/// declared icon metadata. `icon_default` names the selection an omitted
/// icon request uses; `icon_catalogues` the family/style → font face →
/// codepoints mapping; `icon_assets` the named non-font icons. A v1
/// manifest is read through the unchanged [`Manifest`] instead.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestV2 {
    /// Always [`SCHEMA_V2`].
    pub schema: String,
    /// The set this manifest describes; equals its `sets/<id>` directory.
    pub set_id: String,
    /// Native font roles to a locked font file, as in v1.
    pub fonts: BTreeMap<String, String>,
    /// The family name each role's font declares, as in v1.
    #[serde(default)]
    pub font_families: BTreeMap<String, String>,
    /// Every file in the set, with its provenance and hashes.
    pub files: Vec<AssetFile>,
    /// The exact text of the set's `fonts.css`.
    pub web_css: String,
    /// The default icon selection: the catalogue an omitted icon request
    /// uses. Must name a declared catalogue's family/style pair.
    pub icon_default: IconDefault,
    /// The declared icon catalogues, each a (family, style) pair mapped
    /// to a locked font face and a locked codepoints file.
    pub icon_catalogues: Vec<IconCatalogue>,
    /// The declared non-font icon assets (SVG or raster).
    #[serde(default)]
    pub icon_assets: Vec<IconAsset>,
}

impl ManifestV2 {
    /// The v1 view of this manifest: the shared fields, with the icon
    /// metadata set aside. The schema stays [`SCHEMA_V2`].
    pub(crate) fn v1(&self) -> Manifest {
        Manifest {
            schema: self.schema.clone(),
            set_id: self.set_id.clone(),
            fonts: self.fonts.clone(),
            font_families: self.font_families.clone(),
            files: self.files.clone(),
            web_css: self.web_css.clone(),
        }
    }
}

/// The manifest-declared default icon selection.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IconDefault {
    /// The declared icon family name, 1–[`MAX_FAMILY_BYTES`] UTF-8 bytes.
    pub family: String,
    /// The declared style (e.g. `default`, `rounded`, `outlined`).
    pub style: String,
    /// The exact weight, 1–1000 (a static weight or a supported `wght`
    /// axis value).
    pub weight: u16,
}

/// One declared icon catalogue: a (family, style) pair bound to a locked
/// font face and a locked codepoints file. The intrinsic family name and
/// face index of the font bytes are verified downstream, where the font
/// bytes are parsed; this metadata is validated structurally here and the
/// verified bytes are exposed as owned data.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IconCatalogue {
    /// The declared icon family name.
    pub family: String,
    /// The declared style; unique with [`family`](Self::family).
    pub style: String,
    /// The locked font file (`fonts/…`).
    pub font: String,
    /// The face index inside the font file (0 for a plain face).
    #[serde(default)]
    pub face_index: u32,
    /// The locked codepoints file (`name hex` per line).
    pub codepoints: String,
}

/// One declared non-font icon asset (SVG or raster). Its bytes are a
/// locked, verified file; decoding them is downstream.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IconAsset {
    /// The icon name; unique with [`style`](Self::style).
    pub name: String,
    /// The style this asset belongs to.
    pub style: String,
    /// The locked asset path (`icons/…`).
    pub path: String,
    /// Whether the asset is a symbolic (tintable) icon.
    #[serde(default)]
    pub symbolic: bool,
}

/// One locked file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetFile {
    /// Relative path inside the set.
    pub path: String,
    /// Where the bytes were fetched from (HTTPS).
    pub url: String,
    /// The upstream revision the bytes come from.
    pub revision: String,
    /// The upstream project (HTTPS).
    pub upstream: String,
    /// The licence the file is distributed under.
    pub licence: String,
    /// Exact size.
    pub bytes: u64,
    /// Lower-case hex SHA-256 of the bytes.
    pub sha256: String,
    /// Lower-case hex BLAKE3 of the bytes.
    pub blake3: String,
}

/// A set ID or role name: 1–96 ASCII letters, digits, `-` or `_`.
pub fn valid_set_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 96
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

/// A relative path inside a set: `/`-separated components of ASCII
/// letters, digits, `.`, `_` and `-`, none empty, `.` or `..`, each at
/// most 96 bytes, the whole at most 256.
pub fn valid_relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 256
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part.len() <= 96
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        })
}

/// The rules a parsed v1 manifest must satisfy before its set is opened.
pub(crate) fn validate(manifest: &Manifest, id: &str) -> Result<()> {
    if manifest.schema != SCHEMA {
        return Err(invalid(format!(
            "unsupported asset manifest schema {:?} (expected {SCHEMA:?})",
            manifest.schema
        )));
    }
    validate_shared(manifest, id)
}

/// The schema-independent rules both manifest versions share.
fn validate_shared(manifest: &Manifest, id: &str) -> Result<()> {
    if manifest.set_id != id {
        return Err(invalid(format!(
            "asset manifest set ID {:?} does not match its directory {id:?}",
            manifest.set_id
        )));
    }
    if manifest.files.is_empty() || manifest.files.len() > MAX_FILES {
        return Err(invalid("invalid locked asset count"));
    }
    if manifest.fonts.is_empty() || manifest.fonts.len() > MAX_ROLES {
        return Err(invalid("invalid font role count"));
    }
    let mut paths = BTreeSet::new();
    for file in &manifest.files {
        if !valid_relative_path(&file.path)
            || file.path == MANIFEST_FILE
            || file.path == STYLESHEET_FILE
        {
            return Err(invalid(format!("invalid locked asset path {:?}", file.path)));
        }
        if !paths.insert(file.path.as_str()) {
            return Err(invalid(format!("duplicate locked asset path {:?}", file.path)));
        }
        if file.bytes == 0 || file.bytes > MAX_FILE_BYTES {
            return Err(invalid(format!("invalid asset size for {:?}", file.path)));
        }
        if !valid_hash(&file.sha256) || !valid_hash(&file.blake3) {
            return Err(invalid(format!("invalid asset hash for {:?}", file.path)));
        }
        if !file.url.starts_with("https://") || !file.upstream.starts_with("https://") {
            return Err(invalid(format!(
                "asset provenance must use HTTPS: {:?}",
                file.path
            )));
        }
        if file.revision.is_empty() || file.licence.is_empty() {
            return Err(invalid(format!("missing asset provenance for {:?}", file.path)));
        }
    }
    for (role, path) in &manifest.fonts {
        if !valid_set_id(role) || !paths.contains(path.as_str()) {
            return Err(invalid(format!(
                "font role {role:?} references an unlocked asset {path:?}"
            )));
        }
        if !path.ends_with(".ttf") && !path.ends_with(".otf") {
            return Err(invalid(format!("font role {role:?} must name a font: {path:?}")));
        }
    }
    for (role, family) in &manifest.font_families {
        if !manifest.fonts.contains_key(role) || !valid_family(family) {
            return Err(invalid(format!("invalid font family metadata for role {role:?}")));
        }
    }
    Ok(())
}

/// The rules a parsed v2 manifest must satisfy: every shared rule plus
/// strict, bounded, unique icon metadata. All referenced font faces and
/// codepoints files must be locked; catalogue family/style pairs and
/// asset name/style pairs must be unique; the default must name a
/// declared catalogue.
pub(crate) fn validate_v2(manifest: &ManifestV2, id: &str) -> Result<()> {
    if manifest.schema != SCHEMA_V2 {
        return Err(invalid(format!(
            "unsupported asset manifest schema {:?} (expected {SCHEMA_V2:?})",
            manifest.schema
        )));
    }
    validate_shared(&manifest.v1(), id)?;
    let default = &manifest.icon_default;
    if !valid_family(&default.family) || !valid_set_id(&default.style) {
        return Err(invalid("invalid icon_default family or style"));
    }
    if default.weight == 0 || default.weight > 1000 {
        return Err(invalid(format!(
            "icon_default weight {} is outside 1..=1000",
            default.weight
        )));
    }
    if manifest.icon_catalogues.is_empty()
        || manifest.icon_catalogues.len() > MAX_ICON_CATALOGUES
    {
        return Err(invalid("invalid icon catalogue count"));
    }
    let mut pairs = BTreeSet::new();
    for catalogue in &manifest.icon_catalogues {
        if !valid_family(&catalogue.family) || !valid_set_id(&catalogue.style) {
            return Err(invalid(format!(
                "invalid icon catalogue family or style {:?}",
                catalogue.family
            )));
        }
        if !pairs.insert((catalogue.family.as_str(), catalogue.style.as_str())) {
            return Err(invalid(format!(
                "duplicate icon catalogue family/style {:?} {:?}",
                catalogue.family, catalogue.style
            )));
        }
        if catalogue.face_index > MAX_FACE_INDEX {
            return Err(invalid(format!(
                "icon catalogue face index {} exceeds {MAX_FACE_INDEX}",
                catalogue.face_index
            )));
        }
        if !locked_font(&manifest.files, &catalogue.font) {
            return Err(invalid(format!(
                "icon catalogue font {:?} is not a locked font",
                catalogue.font
            )));
        }
        if !locked_catalogue(&manifest.files, &catalogue.codepoints) {
            return Err(invalid(format!(
                "icon catalogue codepoints {:?} are not locked",
                catalogue.codepoints
            )));
        }
    }
    if !pairs.contains(&(default.family.as_str(), default.style.as_str())) {
        return Err(invalid(format!(
            "icon_default {:?} {:?} names no declared icon catalogue",
            default.family, default.style
        )));
    }
    if manifest.icon_assets.len() > MAX_ICON_ASSETS {
        return Err(invalid("too many icon assets"));
    }
    let mut names = BTreeSet::new();
    for asset in &manifest.icon_assets {
        if !valid_set_id(&asset.name) || !valid_set_id(&asset.style) {
            return Err(invalid(format!(
                "invalid icon asset name or style {:?}",
                asset.name
            )));
        }
        if !names.insert((asset.name.as_str(), asset.style.as_str())) {
            return Err(invalid(format!(
                "duplicate icon asset name/style {:?} {:?}",
                asset.name, asset.style
            )));
        }
        if !manifest.files.iter().any(|file| file.path == asset.path) {
            return Err(invalid(format!(
                "icon asset {:?} references an unlocked asset {:?}",
                asset.name, asset.path
            )));
        }
    }
    Ok(())
}

/// A locked font file reference: a locked path naming a `.ttf`/`.otf` file.
fn locked_font(files: &[AssetFile], path: &str) -> bool {
    (path.ends_with(".ttf") || path.ends_with(".otf"))
        && files.iter().any(|file| file.path == path)
}

/// A locked codepoints catalogue reference.
fn locked_catalogue(files: &[AssetFile], path: &str) -> bool {
    path.ends_with(".codepoints") && files.iter().any(|file| file.path == path)
}

/// A manifest of either accepted schema, chosen by its `schema` field
/// before deserialization: [`SCHEMA_V2`] hydrates the versioned
/// [`ManifestV2`], anything else the legacy [`Manifest`], whose rules
/// then refuse an unsupported schema.
#[derive(Debug)]
pub(crate) enum ParsedManifest {
    V1(Manifest),
    V2(ManifestV2),
}

impl ParsedManifest {
    /// Parse and hydrate a manifest, picking the struct from its declared
    /// schema. Strict-data syntax and hydration failures keep
    /// [`Error::Manifest`]; an unsupported schema is refused by
    /// [`validate`](Self::validate) with [`Error::Invalid`].
    pub(crate) fn parse(text: &str, path: &Path) -> Result<Self> {
        let value = strict::parse(text).map_err(|source| Error::Manifest {
            path: path.to_path_buf(),
            source,
        })?;
        let hydrate = |value: &strict::Value| {
            strict::from_value::<Manifest>(value).map_err(|source| Error::Manifest {
                path: path.to_path_buf(),
                source,
            })
        };
        match value.get("schema").and_then(strict::Value::as_str) {
            Some(SCHEMA) => Ok(Self::V1(hydrate(&value)?)),
            Some(SCHEMA_V2) => {
                let manifest: ManifestV2 = strict::from_value(&value).map_err(|source| {
                    Error::Manifest {
                        path: path.to_path_buf(),
                        source,
                    }
                })?;
                Ok(Self::V2(manifest))
            }
            Some(other) => Err(invalid(format!(
                "unsupported asset manifest schema {other:?} (expected {SCHEMA:?} or {SCHEMA_V2:?})"
            ))),
            // Without a schema the v1 hydration itself refuses; keep its
            // manifest error kind rather than reword it here.
            None => {
                let _ = hydrate(&value)?;
                unreachable!("Manifest::schema is required and always refuses its absence")
            }
        }
    }

    /// The schema-specific rules of this manifest.
    pub(crate) fn validate(&self, id: &str) -> Result<()> {
        match self {
            Self::V1(manifest) => validate(manifest, id),
            Self::V2(manifest) => validate_v2(manifest, id),
        }
    }

    /// The v1 view: for a v2 manifest the icon metadata is set aside and
    /// the shared v1 fields are projected unchanged.
    pub(crate) fn v1(&self) -> Manifest {
        match self {
            Self::V1(manifest) => manifest.clone(),
            Self::V2(manifest) => manifest.v1(),
        }
    }

    /// The resolved icon metadata: declared by a v2 manifest, derived for
    /// a v1 manifest from its `icons` role.
    pub(crate) fn icon_meta(&self) -> Result<IconMeta> {
        match self {
            Self::V1(manifest) => IconMeta::from_v1(manifest),
            Self::V2(manifest) => Ok(IconMeta {
                v1: false,
                default: Some(manifest.icon_default.clone()),
                catalogues: manifest.icon_catalogues.clone(),
                assets: manifest.icon_assets.clone(),
            }),
        }
    }
}

/// The icon metadata a set resolves to: a v2 manifest declares it; a v1
/// manifest derives its one default catalogue from the `icons` role —
/// style [`DEFAULT_ICON_STYLE`], weight 400, face index 0, the codepoints
/// sibling — with the family its `font_families` records (empty when it
/// records none).
#[derive(Debug, Clone)]
pub(crate) struct IconMeta {
    pub v1: bool,
    pub default: Option<IconDefault>,
    pub catalogues: Vec<IconCatalogue>,
    pub assets: Vec<IconAsset>,
}

impl IconMeta {
    pub(crate) fn from_v1(manifest: &Manifest) -> Result<Self> {
        let Some(font) = manifest.fonts.get("icons") else {
            return Ok(Self {
                v1: true,
                default: None,
                catalogues: Vec::new(),
                assets: Vec::new(),
            });
        };
        let family = manifest
            .font_families
            .get("icons")
            .cloned()
            .unwrap_or_default();
        let catalogue = IconCatalogue {
            family: family.clone(),
            style: DEFAULT_ICON_STYLE.to_owned(),
            font: font.clone(),
            face_index: 0,
            codepoints: v1_catalogue_path(font)?,
        };
        Ok(Self {
            v1: true,
            default: Some(IconDefault {
                family,
                style: DEFAULT_ICON_STYLE.to_owned(),
                weight: 400,
            }),
            catalogues: vec![catalogue],
            assets: Vec::new(),
        })
    }

    /// The catalogue an explicit `(family, style)` request selects:
    /// `Ok(None)` when the set does not declare the pair; an error when it
    /// cannot express the request at all — a metadata-free v1 set asked
    /// for a nondefault style. The declared default
    /// ([`default`](Self::default)) answers an omitted request instead.
    pub(crate) fn select(&self, family: &str, style: &str) -> Result<Option<&IconCatalogue>> {
        if self.v1 {
            if style != DEFAULT_ICON_STYLE {
                return Err(invalid(format!(
                    "icon style {style:?} is not supported: a {SCHEMA:?} manifest records no per-style icon metadata (a {SCHEMA_V2:?} manifest declares it)"
                )));
            }
            // The default catalogue is the only one; its family must match
            // the recorded claim (possibly absent).
            return Ok(self
                .default
                .as_ref()
                .filter(|default| default.family == family)
                .and(self.catalogues.first()));
        }
        Ok(self
            .catalogues
            .iter()
            .find(|catalogue| catalogue.family == family && catalogue.style == style))
    }
}

/// The codepoints sibling of a v1 icons font, as the installer derives it.
pub(crate) fn v1_catalogue_path(font: &str) -> Result<String> {
    let catalogue = Path::new(font).with_extension("codepoints");
    catalogue
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid("invalid icon catalogue path"))
}

/// An icon or font family name: non-empty, at most [`MAX_FAMILY_BYTES`]
/// UTF-8 bytes, no control characters.
fn valid_family(family: &str) -> bool {
    !family.is_empty() && family.len() <= MAX_FAMILY_BYTES && !family.chars().any(char::is_control)
}

/// 64 lower-case hex digits.
fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// An icon catalogue (`<name> <hex codepoint>` per line, as Material
/// Symbols' `.codepoints` files are written) to a name → character table.
pub(crate) fn parse_codepoints(text: &str) -> Result<BTreeMap<String, char>> {
    let mut icons = BTreeMap::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let mut parts = line.split_whitespace();
        let name = parts.next().ok_or_else(|| invalid("missing icon name"))?;
        let hex = parts
            .next()
            .ok_or_else(|| invalid(format!("missing icon codepoint for {name:?}")))?;
        if parts.next().is_some() || !valid_set_id(name) {
            return Err(invalid(format!("invalid icon catalogue row {line:?}")));
        }
        let scalar = u32::from_str_radix(hex, 16)
            .map_err(|_| invalid(format!("invalid icon codepoint {hex:?} for {name:?}")))?;
        let character = char::from_u32(scalar).ok_or_else(|| {
            invalid(format!("icon codepoint {hex:?} for {name:?} is not a Unicode scalar"))
        })?;
        if icons.insert(name.to_owned(), character).is_some() {
            return Err(invalid(format!("duplicate icon catalogue name {name:?}")));
        }
    }
    if icons.is_empty() {
        return Err(invalid("empty icon catalogue"));
    }
    Ok(icons)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_rejects_duplicate_names_and_non_scalars() {
        assert!(parse_codepoints("delete e872\ndelete e873").is_err());
        assert!(parse_codepoints("bad d800").is_err());
        assert!(parse_codepoints("bad 110000").is_err());
        assert!(parse_codepoints("bad e872 extra").is_err());
        assert!(parse_codepoints("").is_err());
        assert_eq!(
            parse_codepoints("folder e2c7\n").unwrap()["folder"],
            '\u{e2c7}'
        );
    }

    #[test]
    fn set_ids_and_relative_paths_are_strict() {
        assert!(valid_set_id("2026-10-04-core-3"));
        assert!(valid_set_id("serif_italic"));
        assert!(!valid_set_id(""));
        assert!(!valid_set_id("a/b"));
        assert!(!valid_set_id(&"x".repeat(97)));
        assert!(valid_relative_path("fonts/InterVariable.ttf"));
        assert!(valid_relative_path("web/NotoColorEmoji-COLRv1.ttf"));
        assert!(!valid_relative_path("/fonts/a.ttf"));
        assert!(!valid_relative_path("fonts//a.ttf"));
        assert!(!valid_relative_path("../a.ttf"));
        assert!(!valid_relative_path("./a.ttf"));
        assert!(!valid_relative_path("fonts/a b.ttf"));
        assert!(!valid_relative_path(""));
    }

    #[test]
    fn families_and_v1_catalogue_paths_are_bounded() {
        assert!(valid_family("Fixture Sans"));
        assert!(!valid_family(""));
        assert!(!valid_family(&"x".repeat(MAX_FAMILY_BYTES + 1)));
        assert!(!valid_family("bad\nname"));
        assert_eq!(
            v1_catalogue_path("icons/Symbols.ttf").unwrap(),
            "icons/Symbols.codepoints"
        );
        assert_eq!(
            v1_catalogue_path("icons/Symbols").unwrap(),
            "icons/Symbols.codepoints"
        );
    }

    fn entry(path: &str) -> serde_json::Value {
        serde_json::json!({
            "path": path, "bytes": 4,
            "url": "https://example.org/font", "upstream": "https://example.org/",
            "revision": "pinned", "licence": "OFL-1.1",
            "sha256": "1111111111111111111111111111111111111111111111111111111111111111",
            "blake3": "2222222222222222222222222222222222222222222222222222222222222222",
        })
    }

    fn text(json: serde_json::Value) -> String {
        strict::encode_pretty(&strict::from_json(json)).unwrap()
    }

    #[test]
    fn the_schema_routes_to_the_right_manifest_struct() {
        let path = Path::new("manifest.conf.mix");
        let v1 = text(serde_json::json!({
            "schema": SCHEMA, "set_id": "one",
            "fonts": { "sans": "fonts/a.ttf" }, "files": [entry("fonts/a.ttf")],
            "web_css": "x"
        }));
        assert!(matches!(ParsedManifest::parse(&v1, path).unwrap(), ParsedManifest::V1(_)));
        let v2 = text(serde_json::json!({
            "schema": SCHEMA_V2, "set_id": "one",
            "fonts": { "sans": "fonts/a.ttf" }, "files": [entry("fonts/a.ttf")],
            "web_css": "x",
            "icon_default": { "family": "F", "style": "default", "weight": 400 },
            "icon_catalogues": []
        }));
        assert!(matches!(ParsedManifest::parse(&v2, path).unwrap(), ParsedManifest::V2(_)));
        // An unsupported schema string is refused as an invalid manifest,
        // not hydrated as either struct.
        let other = text(serde_json::json!({
            "schema": "other.static-assets.v1", "set_id": "one",
            "fonts": {}, "files": [], "web_css": ""
        }));
        let error = ParsedManifest::parse(&other, path).unwrap_err();
        assert!(matches!(error, Error::Invalid(_)), "{error}");
        assert!(error.to_string().contains("unsupported"), "{error}");
        // A schema that is not a string keeps the manifest error kind the
        // legacy single-struct parse would have produced.
        let weird = text(serde_json::json!({
            "schema": 7, "set_id": "one", "fonts": {}, "files": [], "web_css": ""
        }));
        assert!(matches!(
            ParsedManifest::parse(&weird, path).unwrap_err(),
            Error::Manifest { .. }
        ));
    }

    #[test]
    fn v2_metadata_validation_is_strict_and_bounded() {
        let v2 = |mut json: serde_json::Value, key: &str| {
            if key == "missing-default" {
                json.as_object_mut().unwrap().remove("icon_default");
                return text(json);
            }
            match key {
                "default-not-declared" => json["icon_default"]["style"] = serde_json::json!("filled"),
                "weight-zero" => json["icon_default"]["weight"] = serde_json::json!(0),
                "weight-too-big" => json["icon_default"]["weight"] = serde_json::json!(1001),
                "face-too-big" => json["icon_catalogues"][0]["face_index"] = serde_json::json!(65536),
                "unlocked-font" => json["icon_catalogues"][0]["font"] = serde_json::json!("fonts/Absent.ttf"),
                "unlocked-codepoints" => json["icon_catalogues"][0]["codepoints"] = serde_json::json!("icons/Absent.codepoints"),
                "bad-codepoints-suffix" => json["icon_catalogues"][0]["codepoints"] = serde_json::json!("icons/Rounded.ttf"),
                _ => unreachable!("{key}"),
            }
            text(json)
        };
        let base = serde_json::json!({
            "schema": SCHEMA_V2, "set_id": "one",
            "fonts": { "sans": "fonts/Sans.ttf" },
            "files": [entry("fonts/Sans.ttf"), entry("icons/Rounded.ttf"), entry("icons/Rounded.codepoints")],
            "web_css": "x",
            "icon_default": { "family": "F", "style": "rounded", "weight": 400 },
            "icon_catalogues": [{
                "family": "F", "style": "rounded", "font": "icons/Rounded.ttf",
                "face_index": 0, "codepoints": "icons/Rounded.codepoints"
            }]
        });
        // The base manifest validates.
        ParsedManifest::parse(&text(base.clone()), Path::new("manifest.conf.mix"))
            .unwrap()
            .validate("one")
            .unwrap();
        for bad in [
            "missing-default",
            "default-not-declared",
            "weight-zero",
            "weight-too-big",
            "face-too-big",
            "unlocked-font",
            "unlocked-codepoints",
            "bad-codepoints-suffix",
        ] {
            let parsed = ParsedManifest::parse(&v2(base.clone(), bad), Path::new("manifest.conf.mix"));
            let error = match parsed {
                Ok(parsed) => parsed.validate("one").unwrap_err(),
                Err(error) => error,
            };
            assert!(
                error.to_string().contains("icon") || matches!(&error, Error::Manifest { .. }),
                "{bad}: {error}"
            );
        }
    }
}
