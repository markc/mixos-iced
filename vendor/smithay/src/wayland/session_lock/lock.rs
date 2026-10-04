//! ext-session-lock lock.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::wayland::compositor::SurfaceAttributes;
use crate::wayland::compositor::{self, BufferAssignment};
use _session_lock::ext_session_lock_surface_v1::ExtSessionLockSurfaceV1;
use _session_lock::ext_session_lock_v1::{Error, ExtSessionLockV1, Request};
use wayland_protocols::ext::session_lock::v1::server::{self as _session_lock};
use wayland_server::protocol::wl_output::WlOutput;
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, Resource};

use crate::wayland::Dispatch2;
use crate::wayland::session_lock::surface::{ExtLockSurfaceUserData, LockSurface, LockSurfaceAttributes};
use crate::wayland::session_lock::{LockStatus, SessionLockHandler};

/// Surface role for ext-session-lock surfaces.
const LOCK_SURFACE_ROLE: &str = "ext_session_lock_surface_v1";

// compd: per-lock output registry that remembers the
// exact lock-surface object owning each output.
// Upstream kept a bare `Vec<WlOutput>` that was never pruned, so a destroyed
// lock surface kept its output "locked" forever (re-creating a surface for the
// same output raised DuplicateOutput), and nothing tied the entry to the surface
// that created it. Removal is by owning surface, never by output, so a stale
// destructor can never erase a different surface's registration.
#[derive(Debug)]
pub(super) struct LockedOutputs<O, S> {
    entries: Vec<(O, S)>,
}

impl<O: PartialEq, S: PartialEq> LockedOutputs<O, S> {
    pub(super) fn new() -> Self {
        Self { entries: Vec::new() }
    }

    pub(super) fn contains_output(&self, output: &O) -> bool {
        self.entries.iter().any(|(entry, _)| entry == output)
    }

    pub(super) fn register(&mut self, output: O, surface: S) {
        self.entries.push((output, surface));
    }

    pub(super) fn remove_surface(&mut self, surface: &S) {
        self.entries.retain(|(_, entry)| entry != surface);
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
}

pub(super) type LockOutputRegistry = Mutex<LockedOutputs<WlOutput, ExtSessionLockSurfaceV1>>;

/// [`ExtSessionLockV1`] state.
#[derive(Debug)]
pub struct SessionLockState {
    pub(super) done: Arc<AtomicBool>,
    // compd: shared (weakly) with each lock surface so its
    // destructor can retire exactly its own entry.
    pub(super) locked_outputs: Arc<LockOutputRegistry>,
}

impl SessionLockState {
    pub(super) fn new() -> Self {
        Self {
            done: Arc::new(AtomicBool::new(false)),
            locked_outputs: Arc::new(Mutex::new(LockedOutputs::new())),
        }
    }

    /// Number of outputs currently claimed by live lock surfaces of this lock object.
    // compd: narrow invariant probe for the registry
    // regressions.
    pub fn locked_output_count(&self) -> usize {
        self.locked_outputs.lock().unwrap().len()
    }
}

impl<D> Dispatch2<ExtSessionLockV1, D> for SessionLockState
where
    D: Dispatch<ExtSessionLockSurfaceV1, ExtLockSurfaceUserData>,
    D: SessionLockHandler,
    D: 'static,
{
    fn request(
        &self,
        state: &mut D,
        _client: &Client,
        lock: &ExtSessionLockV1,
        request: Request,
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            Request::GetLockSurface { id, surface, output } => {
                // compd: bind lock surfaces to the ACCEPTED
                // lock. Upstream's `done` flag already keeps
                // a rejected lock's surfaces away from `new_surface`; this hook lets
                // the compositor refuse a non-owner lock object outright, before it
                // can take a role or claim an output. Default `true` = upstream.
                if !state.lock_object_may_create_surface(lock) {
                    lock.post_error(
                        Error::InvalidUnlock,
                        "Lock object does not own the active lock generation.",
                    );
                    return;
                }

                // Assign surface a role and ensure it never had one before.
                if compositor::give_role(&surface, LOCK_SURFACE_ROLE).is_err() {
                    lock.post_error(Error::Role, "Surface already has a role.");
                    return;
                }

                // compd: validate BEFORE registering the output. Upstream pushed the output first, so an
                // AlreadyConstructed failure leaked a registry entry with no lock
                // surface object left to clean it up.
                //
                // Ensure surface has no existing buffers attached. The protocol's
                // `already_constructed` covers "a buffer attached or committed": any
                // pending attach (including a NULL attach) counts, and commit history
                // Smithay cannot reconstruct is asked of the compositor.
                let has_buffer = compositor::with_states(&surface, |states| {
                    let cached = &states.cached_state;
                    let mut guard = cached.get::<SurfaceAttributes>();
                    let pending = guard.pending().buffer.is_some();
                    let current = matches!(guard.current().buffer, Some(BufferAssignment::NewBuffer(_)));
                    pending || current
                });
                if has_buffer || state.lock_surface_already_constructed(&surface) {
                    lock.post_error(
                        Error::AlreadyConstructed,
                        "Surface was already committed or had a buffer attached.",
                    );
                    return;
                }

                // Ensure output is not already locked.
                if self.locked_outputs.lock().unwrap().contains_output(&output) {
                    lock.post_error(Error::DuplicateOutput, "Output is already locked.");
                    return;
                }

                let data = ExtLockSurfaceUserData {
                    surface: surface.downgrade(),
                    done: Arc::clone(&self.done),
                    locked_outputs: Arc::downgrade(&self.locked_outputs),
                };
                let lock_surface = data_init.init(id, data);

                // compd: register only once every validation has
                // passed, keyed by the owning lock-surface object.
                self.locked_outputs
                    .lock()
                    .unwrap()
                    .register(output.clone(), lock_surface.clone());

                // Initialize surface data.
                compositor::with_states(&surface, |states| {
                    let inserted = states.data_map.insert_if_missing_threadsafe(|| {
                        Mutex::new(LockSurfaceAttributes::new(lock_surface.clone()))
                    });

                    if !inserted {
                        let mut attributes = states
                            .data_map
                            .get::<Mutex<LockSurfaceAttributes>>()
                            .unwrap()
                            .lock()
                            .unwrap();
                        attributes.surface = lock_surface.clone();
                    }
                });

                // Add pre-commit hook for updating surface state.
                compositor::add_pre_commit_hook::<D, _>(&surface, LockSurface::pre_commit_hook);

                if !self.done.load(Ordering::Acquire) {
                    // Call compositor handler.
                    let lock_surface = LockSurface::new(lock.clone(), surface, lock_surface);
                    state.new_surface(lock_surface.clone(), output);

                    // Send initial configure when the interface is bound.
                    lock_surface.send_configure();
                }
            }
            Request::UnlockAndDestroy => {
                // Ensure session is locked, and with the same lock instance.
                // ("A rejected lock cannot unlock" and "unlock consumed exactly
                // once" are upstream behaviour: the
                // owner check is `is_locked_by`, and the status flips to Unlocked
                // before `unlock()` runs on this destructor request.)
                if !state.lock_state().lock_status.lock().unwrap().is_locked_by(lock) {
                    lock.post_error(Error::InvalidUnlock, "Session is not locked.");
                } else {
                    *state.lock_state().lock_status.lock().unwrap() = LockStatus::Unlocked;
                    state.unlock();
                }
            }
            Request::Destroy => {
                // Ensure session is not locked.
                if state.lock_state().lock_status.lock().unwrap().is_locked_by(lock) {
                    lock.post_error(Error::InvalidDestroy, "Cannot destroy session lock while locked.");
                }
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(&self, state: &mut D, _client: wayland_server::backend::ClientId, lock: &ExtSessionLockV1) {
        let was_owner = {
            let mut lock_status = state.lock_state().lock_status.lock().unwrap();
            if lock_status.is_locked_by(lock) {
                // The client has disconnected without unlocking the session, so reset our state.  It
                // is up to the compositor's policy to decide whether it is allowed for another client
                // to connect and take over the session-locker responsibility.
                *lock_status = LockStatus::Defunct;
                true
            } else {
                false
            }
        };

        // compd: bind the Locking lifetime to the lock object. Destroying the lock while Locking (or after
        // rejection) is legal; mark it done so its surfaces stop reaching the
        // compositor and a late `SessionLocker::lock()` cannot install a
        // `Locked` status owned by a dead object (an un-unlockable session).
        if !was_owner {
            self.done.store(true, Ordering::Release);
        }
        state.lock_destroyed(lock.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::LockedOutputs;

    /// Production part of a source file (everything before its test module).
    fn prod(src: &'static str) -> &'static str {
        src.split("#[cfg(test)]").next().unwrap()
    }

    fn pos(haystack: &str, needle: &str) -> usize {
        assert_eq!(
            haystack.matches(needle).count(),
            1,
            "needle {needle:?} must occur exactly once in production code"
        );
        haystack.find(needle).unwrap()
    }

    // compd guard
    // `session_lock_repeated_already_constructed_failures_do_not_grow_output_registry`
    // and `..._finished_same_client_object_cannot_create_active_generation_surfaces`:
    // the accepted-lock check precedes the role, every validation precedes the
    // output registration, and registration needs the initialised object.
    #[test]
    fn get_lock_surface_validates_before_registering_output() {
        let src = prod(include_str!("lock.rs"));
        let may_create = pos(src, "state.lock_object_may_create_surface(lock)");
        let role = pos(src, "compositor::give_role(&surface, LOCK_SURFACE_ROLE)");
        let constructed = pos(src, "state.lock_surface_already_constructed(&surface)");
        let duplicate = pos(src, ".contains_output(&output)");
        let init = pos(src, "data_init.init(id, data)");
        let register = pos(src, ".register(output.clone(), lock_surface.clone())");
        assert!(may_create < role, "accepted-lock check must precede give_role");
        assert!(role < constructed && constructed < duplicate);
        assert!(duplicate < init && init < register, "register only after all validation");
    }

    // compd guard
    // `session_lock_destroyed_during_locking_aborts_and_ignores_late_completion`.
    #[test]
    fn lock_destroy_aborts_locking_and_late_lock_is_inert() {
        let src = prod(include_str!("lock.rs"));
        let destroyed = pos(src, "fn destroyed(&self, state: &mut D");
        let done = pos(src, "self.done.store(true, Ordering::Release)");
        let hook = pos(src, "state.lock_destroyed(lock.clone())");
        assert!(destroyed < done && done < hook);

        let module = prod(include_str!("mod.rs"));
        let lock_fn = pos(module, "pub fn lock(mut self)");
        let gate = pos(module, "if self.done.load(Ordering::Acquire) || !lock.is_alive()");
        let install = pos(module, "LockStatus::Locked(lock.clone())");
        assert!(lock_fn < gate && gate < install, "a destroyed lock must never become Locked");
    }

    // compd guard — "a rejected lock cannot unlock" (upstream behaviour; this
    // pins the upstream owner check ahead of `unlock()`).
    #[test]
    fn unlock_requires_the_owning_lock() {
        let src = prod(include_str!("lock.rs"));
        let arm = pos(src, "Request::UnlockAndDestroy =>");
        let owner = pos(src, "if !state.lock_state().lock_status.lock().unwrap().is_locked_by(lock) {");
        let unlock = pos(src, "state.unlock();");
        assert!(arm < owner && owner < unlock);
    }

    // compd guard
    // `session_lock_aborted_generation_releases_output_without_stale_destructor_aliasing`.
    #[test]
    fn registry_removes_by_owning_surface_only() {
        let mut registry = LockedOutputs::<u32, &str>::new();
        registry.register(1, "old");
        assert!(registry.contains_output(&1));
        // Old surface destroyed: the output is free again.
        registry.remove_surface(&"old");
        assert!(!registry.contains_output(&1));
        assert_eq!(registry.len(), 0);

        registry.register(1, "new");
        registry.register(2, "other");
        // A stale destructor for a surface that no longer owns anything is a no-op.
        registry.remove_surface(&"old");
        assert!(registry.contains_output(&1));
        assert_eq!(registry.len(), 2);
        registry.remove_surface(&"new");
        assert!(!registry.contains_output(&1));
        assert!(registry.contains_output(&2));
    }
}
