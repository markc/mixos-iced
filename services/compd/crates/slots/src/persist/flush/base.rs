use crate::persist::document::entry::base::DocumentEntry;
use crate::persist::entry::base::PersistEntry;
use crate::storage::slot::base::Storage;
use crate::trait_::system::base::System;
use uuid::Uuid;

/// Commit a world's persisted state: gather every system's slot + document entries
/// and persist the changed ones. Called at end-of-buffer-reduction for a world the
/// mark registry reports due (see `flow::flush`).
pub fn commit_world(world: Uuid, storage: &Storage, systems: &[Box<dyn System>]) {
    let slots: Vec<&'static PersistEntry> =
        systems.iter().flat_map(|s| s.persist().iter().copied()).collect();
    let docs: Vec<&'static DocumentEntry> =
        systems.iter().flat_map(|s| s.documents().iter().copied()).collect();
    persist_system(world, storage, &slots, &docs);
}

/// Persist a system's changed state at the `buffer()` boundary: single-value
/// slots and table collections. No-op for empty lists.
pub fn persist_system(
    world: Uuid,
    storage: &Storage,
    slots: &[&'static PersistEntry],
    docs: &[&'static DocumentEntry],
) {
    if !slots.is_empty() {
        crate::persist::engine::base::sync(world, storage, slots);
    }
    if !docs.is_empty() {
        crate::persist::document::sync::base::sync_documents(docs, storage, world);
    }
}
