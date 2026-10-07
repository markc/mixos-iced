// SPDX-License-Identifier: MIT OR Apache-2.0
//! Exactly one profile writer. Accepted state and receipts share one synced
//! replacement. Invalid candidates never occupy the accepted path.
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use settings::*;

const MAX_STORE_BYTES: u64 = 2 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Accepted {
    pub schema: u32, pub binding: Binding, pub incarnation: String,
    pub revision: Revision, pub design_revision: Revision, pub desktop: Desktop,
    pub receipts: Vec<Receipt>,
}
impl Accepted {
    pub fn check(&self, binding: &Binding) -> anyhow::Result<()> {
        anyhow::ensure!(self.schema == SCHEMA, "unsupported accepted schema");
        anyhow::ensure!(&self.binding == binding, "wrong stored authority binding");
        binding.validate().map_err(|e| anyhow::anyhow!(e.message))?;
        anyhow::ensure!(uuid::Uuid::parse_str(&self.incarnation).is_ok(), "invalid store incarnation");
        anyhow::ensure!(self.revision.0 > 0 && self.design_revision.0 > 0 && self.design_revision <= self.revision, "invalid accepted revision");
        anyhow::ensure!(self.receipts.len() <= MAX_RECEIPTS, "receipt limit exceeded");
        let mut ids = std::collections::BTreeSet::new();
        for receipt in &self.receipts {
            anyhow::ensure!(receipt.incarnation == self.incarnation && receipt.revision <= self.revision && receipt.revision.0 > 0, "receipt lineage mismatch");
            anyhow::ensure!(valid_operation_id(&receipt.operation_id) && ids.insert(&receipt.operation_id), "invalid or duplicate receipt identity");
            anyhow::ensure!(receipt.request_digest.len() == 64 && receipt.request_digest.bytes().all(|b| b.is_ascii_hexdigit()), "invalid receipt digest");
        }
        Ok(())
    }
}
pub fn valid_operation_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub struct Store {
    root: PathBuf, _lock: File, pub recovering: bool, pub restored: bool,
}
impl Store {
    fn lock(root: &Path, create: bool) -> anyhow::Result<Self> {
        if create { std::fs::DirBuilder::new().recursive(true).mode(0o700).create(root)?; }
        let metadata = std::fs::symlink_metadata(root)?;
        anyhow::ensure!(metadata.is_dir() && metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o022 == 0, "profile root must be an owned directory not writable by others");
        let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(root.join("writer.lock"))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 { anyhow::bail!("profile already has an active writer: {}", std::io::Error::last_os_error()); }
        Ok(Self { root: root.into(), _lock: lock, recovering:false, restored:false })
    }
    pub fn create(root: &Path, binding: Binding, desktop: Desktop) -> anyhow::Result<(Self, Accepted)> {
        let store = Self::lock(root, true)?;
        anyhow::ensure!(!store.path().exists() && !store.backup().exists(), "profile already initialised; initialise never overwrites accepted/backup data");
        settings::resolve(&desktop).map_err(|e| anyhow::anyhow!("invalid initial desktop: {e:?}"))?;
        let accepted = Accepted { schema:SCHEMA, binding, incarnation:uuid::Uuid::now_v7().to_string(), revision:Revision(1), design_revision:Revision(1), desktop, receipts:Vec::new() };
        accepted.check(&accepted.binding)?;
        config::atomic::replace(&store.path(), &encode(&accepted)?)?;
        Ok((store,accepted))
    }
    pub fn open(root: &Path, binding: &Binding) -> anyhow::Result<(Self, Accepted)> {
        let mut store = Self::lock(root, false)?;
        match read(&store.path()).and_then(|data| {
            data.check(binding)?;
            settings::resolve(&data.desktop).map_err(|e| anyhow::anyhow!("invalid accepted desktop: {e:?}"))?;
            Ok(data)
        }) {
            Ok(data) => Ok((store,data)),
            Err(primary_error) => {
                // Unsupported schema/wrong binding must fail, never silently
                // restore an older document in a different contract/history.
                #[derive(Deserialize)]
                struct Header { schema:u32, binding:Binding }
                if let Ok(parsed) = read_typed::<Header>(&store.path()) {
                    if parsed.schema != SCHEMA || &parsed.binding != binding { return Err(primary_error); }
                }
                let mut previous = read(&store.backup()).map_err(|e| anyhow::anyhow!("accepted store unavailable ({primary_error}); backup unavailable ({e})"))?;
                previous.check(binding)?;
                settings::resolve(&previous.desktop).map_err(|e| anyhow::anyhow!("backup invalid: {e:?}"))?;
                // Preserve user/corrupt evidence before replacing anything.
                if store.path().exists() { std::fs::rename(store.path(), root.join(format!("corrupt-{}.conf.mix",uuid::Uuid::now_v7())))?; }
                previous.incarnation = uuid::Uuid::now_v7().to_string();
                previous.receipts.clear();
                config::atomic::replace(&store.path(), &encode(&previous)?)?;
                store.restored = true;
                Ok((store,previous))
            }
        }
    }
    fn path(&self) -> PathBuf { self.root.join("desktop.conf.mix") }
    fn backup(&self) -> PathBuf { self.root.join("desktop.previous.conf.mix") }
    pub fn commit(&mut self, current: &Accepted, next: &Accepted) -> anyhow::Result<()> {
        anyhow::ensure!(!self.recovering, "store outcome unknown; restart/readback recovery required");
        next.check(&current.binding)?;
        // Last-good copy is made durable before replacing the accepted document.
        config::atomic::replace(&self.backup(), &encode(current)?)?;
        if let Err(error) = config::atomic::replace(&self.path(), &encode(next)?) {
            self.recovering = error.may_have_replaced;
            return Err(error.into());
        }
        Ok(())
    }
}
fn encode(data: &Accepted) -> anyhow::Result<Vec<u8>> {
    let bytes = strict::to_string_pretty(data)?.into_bytes();
    anyhow::ensure!(bytes.len() <= MAX_STORE_BYTES as usize, "accepted document limit exceeded");
    Ok(bytes)
}
fn read(path: &Path) -> anyhow::Result<Accepted> {
    read_typed(path)
}
fn read_typed<T:serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    let file = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(path)?;
    anyhow::ensure!(file.metadata()?.is_file(), "accepted document is not a regular file");
    let mut bytes = Vec::new();
    file.take(MAX_STORE_BYTES+1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= MAX_STORE_BYTES as usize, "accepted document limit exceeded");
    Ok(strict::from_str(std::str::from_utf8(&bytes)?)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binding() -> Binding { Binding { instance:"test".into(), profile:"default".into() } }
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
