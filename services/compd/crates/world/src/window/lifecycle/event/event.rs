use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use uuid::Uuid;
use dispatcher::wayland::xdg::activation::dispatch::wire::ActivationDetails;
use dispatcher::wire::trait_::wire_trait::ActivationOrigin;
use crate::window::interface::record::window::LoopWindow;

pub enum WindowLifecycleEvent {
    InitialMap(Window),
    // Resize(Window),
    /// The `bool` is the surface's `DiscardPlaceholder` mark (Shift-close from the
    /// selection toolbar): destroy must leave no placeholder behind.
    Destroyed(Uuid, Vec<ActivationDetails>, bool),
    /// (Un)fullscreen request for a window. `true` = enter fullscreen.
    Fullscreen(Window, bool),
    /// Bring `window` into view (camera `view`) and activate it. Queued by the neutral wire
    /// layer (which can't run the camera logic); `origin` records the source (e.g. a dock via
    /// wlr foreign-toplevel `activate`) for source-specific treatment later.
    Activate(Window, ActivationOrigin),
    /// An `xdg_toplevel_drag_v1` finished carrying this toplevel, so its
    /// placeholder record has to be re-synced to where the carry left it.
    ///
    /// Queued rather than applied directly so it stays ORDERED against the rest:
    /// a tab torn off and dropped inside one frame produces `InitialMap` and this
    /// in the same drain, and the record the settle needs does not exist until
    /// the `InitialMap` ahead of it has been processed.
    DragSettled(WlSurface),
}

impl WindowLifecycleEvent {
    /// Identity is independent of the current world, buffer and registry role.
    /// Teardown still needs to find a stale Space element after its role died.
    pub fn targets(&self, window: &Window) -> bool {
        match self {
            Self::InitialMap(candidate) | Self::Fullscreen(candidate, _)
            | Self::Activate(candidate, _) => candidate == window,
            Self::Destroyed(uuid, _, _) => window.uuid() == Some(*uuid),
            Self::DragSettled(surface) => protocols::window::find::find::is_surface(window, surface),
        }
    }

    /// Match queued identities even after Space membership or the role is gone.
    /// Window-bearing events also cover a surface not yet stamped with its uuid.
    pub fn same_window(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::InitialMap(window) | Self::Fullscreen(window, _) | Self::Activate(window, _), _) => {
                other.targets(window)
            }
            (_, Self::InitialMap(window) | Self::Fullscreen(window, _) | Self::Activate(window, _)) => {
                self.targets(window)
            }
            (Self::Destroyed(a, _, _), Self::Destroyed(b, _, _)) => a == b,
            (Self::DragSettled(a), Self::DragSettled(b)) => a == b,
            (Self::Destroyed(uuid, _, _), Self::DragSettled(surface))
            | (Self::DragSettled(surface), Self::Destroyed(uuid, _, _)) => {
                smithay::wayland::compositor::with_states(surface, |states| {
                    states.data_map
                        .get::<std::sync::Mutex<crate::window::interface::data::data::WindowData>>()
                        .is_some_and(|data| data.lock().unwrap().UUID == *uuid)
                })
            }
        }
    }

    /// A retained tail may live off-world. Every producer must append to that
    /// queue before falling back to the hosted dispatch queue, otherwise a later
    /// request can be serviced before the same window's older work.
    pub fn pending_queue<'a, K>(
        &self,
        queues: impl IntoIterator<Item = (K, &'a [Self])>,
    ) -> Option<K> {
        queues.into_iter().find_map(|(owner, events)| {
            events.iter().any(|pending| self.same_window(pending)).then_some(owner)
        })
    }

    pub fn owning_space<'a>(
        &self,
        spaces: impl IntoIterator<Item = &'a protocols::space::state::SpaceState>,
    ) -> Option<&'a protocols::space::state::SpaceState> {
        spaces.into_iter().find(|space| space.state.elements().any(|window| self.targets(window)))
    }

    /// Retain a live candidate until it has a buffer and its owning Space is
    /// current for placement, plus the events ordered behind it for the same
    /// window. Unrelated windows must keep making progress.
    pub fn defer_for_placement<'a>(
        &self,
        comp: &crate::comp::CompState,
        spaces: impl IntoIterator<Item = &'a protocols::space::state::SpaceState>,
        placement_space: &protocols::space::state::SpaceState,
        waiting: &[Self],
    ) -> bool {
        waiting.iter().any(|event| {
            let Self::InitialMap(candidate) = event else {
                return false;
            };
            self.targets(candidate)
        }) || match self {
            Self::InitialMap(window) => {
                crate::comp::live_window_space(comp, spaces, window).is_some_and(|owner| {
                    !std::ptr::eq(owner, placement_space)
                        || !crate::comp::initial_map_is_live(comp, &owner.state, window)
                })
            }
            _ => false,
        }
    }
}
