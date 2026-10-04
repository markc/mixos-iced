use smithay::{
    utils::{Clock, Monotonic},
    wayland::compositor::CompositorState,
};

/// The `Compositor` struct aggregates the core Wayland protocols and Smithay abstractions
/// required to build a functional desktop environment.
///
/// **Compositor Hookup:**
/// The fields in this struct are typically mutated inside the `calloop` event handlers.
/// For example, when a client creates a new window, the `XdgShellState` delegate methods
/// are triggered, and you will insert the resulting `Window` into the `Space`.
pub struct Compositor {
    // pub space_layer_shell: Vec<Output>,
    // Manages the fundamental `wl_compositor` and `wl_subcompositor` globals.
    // Clients use this to create bare `wl_surface`s and `wl_region`s.
    pub state: CompositorState,
    pub clock: Clock<Monotonic>,
}

// A marker used to indicate a nested compositor surface, allows disabling fractional scale for winit
