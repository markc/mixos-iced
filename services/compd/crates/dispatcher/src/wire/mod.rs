
pub mod wire;

// Façade re-exports: keep the old module paths working for any caller.
/// Re-export the old `pub mod delegate` path (callers used ::delegate::).
pub mod delegate {
    pub use super::wire::*;
}
/// Re-export the old `pub mod color_management` path.
pub mod color_management {
    pub use crate::wire::color::color::*;
}

pub mod clipboard;
pub mod color;
pub mod colorsurf;
pub mod icon;
pub mod redraw;
pub mod session;
pub mod tablet;
pub mod tearing;
pub mod trait_;
