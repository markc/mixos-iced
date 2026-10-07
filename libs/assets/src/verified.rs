// SPDX-License-Identifier: MIT OR Apache-2.0

//! Immutable verified asset byte reads (the `verified` feature).
//!
//! [`AssetSet::read_verified`] re-reads the manifest from the set and
//! captures every locked file through descriptor-relative opens, checking
//! each one's length and both digests against the same owned bytes that
//! are retained. The result is a [`VerifiedSet`]: the parsed manifest, the
//! exact manifest bytes (whose BLAKE3 is the identity's digest), the
//! stylesheet and every file's bytes, and the parsed icon catalogue.
//! Nothing is reopened later.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::Digest;

use crate::error::{Error, Result, invalid, io};
use crate::manifest::{
    AssetFile, MANIFEST_FILE, Manifest, STYLESHEET_FILE, parse_codepoints, valid_set_id, validate,
};
use crate::set::{CATALOGUE_LIMIT, MANIFEST_LIMIT};

/// Bounds on how many source bytes a verified read may capture at once.
///
/// These are read-time policy, not properties of a set: a set whose locked
/// files exceed a bound is refused at read time with [`Error::Invalid`].
/// The total counts captured bytes — the manifest, the stylesheet and
/// every locked file — not the process heap: the parsed manifest, the
/// icon table and the one-time bounded read and `Arc` conversion scratch
/// are outside these bounds. Every field is itself capped by a hard limit
/// ([`Self::MAX_MANIFEST_BYTES`], [`Self::MAX_FILE_BYTES`],
/// [`Self::MAX_TOTAL_BYTES`]); a larger request is refused outright, so
/// no caller can ask the reader to capture unbounded bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadLimits {
    /// The largest manifest (or stylesheet) captured, in bytes.
    pub max_manifest_bytes: u64,
    /// The largest single locked file captured, in bytes.
    pub max_file_bytes: u64,
    /// The largest sum of captured bytes: the manifest, the stylesheet
    /// and every locked file.
    pub max_total_bytes: u64,
}

impl ReadLimits {
    /// Hard cap on a staged manifest: the same bound `open` applies.
    pub const MAX_MANIFEST_BYTES: u64 = MANIFEST_LIMIT;
    /// Hard cap on one staged file: the largest a manifest may lock.
    pub const MAX_FILE_BYTES: u64 = crate::manifest::MAX_FILE_BYTES;
    /// Hard cap on all captured bytes at once. Deliberately not
    /// `MAX_FILES × MAX_FILE_BYTES`.
    pub const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

    /// The default bounds: 256 KiB for the manifest, 64 MiB per file,
    /// 128 MiB in all.
    pub const fn new() -> Self {
        Self {
            max_manifest_bytes: Self::MAX_MANIFEST_BYTES,
            max_file_bytes: 64 * 1024 * 1024,
            max_total_bytes: 128 * 1024 * 1024,
        }
    }

    fn checked(self) -> Result<Self> {
        let bound = |name: &str, value: u64, hard: u64| {
            if value == 0 {
                return Err(invalid(format!("{name} read limit must be positive")));
            }
            if value > hard {
                return Err(invalid(format!(
                    "{name} read limit of {value} bytes exceeds the hard cap of {hard} bytes"
                )));
            }
            Ok(())
        };
        bound(
            "manifest",
            self.max_manifest_bytes,
            Self::MAX_MANIFEST_BYTES,
        )?;
        bound("file", self.max_file_bytes, Self::MAX_FILE_BYTES)?;
        bound("total", self.max_total_bytes, Self::MAX_TOTAL_BYTES)?;
        Ok(self)
    }
}

impl Default for ReadLimits {
    fn default() -> Self {
        Self::new()
    }
}

/// The identity of a verified set: its `sets/<id>` directory name and the
/// BLAKE3 of the exact manifest bytes the set was verified from.
///
/// The digest describes bytes, not a path: two directories that share an
/// ID but hold different manifests are different identities. There is no
/// constructor; an identity only exists as the result of a verified read.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SetIdentity {
    set_id: String,
    manifest_blake3: [u8; 32],
}

impl SetIdentity {
    /// The set's ID (its `sets/<id>` directory name).
    pub fn set_id(&self) -> &str {
        &self.set_id
    }

    /// The BLAKE3 of the exact manifest bytes the set was verified from.
    pub fn manifest_blake3(&self) -> [u8; 32] {
        self.manifest_blake3
    }

    /// `manifest_blake3` as 64 lower-case hex characters.
    pub fn manifest_blake3_hex(&self) -> String {
        hex::encode(self.manifest_blake3)
    }
}

impl fmt::Debug for SetIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SetIdentity")
            .field("set_id", &self.set_id)
            .field("manifest_blake3", &hex::encode(self.manifest_blake3))
            .finish()
    }
}

/// One locked file, captured and verified: the declared path and digests
/// together with the owned bytes both digests were computed from.
///
/// There is no constructor: a `VerifiedFile` only exists after its bytes
/// passed the length and both digest checks, so [`bytes`](Self::bytes) is
/// exactly what the digests describe. The bytes stay usable after the
/// set's paths are replaced or removed — nothing reads them from disk
/// again.
#[derive(Clone, Debug)]
pub struct VerifiedFile {
    path: String,
    bytes: Arc<[u8]>,
    sha256: String,
    blake3: String,
}

impl VerifiedFile {
    /// The locked relative path inside the set.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The owned, verified bytes. Checked against
    /// [`sha256`](Self::sha256) and [`blake3`](Self::blake3) at read time.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The SHA-256 of [`bytes`](Self::bytes), lower-case hex.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// The BLAKE3 of [`bytes`](Self::bytes), lower-case hex.
    pub fn blake3(&self) -> &str {
        &self.blake3
    }
}

/// A set whose every byte was read once through a held directory
/// descriptor, checked against the locked manifest and retained: the
/// re-parsed manifest, the manifest bytes themselves (whose BLAKE3 is the
/// identity's digest), the captured stylesheet and every locked file's
/// owned bytes, and the parsed icon catalogue.
///
/// Nothing is reopened later: font bytes come from
/// [`font`](Self::font).`bytes()`, never from the path. Replacing or
/// removing `sets/<id>` afterwards changes what the next reader sees,
/// never what this one holds. The opened set directory descriptor is
/// retained for the life of the set.
pub struct VerifiedSet {
    identity: SetIdentity,
    manifest: Manifest,
    manifest_bytes: Arc<[u8]>,
    stylesheet: String,
    files: Vec<VerifiedFile>,
    icons: BTreeMap<String, char>,
    /// Retained for the lifetime of the set: holding the descriptor pins
    /// the set directory inode the bytes were read from. Never read after
    /// construction.
    _directory: File,
}

impl VerifiedSet {
    /// Read, check and capture a set through a caller-held directory
    /// descriptor. The manifest is re-parsed and re-validated rather than
    /// trusted from an earlier open: the identity and every byte come from
    /// what the descriptor reads, not from a path. A caller that wants a
    /// path-based entry opens the set directory with
    /// `config::atomic::open_directory` (or uses
    /// [`AssetSet::read_verified`](crate::AssetSet::read_verified), which
    /// does).
    pub fn read_in(directory: File, set_id: &str, limits: ReadLimits) -> Result<Self> {
        let display = PathBuf::from(format!("sets/{set_id}"));
        if !directory
            .metadata()
            .map_err(io("inspect", &display))?
            .is_dir()
        {
            return Err(invalid(format!(
                "{} must be a directory",
                display.display()
            )));
        }
        read_in_at(directory, set_id, limits, &display)
    }

    /// The set's identity: ID and manifest digest.
    pub fn identity(&self) -> &SetIdentity {
        &self.identity
    }

    /// The set's ID (its `sets/<id>` directory name).
    pub fn set_id(&self) -> &str {
        self.identity.set_id()
    }

    /// The re-parsed, re-validated manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The exact manifest bytes the identity's digest covers.
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }

    /// The captured stylesheet, byte-identical to the locked `web_css`.
    pub fn stylesheet(&self) -> &str {
        &self.stylesheet
    }

    /// One captured file by its locked relative path.
    pub fn file(&self, path: &str) -> Option<&VerifiedFile> {
        self.files.iter().find(|file| file.path == path)
    }

    /// Every captured file, in manifest order.
    pub fn files(&self) -> impl Iterator<Item = &VerifiedFile> {
        self.files.iter()
    }

    /// The captured font file for a role; `None` for an unknown role.
    pub fn font(&self, role: &str) -> Option<&VerifiedFile> {
        self.manifest
            .fonts
            .get(role)
            .and_then(|path| self.file(path))
    }

    /// The character for a named icon in the `icons` font's catalogue.
    pub fn icon(&self, name: &str) -> Option<char> {
        self.icons.get(name).copied()
    }

    /// The icon catalogue: name → character.
    pub fn icons(&self) -> &BTreeMap<String, char> {
        &self.icons
    }
}

impl fmt::Debug for VerifiedSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerifiedSet")
            .field("identity", &self.identity)
            .field("files", &self.files.len())
            .field("icons", &self.icons.len())
            .finish()
    }
}

/// [`AssetSet::read_verified`](crate::AssetSet::read_verified)'s core: open
/// the set directory through the safe absolute-path walk and capture it.
pub(crate) fn read(assets_root: &Path, set_id: &str, limits: ReadLimits) -> Result<VerifiedSet> {
    if !valid_set_id(set_id) {
        return Err(invalid(format!("invalid asset set ID {set_id:?}")));
    }
    let display = assets_root.join("sets").join(set_id);
    let directory = config::atomic::open_directory(&display).map_err(io("open", &display))?;
    read_in_at(directory, set_id, limits, &display)
}

fn read_in_at(
    directory: File,
    set_id: &str,
    limits: ReadLimits,
    display: &Path,
) -> Result<VerifiedSet> {
    let limits = limits.checked()?;
    if !valid_set_id(set_id) {
        return Err(invalid(format!("invalid asset set ID {set_id:?}")));
    }
    let mut staged = 0u64;
    // The manifest: opened and read exactly once, through the descriptor.
    // Its cap is the manifest bound or the total allowance left unstaged,
    // whichever is smaller, so the total limit covers these reads too.
    let manifest_path = display.join(MANIFEST_FILE);
    let manifest_bytes = read_limited(
        &directory,
        MANIFEST_FILE,
        limits.max_manifest_bytes,
        limits.max_total_bytes,
        &mut staged,
        &manifest_path,
    )?;
    let text = std::str::from_utf8(&manifest_bytes).map_err(|_| {
        invalid(format!(
            "asset manifest {} is not UTF-8 text",
            manifest_path.display()
        ))
    })?;
    let manifest: Manifest = strict::from_str(text).map_err(|source| Error::Manifest {
        path: manifest_path,
        source,
    })?;
    validate(&manifest, set_id)?;

    let stylesheet_path = display.join(STYLESHEET_FILE);
    let css = read_limited(
        &directory,
        STYLESHEET_FILE,
        limits.max_manifest_bytes,
        limits.max_total_bytes,
        &mut staged,
        &stylesheet_path,
    )?;
    if css.as_slice() != manifest.web_css.as_bytes() {
        return Err(Error::Mismatch(format!(
            "{STYLESHEET_FILE} differs from the locked web_css in {set_id}"
        )));
    }
    // Equality with the manifest's `web_css` string makes this UTF-8.
    let stylesheet =
        String::from_utf8(css).expect("stylesheet equals the manifest's web_css string");

    let identity = SetIdentity {
        set_id: manifest.set_id.clone(),
        manifest_blake3: blake3::hash(&manifest_bytes).into(),
    };

    let mut files = Vec::with_capacity(manifest.files.len());
    for entry in &manifest.files {
        files.push(read_file(&directory, set_id, entry, limits, &mut staged)?);
    }

    let icons = icons(&manifest, &files, set_id)?;

    Ok(VerifiedSet {
        identity,
        manifest,
        manifest_bytes: Arc::from(manifest_bytes.into_boxed_slice()),
        stylesheet,
        files,
        icons,
        _directory: directory,
    })
}

/// Open `relative` under the held directory and read it, exactly once,
/// charging the captured bytes to `staged`. The read is capped at
/// `per_read` bytes or at the allowance `total` leaves unstaged, whichever
/// is smaller — plus one bounded lookahead byte that only detects growth
/// past the cap (see [`read_fixed`]). A read refused because the total
/// allowance bound first is reported as a total fault, otherwise as a
/// per-read fault. The descriptor opens (a regular file, no symlink at
/// any level) are checked by `config::atomic::open_nested`.
fn read_limited(
    dir: &File,
    relative: &str,
    per_read: u64,
    total: u64,
    staged: &mut u64,
    display: &Path,
) -> Result<Vec<u8>> {
    let remaining = total.saturating_sub(*staged);
    let cap = per_read.min(remaining);
    let file =
        config::atomic::open_nested(dir, Path::new(relative)).map_err(io("open", display))?;
    let bytes = read_fixed(file, cap, display)?;
    if bytes.len() as u64 > cap {
        if remaining < per_read {
            return Err(invalid(format!(
                "{} exceeds the {remaining} bytes the total read limit leaves unstaged",
                display.display()
            )));
        }
        return Err(invalid(format!(
            "{} exceeds {cap} bytes",
            display.display()
        )));
    }
    *staged += bytes.len() as u64;
    Ok(bytes)
}

/// The one bounded read both paths use: `bound` bytes of allowance plus a
/// single lookahead byte. The buffer is allocated exactly once at
/// `bound + 1` bytes — a bounded one-time scratch: zeroed, filled, then
/// truncated to what was actually read — by plain `read` calls into the
/// remaining slice. It is never resized: no `read_to_end`, no reserve, no
/// extend, so nothing can grow the allocation past `bound + 1`. A file
/// longer than `bound` yields `bound + 1` bytes, which callers detect as
/// growth; `Interrupted` retries the same call.
fn read_fixed(mut file: File, bound: u64, display: &Path) -> Result<Vec<u8>> {
    let capacity = bound
        .checked_add(1)
        .ok_or_else(|| invalid("invalid byte bound"))?;
    let capacity = usize::try_from(capacity).map_err(|_| invalid("invalid byte bound"))?;
    let mut bytes = vec![0u8; capacity];
    let mut filled = 0;
    while filled < capacity {
        match file.read(&mut bytes[filled..]) {
            Ok(0) => break,
            Ok(length) => filled += length,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(io("read", display)(err)),
        }
    }
    bytes.truncate(filled);
    Ok(bytes)
}

/// One locked file: descriptor-opened once, its metadata and exact length
/// checked, read once, both digests computed over the same owned bytes.
fn read_file(
    dir: &File,
    set_id: &str,
    entry: &AssetFile,
    limits: ReadLimits,
    staged: &mut u64,
) -> Result<VerifiedFile> {
    let display = PathBuf::from(format!("sets/{set_id}/{}", entry.path));
    if entry.bytes > limits.max_file_bytes {
        return Err(invalid(format!(
            "locked asset {:?} is {} bytes, beyond the per-file read limit of {} bytes",
            entry.path, entry.bytes, limits.max_file_bytes
        )));
    }
    if *staged + entry.bytes > limits.max_total_bytes {
        return Err(invalid(format!(
            "locked assets would stage more than the total read limit of {} bytes",
            limits.max_total_bytes
        )));
    }
    let file =
        config::atomic::open_nested(dir, Path::new(&entry.path)).map_err(io("open", &display))?;
    let actual = file.metadata().map_err(io("inspect", &display))?.len();
    if actual != entry.bytes {
        return Err(Error::Mismatch(format!(
            "asset size mismatch: {} is {actual} bytes, locked {}",
            entry.path, entry.bytes
        )));
    }
    // The locked size bounds the read ([`read_fixed`] adds one lookahead
    // byte): a file that grew while being read is caught by the length
    // check below.
    let bytes = read_fixed(file, entry.bytes, &display)?;
    if bytes.len() as u64 != entry.bytes {
        return Err(Error::Mismatch(format!(
            "asset size mismatch: {} changed while being read",
            entry.path
        )));
    }
    *staged += bytes.len() as u64;
    let sha256 = hex::encode(sha2::Sha256::digest(&bytes));
    if sha256 != entry.sha256 {
        return Err(Error::Mismatch(format!(
            "asset SHA-256 mismatch: {}",
            entry.path
        )));
    }
    let blake3 = blake3::hash(&bytes).to_hex().to_string();
    if blake3 != entry.blake3 {
        return Err(Error::Mismatch(format!(
            "asset BLAKE3 mismatch: {}",
            entry.path
        )));
    }
    Ok(VerifiedFile {
        path: entry.path.clone(),
        bytes: Arc::from(bytes.into_boxed_slice()),
        sha256,
        blake3,
    })
}

/// The icon table, parsed from the verified catalogue bytes, under the same
/// rules as `open`: the `icons` role's font with a `.codepoints` sibling
/// that must itself be locked.
fn icons(
    manifest: &Manifest,
    files: &[VerifiedFile],
    set_id: &str,
) -> Result<BTreeMap<String, char>> {
    let Some(font) = manifest.fonts.get("icons") else {
        return Ok(BTreeMap::new());
    };
    let catalogue = Path::new(font).with_extension("codepoints");
    let catalogue = catalogue
        .to_str()
        .ok_or_else(|| invalid("invalid icon catalogue path"))?;
    let file = files
        .iter()
        .find(|file| file.path == catalogue)
        .ok_or_else(|| invalid(format!("icon catalogue {catalogue:?} is not locked")))?;
    if file.bytes.len() as u64 > CATALOGUE_LIMIT {
        return Err(invalid(format!(
            "icon catalogue {catalogue:?} in {set_id} exceeds {CATALOGUE_LIMIT} bytes"
        )));
    }
    let text = std::str::from_utf8(file.bytes())
        .map_err(|_| invalid(format!("icon catalogue {catalogue:?} is not UTF-8 text")))?;
    parse_codepoints(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_refuse_zero_and_oversized_requests() {
        assert!(ReadLimits::default().checked().is_ok());
        assert!(
            ReadLimits {
                max_manifest_bytes: 0,
                ..ReadLimits::default()
            }
            .checked()
            .is_err()
        );
        assert!(
            ReadLimits {
                max_file_bytes: ReadLimits::MAX_FILE_BYTES + 1,
                ..ReadLimits::default()
            }
            .checked()
            .is_err()
        );
        assert!(
            ReadLimits {
                max_total_bytes: ReadLimits::MAX_TOTAL_BYTES + 1,
                ..ReadLimits::default()
            }
            .checked()
            .is_err()
        );
    }
}
