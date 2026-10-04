//! ext-session-lock-v1 (integration batch C).
//!
//! What the protocol must decide on the spot is decided here, in the
//! handlers: whether a lock is accepted (none active, the owner alive, an
//! output to show it on), which lock object may create surfaces, each lock
//! surface's immediate configure at its output's size, and the
//! Locking → Unlocked abort / Locked → Orphaned transitions when the lock
//! object or its client goes. What the lock does to the rest of the
//! compositor (input teardown, focus, drawing, the Bus surface) is the comp
//! policy's, in world `comp::session_lock`, which reads [`SessionLock`].
//!
//! `locked` is sent only once every output has PRESENTED a lock frame (the
//! blank, plus whatever lock surface it has) since the lock began: the scene
//! marks a frame built ([`SessionLock::mark_built`]), the present path marks
//! it presented ([`SessionLock::mark_presented`]), and the policy confirms
//! ([`SessionLock::confirm_if_presented`]). There is no deadline, and an
//! orphaned lock (owner gone while Locked) stays until restart.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use smithay::output::Output;
use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_v1::ExtSessionLockV1;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DisplayHandle, Resource};
use smithay::wayland::session_lock::{
    LockSurface, LockSurfaceConfigure, SessionLockHandler, SessionLockManagerState, SessionLocker,
};

use crate::state::state::{Dispatch, RedrawReason};
use crate::wire::trait_::surface_event::{SurfaceEvent, SurfaceHandle};

/// `focus.session_lock` (the wire values; `unlocking` is a KMS-gate phase
/// with no compd counterpart yet).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Unlocked,
    Locking,
    Locked,
    /// The owner died while Locked: the blank stays, nothing can unlock it.
    Orphaned,
}

impl Phase {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Unlocked => "none",
            Self::Locking => "locking",
            Self::Locked => "locked",
            Self::Orphaned => "orphaned",
        }
    }

    pub const fn active(self) -> bool {
        !matches!(self, Self::Unlocked)
    }
}

pub struct SessionLock {
    manager: SessionLockManagerState,
    phase: Phase,
    resource: Option<ExtSessionLockV1>,
    locker: Option<SessionLocker>,
    /// Bumped by every accepted lock.
    generation: u64,
    /// The lock surfaces, by output name.
    surfaces: BTreeMap<String, LockSurface>,
    /// Outputs that drew / presented a lock frame in this generation. Cells:
    /// the scene and the present path hold the loop shared.
    built: RefCell<BTreeSet<String>>,
    presented: RefCell<BTreeSet<String>>,
}

impl SessionLock {
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            // Any client may lock (agentic-first: no gate on who).
            manager: SessionLockManagerState::new::<Dispatch, _>(display, |_client: &Client| true),
            phase: Phase::Unlocked,
            resource: None,
            locker: None,
            generation: 0,
            surfaces: BTreeMap::new(),
            built: RefCell::new(BTreeSet::new()),
            presented: RefCell::new(BTreeSet::new()),
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The lock surface on `output`, if the lock has one there.
    pub fn surface_for(&self, output: &str) -> Option<&LockSurface> {
        self.surfaces.get(output)
    }

    /// Whether `surface` is one of this lock's surfaces.
    pub fn owns(&self, surface: &WlSurface) -> bool {
        self.surfaces.values().any(|lock| lock.wl_surface() == surface)
    }

    pub fn surfaces(&self) -> impl Iterator<Item = (&String, &LockSurface)> {
        self.surfaces.iter()
    }

    /// The scene drew a lock frame for `output` in this generation.
    pub fn mark_built(&self, output: &str) {
        if self.phase.active() {
            self.built.borrow_mut().insert(output.to_string());
        }
    }

    /// `output` presented a frame; it counts only if a lock frame was built
    /// for it in this generation.
    pub fn mark_presented(&self, output: &str) {
        if self.phase == Phase::Locking && self.built.borrow().contains(output) {
            self.presented.borrow_mut().insert(output.to_string());
        }
    }

    /// Send `locked` once every one of `outputs` presented a lock frame.
    /// Returns whether the lock was confirmed now.
    pub fn confirm_if_presented<'a>(&mut self, outputs: impl IntoIterator<Item = &'a str>) -> bool {
        if self.phase != Phase::Locking {
            return false;
        }
        let presented = self.presented.borrow();
        let mut any = false;
        for output in outputs {
            any = true;
            if !presented.contains(output) {
                return false;
            }
        }
        drop(presented);
        if !any {
            return false;
        }
        if let Some(locker) = self.locker.take() {
            locker.lock();
        }
        self.phase = Phase::Locked;
        true
    }

    /// Back to Unlocked: the lock is gone (unlocked, or aborted while
    /// Locking). A pending locker is dropped, which answers `finished`.
    fn clear(&mut self) {
        self.phase = Phase::Unlocked;
        self.resource = None;
        self.locker = None;
        self.surfaces.clear();
        self.built.borrow_mut().clear();
        self.presented.borrow_mut().clear();
    }
}

/// An output's logical size (mode through transform and scale): what a lock
/// surface on it is configured to.
fn logical_size(output: &Output) -> Option<(u32, u32)> {
    let mode = output.current_mode()?;
    let size = output
        .current_transform()
        .transform_size(mode.size)
        .to_f64()
        .to_logical(output.current_scale().fractional_scale())
        .to_i32_round::<i32>();
    Some((u32::try_from(size.w).ok()?, u32::try_from(size.h).ok()?))
}

impl SessionLockHandler for Dispatch {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock.manager
    }

    /// A lock request: refused (the dropped locker answers `finished`) while
    /// a lock is active, when the owner is gone, or with no output to show it
    /// on.
    fn lock(&mut self, confirmation: SessionLocker) {
        let lock = &mut self.session_lock;
        if lock.phase.active() {
            return;
        }
        if confirmation.ext_session_lock().client().is_none() {
            return;
        }
        if self.outputs_snapshot.is_empty() {
            return;
        }
        lock.generation = lock.generation.wrapping_add(1);
        lock.phase = Phase::Locking;
        lock.resource = Some(confirmation.ext_session_lock().clone());
        lock.locker = Some(confirmation);
        lock.surfaces.clear();
        lock.built.borrow_mut().clear();
        lock.presented.borrow_mut().clear();
        self.force_redraw(RedrawReason::Output);
    }

    /// The owner unlocked (the vendor calls this only for the owner).
    fn unlock(&mut self) {
        if self.session_lock.phase != Phase::Locked {
            return;
        }
        self.session_lock.clear();
        self.force_redraw(RedrawReason::Output);
    }

    fn new_surface(&mut self, surface: LockSurface, wl_output: WlOutput) {
        if !matches!(self.session_lock.phase, Phase::Locking | Phase::Locked) {
            return;
        }
        let Some(output) = Output::from_resource(&wl_output) else {
            return;
        };
        // One logical size for the output, configured at once: the initial
        // configure is immediate and exactly the output's size.
        if let Some(size) = logical_size(&output) {
            surface.with_pending_state(|state| state.size = Some(size.into()));
        }
        surface.send_configure();
        self.push_surface_event(SurfaceEvent::RoleTaken {
            handle: SurfaceHandle::wl(surface.wl_surface()),
            role: surfaces::SurfaceRole::Lock,
            parent: None,
        });
        self.session_lock.surfaces.insert(output.name(), surface);
        self.force_redraw(RedrawReason::Map);
    }

    fn ack_configure(&mut self, _surface: WlSurface, _configure: LockSurfaceConfigure) {}

    /// Only the active lock object creates surfaces.
    fn lock_object_may_create_surface(&self, lock: &ExtSessionLockV1) -> bool {
        matches!(self.session_lock.phase, Phase::Locking | Phase::Locked)
            && self.session_lock.resource.as_ref() == Some(lock)
    }

    fn lock_surface_destroyed(&mut self, surface: WlSurface) {
        let lock = &mut self.session_lock;
        let before = lock.surfaces.len();
        lock.surfaces.retain(|_, entry| entry.wl_surface() != &surface);
        if lock.surfaces.len() != before {
            self.push_surface_event(SurfaceEvent::Dormant(SurfaceHandle::wl(&surface)));
            self.force_redraw(RedrawReason::Unmap);
        }
    }

    /// The lock object went (destroyed, or its client died): Locking aborts
    /// to Unlocked; Locked becomes Orphaned (the blank stays).
    fn lock_destroyed(&mut self, lock: ExtSessionLockV1) {
        let ours = self.session_lock.resource.as_ref() == Some(&lock);
        if !ours {
            return;
        }
        match self.session_lock.phase {
            Phase::Locking => self.session_lock.clear(),
            Phase::Locked => {
                let state = &mut self.session_lock;
                state.phase = Phase::Orphaned;
                state.resource = None;
                state.surfaces.clear();
            }
            Phase::Unlocked | Phase::Orphaned => {}
        }
        self.force_redraw(RedrawReason::Output);
    }
}
