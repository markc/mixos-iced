//! xdg-decoration mode negotiation, and whether compd draws a toplevel's
//! chrome.
//!
//! - SSD disabled (`decorations_ssd` preference off): client-side, always.
//! - Otherwise server-side unless the client explicitly asked for client-side,
//!   which is honoured (compd used to override it).
//! - The mode is only recorded until the initial configure has gone out: that
//!   configure carries it (Chromium CHECK-crashes on a second configure before
//!   it acked the first; see the handler impl in `state.rs`).
//! - A client that destroys its decoration object draws its own decorations
//!   from then on ([`destroyed`]); a new decoration object starts over.

use std::sync::atomic::{AtomicBool, Ordering};

use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::ToplevelSurface;

/// Set on a toplevel's wl_surface while its decoration object is destroyed.
#[derive(Default)]
struct Reverted(AtomicBool);

fn set_reverted(toplevel: &ToplevelSurface, reverted: bool) {
    with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get_or_insert_threadsafe(Reverted::default)
            .0
            .store(reverted, Ordering::Relaxed);
    });
}

fn reverted(toplevel: &ToplevelSurface) -> bool {
    with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get::<Reverted>()
            .is_some_and(|r| r.0.load(Ordering::Relaxed))
    })
}

/// The mode compd answers `requested` with (`None`: new decoration object or
/// `unset_mode`).
pub fn answer(requested: Option<Mode>, ssd_enabled: bool) -> Mode {
    match requested {
        _ if !ssd_enabled => Mode::ClientSide,
        Some(Mode::ClientSide) => Mode::ClientSide,
        _ => Mode::ServerSide,
    }
}

/// Record the answer to `requested` and, once the toplevel has had its initial
/// configure, send it.
pub fn configure(toplevel: &ToplevelSurface, requested: Option<Mode>) {
    let mode = answer(
        requested,
        model::environment::preference::base::decorations_ssd(),
    );
    set_reverted(toplevel, false);
    toplevel.with_pending_state(|state| {
        state.decoration_mode = Some(mode);
    });
    if toplevel.is_initial_configure_sent() {
        toplevel.send_pending_configure();
    }
}

/// The client destroyed the decoration object: no more server-side chrome.
pub fn destroyed(toplevel: &ToplevelSurface) {
    set_reverted(toplevel, true);
}

/// Whether compd draws this toplevel's chrome: the client acked server-side
/// (the committed state, so chrome appears with the configure the client
/// acted on, never before) and still has its decoration object.
pub fn server_side(toplevel: &ToplevelSurface) -> bool {
    !reverted(toplevel)
        && toplevel.with_committed_state(|state| state.and_then(|state| state.decoration_mode))
            == Some(Mode::ServerSide)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_side_unless_asked_otherwise_or_disabled() {
        assert_eq!(answer(None, true), Mode::ServerSide);
        assert_eq!(answer(Some(Mode::ServerSide), true), Mode::ServerSide);
        assert_eq!(answer(Some(Mode::ClientSide), true), Mode::ClientSide);
        for requested in [None, Some(Mode::ServerSide), Some(Mode::ClientSide)] {
            assert_eq!(answer(requested, false), Mode::ClientSide);
        }
    }
}
