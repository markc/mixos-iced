use smithay::reexports::wayland_server::DisplayHandle;
use smithay::wayland::output::OutputManagerState;

pub struct OutputState {
    // Manages `wl_output` and `zxdg_output_manager_v1`. Communicates screen geometry, scale,
    // and refresh rate to clients.
    pub output_manager_state: OutputManagerState,
    pub display_handle: DisplayHandle
}