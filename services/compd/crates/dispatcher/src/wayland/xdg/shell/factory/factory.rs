use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::WmCapabilities;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_wm_base::XdgWmBase;
use smithay::reexports::wayland_server::{DisplayHandle, GlobalDispatch};
use smithay::wayland::GlobalData;
use smithay::wayland::shell::xdg::XdgShellState;
use crate::state::state::DispatchWire;
use protocols::xdg::shell::state::XDGShell;

pub fn new<I: DispatchWire>(display_handle: &DisplayHandle) -> XDGShell where
    I: GlobalDispatch<XdgWmBase, GlobalData> + 'static {
    // Initialize the XDG Shell protocol.
    // Side-effect: Triggers window mapping/unmapping. When a client requests a new Toplevel,
    // it dispatches an event in your XDG delegate to assign the window a position in the `Space`.
    //
    // `new_with_capabilities`, NOT `new`. The latter advertises the whole
    // `wm_capabilities` enum — a hardcoded `[Fullscreen, Maximize, Minimize, WindowMenu]`
    // — and the compositor implemented exactly one of those: `fullscreen_request` /
    // `unfullscreen_request`. `maximize_request`, `unmaximize_request`,
    // `minimize_request` and `show_window_menu` all fall through to smithay's no-op
    // defaults, so the other three were a claim about behaviour that does not exist.
    //
    // This event is not decorative. xdg-shell: "If a capability isn't supported, clients
    // should hide or disable the UI elements that expose this functionality" — GTK and Qt
    // draw real titlebar buttons from it, so advertising maximize and minimize put two
    // dead buttons on every CSD window.
    //
    // The X11 counterpart is `_NET_WM_ALLOWED_ACTIONS` (`shell::ALLOWED_ACTIONS`), and the
    // two lists say the same thing. The one asymmetry is CLOSE, which appears there and
    // has no entry here: `xdg_toplevel.close` is unconditional, so there is nothing to
    // advertise. Move and resize have no entry in the enum: `xdg_toplevel.move`/`.resize`
    // are always available, and compd answers them with the interactive grab
    // (`XdgShellHandler::move_request` / `resize_request`).
    let xdg_shell_state =
        XdgShellState::new_with_capabilities::<I>(
            &display_handle,
            // The comp policy's window model answers maximise and
            // minimise, so GTK/Qt draw those buttons.
            [WmCapabilities::Fullscreen, WmCapabilities::Maximize, WmCapabilities::Minimize],
        );
    XDGShell {
        state: xdg_shell_state
    }
}
