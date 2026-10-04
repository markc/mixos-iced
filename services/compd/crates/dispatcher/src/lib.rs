//! The compositor's dispatch layer: the calloop event-loop data (`Wire`), the
//! concrete `Dispatch` state that hosts every smithay handler impl, the Wayland
//! global factories, and the X11 window-manager hooks.

#[macro_use]
extern crate model;

pub mod frame;
pub mod idle;
pub mod state;
pub mod wayland;
pub mod wire;
pub mod xwm;
