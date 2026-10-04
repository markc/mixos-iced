use smithay::wayland::shell::xdg::XdgShellState;

pub struct XDGShell {
    // Manages `xdg_wm_base`. This is what turns a bare `wl_surface` into a "Window" (Toplevel)
    // or a menu (Popup).
    pub state: XdgShellState,
}