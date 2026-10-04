use crate::persist::entry::base::{PersistEntry, SnapshotOutcome};
use crate::persist::envelope::base as envelope;
use crate::persist::path::base as path;
use crate::persist::write::base::{spawn_worker, WriteDone, WriteJob};
use crate::storage::slot::base::Storage;
use std::any::Any;
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;
use uuid::Uuid;

/// `cache` = last durably-written value; `pending` = in-flight value (-> cache on ok).
struct Saved {
    cache: Option<Box<dyn Any + Send>>,
    pending: Option<Box<dyn Any + Send>>,
    in_flight: Option<u64>,
    epoch: u64,
}

struct Engine {
    tx: Sender<WriteJob>,
    done: Receiver<WriteDone>,
    /// (world, slot key) → state, so the same slot in different worlds is separate.
    ledger: HashMap<(Uuid, &'static str), Saved>,
    _worker: JoinHandle<()>,
}

static ENGINE: OnceLock<Mutex<Engine>> = OnceLock::new();

/// Spawn the writer thread and arm the engine. Call once at startup; until then
/// [`sync`] is a no-op (safe to call from the mutation path unconditionally).
pub fn init() {
    let (tx, jobs) = mpsc::channel::<WriteJob>();
    let (done_tx, done) = mpsc::channel::<WriteDone>();
    let _worker = spawn_worker(jobs, done_tx);
    let engine = Engine { tx, done, ledger: HashMap::new(), _worker };
    if ENGINE.set(Mutex::new(engine)).is_err() {
        warn!("persist: init() called twice; ignoring");
    }
}

/// Persist a system's changed slots at the `buffer()` boundary. Drains worker
/// confirmations, then per entry compares by `PartialEq` and enqueues an atomic
/// write if it changed and none is in flight (fsync runs on the worker).
pub fn sync(world: Uuid, storage: &Storage, entries: &[&'static PersistEntry]) {
    let Some(engine) = ENGINE.get() else { return };
    let mut guard = engine.lock().expect("persist engine mutex");
    let Engine { tx, done, ledger, .. } = &mut *guard;

    while let Ok(msg) = done.try_recv() {
        if let Some(s) = ledger.get_mut(&(msg.world, msg.key)) {
            if s.in_flight == Some(msg.epoch) {
                s.in_flight = None;
                if msg.ok { s.cache = s.pending.take(); } else { s.pending = None; }
            }
        }
    }

    for entry in entries {
        let s = ledger.entry((world, entry.key)).or_insert_with(|| Saved {
            cache: None, pending: None, in_flight: None, epoch: 0,
        });
        // One write in flight per key; the diff is re-checked at the next buffer.
        if s.in_flight.is_some() {
            continue;
        }
        let (bytes, cache) = match (entry.snapshot)(storage, s.cache.as_deref()) {
            SnapshotOutcome::Absent | SnapshotOutcome::Unchanged => continue,
            SnapshotOutcome::Changed { bytes, cache } => (bytes, cache),
        };
        let file_bytes = match envelope::wrap(entry.key, entry.version, &bytes) {
            Ok(b) => b,
            Err(e) => { warn!("persist: envelope for {} failed: {e}", entry.key); continue; }
        };
        s.epoch += 1;
        let job = WriteJob {
            world,
            key: entry.key,
            path: path::file_path(world, entry.key),
            bytes: file_bytes,
            epoch: s.epoch,
        };
        if tx.send(job).is_err() {
            warn!("persist: worker gone; cannot persist {}", entry.key);
            continue;
        }
        s.in_flight = Some(s.epoch);
        s.pending = Some(cache);
    }
}
