//! Surface lifecycle events for the comp surface registry.
//!
//! The registry (`surfaces::Registry<SurfaceHandle>`) lives on the host
//! (world's `Orchestrator`). The world-free handlers here cannot reach it,
//! so they queue events on `Dispatch::surface_events`, which `drain_protocol`
//! hands to [`WireTrait::surface_event`](super::wire_trait::WireTrait::surface_event)
//! in arrival order. The drain arms and the frame hook, which do have the host,
//! call it directly.

use surfaces::SurfaceRole;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::backend::ObjectId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::xwayland::X11Surface;

/// What a registry record is keyed on. An X11 window is keyed on its X window
/// id for its whole life: its `wl_surface` association arrives late, can
/// change, and may already be gone when the X server reports the window
/// dead. The `wl_surface` Xwayland backs it with therefore never has a record
/// of its own.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum SurfaceHandle {
    Wl(ObjectId),
    X11(u32),
}

impl SurfaceHandle {
    pub fn wl(surface: &WlSurface) -> Self {
        Self::Wl(surface.id())
    }

    pub fn x11(surface: &X11Surface) -> Self {
        Self::X11(surface.window_id())
    }

    /// The record a focused or hovered `wl_surface` belongs to: the X window an
    /// Xwayland-backed surface carries, else the surface itself.
    pub fn resolve(surface: &WlSurface) -> Self {
        match x11_wm::focus::focus::indexed(surface) {
            Some(x11) => Self::x11(&x11),
            None => Self::wl(surface),
        }
    }

    /// The record a `Window` is keyed on: its toplevel's surface, or its X window.
    pub fn of_window(window: &smithay::desktop::Window) -> Option<Self> {
        if let Some(x11) = window.x11_surface() {
            return Some(Self::x11(x11));
        }
        window.toplevel().map(|toplevel| Self::wl(toplevel.wl_surface()))
    }
}

#[derive(Clone, Debug)]
pub enum SurfaceEvent {
    /// The surface took `role` (a new surface gets an id, a known one keeps
    /// it; either way a fresh generation).
    RoleTaken {
        handle: SurfaceHandle,
        role: SurfaceRole,
        parent: Option<SurfaceHandle>,
    },
    /// The role object went away while the surface lives on.
    Dormant(SurfaceHandle),
    /// The surface (or the X window) is gone.
    Destroyed(SurfaceHandle),
    /// A committed surface's buffer state after this iteration's commits:
    /// a buffer attached or not. Maps buffer-mapped roles, unmaps every role
    /// (a toplevel maps only once it is placed: [`Self::Placed`]).
    Buffer {
        handle: SurfaceHandle,
        attached: bool,
    },
    /// A window was placed and mapped (the frame hook's initial map, an X11
    /// readmit, an X11 window tracked as a popup).
    Placed(SurfaceHandle),
    /// A window's title and app id, both read afresh (xdg `set_title` /
    /// `set_app_id`, X11 `WM_NAME` / `WM_CLASS`, an X11 window's role take).
    Names {
        handle: SurfaceHandle,
        title: Option<String>,
        app_id: Option<String>,
    },
    /// The PRIMARY seat's keyboard focus moved (`None`: nothing focused,
    /// which smithay now reports too), resolved with [`SurfaceHandle::resolve`].
    Focus(Option<SurfaceHandle>),
    /// A layer surface named its output (`explicit`) or left it to the
    /// compositor (the `layer.binding` prop: `explicit` / `default`).
    LayerBinding { handle: SurfaceHandle, explicit: bool },
    /// A client asked for a window state (xdg `set_maximized` /
    /// `unset_maximized` / `set_minimized`, the X11 `_NET_WM_STATE` and
    /// `WM_CHANGE_STATE` equivalents). Queued for the host's policy, which
    /// answers it after the drain.
    Request { handle: SurfaceHandle, request: WindowRequest },
    /// An interactive move/resize grab, driven by
    /// `wayland::grab::interactive::InteractiveGrab`.
    Interactive { handle: SurfaceHandle, op: InteractiveOp },
    /// A layer surface acknowledged a configure: its
    /// client is alive, and an ack at or after a liveness probe's serial
    /// answers the probe.
    LayerAck { handle: SurfaceHandle, serial: smithay::utils::Serial },
    /// An EWMH `_NET_CURRENT_DESKTOP` root request: a pager
    /// asks for the 0-based desktop to become current; the host's policy
    /// decides.
    CurrentDesktop(u32),
    /// An X11 window's `WM_TRANSIENT_FOR` owner (`None`: it names none),
    /// read after its role take and again on every property change
    /// (integration E4). A move relabels the override-redirect windows that
    /// name the moved one.
    TransientFor { handle: SurfaceHandle, owner: Option<SurfaceHandle> },
}

/// One step of an interactive move/resize grab.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InteractiveOp {
    /// A grab began: a move (`edges` 0) or a resize on `edges` (the
    /// `xdg_toplevel.resize_edge` bits: top 1, bottom 2, left 4, right 8).
    Begin { edges: u32 },
    /// The pointer is this far from where the grab began.
    Update { dx: f64, dy: f64 },
    /// The grab ended (button released, or the grab was replaced).
    End,
}

/// A window state a client asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowRequest {
    Maximize(bool),
    Minimize,
    /// The window's own close control was used (compd's server-side chrome):
    /// the host answers it with the polite close.
    Close,
    /// X11 `WM_CHANGE_STATE` NormalState: restore a minimised window
    /// (integration E2).
    Unminimize,
    /// EWMH `_NET_ACTIVE_WINDOW`: focus and raise this window (E2).
    Activate,
    /// EWMH `_NET_WM_DESKTOP`: move this window to the 0-based desktop (E2;
    /// `0xFFFFFFFF`, all desktops, is refused).
    Desktop(u32),
    /// An X11 ConfigureRequest restacking to the top (`Reorder::Top`): raise,
    /// no focus. Relative restacks are ignored: the compositor owns the
    /// stacking.
    Raise,
}

fn non_empty(value: String) -> Option<String> {
    Some(value).filter(|value| !value.is_empty())
}

impl SurfaceEvent {
    /// An xdg toplevel's current title and app id.
    pub fn xdg_names(toplevel: &smithay::wayland::shell::xdg::ToplevelSurface) -> Self {
        let (title, app_id) = smithay::wayland::compositor::with_states(toplevel.wl_surface(), |states| {
            states
                .data_map
                .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok().map(|role| (role.title.clone(), role.app_id.clone())))
                .unwrap_or_default()
        });
        Self::Names {
            handle: SurfaceHandle::wl(toplevel.wl_surface()),
            title: title.and_then(non_empty),
            app_id: app_id.and_then(non_empty),
        }
    }

    /// An X11 window's `WM_NAME` and the class half of `WM_CLASS` (the
    /// per-application string, as `ident::names` reads it).
    pub fn x11_names(x11: &X11Surface) -> Self {
        Self::Names {
            handle: SurfaceHandle::x11(x11),
            title: non_empty(x11.title()),
            app_id: non_empty(x11.class()),
        }
    }

    /// An X11 window's current `WM_TRANSIENT_FOR`.
    pub fn x11_transient_for(x11: &X11Surface) -> Self {
        Self::TransientFor {
            handle: SurfaceHandle::x11(x11),
            owner: x11.is_transient_for().map(SurfaceHandle::X11),
        }
    }
}
