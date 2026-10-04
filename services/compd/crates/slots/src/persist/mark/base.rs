use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Maximum total debounce for a non-immediate commit: updates within this window
/// batch into one commit.
const MAX_DEBOUNCE: Duration = Duration::from_millis(1000);

struct Mark {
    immediate: bool,
    first_seen: Instant,
}

static REGISTRY: OnceLock<Mutex<HashMap<Uuid, Mark>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<Uuid, Mark>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Flag a world's persisted state dirty (memory already mutated by the caller).
/// `immediate` = commit at the next frame-end; otherwise debounced (batched up to
/// `MAX_DEBOUNCE`). Re-marking keeps the original debounce deadline; an immediate
/// mark upgrades a pending debounced one.
pub fn mark_world(world: Uuid, immediate: bool) {
    let mut reg = registry().lock().expect("persist mark registry");
    let mark = reg.entry(world).or_insert(Mark { immediate, first_seen: Instant::now() });
    if immediate {
        mark.immediate = true;
    }
}

/// Run `f` (the in-memory mutation) and flag `world` for an end-of-frame commit.
pub fn transact(world: Uuid, immediate: bool, f: impl FnOnce()) {
    f();
    mark_world(world, immediate);
}

/// True (draining the mark) if `world`'s commit is due now — it was marked, and is
/// either immediate or its debounce window elapsed. Checked at the END OF BUFFER
/// REDUCTION (`flow::flush`) for the world that just ran: a trivial timestamp
/// compare, NOT a storage poll, and only when a mutation actually flagged it. This
/// is path 1 — a `transact()` inside `buffer()` commits at its own buffer boundary.
pub fn take_if_due(world: Uuid) -> bool {
    let mut reg = registry().lock().expect("persist mark registry");
    let due = is_due(reg.get(&world));
    if due {
        reg.remove(&world);
    }
    due
}

/// Marked worlds whose debounce is due now, draining them. This is path 2 — the
/// debounce-driven catch-all for RIM mutations (a world flagged by `mark_world`
/// outside `buffer()`, e.g. an overlay world that has since stopped being flushed).
/// Committed at frame-end so the work lands "later on the debounce period, not on
/// the actual frame"; only ever touches worlds a mutation explicitly flagged.
pub fn due_worlds() -> Vec<Uuid> {
    let mut reg = registry().lock().expect("persist mark registry");
    let due: Vec<Uuid> =
        reg.iter().filter(|(_, m)| is_due(Some(m))).map(|(w, _)| *w).collect();
    for w in &due {
        reg.remove(w);
    }
    due
}

fn is_due(mark: Option<&Mark>) -> bool {
    match mark {
        Some(m) => m.immediate || Instant::now().duration_since(m.first_seen) >= MAX_DEBOUNCE,
        None => false,
    }
}
