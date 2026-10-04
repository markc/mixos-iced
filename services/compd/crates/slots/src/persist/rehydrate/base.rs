use crate::persist::entry::base::{PersistEntry, PersistError};
use crate::persist::envelope::base as envelope;
use crate::persist::path::base as path;
use crate::storage::slot::base::Storage;

/// Rehydrate a world's storages from disk. For each entry: load `<key>.json`,
/// unwrap + migrate, and write the reconstructed live value through its slot.
/// A missing file is a normal first run (default kept, `trace!`). A corrupt or
/// unmigratable file is quarantined (`warn!`) and the default kept — never fatal.
/// Needs `&mut Storage`; call where storage is mutable (world build / register).
pub fn rehydrate_storage(world: uuid::Uuid, storage: &mut Storage, entries: &[&'static PersistEntry]) {
    for entry in entries {
        let file = path::file_path(world, entry.key);
        let bytes = match std::fs::read(&file) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                trace!("persist: no saved {} for world {world}, first run", entry.key);
                continue;
            }
            Err(e) => {
                warn!("persist: cannot read {}: {e}", file.display());
                continue;
            }
        };
        match load(entry, &bytes, storage) {
            Ok(()) => info!("persist: restored {}@{world} v{}", entry.key, entry.version),
            Err(e) => {
                warn!("persist: {}@{world} unreadable ({e}); keeping defaults", entry.key);
                quarantine(world, entry.key, &file);
            }
        }
    }
}

fn load(entry: &PersistEntry, file_bytes: &[u8], storage: &mut Storage) -> Result<(), PersistError> {
    let (version, data) = envelope::unwrap(file_bytes)?;
    (entry.rehydrate)(storage, &data, version)
}

fn quarantine(world: uuid::Uuid, key: &str, src: &std::path::Path) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dst = path::quarantine_path(world, key, ts);
    if let Err(e) = std::fs::rename(src, &dst) {
        warn!("persist: could not quarantine {}: {e}", src.display());
    }
}
