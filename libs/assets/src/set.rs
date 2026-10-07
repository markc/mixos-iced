// SPDX-License-Identifier: MIT OR Apache-2.0

//! A selected set, pinned to one published `sets/<id>` directory.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

use sha2::Digest;

use crate::error::{Error, Result, invalid, io};
use crate::manifest::{
    IconAsset, IconCatalogue, IconDefault, MANIFEST_FILE, Manifest, ManifestV2, ParsedManifest,
    STYLESHEET_FILE, parse_codepoints, valid_relative_path, valid_set_id,
};

/// The activation link inside a root: `current -> sets/<id>`.
pub const CURRENT_LINK: &str = "current";

pub(crate) const MANIFEST_LIMIT: u64 = 256 * 1024;
pub(crate) const CATALOGUE_LIMIT: u64 = 1024 * 1024;

/// A complete selection, pinned to a concrete published directory.
///
/// Once opened, a set never follows `current` again: swapping the link
/// changes what the next reader selects, not what this one holds.
#[derive(Debug, Clone)]
pub struct AssetSet {
    assets_root: PathBuf,
    root: PathBuf,
    manifest: Manifest,
    manifest_v2: Option<ManifestV2>,
    icon_meta: crate::manifest::IconMeta,
    icons: BTreeMap<String, char>,
}

impl AssetSet {
    /// Follow a root's activation link exactly once. `None` when the root
    /// has no `current`; an error when it has one that is not a symlink to
    /// a valid `sets/<id>`, or when that set does not open.
    pub fn current(root: &Path) -> Result<Option<Self>> {
        let current = root.join(CURRENT_LINK);
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(io("inspect", &current)(err)),
        };
        if !metadata.file_type().is_symlink() {
            return Err(invalid(format!(
                "{} must be a symlink to sets/<id>",
                current.display()
            )));
        }
        let target = fs::read_link(&current).map_err(io("read link", &current))?;
        let id = target
            .to_str()
            .and_then(|text| text.strip_prefix("sets/"))
            .filter(|id| valid_set_id(id))
            .ok_or_else(|| {
                invalid(format!(
                    "{} must point at sets/<id>, not {}",
                    current.display(),
                    target.display()
                ))
            })?;
        Self::open(root, id).map(Some)
    }

    /// Open the published set `sets/<set_id>` under `assets_root`.
    ///
    /// Checks the layout, the manifest, every locked file's size and the
    /// stylesheet text, and loads the default icon catalogue. Payload
    /// hashes are left to [`verify`](Self::verify) so opening does no
    /// bulk I/O. A [`SCHEMA_V2`](crate::SCHEMA_V2) manifest is read as
    /// versioned icon metadata; the v1 fields and behaviour are shared.
    pub fn open(assets_root: &Path, set_id: &str) -> Result<Self> {
        if !valid_set_id(set_id) {
            return Err(invalid(format!("invalid asset set ID {set_id:?}")));
        }
        let assets_root = fs::canonicalize(assets_root).map_err(io("resolve", assets_root))?;
        let root = checked_directory(&assets_root, &format!("sets/{set_id}"))?;
        let manifest_path = checked_file(&root, MANIFEST_FILE)?;
        let text = read_bounded(&manifest_path, MANIFEST_LIMIT)?;
        let parsed = ParsedManifest::parse(&text, &manifest_path)?;
        parsed.validate(set_id)?;
        let manifest_v2 = parsed.v2().cloned();
        let manifest = parsed.v1();
        for file in &manifest.files {
            let path = checked_file(&root, &file.path)?;
            check_size(&path, &file.path, file.bytes)?;
        }
        let css = read_bounded(&checked_file(&root, STYLESHEET_FILE)?, MANIFEST_LIMIT)?;
        if css != manifest.web_css {
            return Err(Error::Mismatch(format!(
                "{STYLESHEET_FILE} differs from the locked web_css in {set_id}"
            )));
        }
        let icons = icons_for_open(&parsed, &root, set_id)?;
        let icon_meta = parsed.icon_meta()?;
        Ok(Self {
            assets_root,
            root,
            manifest,
            manifest_v2,
            icon_meta,
            icons,
        })
    }

    /// The set's ID (its `sets/<id>` directory name).
    pub fn set_id(&self) -> &str {
        &self.manifest.set_id
    }

    /// The published directory, `<assets_root>/sets/<id>`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The canonical root the set was opened under.
    pub fn assets_root(&self) -> &Path {
        &self.assets_root
    }

    /// The locked manifest. For a v2 set this is the read-only v1
    /// projection of the shared fields — its `schema` stays
    /// [`SCHEMA_V2`](crate::SCHEMA_V2), so it must not be re-validated or
    /// re-serialized as a v1 manifest; the versioned metadata is read
    /// through [`manifest_v2`](Self::manifest_v2) and the `icon_*`
    /// accessors.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The versioned icon-metadata manifest, when this set declares one
    /// ([`SCHEMA_V2`](crate::SCHEMA_V2)); `None` for a v1 set. The parsed
    /// DTO is retained whole, so the declared `icon_default`,
    /// `icon_catalogues` and `icon_assets` are readable here as declared,
    /// beside the resolved views of the `icon_*` accessors.
    pub fn manifest_v2(&self) -> Option<&ManifestV2> {
        self.manifest_v2.as_ref()
    }

    /// The family name a role's font declares, when the manifest records it.
    pub fn family(&self, role: &str) -> Option<&str> {
        self.manifest.font_families.get(role).map(String::as_str)
    }

    /// The font file for a role.
    pub fn font_path(&self, role: &str) -> Option<PathBuf> {
        self.manifest
            .fonts
            .get(role)
            .map(|path| self.root.join(path))
    }

    /// Every role's font file, in role order.
    pub fn font_paths(&self) -> Vec<PathBuf> {
        self.manifest
            .fonts
            .values()
            .map(|path| self.root.join(path))
            .collect()
    }

    /// The roles the set provides, sorted.
    pub fn roles(&self) -> impl Iterator<Item = &str> {
        self.manifest.fonts.keys().map(String::as_str)
    }

    /// The character for a named icon in the set's default icon catalogue
    /// (for a v1 set, the `icons` font's `.codepoints` sibling).
    pub fn icon(&self, name: &str) -> Option<char> {
        self.icons.get(name).copied()
    }

    /// The default icon catalogue: name → character.
    pub fn icons(&self) -> &BTreeMap<String, char> {
        &self.icons
    }

    /// The manifest-declared default icon selection: the family, style and
    /// exact weight an omitted icon request uses. For a v1 set this is
    /// derived from the `icons` role (style
    /// [`DEFAULT_ICON_STYLE`](crate::DEFAULT_ICON_STYLE), weight 400); for
    /// a v2 set it is the declared `icon_default`. `None` when the set has
    /// no icon catalogue.
    pub fn icon_default(&self) -> Option<&IconDefault> {
        self.icon_meta.default.as_ref()
    }

    /// Every declared icon catalogue: family, style, locked font path and
    /// face index, and the locked codepoints path. A v1 set derives its one
    /// default catalogue from the `icons` role.
    pub fn icon_catalogues(&self) -> &[IconCatalogue] {
        &self.icon_meta.catalogues
    }

    /// Every declared non-font icon asset (SVG or raster). Always empty
    /// for a v1 set.
    pub fn icon_assets(&self) -> &[IconAsset] {
        &self.icon_meta.assets
    }

    /// The catalogue an explicit `(family, style)` request selects:
    /// `Ok(None)` when the set does not declare the pair; an error when it
    /// cannot express the request at all — a v1 set asked for a nondefault
    /// style, whose per-style metadata it does not record. The declared
    /// default ([`icon_default`](Self::icon_default)) answers an omitted
    /// request instead.
    pub fn icon_catalogue(&self, family: &str, style: &str) -> Result<Option<&IconCatalogue>> {
        self.icon_meta.select(family, style)
    }

    /// Resolve a locked file, the manifest or the stylesheet to its path,
    /// re-checking the layout on the way. `None` for a path the manifest
    /// does not lock; an error for one that is not a valid relative path
    /// or that has stopped being a regular file.
    pub fn file_path(&self, relative: &str) -> Result<Option<PathBuf>> {
        if !valid_relative_path(relative) {
            return Err(invalid(format!("invalid asset path {relative:?}")));
        }
        let entry = self
            .manifest
            .files
            .iter()
            .find(|file| file.path == relative);
        if entry.is_none() && relative != MANIFEST_FILE && relative != STYLESHEET_FILE {
            return Ok(None);
        }
        checked_directory(&self.assets_root, &format!("sets/{}", self.set_id()))?;
        let path = checked_file(&self.root, relative)?;
        if let Some(file) = entry {
            check_size(&path, relative, file.bytes)?;
        }
        Ok(Some(path))
    }

    /// Check every locked file's size, SHA-256 and BLAKE3, and the
    /// stylesheet text, streaming with bounded memory.
    pub fn verify(&self) -> Result<()> {
        let css = self
            .file_path(STYLESHEET_FILE)?
            .ok_or_else(|| invalid("asset stylesheet unavailable"))?;
        if read_bounded(&css, MANIFEST_LIMIT)? != self.manifest.web_css {
            return Err(Error::Mismatch(format!(
                "{STYLESHEET_FILE} differs from the locked web_css in {}",
                self.set_id()
            )));
        }
        for entry in &self.manifest.files {
            let path = self
                .file_path(&entry.path)?
                .ok_or_else(|| invalid(format!("locked asset {:?} unavailable", entry.path)))?;
            let mut file = File::open(&path).map_err(io("open", &path))?;
            let mut sha = sha2::Sha256::new();
            let mut b3 = blake3::Hasher::new();
            let mut buffer = [0u8; 64 * 1024];
            let mut bytes = 0u64;
            loop {
                let length = file.read(&mut buffer).map_err(io("read", &path))?;
                if length == 0 {
                    break;
                }
                bytes += length as u64;
                if bytes > entry.bytes {
                    return Err(Error::Mismatch(format!(
                        "asset grew during verification: {}",
                        entry.path
                    )));
                }
                sha.update(&buffer[..length]);
                b3.update(&buffer[..length]);
            }
            if bytes != entry.bytes {
                return Err(Error::Mismatch(format!(
                    "asset size mismatch: {}",
                    entry.path
                )));
            }
            if hex::encode(sha.finalize()) != entry.sha256 {
                return Err(Error::Mismatch(format!(
                    "asset SHA-256 mismatch: {}",
                    entry.path
                )));
            }
            if b3.finalize().to_hex().as_str() != entry.blake3 {
                return Err(Error::Mismatch(format!(
                    "asset BLAKE3 mismatch: {}",
                    entry.path
                )));
            }
        }
        Ok(())
    }

    /// Read, check and capture every byte of this set (the `verified`
    /// feature): the set directory is opened through a held descriptor,
    /// the manifest is re-read and re-validated rather than trusted from
    /// this handle, and every locked file is descriptor-opened and read
    /// exactly once, its exact length and both digests checked against the
    /// same owned bytes that are retained.
    ///
    /// The returned [`VerifiedSet`](crate::VerifiedSet) owns its bytes, so
    /// fonts come from `font("sans").bytes()` and nothing is reopened
    /// later: replacing or removing `sets/<id>` afterwards changes what
    /// the next reader sees, never what this one holds. `limits` bounds
    /// how much is staged in memory at once, under hard caps.
    #[cfg(feature = "verified")]
    pub fn read_verified(
        &self,
        limits: crate::verified::ReadLimits,
    ) -> Result<crate::verified::VerifiedSet> {
        crate::verified::read(self.assets_root(), self.set_id(), limits)
    }
}

/// The default icon catalogue, parsed at open: for a v1 set the `icons`
/// role's locked `.codepoints` sibling (the legacy rule), for a v2 set the
/// declared default catalogue's locked codepoints file.
fn icons_for_open(
    parsed: &ParsedManifest,
    root: &Path,
    set_id: &str,
) -> Result<BTreeMap<String, char>> {
    match parsed {
        ParsedManifest::V1(manifest) => match manifest.fonts.get("icons") {
            Some(font) => {
                let catalogue = Path::new(font).with_extension("codepoints");
                let catalogue = catalogue
                    .to_str()
                    .ok_or_else(|| invalid("invalid icon catalogue path"))?;
                if !manifest.files.iter().any(|file| file.path == catalogue) {
                    return Err(invalid(format!(
                        "icon catalogue {catalogue:?} is not locked"
                    )));
                }
                parse_codepoints(&read_bounded(
                    &checked_file(root, catalogue)?,
                    CATALOGUE_LIMIT,
                )?)
            }
            None => Ok(BTreeMap::new()),
        },
        ParsedManifest::V2(manifest) => {
            let default = &manifest.icon_default;
            // `validate` guarantees the default names a declared
            // catalogue; this is a defensive re-check.
            let catalogue = manifest
                .icon_catalogues
                .iter()
                .find(|entry| entry.family == default.family && entry.style == default.style)
                .ok_or_else(|| {
                    invalid(format!(
                        "icon_default names no declared icon catalogue in {set_id}"
                    ))
                })?;
            parse_codepoints(&read_bounded(
                &checked_file(root, &catalogue.codepoints)?,
                CATALOGUE_LIMIT,
            )?)
        }
    }
}

fn check_size(path: &Path, relative: &str, expected: u64) -> Result<()> {
    let actual = fs::metadata(path).map_err(io("inspect", path))?.len();
    if actual != expected {
        return Err(Error::Mismatch(format!(
            "asset size mismatch: {relative} is {actual} bytes, locked {expected}"
        )));
    }
    Ok(())
}

/// `root/relative`, every component an existing directory that is not a
/// symlink.
fn checked_directory(root: &Path, relative: &str) -> Result<PathBuf> {
    if !valid_relative_path(relative) {
        return Err(invalid(format!(
            "invalid asset directory path {relative:?}"
        )));
    }
    let mut path = root.to_path_buf();
    for part in relative.split('/') {
        path.push(part);
        let metadata = fs::symlink_metadata(&path).map_err(io("inspect", &path))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(invalid(format!(
                "{} must be a directory and not a symlink",
                path.display()
            )));
        }
    }
    Ok(path)
}

/// `root/relative`, an existing regular file (not a symlink) inside
/// checked directories under a root that is itself a plain directory.
fn checked_file(root: &Path, relative: &str) -> Result<PathBuf> {
    if !valid_relative_path(relative) {
        return Err(invalid(format!("invalid asset path {relative:?}")));
    }
    let relative_path = Path::new(relative);
    let parent = relative_path
        .parent()
        .ok_or_else(|| invalid(format!("asset {relative:?} has no parent")))?;
    let directory = if parent.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        let parent = parent
            .to_str()
            .ok_or_else(|| invalid(format!("non-UTF-8 parent for asset {relative:?}")))?;
        checked_directory(root, parent)?
    };
    // Re-check the pinned set itself, including after an administrative
    // update that replaced it.
    let root_metadata = fs::symlink_metadata(root).map_err(io("inspect", root))?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err(invalid(format!(
            "asset set {} must be a directory and not a symlink",
            root.display()
        )));
    }
    let name = relative_path
        .file_name()
        .ok_or_else(|| invalid(format!("asset {relative:?} has no file name")))?;
    let path = directory.join(name);
    let metadata = fs::symlink_metadata(&path).map_err(io("inspect", &path))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(invalid(format!(
            "asset must be a regular file and not a symlink: {relative}"
        )));
    }
    Ok(path)
}

/// Read a text file of at most `limit` bytes.
fn read_bounded(path: &Path, limit: u64) -> Result<String> {
    let too_large = || Error::Invalid(format!("{} exceeds {limit} bytes", path.display()));
    if fs::metadata(path).map_err(io("inspect", path))?.len() > limit {
        return Err(too_large());
    }
    let mut text = String::new();
    File::open(path)
        .map_err(io("open", path))?
        .take(limit + 1)
        .read_to_string(&mut text)
        .map_err(|source| {
            // Invalid UTF-8 is content, not I/O: the same defect the
            // verified path classifies as `Error::Invalid`. Ordinary I/O
            // failures keep their kind.
            if source.kind() == ErrorKind::InvalidData {
                invalid(format!("{} is not UTF-8 text", path.display()))
            } else {
                io("read", path)(source)
            }
        })?;
    if text.len() as u64 > limit {
        return Err(too_large());
    }
    Ok(text)
}
