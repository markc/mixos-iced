use crate::storage::slot::base::Storage;
use std::any::Any;

/// The result of checking a slot against its last-persisted value. Persistence is
/// driven at the `buffer()` mutation boundary, and change is detected by `PartialEq`
/// on the persisted form (not byte comparison — that gives false diffs from e.g.
/// `HashMap` key ordering).
pub enum SnapshotOutcome {
    /// Slot absent from this storage — skip it for this world.
    Absent,
    /// `PartialEq`-equal to the cached last value — nothing to write.
    Unchanged,
    /// Differs: the serialized DATA bytes to write, plus the new value to cache
    /// (promoted to the live cache once the write is confirmed).
    Changed { bytes: Vec<u8>, cache: Box<dyn Any + Send> },
}

/// One persistable storage slot, type-erased. Built by the `persist_version!` macro
/// (see `persist.trait`) and returned from `System::persist()`. The fn pointers
/// are monomorphized in the owning crate and close over the slot's read/write
/// tokens, so this struct carries no generics and is object-safe to list
/// heterogeneously.
pub struct PersistEntry {
    /// Stable on-disk identity; the file is `<key>.json`. Unique per process.
    pub key: &'static str,
    /// The current schema version, written into the saved envelope.
    pub version: u32,
    /// Compute the persisted form of the live slot and compare (by `PartialEq`)
    /// against the cached last value, returning whether it changed + the bytes.
    pub snapshot: fn(&Storage, Option<&(dyn Any + Send)>) -> SnapshotOutcome,
    /// Reconstruct the live slot from DATA bytes saved at `from_version`,
    /// migrating to the current version as needed, and write it through the
    /// slot's write token. Only called where storage is mutable.
    pub rehydrate: fn(&mut Storage, &[u8], u32) -> Result<(), PersistError>,
}

/// What went wrong loading a persisted slot. Never fatal to the compositor —
/// callers keep the in-memory default and quarantine the bad file.
#[derive(Debug)]
pub struct PersistError {
    pub kind: PersistErrorKind,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistErrorKind {
    /// The bytes aren't valid JSON / don't match the persisted shape.
    Parse,
    /// A migration step failed.
    Migrate,
    /// No migration path from the on-disk version to the current one.
    UnknownVersion,
    /// Filesystem error reading/writing the file.
    Io,
}

impl PersistError {
    pub fn parse(detail: impl Into<String>) -> Self {
        Self { kind: PersistErrorKind::Parse, detail: detail.into() }
    }
    pub fn migrate(detail: impl Into<String>) -> Self {
        Self { kind: PersistErrorKind::Migrate, detail: detail.into() }
    }
    pub fn unknown_version(key: &str, from: u32, to: u32) -> Self {
        Self {
            kind: PersistErrorKind::UnknownVersion,
            detail: format!("{key}: no migration from v{from} to v{to}"),
        }
    }
    pub fn io(detail: impl Into<String>) -> Self {
        Self { kind: PersistErrorKind::Io, detail: detail.into() }
    }
}

impl std::fmt::Display for PersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.detail)
    }
}

impl std::error::Error for PersistError {}
