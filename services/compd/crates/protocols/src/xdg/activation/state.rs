use smithay::reexports::wayland_server::DisplayHandle;
use smithay::wayland::xdg_activation::XdgActivationState;

pub struct Activation {
    pub xdg_activation: XdgActivationState,
}
