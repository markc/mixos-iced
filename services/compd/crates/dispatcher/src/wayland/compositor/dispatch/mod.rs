pub mod wire {
    pub use crate::wayland::compositor::session::{
        compositor_state, commit, apply_commit, awaits_initial_configure,
    };
    pub use protocols::compositor::client::client::client_compositor_state;
    pub use crate::wayland::compositor::place::WindowPlacedMarker;
    pub use crate::wayland::compositor::place::handle_commit;
}
