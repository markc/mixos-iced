use crate::persist::document::entry::base::DocumentEntry;
use crate::persist::path::base as path;
use crate::persist::store::base::Store;
use crate::storage::slot::base::Storage;
use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

struct RecordCache {
    record: Box<dyn Any + Send>,
    partitions: Vec<(&'static str, String)>,
}
type TableCache = HashMap<String, RecordCache>;
// Keyed by (world, table): a partitioned table (e.g. `world.placeholder`) is
// committed by EVERY world, each owning only its own partition. A single
// per-table cache would make world B's `gone` sweep delete world A's records
// (they're absent from B's projection), wiping the other world's partition off
// disk. Per-world keying confines the diff (and the delete sweep) to the world
// that actually owns those records.
static LEDGER: OnceLock<Mutex<HashMap<(uuid::Uuid, &'static str), TableCache>>> = OnceLock::new();

/// Persist a system's changed table records. Called at the `buffer()` boundary.
/// For each entry: project the collection, write records that are new/changed (by
/// `PartialEq`), re-link records that moved partition, and delete records gone
/// from memory. World-partitioned tables get a `world_id` partition (the building
/// world) injected here, since `Document::rows` doesn't know its world.
pub fn sync_documents(entries: &[&'static DocumentEntry], storage: &Storage, world: uuid::Uuid) {
    let mut all = LEDGER.get_or_init(|| Mutex::new(HashMap::new())).lock().expect("doc ledger");
    let world_id = world.to_string();
    for entry in entries {
        let Some(mut rows) = (entry.rows)(storage) else { continue };
        if entry.world_partitioned {
            for row in &mut rows {
                row.partitions.push(("world_id", world_id.clone()));
            }
        }
        let store = Store::new(path::table_dir(entry.table));
        let cache = all.entry((world, entry.table)).or_default();
        let mut seen = HashSet::new();
        for row in rows {
            seen.insert(row.id.clone());
            let prev = cache.get(&row.id);
            let changed = match prev {
                Some(p) => !(entry.eq)(p.record.as_ref(), row.record.as_ref()) || p.partitions != row.partitions,
                None => true,
            };
            if !changed {
                continue;
            }
            if let Some(p) = prev {
                if p.partitions != row.partitions {
                    let _ = store.unlink_partitions(&row.id, &borrow(&p.partitions));
                }
            }
            if let Err(e) = store.put(&row.id, entry.table, entry.version, &row.bytes, &borrow(&row.partitions)) {
                warn!("persist: put {}/{} failed: {e}", entry.table, row.id);
                continue;
            }
            cache.insert(row.id.clone(), RecordCache { record: row.record, partitions: row.partitions });
        }
        let gone: Vec<String> = cache.keys().filter(|k| !seen.contains(k.as_str())).cloned().collect();
        for id in gone {
            if let Some(p) = cache.remove(&id) {
                let _ = store.delete(&id, &borrow(&p.partitions));
            }
        }
    }
}

fn borrow<'a>(p: &'a [(&'static str, String)]) -> Vec<(&'a str, &'a str)> {
    p.iter().map(|(k, v)| (*k as &str, v.as_str())).collect()
}
