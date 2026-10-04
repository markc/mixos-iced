use crate::persist::document::entry::base::DocumentEntry;
use crate::persist::path::base as path;
use crate::persist::store::base::Store;
use crate::persist::store::reconcile::base::reconcile;
use crate::storage::slot::base::Storage;
use crate::world::identity::base::OVERLAY_WORLDS;

/// The picker's world registry (`WorldsDoc::TABLE`) — the one table keyed by
/// world id, and so the only one the filter below applies to.
const WORLD_TABLE: &str = "world";

/// Rehydrate a building world's document tables into their collection slots: each
/// entry that is `world_partitioned` loads only this world's `world_id` partition;
/// global tables load whole. Tables are reconciled first (corrupt quarantined,
/// dangling symlinks pruned). Call where storage is mutable (world build).
pub fn rehydrate_world_documents(
    entries: &[&'static DocumentEntry],
    storage: &mut Storage,
    world: uuid::Uuid,
) {
    let world_id = world.to_string();
    for entry in entries {
        let partition = if entry.world_partitioned {
            Some(("world_id", world_id.as_str()))
        } else {
            None
        };
        load_entry(entry, storage, partition);
    }
}

/// Generic rehydration with an explicit partition filter (`None` = whole table).
pub fn rehydrate_documents(
    entries: &[&'static DocumentEntry],
    storage: &mut Storage,
    partition: Option<(&str, &str)>,
) {
    for entry in entries {
        load_entry(entry, storage, partition);
    }
}

fn load_entry(entry: &DocumentEntry, storage: &mut Storage, partition: Option<(&str, &str)>) {
    let dir = path::table_dir(entry.table);
    reconcile(&dir, now_unix(), |_| true);
    let store = Store::new(dir);
    let ids = match partition {
        Some((k, v)) => store.list_partition(k, v).unwrap_or_default(),
        None => store.list().unwrap_or_default(),
    };
    // TEMPORARY — this should move up: the rule belongs to the document that owns
    // the `world` registry (the picker's persistence), not to the generic loader. The OVERLAY worlds own no records, so an id of one of them listed in
    // that table is not data and leaves the listing here, before any read. Scoped to
    // the `world` table — every other table keys on its own ids.
    let ids: Vec<String> = if entry.table == WORLD_TABLE {
        ids.into_iter()
            .filter(|id| !uuid::Uuid::parse_str(id).is_ok_and(|id| OVERLAY_WORLDS.contains(&id)))
            .collect()
    } else {
        ids
    };
    let mut restored = 0usize;
    for id in ids {
        match store.get(&id) {
            Ok(Some((version, bytes))) => match (entry.apply)(storage, &id, &bytes, version) {
                Ok(()) => restored += 1,
                Err(e) => warn!("persist: apply {}/{} failed ({e})", entry.table, id),
            },
            Ok(None) => {} // pruned by reconcile
            Err(e) => warn!("persist: read {}/{} failed ({e})", entry.table, id),
        }
    }
    if restored > 0 {
        info!("persist: restored {restored} record(s) from {}", entry.table);
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
