// SPDX-License-Identifier: MIT OR Apache-2.0

//! The locked manifest: what a published set contains and the rules every
//! manifest must satisfy before a set is opened.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::error::{Result, invalid};

/// The one manifest schema this crate accepts.
pub const SCHEMA: &str = "mixos.static-assets.v1";

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

/// The rules a parsed manifest must satisfy before its set is opened.
pub(crate) fn validate(manifest: &Manifest, id: &str) -> Result<()> {
    if manifest.schema != SCHEMA {
        return Err(invalid(format!(
            "unsupported asset manifest schema {:?} (expected {SCHEMA:?})",
            manifest.schema
        )));
    }
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
        if !manifest.fonts.contains_key(role)
            || family.is_empty()
            || family.len() > 128
            || family.chars().any(char::is_control)
        {
            return Err(invalid(format!("invalid font family metadata for role {role:?}")));
        }
    }
    Ok(())
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
}
