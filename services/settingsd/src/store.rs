// SPDX-License-Identifier: MIT OR Apache-2.0
//! Exactly one profile writer. Accepted state and receipts share one synced
//! replacement. Invalid candidates never occupy the accepted path.
use serde::{Deserialize, Serialize};
use settings::*;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const MAX_STORE_BYTES: u64 = 2 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Accepted {
    pub schema: u32,
    pub binding: Binding,
    pub incarnation: String,
    pub revision: Revision,
    pub design_revision: Revision,
    pub desktop: Desktop,
    pub embedded_source: String,
    pub effective_digest: String,
    pub receipts: Vec<Receipt>,
    pub content_digest: String,
}
impl Accepted {
    fn digest(&self) -> anyhow::Result<String> {
        Ok(settings::digest(&(
            &self.schema,
            &self.binding,
            &self.incarnation,
            &self.revision,
            &self.design_revision,
            &self.desktop,
            &self.embedded_source,
            &self.effective_digest,
            &self.receipts,
        ))?)
    }
    pub fn seal(&mut self) -> anyhow::Result<()> {
        self.content_digest = self.digest()?;
        Ok(())
    }
    pub fn effective(&self) -> anyhow::Result<std::collections::BTreeMap<String, Effective>> {
        let effective = settings::resolve_with_embedded(&self.desktop, &self.embedded_source)
            .map_err(|e| anyhow::anyhow!("accepted interpretation unsupported: {e:?}"))?;
        anyhow::ensure!(
            self.effective_digest == settings::digest(&effective)?,
            "accepted interpretation changed; explicit migration required"
        );
        Ok(effective)
    }
    pub fn check(&self, binding: &Binding) -> anyhow::Result<()> {
        anyhow::ensure!(self.schema == SCHEMA, "unsupported accepted schema");
        anyhow::ensure!(&self.binding == binding, "wrong stored authority binding");
        anyhow::ensure!(
            self.embedded_source.len() <= MAX_SOURCE_BYTES,
            "pinned source limit exceeded"
        );
        anyhow::ensure!(
            self.content_digest == self.digest()?,
            "accepted content digest mismatch"
        );
        binding.validate().map_err(|e| anyhow::anyhow!(e.message))?;
        anyhow::ensure!(
            uuid::Uuid::parse_str(&self.incarnation).is_ok(),
            "invalid store incarnation"
        );
        anyhow::ensure!(
            self.revision.0 > 0
                && self.design_revision.0 > 0
                && self.design_revision <= self.revision,
            "invalid accepted revision"
        );
        anyhow::ensure!(
            self.receipts.len() <= MAX_RECEIPTS,
            "receipt limit exceeded"
        );
        let mut ids = std::collections::BTreeSet::new();
        for receipt in &self.receipts {
            anyhow::ensure!(
                receipt.incarnation == self.incarnation
                    && receipt.revision <= self.revision
                    && receipt.revision.0 > 0,
                "receipt lineage mismatch"
            );
            anyhow::ensure!(
                valid_operation_id(&receipt.operation_id) && ids.insert(&receipt.operation_id),
                "invalid or duplicate receipt identity"
            );
            anyhow::ensure!(
                receipt.request_digest.len() == 64
                    && receipt
                        .request_digest
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit()),
                "invalid receipt digest"
            );
        }
        Ok(())
    }
}
pub fn valid_operation_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub struct Store {
    root: PathBuf,
    directory: File,
    _lock: File,
    pub recovering: bool,
    pub restored: bool,
}
impl Store {
    fn lock(root: &Path, create: bool) -> anyhow::Result<Self> {
        if create {
            let mut missing = Vec::new();
            let mut cursor = root;
            while !cursor.exists() {
                missing.push(cursor.to_path_buf());
                cursor = cursor
                    .parent()
                    .ok_or_else(|| anyhow::anyhow!("profile root has no existing ancestor"))?;
            }
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(root)?;
            // Sync each newly created directory entry in its parent, outermost first.
            for created in missing.iter().rev() {
                File::open(
                    created
                        .parent()
                        .ok_or_else(|| anyhow::anyhow!("missing parent"))?,
                )?
                .sync_all()?;
            }
        }
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(root)?;
        let metadata = directory.metadata()?;
        anyhow::ensure!(
            metadata.is_dir()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o022 == 0,
            "profile root must be an owned directory not writable by others"
        );
        let lock = open_in(&directory, "writer.lock", libc::O_RDWR | libc::O_CREAT)?;
        let lock_meta = lock.metadata()?;
        anyhow::ensure!(
            lock_meta.is_file() && lock_meta.uid() == unsafe { libc::geteuid() },
            "writer lock must be an owned regular file"
        );
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            anyhow::bail!(
                "profile already has an active writer: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(Self {
            root: root.into(),
            directory,
            _lock: lock,
            recovering: false,
            restored: false,
        })
    }
    pub fn create(
        root: &Path,
        binding: Binding,
        desktop: Desktop,
    ) -> anyhow::Result<(Self, Accepted)> {
        let store = Self::lock(root, true)?;
        Self::initialise(store, binding, desktop)
    }
    /// Session provisioning is idempotent only for a valid existing primary.
    /// A retained lock inode is evidence of an established or interrupted
    /// profile: never recreate its missing primary, even without a backup.
    pub fn seed(
        root: &Path,
        binding: Binding,
        allow_create: bool,
    ) -> anyhow::Result<(Self, Accepted)> {
        // Automatic startup cannot distinguish first install from a whole lost
        // directory/mount. Only an explicit installer operation permits creation.
        if !allow_create {
            return Self::open(root, &binding);
        }
        if root.try_exists()? {
            // An empty directory pre-provisioned for the account is allowed.
            // Once writer.lock exists, only explicit init can recover an
            // interrupted first creation; normal seed cannot erase history.
            if std::fs::symlink_metadata(root.join("writer.lock")).is_ok() {
                return Self::open(root, &binding);
            }
        }
        Self::create(root, binding, Desktop::default())
    }
    fn initialise(
        store: Self,
        binding: Binding,
        desktop: Desktop,
    ) -> anyhow::Result<(Self, Accepted)> {
        anyhow::ensure!(
            !store.path().exists() && !store.backup().exists(),
            "profile already initialised; initialise never overwrites accepted/backup data"
        );
        let effective = settings::resolve(&desktop)
            .map_err(|e| anyhow::anyhow!("invalid initial desktop: {e:?}"))?;
        let mut accepted = Accepted {
            schema: SCHEMA,
            binding,
            incarnation: uuid::Uuid::now_v7().to_string(),
            revision: Revision(1),
            design_revision: Revision(1),
            desktop,
            embedded_source: settings::EMBEDDED_DEFAULT_SOURCE.into(),
            effective_digest: settings::digest(&effective)?,
            receipts: Vec::new(),
            content_digest: String::new(),
        };
        accepted.seal()?;
        accepted.check(&accepted.binding)?;
        crate::authority::snapshot(&accepted, effective)?;
        store.check_root()?;
        config::atomic::replace_in(
            &store.directory,
            "desktop.conf.mix".as_ref(),
            &encode(&accepted)?,
        )?;
        store.check_root()?;
        Ok((store, accepted))
    }
    pub fn open(root: &Path, binding: &Binding) -> anyhow::Result<(Self, Accepted)> {
        let mut store = Self::lock(root, false)?;
        store.check_root()?;
        // Loss/unmount of an established primary must be visible even when an
        // old backup exists. Corrupt present documents have the recovery path.
        let primary = open_in(&store.directory, "desktop.conf.mix", libc::O_RDONLY)?;
        let bytes = read_bytes(primary)?;
        let text = std::str::from_utf8(&bytes);
        #[derive(Deserialize)]
        struct Header {
            schema: u32,
            binding: Binding,
        }
        if let Ok(text) = text
            && let Ok(header) = strict::from_str::<Header>(text)
            && (header.schema != SCHEMA || &header.binding != binding)
        {
            anyhow::bail!("unsupported schema or wrong stored binding");
        }
        // Header and full record use the exact same bounded bytes. A second
        // read cannot miss a version fence after transient I/O.
        match text
            .map_err(anyhow::Error::from)
            .and_then(|text| Ok(strict::from_str::<Accepted>(text)?))
            .and_then(|data| {
                data.check(binding)?;
                Ok(data)
            }) {
            Ok(data) => {
                // An intact accepted document may require a newer compiler or
                // migration. Semantic refusal must preserve it, not roll back.
                data.effective()?;
                Ok((store, data))
            }
            Err(primary_error) => {
                if primary_error.downcast_ref::<std::io::Error>().is_some()
                    || primary_error
                        .downcast_ref::<strict::Error>()
                        .is_some_and(|e| {
                            e.kind() == strict::ErrorKind::Deserialize
                                || e.kind() == strict::ErrorKind::Io
                        })
                {
                    // Shape/version changes and I/O faults require diagnosis;
                    // only malformed syntax/integrity enters automatic restore.
                    return Err(primary_error);
                }
                let mut previous: Accepted = open_in(
                    &store.directory,
                    "desktop.previous.conf.mix",
                    libc::O_RDONLY,
                )
                .and_then(read_typed)
                .map_err(|e| {
                    anyhow::anyhow!(
                        "accepted store unavailable ({primary_error}); backup unavailable ({e})"
                    )
                })?;
                previous.check(binding)?;
                previous.effective()?;
                // Preserve user/corrupt evidence before replacing anything.
                store.check_root()?;
                let corrupt =
                    std::ffi::CString::new(format!("corrupt-{}.conf.mix", uuid::Uuid::now_v7()))?;
                // Preserve the complete corrupt inode before replacement. A
                // hard link leaves the primary present across every crash cut;
                // rename-away would create an unrecoverable missing-store gap.
                if unsafe {
                    libc::linkat(
                        store.directory.as_raw_fd(),
                        c"desktop.conf.mix".as_ptr(),
                        store.directory.as_raw_fd(),
                        corrupt.as_ptr(),
                        0,
                    )
                } != 0
                {
                    return Err(std::io::Error::last_os_error().into());
                }
                store.directory.sync_all()?;
                previous.incarnation = uuid::Uuid::now_v7().to_string();
                previous.receipts.clear();
                previous.seal()?;
                config::atomic::replace_in(
                    &store.directory,
                    "desktop.conf.mix".as_ref(),
                    &encode(&previous)?,
                )?;
                store.check_root()?;
                store.restored = true;
                Ok((store, previous))
            }
        }
    }
    fn path(&self) -> PathBuf {
        self.root.join("desktop.conf.mix")
    }
    fn backup(&self) -> PathBuf {
        self.root.join("desktop.previous.conf.mix")
    }
    fn check_root(&self) -> anyhow::Result<()> {
        let path = std::fs::symlink_metadata(&self.root)?;
        let held = self.directory.metadata()?;
        anyhow::ensure!(
            path.is_dir() && path.dev() == held.dev() && path.ino() == held.ino(),
            "profile directory replaced or detached; restart required"
        );
        let lock = open_in(&self.directory, "writer.lock", libc::O_RDONLY)?.metadata()?;
        let held_lock = self._lock.metadata()?;
        anyhow::ensure!(
            lock.dev() == held_lock.dev() && lock.ino() == held_lock.ino(),
            "profile writer lock replaced; restart required"
        );
        Ok(())
    }
    pub fn commit(&mut self, current: &Accepted, next: &Accepted) -> anyhow::Result<()> {
        self.commit_using(current, next, |dir, name, bytes| {
            config::atomic::replace_in(dir, name.as_ref(), bytes)
        })
    }
    fn commit_using(
        &mut self,
        current: &Accepted,
        next: &Accepted,
        mut replace: impl FnMut(&File, &str, &[u8]) -> Result<(), config::atomic::ReplaceError>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.recovering,
            "store outcome unknown; restart/readback recovery required"
        );
        if let Err(error) = self.check_root() {
            self.recovering = true;
            return Err(error);
        }
        next.check(&current.binding)?;
        // Last-good copy is made durable before replacing the accepted document.
        replace(
            &self.directory,
            "desktop.previous.conf.mix",
            &encode(current)?,
        )?;
        if let Err(error) = replace(&self.directory, "desktop.conf.mix", &encode(next)?) {
            self.recovering = error.may_have_replaced;
            return Err(error.into());
        }
        if let Err(error) = self.check_root() {
            self.recovering = true;
            return Err(error);
        }
        Ok(())
    }
}
fn encode(data: &Accepted) -> anyhow::Result<Vec<u8>> {
    let bytes = strict::to_string_pretty(data)?.into_bytes();
    anyhow::ensure!(
        bytes.len() <= MAX_STORE_BYTES as usize,
        "accepted document limit exceeded"
    );
    Ok(bytes)
}
fn open_in(directory: &File, name: &str, flags: libc::c_int) -> anyhow::Result<File> {
    anyhow::ensure!(
        !name.contains('/') && name != "." && name != "..",
        "single file name required"
    );
    let name = std::ffi::CString::new(name)?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn read_typed<T: serde::de::DeserializeOwned>(file: File) -> anyhow::Result<T> {
    let bytes = read_bytes(file)?;
    Ok(strict::from_str(std::str::from_utf8(&bytes)?)?)
}
fn read_bytes(file: File) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(
        file.metadata()?.is_file(),
        "accepted document is not a regular file"
    );
    let mut bytes = Vec::new();
    file.take(MAX_STORE_BYTES + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= MAX_STORE_BYTES as usize,
        "accepted document limit exceeded"
    );
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binding() -> Binding {
        Binding {
            instance: "test".into(),
            profile: "default".into(),
        }
    }
    #[test]
    fn ambiguous_primary_sync_fences_mutations_and_reload_resolves_accepted_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, current) =
            Store::create(dir.path(), binding(), Desktop::default()).unwrap();
        let mut next = current.clone();
        next.revision = Revision(2);
        next.desktop.ui.density = 1.5;
        next.effective_digest = settings::digest(
            &settings::resolve_with_embedded(&next.desktop, &next.embedded_source).unwrap(),
        )
        .unwrap();
        next.receipts.push(Receipt {
            operation_id: "ambiguous".into(),
            request_digest: "a".repeat(64),
            incarnation: next.incarnation.clone(),
            revision: next.revision,
            outcome: Outcome::Changed,
        });
        next.seal().unwrap();
        let result = store.commit_using(&current, &next, |dir, name, bytes| {
            config::atomic::replace_in(dir, name.as_ref(), bytes)?;
            if name == "desktop.conf.mix" {
                Err(config::atomic::ReplaceError {
                    stage: config::atomic::Stage::DirectorySync,
                    may_have_replaced: true,
                    source: std::io::Error::other("injected ambiguous sync outcome"),
                })
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        assert!(store.recovering);
        assert!(store.commit(&current, &next).is_err());
        drop(store);
        let (_, accepted) = Store::open(dir.path(), &binding()).unwrap();
        assert_eq!(accepted.revision, Revision(2));
        assert_eq!(accepted.receipts[0].operation_id, "ambiguous");
    }
    #[test]
    fn writer_lock_and_missing_store_do_not_materialise_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Store::open(dir.path(), &binding()).is_err());
        assert!(!dir.path().join("desktop.conf.mix").exists());
        let (first, _) = Store::create(dir.path(), binding(), Desktop::default()).unwrap();
        assert!(Store::open(dir.path(), &binding()).is_err());
        drop(first);
        assert!(Store::open(dir.path(), &binding()).is_ok());
    }
}
