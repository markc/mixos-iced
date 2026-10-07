// SPDX-License-Identifier: MIT OR Apache-2.0
//! Optional bounded local presentation cache. Never authority persistence.
//! All I/O belongs on the host's one serial cache worker, not its UI loop.
use crate::{
    consumer::Consumer,
    fallback::{Candidate, validate_inline},
    model::RESOURCE_CACHE_SCHEMA,
    *,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    os::fd::{AsRawFd, FromRawFd},
    path::Path,
    sync::Arc,
};

pub const MAX_CACHE_BYTES: usize = 1024 * 1024;
const CACHE_SCHEMA: u32 = 1;

#[derive(Deserialize)]
struct Header {
    schema: u32,
    interpretation: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    schema: u32,
    interpretation: String,
    context: String,
    shell: bool,
    digest: String,
    snapshot: Snapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    binding: Option<ResourceBinding>,
}

/// Exact interpretation of the one recognised predecessor: settings 0.3.4,
/// cache schema 1, same formula. Legacy envelopes are accepted only under
/// this named interpretation; arbitrary prior package interpretations are
/// refused, and new captures never use it for a resource binding.
fn legacy_interpretation() -> String {
    source_digest(&format!(
        "settings-cache-{CACHE_SCHEMA}-{}-{}",
        "0.3.4",
        source_digest(EMBEDDED_DEFAULT_SOURCE)
    ))
}

/// Versioned resource-selection semantics of schema-2 envelopes. Delegates to
/// the always-compiled `settings::resource_interpretation` so hosts can build
/// bindings without enabling this optional feature; no duplicate hash.
pub fn resource_interpretation() -> String {
    crate::resource_interpretation()
}

/// Domain-separated schema-2 digest covering the unchanged canonical snapshot
/// AND the resource binding, so a legacy snapshot digest can never replay
/// into a resource-aware envelope.
fn resource_digest(
    binding: &ResourceBinding,
    snapshot: &Snapshot,
) -> Result<String, serde_json::Error> {
    crate::digest(&("settings-cache-resource", binding, snapshot))
}

/// A recorded binding must equal the snapshot's authored reference verbatim:
/// an explicit reference matches set/manifest/icons exactly (including an
/// omitted icon selector), and omission must not record an invented selector.
/// Shared by the load and write paths so no envelope the loader rejects is
/// ever persisted.
fn check_binding_matches_snapshot(
    binding: &ResourceBinding,
    snapshot: &Snapshot,
) -> Result<(), Diagnostic> {
    match &snapshot.desktop.appearance.resources {
        // The resolved descriptor default is appearance evidence, never cache
        // binding data.
        Some(reference) => {
            if binding.set_id != reference.set_id
                || binding.manifest_blake3 != reference.manifest_blake3
                || binding.icons != reference.icons
            {
                return Err(fault(
                    "cache_binding_mismatch",
                    "Cache resource binding differs from the authored reference",
                ));
            }
        }
        // Omission records only the host's pinned exact set identity
        // (default pin). An icon selector must not be invented here.
        None => {
            if binding.icons.is_some() {
                return Err(fault(
                    "cache_binding_mismatch",
                    "Omitted resources must not record an icon selector",
                ));
            }
        }
    }
    Ok(())
}
fn fault(code: &str, error: impl std::fmt::Display) -> Diagnostic {
    Diagnostic::new(code, "cache", error.to_string())
}
fn name(binding: &Binding, context: &str, shell: bool) -> String {
    format!(
        "{}.json",
        crate::digest(&(binding, context, shell)).expect("string data encodes")
    )
}

/// Captured only from applied authority/retained data, never readback, embedded
/// or cached fallback. The immutable snapshot survives asynchronous I/O.
#[derive(Clone, Debug)]
pub struct Save {
    owner: u64,
    serial: u64,
    snapshot: Arc<Snapshot>,
    context: String,
    shell: bool,
    binding: Option<ResourceBinding>,
}
impl Save {
    /// Identity of this activated capture. Persistence callers can expose a
    /// successful write receipt without copying the projection or private fence.
    pub fn identity(&self) -> crate::consumer::SnapshotIdentity {
        crate::consumer::SnapshotIdentity::from(self.snapshot.as_ref())
    }
    /// Compare private producer and activation fences without copying data.
    /// A serial cannot change its resource binding.
    pub fn same_capture(&self, other: &Self) -> bool {
        self.owner == other.owner && self.serial == other.serial && self.binding == other.binding
    }
    /// The renderer-neutral resource binding captured with this activation.
    /// None means the presentation had no explicit reference and no pinned
    /// default identity was captured.
    pub fn binding(&self) -> Option<&ResourceBinding> {
        self.binding.as_ref()
    }
    pub(crate) fn capture_resources(
        owner: u64,
        serial: u64,
        snapshot: Arc<Snapshot>,
        context: String,
        shell: bool,
        binding: Option<ResourceBinding>,
    ) -> Self {
        Self {
            owner,
            serial,
            snapshot,
            context,
            shell,
            binding,
        }
    }
}

/// Immutable producer fence and persistent cache identity. The producer is
/// local to this process and never contributes to the cache filename.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    owner: u64,
    binding: Binding,
    context: String,
    shell: bool,
}
impl Target {
    pub(crate) fn capture(owner: u64, binding: Binding, context: String, shell: bool) -> Self {
        Self {
            owner,
            binding,
            context,
            shell,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    Written,
    Unchanged,
    Superseded,
}

/// The directory must already exist. No cache operation provisions user paths.
/// Each binding/context/capability has one stable lock inode and one serial
/// writer. Retire/drop the writer when replacing its producer consumer.
pub struct Writer {
    directory: File,
    _lock: File,
    lock_name: std::ffi::CString,
    name: String,
    binding: Binding,
    context: String,
    shell: bool,
    owner: Option<u64>,
    latest: Option<(u64, String, bool)>,
}
impl Writer {
    pub fn open(directory: &Path, consumer: &Consumer) -> Result<Self, Diagnostic> {
        Self::open_for(directory, &consumer.cache_target())
    }

    pub fn open_for(directory: &Path, target: &Target) -> Result<Self, Diagnostic> {
        let directory =
            config::atomic::open_directory(directory).map_err(|e| fault("cache_open_failed", e))?;
        let name = name(&target.binding, &target.context, target.shell);
        let lock_name = std::ffi::CString::new(format!("{name}.lock")).unwrap();
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                lock_name.as_ptr(),
                libc::O_RDWR
                    | libc::O_CREAT
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK
                    | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(fault("cache_lock_failed", std::io::Error::last_os_error()));
        }
        let lock = unsafe { File::from_raw_fd(fd) };
        if !lock
            .metadata()
            .map_err(|e| fault("cache_lock_failed", e))?
            .is_file()
        {
            return Err(fault("cache_lock_failed", "Lock is not a regular file"));
        }
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(fault("cache_lock_failed", std::io::Error::last_os_error()));
        }
        Ok(Self {
            directory,
            _lock: lock,
            lock_name,
            name,
            binding: target.binding.clone(),
            context: target.context.clone(),
            shell: target.shell,
            owner: Some(target.owner),
            latest: None,
        })
    }
    pub fn write(&mut self, save: &Save) -> Result<WriteOutcome, Diagnostic> {
        use std::os::unix::fs::MetadataExt;
        let held = self
            ._lock
            .metadata()
            .map_err(|e| fault("cache_lock_replaced", e))?;
        let mut current = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe {
            libc::fstatat(
                self.directory.as_raw_fd(),
                self.lock_name.as_ptr(),
                current.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(fault(
                "cache_lock_replaced",
                std::io::Error::last_os_error(),
            ));
        }
        let current = unsafe { current.assume_init() };
        if current.st_mode & libc::S_IFMT != libc::S_IFREG
            || current.st_dev != held.dev()
            || current.st_ino != held.ino()
        {
            return Err(fault(
                "cache_lock_replaced",
                "Held writer lock path changed",
            ));
        }
        if self.binding != save.snapshot.binding
            || self.context != save.context
            || self.shell != save.shell
            || self.owner.is_some_and(|owner| owner != save.owner)
        {
            return Err(fault(
                "wrong_cache_producer",
                "Retire writer before changing producer/binding/context",
            ));
        }
        self.owner = Some(save.owner);
        // A capture with a resource binding writes a schema-2 envelope whose
        // digest covers binding AND unchanged snapshot; without one it writes
        // the recognised legacy schema-1 predecessor bytes, which stay
        // readable by the previous release. No envelope this loader
        // deterministically refuses is ever persisted: the binding passes the
        // same authored-reference cross-check at write time that the loader
        // applies at read time, and a resource-bearing snapshot without a
        // binding is refused instead of writing a self-invalidating envelope.
        let (schema, interpretation, digest) = match save.binding.as_ref() {
            Some(binding) => {
                if binding.interpretation != resource_interpretation() {
                    return Err(fault(
                        "unsupported_cache",
                        "Resource binding interpretation differs",
                    ));
                }
                binding
                    .validate()
                    .map_err(|error| fault(&error.code, error.message))?;
                check_binding_matches_snapshot(binding, save.snapshot.as_ref())?;
                (
                    RESOURCE_CACHE_SCHEMA,
                    resource_interpretation(),
                    resource_digest(binding, save.snapshot.as_ref())
                        .map_err(|e| fault("invalid_cache_snapshot", e))?,
                )
            }
            None => {
                if save.snapshot.desktop.appearance.resources.is_some() {
                    return Err(fault(
                        "cache_binding_required",
                        "A resource-bearing snapshot requires a resource binding",
                    ));
                }
                (
                    CACHE_SCHEMA,
                    legacy_interpretation(),
                    crate::digest(save.snapshot.as_ref())
                        .map_err(|e| fault("invalid_cache_snapshot", e))?,
                )
            }
        };
        if let Some((serial, previous, committed)) = &self.latest {
            if save.serial < *serial {
                return Ok(WriteOutcome::Superseded);
            }
            if save.serial == *serial {
                if &digest != previous {
                    return Err(fault(
                        "cache_contradiction",
                        "Capture serial changed contents",
                    ));
                }
                if *committed {
                    return Ok(WriteOutcome::Unchanged);
                }
            }
        }
        // Advance before validation/I/O: an older completion cannot overwrite
        // after even a failed newer attempt. The same latest save may retry.
        self.latest = Some((save.serial, digest.clone(), false));
        validate_inline(&save.snapshot, &self.binding, &self.context)?;
        let envelope = Envelope {
            schema,
            interpretation,
            context: self.context.clone(),
            shell: self.shell,
            digest,
            snapshot: save.snapshot.as_ref().clone(),
            binding: save.binding.clone(),
        };
        let bytes =
            serde_json::to_vec(&envelope).map_err(|e| fault("invalid_cache_snapshot", e))?;
        if bytes.len() > MAX_CACHE_BYTES {
            return Err(fault("cache_too_large", "Cache envelope exceeds budget"));
        }
        config::atomic::replace_in(&self.directory, self.name.as_ref(), &bytes).map_err(|e| {
            fault(
                if e.may_have_replaced {
                    "cache_write_ambiguous"
                } else {
                    "cache_write_failed"
                },
                e,
            )
        })?;
        self.latest = Some((save.serial, envelope.digest, true));
        Ok(WriteOutcome::Written)
    }
}

/// Read-only bootstrap never creates a lock or any files. Missing/corrupt data
/// is an explicit diagnostic; callers continue to the embedded candidate.
pub fn load(directory: &Path, consumer: &Consumer) -> Result<Candidate, Diagnostic> {
    load_for(directory, &consumer.cache_target())
}

pub fn load_for(directory: &Path, target: &Target) -> Result<Candidate, Diagnostic> {
    let directory =
        config::atomic::open_directory(directory).map_err(|e| fault("cache_open_failed", e))?;
    let name = name(&target.binding, &target.context, target.shell);
    let bytes = config::atomic::read_in(&directory, name.as_ref(), MAX_CACHE_BYTES)
        .map_err(|e| fault("cache_read_failed", e))?;
    // Read compatibility evidence before strict body decoding, so a new
    // schema with new fields is unsupported rather than labelled corrupt.
    let header: Header = serde_json::from_slice(&bytes).map_err(|e| fault("invalid_cache", e))?;
    match header.schema {
        // Legacy schema-1 envelopes are accepted only under the named
        // predecessor interpretation, with strict digest/recompile checks and
        // only without an explicit resource reference.
        CACHE_SCHEMA => {
            if header.interpretation != legacy_interpretation() {
                return Err(fault(
                    "unsupported_cache",
                    "Cache schema/interpretation differs",
                ));
            }
            let envelope: Envelope =
                serde_json::from_slice(&bytes).map_err(|e| fault("invalid_cache", e))?;
            if envelope.schema != CACHE_SCHEMA
                || envelope.interpretation != legacy_interpretation()
                || envelope.binding.is_some()
            {
                return Err(fault(
                    "unsupported_cache",
                    "Cache schema/interpretation differs",
                ));
            }
            if envelope.context != target.context || envelope.shell != target.shell {
                return Err(fault("wrong_cache_target", "Context/capability differs"));
            }
            if envelope.digest
                != crate::digest(&envelope.snapshot).map_err(|e| fault("invalid_cache", e))?
            {
                return Err(fault("invalid_cache", "Digest differs"));
            }
            validate_inline(&envelope.snapshot, &target.binding, &target.context)?;
            if envelope.snapshot.desktop.appearance.resources.is_some() {
                return Err(fault(
                    "unsupported_cache",
                    "Legacy cache cannot carry an explicit resource reference",
                ));
            }
            Ok(Candidate {
                snapshot: Arc::new(envelope.snapshot),
                context: envelope.context,
                shell: envelope.shell,
                binding: None,
            })
        }
        // Resource-aware schema-2 envelopes: header/interpretation first, then
        // the binding, context, domain-separated digest and recompiled
        // snapshot. An explicit authored reference must equal the binding.
        RESOURCE_CACHE_SCHEMA => {
            if header.interpretation != resource_interpretation() {
                return Err(fault(
                    "unsupported_cache",
                    "Cache schema/interpretation differs",
                ));
            }
            let envelope: Envelope =
                serde_json::from_slice(&bytes).map_err(|e| fault("invalid_cache", e))?;
            if envelope.schema != RESOURCE_CACHE_SCHEMA
                || envelope.interpretation != resource_interpretation()
            {
                return Err(fault(
                    "unsupported_cache",
                    "Cache schema/interpretation differs",
                ));
            }
            let binding = envelope
                .binding
                .as_ref()
                .ok_or_else(|| fault("invalid_cache", "Resource envelope lacks a binding"))?;
            if binding.interpretation != resource_interpretation() {
                return Err(fault(
                    "unsupported_cache",
                    "Resource binding interpretation differs",
                ));
            }
            binding
                .validate()
                .map_err(|error| fault(&error.code, error.message))?;
            if envelope.context != target.context || envelope.shell != target.shell {
                return Err(fault("wrong_cache_target", "Context/capability differs"));
            }
            if envelope.digest
                != resource_digest(binding, &envelope.snapshot)
                    .map_err(|e| fault("invalid_cache", e))?
            {
                return Err(fault("invalid_cache", "Digest differs"));
            }
            validate_inline(&envelope.snapshot, &target.binding, &target.context)?;
            check_binding_matches_snapshot(binding, &envelope.snapshot)?;
            Ok(Candidate {
                snapshot: Arc::new(envelope.snapshot),
                context: envelope.context,
                shell: envelope.shell,
                binding: envelope.binding,
            })
        }
        _ => Err(fault(
            "unsupported_cache",
            "Cache schema/interpretation differs",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> Binding {
        Binding {
            instance: "fixture".into(),
            profile: "default".into(),
        }
    }
    fn desktop(resources: Option<crate::ResourceReference>) -> Desktop {
        let mut desktop = Desktop::default();
        desktop.appearance.resources = resources;
        desktop
    }
    fn fixture_snapshot(resources: Option<crate::ResourceReference>) -> Snapshot {
        let desktop = desktop(resources);
        Snapshot {
            schema: SCHEMA,
            binding: binding(),
            incarnation: "authority".into(),
            revision: Revision(1),
            design_revision: Revision(1),
            source_digest: source_digest(EMBEDDED_DEFAULT_SOURCE),
            effective: resolve(&desktop).unwrap(),
            desktop,
        }
    }
    fn fixture_reference(set_id: &str) -> crate::ResourceReference {
        crate::ResourceReference {
            schema: crate::RESOURCE_SCHEMA,
            set_id: set_id.into(),
            manifest_blake3: "a".repeat(64),
            icons: None,
        }
    }
    fn resource_binding(set_id: &str) -> crate::ResourceBinding {
        crate::ResourceBinding {
            schema: crate::RESOURCE_SCHEMA,
            set_id: set_id.into(),
            manifest_blake3: "a".repeat(64),
            interpretation: resource_interpretation(),
            icons: None,
        }
    }
    fn target() -> Target {
        Target::capture(7, binding(), "app:ced".into(), false)
    }
    fn store_envelope(dir: &Path, envelope: &Envelope) {
        let directory = config::atomic::open_directory(dir).unwrap();
        let name = name(
            &envelope.snapshot.binding,
            &envelope.context,
            envelope.shell,
        );
        config::atomic::replace_in(
            &directory,
            name.as_ref(),
            &serde_json::to_vec(envelope).unwrap(),
        )
        .unwrap();
    }
    #[test]
    fn legacy_interpretation_is_pinned_and_distinct_from_resources() {
        // Real 0.3.4 envelopes only match while the embedded default source is
        // byte-identical to what that release shipped: the predecessor label
        // and schema are frozen, and the interpretation derives from the
        // actual in-tree embedded source, never a copied constant.
        let predecessor = source_digest(&format!(
            "settings-cache-{CACHE_SCHEMA}-{}-{}",
            "0.3.4",
            source_digest(EMBEDDED_DEFAULT_SOURCE)
        ));
        assert_eq!(legacy_interpretation(), predecessor);
        // The named predecessor never collides with the resource interpretation.
        assert_ne!(legacy_interpretation(), resource_interpretation());
    }
    #[test]
    fn schema2_envelope_round_trips_and_records_omission_default_without_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let target = target();
        let snapshot = fixture_snapshot(None);
        let binding = resource_binding("core-icons");
        store_envelope(
            dir.path(),
            &Envelope {
                schema: RESOURCE_CACHE_SCHEMA,
                interpretation: resource_interpretation(),
                context: target.context.clone(),
                shell: target.shell,
                digest: resource_digest(&binding, &snapshot).unwrap(),
                snapshot: snapshot.clone(),
                binding: Some(binding.clone()),
            },
        );
        let candidate = load_for(dir.path(), &target).unwrap();
        assert_eq!(candidate.snapshot(), &snapshot);
        assert_eq!(candidate.binding(), Some(&binding));
        // The canonical snapshot itself stays untouched by the binding.
        assert_eq!(candidate.snapshot().desktop.appearance.resources, None);
    }
    #[test]
    fn explicit_reference_must_equal_binding_on_schema2_load() {
        let dir = tempfile::tempdir().unwrap();
        let target = target();
        let snapshot = fixture_snapshot(Some(fixture_reference("core-icons")));
        // A different set ID is a binding mismatch, not silent fallback.
        let binding = resource_binding("other-set");
        store_envelope(
            dir.path(),
            &Envelope {
                schema: RESOURCE_CACHE_SCHEMA,
                interpretation: resource_interpretation(),
                context: target.context.clone(),
                shell: target.shell,
                digest: resource_digest(&binding, &snapshot).unwrap(),
                snapshot: snapshot.clone(),
                binding: Some(binding),
            },
        );
        assert_eq!(
            load_for(dir.path(), &target).unwrap_err().code,
            "cache_binding_mismatch"
        );
        // The exact reference loads.
        let binding = resource_binding("core-icons");
        store_envelope(
            dir.path(),
            &Envelope {
                schema: RESOURCE_CACHE_SCHEMA,
                interpretation: resource_interpretation(),
                context: target.context.clone(),
                shell: target.shell,
                digest: resource_digest(&binding, &snapshot).unwrap(),
                snapshot,
                binding: Some(binding),
            },
        );
        assert!(load_for(dir.path(), &target).is_ok());
    }
    #[test]
    fn icon_selectors_follow_the_authored_reference_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let target = target();
        let icon = crate::IconReference {
            family: "Symbols".into(),
            style: "rounded".into(),
            weight: 400,
        };
        // Explicit reference with icons None: a binding that fills in a
        // resolved default selector is a mismatch, never accepted.
        let mut reference = fixture_reference("core-icons");
        reference.icons = None;
        let snapshot = fixture_snapshot(Some(reference));
        let mut binding = resource_binding("core-icons");
        binding.icons = Some(icon.clone());
        store_envelope(
            dir.path(),
            &Envelope {
                schema: RESOURCE_CACHE_SCHEMA,
                interpretation: resource_interpretation(),
                context: target.context.clone(),
                shell: target.shell,
                digest: resource_digest(&binding, &snapshot).unwrap(),
                snapshot: snapshot.clone(),
                binding: Some(binding),
            },
        );
        assert_eq!(
            load_for(dir.path(), &target).unwrap_err().code,
            "cache_binding_mismatch"
        );
        // Explicit reference with icons Some must carry exactly that selector.
        let mut reference = fixture_reference("core-icons");
        reference.icons = Some(icon.clone());
        let snapshot = fixture_snapshot(Some(reference));
        let mut binding = resource_binding("core-icons");
        binding.icons = None;
        store_envelope(
            dir.path(),
            &Envelope {
                schema: RESOURCE_CACHE_SCHEMA,
                interpretation: resource_interpretation(),
                context: target.context.clone(),
                shell: target.shell,
                digest: resource_digest(&binding, &snapshot).unwrap(),
                snapshot: snapshot.clone(),
                binding: Some(binding),
            },
        );
        assert_eq!(
            load_for(dir.path(), &target).unwrap_err().code,
            "cache_binding_mismatch"
        );
        // Omission (default pin) must not record any icon selector.
        let snapshot = fixture_snapshot(None);
        let mut binding = resource_binding("core-icons");
        binding.icons = Some(icon.clone());
        store_envelope(
            dir.path(),
            &Envelope {
                schema: RESOURCE_CACHE_SCHEMA,
                interpretation: resource_interpretation(),
                context: target.context.clone(),
                shell: target.shell,
                digest: resource_digest(&binding, &snapshot).unwrap(),
                snapshot: snapshot.clone(),
                binding: Some(binding),
            },
        );
        assert_eq!(
            load_for(dir.path(), &target).unwrap_err().code,
            "cache_binding_mismatch"
        );
        // Omission with icons None stays the accepted default pin.
        let binding = resource_binding("core-icons");
        store_envelope(
            dir.path(),
            &Envelope {
                schema: RESOURCE_CACHE_SCHEMA,
                interpretation: resource_interpretation(),
                context: target.context.clone(),
                shell: target.shell,
                digest: resource_digest(&binding, &snapshot).unwrap(),
                snapshot,
                binding: Some(binding),
            },
        );
        assert!(load_for(dir.path(), &target).is_ok());
    }
    #[test]
    fn cache_interpretation_delegates_to_the_always_compiled_formula() {
        assert_eq!(crate::resource_interpretation(), resource_interpretation());
    }
    #[test]
    fn legacy_envelope_loads_under_named_predecessor_and_refuses_explicit_reference() {
        let dir = tempfile::tempdir().unwrap();
        let target = target();
        let snapshot = fixture_snapshot(None);
        store_envelope(
            dir.path(),
            &Envelope {
                schema: CACHE_SCHEMA,
                interpretation: legacy_interpretation(),
                context: target.context.clone(),
                shell: target.shell,
                digest: crate::digest(&snapshot).unwrap(),
                snapshot: snapshot.clone(),
                binding: None,
            },
        );
        let candidate = load_for(dir.path(), &target).unwrap();
        assert_eq!(candidate.snapshot(), &snapshot);
        assert_eq!(candidate.binding(), None);
        // A legacy envelope that somehow carries an explicit reference is not
        // a recognised legacy capture.
        let with_reference = fixture_snapshot(Some(fixture_reference("core-icons")));
        store_envelope(
            dir.path(),
            &Envelope {
                schema: CACHE_SCHEMA,
                interpretation: legacy_interpretation(),
                context: target.context.clone(),
                shell: target.shell,
                digest: crate::digest(&with_reference).unwrap(),
                snapshot: with_reference,
                binding: None,
            },
        );
        assert_eq!(
            load_for(dir.path(), &target).unwrap_err().code,
            "unsupported_cache"
        );
        // Arbitrary prior/unknown interpretations are refused, not guessed.
        store_envelope(
            dir.path(),
            &Envelope {
                schema: CACHE_SCHEMA,
                interpretation: "0.3.3-arbitrary".into(),
                context: target.context.clone(),
                shell: target.shell,
                digest: crate::digest(&snapshot).unwrap(),
                snapshot,
                binding: None,
            },
        );
        assert_eq!(
            load_for(dir.path(), &target).unwrap_err().code,
            "unsupported_cache"
        );
    }
    #[test]
    fn schema2_tampering_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let target = target();
        let snapshot = fixture_snapshot(None);
        let binding = resource_binding("core-icons");
        let envelope = Envelope {
            schema: RESOURCE_CACHE_SCHEMA,
            interpretation: resource_interpretation(),
            context: target.context.clone(),
            shell: target.shell,
            digest: resource_digest(&binding, &snapshot).unwrap(),
            snapshot: snapshot.clone(),
            binding: Some(binding),
        };
        // Missing binding.
        let mut lacking = envelope.clone();
        lacking.binding = None;
        store_envelope(dir.path(), &lacking);
        assert_eq!(
            load_for(dir.path(), &target).unwrap_err().code,
            "invalid_cache"
        );
        // Tampered digest: a legacy snapshot digest cannot replay into a
        // resource envelope either.
        let mut forged = envelope.clone();
        forged.digest = crate::digest(&snapshot).unwrap();
        store_envelope(dir.path(), &forged);
        assert_eq!(
            load_for(dir.path(), &target).unwrap_err().code,
            "invalid_cache"
        );
        // Tampered binding bytes without a reseal.
        let mut tampered = envelope.clone();
        tampered.binding.as_mut().unwrap().set_id = "tampered".into();
        store_envelope(dir.path(), &tampered);
        assert_eq!(
            load_for(dir.path(), &target).unwrap_err().code,
            "invalid_cache"
        );
        // Unsupported selection semantics.
        let mut semantics = envelope.clone();
        semantics.binding.as_mut().unwrap().interpretation = "foreign".into();
        store_envelope(dir.path(), &semantics);
        assert_eq!(
            load_for(dir.path(), &target).unwrap_err().code,
            "unsupported_cache"
        );
        // Future envelope schema with the right interpretation is still
        // unsupported, not corrupt.
        let mut future = envelope.clone();
        future.schema = RESOURCE_CACHE_SCHEMA + 1;
        store_envelope(dir.path(), &future);
        assert_eq!(
            load_for(dir.path(), &target).unwrap_err().code,
            "unsupported_cache"
        );
    }
    #[test]
    fn a_serial_cannot_change_its_resource_binding() {
        let dir = tempfile::tempdir().unwrap();
        let target = target();
        let snapshot = Arc::new(fixture_snapshot(None));
        let mut writer = Writer::open_for(dir.path(), &target).unwrap();
        let first = Save::capture_resources(
            7,
            9,
            snapshot.clone(),
            "app:ced".into(),
            false,
            Some(resource_binding("first")),
        );
        assert_eq!(writer.write(&first).unwrap(), WriteOutcome::Written);
        let same_serial_different_binding = Save::capture_resources(
            7,
            9,
            snapshot.clone(),
            "app:ced".into(),
            false,
            Some(resource_binding("second")),
        );
        assert_eq!(
            writer
                .write(&same_serial_different_binding)
                .unwrap_err()
                .code,
            "cache_contradiction"
        );
        // The identical capture is an unchanged retry.
        assert_eq!(writer.write(&first).unwrap(), WriteOutcome::Unchanged);
    }
    #[test]
    fn writer_refuses_bindings_with_unsupported_selection_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let target = target();
        let snapshot = Arc::new(fixture_snapshot(None));
        let mut writer = Writer::open_for(dir.path(), &target).unwrap();
        let mut binding = resource_binding("core-icons");
        binding.interpretation = "foreign-selection-semantics".into();
        let save = Save::capture_resources(7, 3, snapshot, "app:ced".into(), false, Some(binding));
        assert_eq!(writer.write(&save).unwrap_err().code, "unsupported_cache");
    }
    #[test]
    fn writer_refuses_persisting_envelopes_its_loader_rejects() {
        let dir = tempfile::tempdir().unwrap();
        let target = target();
        let authored = fixture_reference("core-icons");
        // A resource-bearing snapshot without a binding would persist a legacy
        // envelope the loader deterministically refuses; the write fails
        // closed and nothing is persisted.
        let mut writer = Writer::open_for(dir.path(), &target).unwrap();
        let bindingless = Save::capture_resources(
            7,
            4,
            Arc::new(fixture_snapshot(Some(authored.clone()))),
            "app:ced".into(),
            false,
            None,
        );
        assert_eq!(
            writer.write(&bindingless).unwrap_err().code,
            "cache_binding_required"
        );
        assert!(load_for(dir.path(), &target).is_err(), "nothing persisted");
        // A binding for a different authored set is refused before
        // serialisation, as is an invented icon selector on omission.
        let mismatched = Save::capture_resources(
            7,
            5,
            Arc::new(fixture_snapshot(Some(authored.clone()))),
            "app:ced".into(),
            false,
            Some(resource_binding("other-set")),
        );
        assert_eq!(
            writer.write(&mismatched).unwrap_err().code,
            "cache_binding_mismatch"
        );
        let mut invented = resource_binding("core-icons");
        invented.icons = Some(crate::IconReference {
            family: "Symbols".into(),
            style: "rounded".into(),
            weight: 400,
        });
        let invented = Save::capture_resources(
            7,
            6,
            Arc::new(fixture_snapshot(None)),
            "app:ced".into(),
            false,
            Some(invented),
        );
        assert_eq!(
            writer.write(&invented).unwrap_err().code,
            "cache_binding_mismatch"
        );
        // Refusals never advance the serial fence: the exact matching binding
        // on the refused serial still writes, and the persisted envelope loads.
        let valid = Save::capture_resources(
            7,
            4,
            Arc::new(fixture_snapshot(Some(authored))),
            "app:ced".into(),
            false,
            Some(resource_binding("core-icons")),
        );
        assert_eq!(writer.write(&valid).unwrap(), WriteOutcome::Written);
        let candidate = load_for(dir.path(), &target).unwrap();
        assert_eq!(candidate.binding(), Some(&resource_binding("core-icons")));
        assert_eq!(
            candidate.snapshot().desktop.appearance.resources,
            Some(fixture_reference("core-icons"))
        );
        // A later refusal leaves the persisted envelope byte-identical.
        let file = dir
            .path()
            .join(name(&binding(), "app:ced", false));
        let bytes = std::fs::read(&file).unwrap();
        let newer = Save::capture_resources(
            7,
            7,
            Arc::new(fixture_snapshot(Some(fixture_reference("core-icons")))),
            "app:ced".into(),
            false,
            Some(resource_binding("other-set")),
        );
        assert_eq!(
            writer.write(&newer).unwrap_err().code,
            "cache_binding_mismatch"
        );
        assert_eq!(std::fs::read(&file).unwrap(), bytes);
        assert!(load_for(dir.path(), &target).is_ok());
    }
}
