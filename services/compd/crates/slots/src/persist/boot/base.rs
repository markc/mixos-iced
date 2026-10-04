use crate::persist::document::entry::base::DocumentEntry;
use crate::persist::entry::base::PersistEntry;
use crate::storage::slot::base::Storage;
use uuid::Uuid;

/// Rehydrate a world at build time: its single-value slots, then its document
/// tables (per-record, partition-filtered by world). No-op for empty lists.
pub fn rehydrate_world(
    world: Uuid,
    storage: &mut Storage,
    slots: &[&'static PersistEntry],
    docs: &[&'static DocumentEntry],
) {
    if !slots.is_empty() {
        crate::persist::rehydrate::base::rehydrate_storage(world, storage, slots);
    }
    if !docs.is_empty() {
        crate::persist::document::rehydrate::base::rehydrate_world_documents(docs, storage, world);
    }
}
