// SPDX-License-Identifier: MIT OR Apache-2.0
//! Optional bounded local presentation cache. Never authority persistence.
//! All I/O belongs on the host's one serial cache worker, not its UI loop.
use crate::{
    consumer::Consumer,
    fallback::{Candidate, validate_inline},
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
}
fn interpretation() -> String {
    source_digest(&format!(
        "settings-cache-{CACHE_SCHEMA}-{}-{}",
        env!("CARGO_PKG_VERSION"),
        source_digest(EMBEDDED_DEFAULT_SOURCE)
    ))
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
}
impl Save {
    /// Identity of this activated capture. Persistence callers can expose a
    /// successful write receipt without copying the projection or private fence.
    pub fn identity(&self) -> crate::consumer::SnapshotIdentity {
        crate::consumer::SnapshotIdentity::from(self.snapshot.as_ref())
    }
    /// Compare private producer and activation fences without copying data.
    pub fn same_capture(&self, other: &Self) -> bool {
        self.owner == other.owner && self.serial == other.serial
    }
    pub(crate) fn capture(
        owner: u64,
        serial: u64,
        snapshot: Arc<Snapshot>,
        context: String,
        shell: bool,
    ) -> Self {
        Self {
            owner,
            serial,
            snapshot,
            context,
            shell,
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
        let digest = crate::digest(save.snapshot.as_ref())
            .map_err(|e| fault("invalid_cache_snapshot", e))?;
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
            schema: CACHE_SCHEMA,
            interpretation: interpretation(),
            context: self.context.clone(),
            shell: self.shell,
            digest,
            snapshot: save.snapshot.as_ref().clone(),
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
    if header.schema != CACHE_SCHEMA || header.interpretation != interpretation() {
        return Err(fault(
            "unsupported_cache",
            "Cache schema/interpretation differs",
        ));
    }
    let envelope: Envelope =
        serde_json::from_slice(&bytes).map_err(|e| fault("invalid_cache", e))?;
    if envelope.schema != CACHE_SCHEMA || envelope.interpretation != interpretation() {
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
    Ok(Candidate {
        snapshot: Arc::new(envelope.snapshot),
        context: envelope.context,
        shell: envelope.shell,
    })
}
