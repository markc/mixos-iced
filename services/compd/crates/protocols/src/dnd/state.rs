use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

pub struct DNDState {
    pub icon: Option<WlSurface>,
}
