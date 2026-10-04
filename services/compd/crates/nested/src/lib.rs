//! nested: the winit backend, which runs the compositor as a window on another
//! desktop. `wire` is the entry point and contract, `window` creates the host
//! window and its output, `input` routes and captures host input, `scene` and
//! `frame` compose and submit each frame.

#[macro_use]
extern crate model;

pub mod frame;
pub mod input;
pub mod scene;
pub mod window;
pub mod wire;
