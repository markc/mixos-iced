//! ext-session-lock surface.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak as SyncWeak};

use crate::backend::renderer::buffer_dimensions;
use crate::utils::{IsAlive, Logical, SERIAL_COUNTER, Serial, Size};
use crate::wayland::compositor::{self, BufferAssignment, Cacheable, SurfaceAttributes};
use crate::wayland::viewporter::{ViewportCachedState, ViewporterSurfaceState};
use _session_lock::ext_session_lock_surface_v1::{Error, ExtSessionLockSurfaceV1, Request};
use tracing::trace_span;
use wayland_protocols::ext::session_lock::v1::server::ext_session_lock_v1::ExtSessionLockV1;
use wayland_protocols::ext::session_lock::v1::server::{self as _session_lock, ext_session_lock_surface_v1};
use wayland_server::protocol::wl_buffer::WlBuffer;
use wayland_server::protocol::wl_surface::WlSurface;
use wayland_server::{Client, DataInit, DisplayHandle, Resource, Weak};

use crate::wayland::Dispatch2;
use crate::wayland::session_lock::SessionLockHandler;
use crate::wayland::session_lock::lock::{LockOutputRegistry, SessionLockState};

/// User data for ext-session-lock surfaces.
#[derive(Debug)]
pub struct ExtLockSurfaceUserData {
    // `LockSurfaceAttributes` stored in the surface `data_map` contains a
    // `ExtSessionLockSurfaceV1`. So this reference needs to be weak to avoid a
    // cycle.
    pub(crate) surface: Weak<WlSurface>,
    pub(super) done: Arc<AtomicBool>,
    // compd: the owning lock's output registry, so this
    // surface's destructor releases exactly its own output.
    // Weak: the registry holds this object's resource handle.
    pub(super) locked_outputs: SyncWeak<LockOutputRegistry>,
}

impl<D> Dispatch2<ExtSessionLockSurfaceV1, D> for ExtLockSurfaceUserData
where
    D: SessionLockHandler,
    D: 'static,
{
    fn request(
        &self,
        state: &mut D,
        _client: &Client,
        lock_surface: &ExtSessionLockSurfaceV1,
        request: Request,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            Request::AckConfigure { serial } => {
                let Ok(surface) = self.surface.upgrade() else {
                    return;
                };

                // Find configure for this serial.
                let serial = Serial::from(serial);
                let configure = compositor::with_states(&surface, |states| {
                    let surface_data = states.data_map.get::<Mutex<LockSurfaceAttributes>>();
                    surface_data.unwrap().lock().unwrap().ack_configure(serial)
                });

                match configure {
                    Some(configure) => {
                        if !self.done.load(Ordering::Acquire) {
                            state.ack_configure(surface.clone(), configure);
                        }
                    }
                    None => lock_surface.post_error(
                        Error::InvalidSerial,
                        format!("wrong configure serial: {}", <u32>::from(serial)),
                    ),
                }
            }
            Request::Destroy => (),
            _ => unreachable!(),
        }
    }

    fn destroyed(
        &self,
        state: &mut D,
        _client: wayland_server::backend::ClientId,
        resource: &ExtSessionLockSurfaceV1,
    ) {
        // compd: release exactly this surface's output claim
        // (by owning object, never by output).
        if let Some(registry) = self.locked_outputs.upgrade() {
            registry.lock().unwrap().remove_surface(resource);
        }

        if let Ok(surface) = self.surface.upgrade() {
            compositor::with_states(&surface, |states| {
                let mut attributes = states
                    .data_map
                    .get::<Mutex<LockSurfaceAttributes>>()
                    .unwrap()
                    .lock()
                    .unwrap();
                attributes.reset();

                let mut guard = states.cached_state.get::<LockSurfaceCachedState>();
                *guard.pending() = Default::default();
                *guard.current() = Default::default();
            });

            // compd: tell the compositor the role object is
            // gone so its per-output ownership cannot go stale.
            state.lock_surface_destroyed(surface);
        }
    }
}

/// Data associated with session lock surface
///
/// ```no_run
/// use smithay::wayland::compositor;
/// use smithay::wayland::session_lock::LockSurfaceData;
///
/// # let wl_surface = todo!();
/// compositor::with_states(&wl_surface, |states| {
///     states.data_map.get::<LockSurfaceData>();
/// });
/// ```
pub type LockSurfaceData = Mutex<LockSurfaceAttributes>;

/// Attributes for ext-session-lock surfaces.
#[derive(Debug)]
pub struct LockSurfaceAttributes {
    pub(crate) surface: ext_session_lock_surface_v1::ExtSessionLockSurfaceV1,

    /// Holds the pending state as set by the server.
    pub server_pending: Option<LockSurfaceState>,

    /// Holds the configures the server has sent out to the client waiting to be
    /// acknowledged by the client. All pending configures that are older than
    /// the acknowledged one will be discarded during processing
    /// layer_surface.ack_configure.
    pub pending_configures: Vec<LockSurfaceConfigure>,

    /// Holds the last configure that has been acknowledged by the client. This state should be
    /// cloned to the current during a commit. Note that this state can be newer than the last
    /// acked state at the time of the last commit.
    pub last_acked: Option<LockSurfaceConfigure>,

    // compd: the effective wl_surface buffer retained across
    // empty commits. SurfaceAttributes' current buffer is
    // consumed by the compositor after commit, so Smithay keeps its own ledger to
    // revalidate a retained buffer against a newly acked size.
    pub(crate) effective_buffer: Option<WlBuffer>,
}

impl LockSurfaceAttributes {
    pub(crate) fn new(surface: ext_session_lock_surface_v1::ExtSessionLockSurfaceV1) -> Self {
        Self {
            surface,
            server_pending: None,
            pending_configures: vec![],
            last_acked: None,
            effective_buffer: None,
        }
    }

    fn ack_configure(&mut self, serial: Serial) -> Option<LockSurfaceConfigure> {
        let configure = self
            .pending_configures
            .iter()
            .find(|configure| configure.serial == serial)
            .cloned()?;

        self.pending_configures
            .retain(|configure| configure.serial > serial);
        self.last_acked = Some(configure);

        Some(configure)
    }

    fn reset(&mut self) {
        self.server_pending = None;
        self.pending_configures = Vec::new();
        self.last_acked = None;
        // compd: a re-created lock surface starts with no
        // effective buffer.
        self.effective_buffer = None;
    }

    fn current_server_state(&self) -> Option<&LockSurfaceState> {
        self.pending_configures
            .last()
            .map(|c| &c.state)
            .or(self.last_acked.as_ref().map(|c| &c.state))
    }
}

/// Handle for a ext-session-lock surface.
#[derive(Clone, Debug)]
pub struct LockSurface {
    lock: ExtSessionLockV1,
    shell_surface: ExtSessionLockSurfaceV1,
    surface: WlSurface,
}

impl PartialEq for LockSurface {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.surface == other.surface
    }
}

impl LockSurface {
    pub(crate) fn new(
        lock: ExtSessionLockV1,
        surface: WlSurface,
        shell_surface: ExtSessionLockSurfaceV1,
    ) -> Self {
        Self {
            lock,
            surface,
            shell_surface,
        }
    }

    /// Check if the surface is still alive.
    #[inline]
    pub fn alive(&self) -> bool {
        self.surface.alive()
    }

    /// Returns the lock instance this surface is associated with.
    pub fn ext_session_lock(&self) -> &ExtSessionLockV1 {
        &self.lock
    }

    /// Get the current pending configure state.
    pub fn get_pending_state(&self, attributes: &mut LockSurfaceAttributes) -> Option<LockSurfaceState> {
        let server_pending = attributes.server_pending.take()?;

        // Check if last state matches pending state.
        match attributes.current_server_state() {
            Some(state) if state == &server_pending => None,
            _ => Some(server_pending),
        }
    }

    /// Send a configure to the surface.
    ///
    /// You can manipulate the client's state using
    /// [`LockSurface::with_pending_state`].
    pub fn send_configure(&self) {
        let _ = self.send_configure_with_serial();
    }

    /// Send a configure and return the serial queued for the client, or `None` if
    /// there was no new state to configure.
    // compd: lets a compositor-side configure ledger record the
    // ext-session-lock serial without reaching into Smithay's queue.
    pub fn send_configure_with_serial(&self) -> Option<Serial> {
        compositor::with_states(&self.surface, |states| {
            // Get surface attributes.
            let attributes = states.data_map.get::<Mutex<LockSurfaceAttributes>>();
            let mut attributes = attributes.unwrap().lock().unwrap();

            // Create our new configure event.
            let pending = match self.get_pending_state(&mut attributes) {
                Some(pending) => pending,
                None => return None,
            };
            let configure = LockSurfaceConfigure::new(pending);

            // Extract client configure state.
            let (width, height) = configure.state.size.unwrap_or_default().into();
            let serial = configure.serial;

            // Update pending state.
            attributes.pending_configures.push(configure);

            // Send configure to the client.
            self.shell_surface.configure(serial.into(), width, height);

            Some(serial)
        })
    }

    /// Release this lock surface's output claim without destroying it.
    ///
    /// For when the physical output went away (hot unplug / replacement) while the
    /// protocol object lives on: the ordinary destructor cleanup would otherwise keep
    /// the dead output registered on this lock.
    // compd: upstream's registry is per lock object, so this is a method on the surface.
    pub fn retire_output_registration(&self) {
        if let Some(lock_state) = self.lock.data::<SessionLockState>() {
            lock_state
                .locked_outputs
                .lock()
                .unwrap()
                .remove_surface(&self.shell_surface);
        }
    }

    /// Access the underlying [`WlSurface`].
    #[inline]
    pub fn wl_surface(&self) -> &WlSurface {
        &self.surface
    }

    /// Manipulate this surface's pending state.
    pub fn with_pending_state<F, T>(&self, f: F) -> T
    where
        F: FnOnce(&mut LockSurfaceState) -> T,
    {
        compositor::with_states(&self.surface, |states| {
            let attributes = states.data_map.get::<Mutex<LockSurfaceAttributes>>();
            let mut attributes = attributes.unwrap().lock().unwrap();

            // Ensure pending state is initialized.
            if attributes.server_pending.is_none() {
                attributes.server_pending =
                    Some(attributes.current_server_state().cloned().unwrap_or_default());
            }

            let server_pending = attributes.server_pending.as_mut().unwrap();
            f(server_pending)
        })
    }

    /// Provides access to the current committed cached state.
    pub fn with_cached_state<F, T>(&self, f: F) -> T
    where
        F: FnOnce(&LockSurfaceCachedState) -> T,
    {
        compositor::with_states(&self.surface, |states| {
            let mut guard = states.cached_state.get::<LockSurfaceCachedState>();
            f(guard.current())
        })
    }

    /// Provides access to the current committed state.
    ///
    /// This is the state that the client last acked before making the current commit.
    pub fn with_committed_state<F, T>(&self, f: F) -> T
    where
        F: FnOnce(Option<&LockSurfaceState>) -> T,
    {
        self.with_cached_state(move |state| f(state.last_acked.as_ref().map(|c| &c.state)))
    }

    /// Handles the role specific commit error checking
    ///
    /// This should be called when the underlying WlSurface
    /// handles a wl_surface.commit request.
    pub(crate) fn pre_commit_hook<D: 'static>(_state: &mut D, _dh: &DisplayHandle, surface: &WlSurface) {
        let _span = trace_span!("session-lock-surface pre-commit", surface = %surface.id()).entered();

        compositor::with_states(surface, |states| {
            let mut role = states.data_map.get::<LockSurfaceData>().unwrap().lock().unwrap();

            let Some(last_acked) = role.last_acked else {
                role.surface.post_error(
                    ext_session_lock_surface_v1::Error::CommitBeforeFirstAck,
                    "Committed before the first ack_configure.",
                );
                return;
            };
            let LockSurfaceConfigure { state, serial: _ } = &last_acked;

            let mut guard_layer = states.cached_state.get::<LockSurfaceCachedState>();
            let pending = guard_layer.pending();

            // compd: validate the EFFECTIVE buffer after this
            // commit, not only an explicit pending assignment.
            // "Committing the surface with a null buffer at any time is a protocol
            // error": an acked empty first commit has no buffer (NullBuffer), and an
            // empty commit after a resize retains the old buffer, which must still
            // match the newly acked size (upstream skipped both).
            let mut guard_surface = states.cached_state.get::<SurfaceAttributes>();
            let surface_attrs = guard_surface.pending();
            let assignment = surface_attrs.buffer.as_ref().map(|assignment| match assignment {
                BufferAssignment::Removed => None,
                BufferAssignment::NewBuffer(buffer) => Some(buffer.clone()),
            });
            let Some(buffer) = resolve_effective_buffer(assignment, &mut role.effective_buffer) else {
                role.surface.post_error(
                    ext_session_lock_surface_v1::Error::NullBuffer,
                    "Surface commit has no effective buffer.",
                );
                return;
            };

            // Verify buffer size.
            if let Some(buf_size) = buffer_dimensions(&buffer) {
                let viewport = states
                    .data_map
                    .get::<ViewporterSurfaceState>()
                    .map(|v| v.lock().unwrap());
                let surface_size = if let Some(dest) = viewport.as_ref().and_then(|_| {
                    let mut guard = states.cached_state.get::<ViewportCachedState>();
                    let viewport_state = guard.pending();
                    viewport_state.dst
                }) {
                    Size::from((dest.w as u32, dest.h as u32))
                } else {
                    let scale = surface_attrs.buffer_scale;
                    let transform = surface_attrs.buffer_transform.into();
                    let surface_size = buf_size.to_logical(scale, transform);

                    Size::from((surface_size.w as u32, surface_size.h as u32))
                };

                if Some(surface_size) != state.size {
                    role.surface.post_error(
                        ext_session_lock_surface_v1::Error::DimensionsMismatch,
                        "Surface dimensions do not match acked configure.",
                    );
                    return;
                }
            }

            // The surface is (and stays) mapped: lock surfaces can never commit without
            // an effective buffer, so they cannot unmap. Track the last acked state.
            pending.last_acked = Some(last_acked);
        });
    }
}

/// State of an ext-session-lock surface.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct LockSurfaceState {
    /// The suggested size of the surface.
    pub size: Option<Size<u32, Logical>>,
}

/// A configure message for ext-session-lock surfaces.
#[derive(Debug, Copy, Clone)]
pub struct LockSurfaceConfigure {
    /// The state associated with this configure.
    pub state: LockSurfaceState,

    /// A serial number to track acknowledgment from the client.
    pub serial: Serial,
}

impl LockSurfaceConfigure {
    fn new(state: LockSurfaceState) -> Self {
        Self {
            serial: SERIAL_COUNTER.next_serial(),
            state,
        }
    }
}

/// Represents the client pending state
#[derive(Debug, Default, Copy, Clone)]
pub struct LockSurfaceCachedState {
    /// Configure last acknowledged by the client at the time of the commit.
    ///
    /// Reset to `None` when the surface unmaps.
    pub last_acked: Option<LockSurfaceConfigure>,
}

impl Cacheable for LockSurfaceCachedState {
    fn commit(&mut self, _dh: &DisplayHandle) -> Self {
        *self
    }
    fn merge_into(self, into: &mut Self, _dh: &DisplayHandle) {
        *into = self;
    }
}

// compd: the effective buffer after a commit.
// `assignment` is the pending wl_surface.attach: `None` = no attach this commit (the
// retained buffer stays effective), `Some(None)` = NULL attach, `Some(Some(b))` = new
// buffer. Updates the retained ledger and returns the buffer the commit will show.
fn resolve_effective_buffer<B: Clone>(assignment: Option<Option<B>>, retained: &mut Option<B>) -> Option<B> {
    if let Some(assignment) = assignment {
        *retained = assignment;
    }
    retained.clone()
}

#[cfg(test)]
mod tests {
    use super::resolve_effective_buffer;

    // compd guard
    // `session_lock_acked_empty_first_commit_is_null_buffer_error`.
    #[test]
    fn empty_first_commit_has_no_effective_buffer() {
        let mut retained: Option<u32> = None;
        assert_eq!(resolve_effective_buffer(None, &mut retained), None);
    }

    // compd guard
    // `session_lock_resize_empty_commit_revalidates_retained_buffer_size`: an empty
    // commit after a mapped commit still yields the retained buffer, so its size is
    // checked against the newly acked configure.
    #[test]
    fn empty_commit_after_attach_keeps_the_retained_buffer() {
        let mut retained = None;
        assert_eq!(resolve_effective_buffer(Some(Some(7u32)), &mut retained), Some(7));
        assert_eq!(resolve_effective_buffer(None, &mut retained), Some(7));
        assert_eq!(resolve_effective_buffer(Some(Some(8)), &mut retained), Some(8));
        assert_eq!(resolve_effective_buffer(None, &mut retained), Some(8));
    }

    // compd guard: a NULL attach is never an effective buffer,
    // even with a buffer retained from an earlier commit.
    #[test]
    fn null_attach_clears_the_effective_buffer() {
        let mut retained = Some(7u32);
        assert_eq!(resolve_effective_buffer(Some(None), &mut retained), None);
        assert_eq!(retained, None);
    }

    // compd guard: the commit hook validates the effective buffer
    // (not only an explicit pending NewBuffer) and errors with NullBuffer without one.
    #[test]
    fn pre_commit_hook_checks_the_effective_buffer() {
        let src = include_str!("surface.rs").split("#[cfg(test)]").next().unwrap();
        let hook = src.find("pub(crate) fn pre_commit_hook").unwrap();
        let resolve = src
            .find("resolve_effective_buffer(assignment, &mut role.effective_buffer)")
            .unwrap();
        let null = src.find("\"Surface commit has no effective buffer.\"").unwrap();
        let dims = src.find("buffer_dimensions(&buffer)").unwrap();
        assert!(hook < resolve && resolve < null && null < dims);
        assert!(!src.contains("had_buffer_before"), "empty commits must not bypass validation");
    }
}
