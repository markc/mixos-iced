pub mod movement {
    pub mod state {
        pub use crate::wayland::grab::move_::state::GrabMovement;
    }
    // The PointerGrab impl lives with GrabMovement in grab.move.state.
    pub mod wire {}
}

pub mod resize {
    pub mod state {
        pub use crate::wayland::grab::resize_impl::state::GrabResize;
        pub use protocols::grab::resize::surface::{
            ResizeEdge, ResizeSurfaceState,
        };
    }
    pub mod dispatch {
        pub use protocols::grab::resize::commit::handle_commit;
    }
    // The PointerGrab impl lives with GrabResize in grab.resize.state.
    pub mod wire {}
}

pub mod interactive;
pub mod move_;
pub mod resize_impl;
