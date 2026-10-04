//! X11 window-manager helpers: the Xwayland `DISPLAY` number as a process global,
//! and the `wl_surface` -> `X11Surface` link the keyboard-focus path needs.

pub mod display;
pub mod focus;
