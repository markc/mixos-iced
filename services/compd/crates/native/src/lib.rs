//! The native (KMS/DRM) backend: device selection, display and renderer
//! assembly, the per-output render loop with its watchdogs, and the
//! session, input and vblank wiring into the event loop.

#[macro_use]
extern crate model;

pub mod assemble;
pub mod context;
pub mod device;
pub mod plugin;
pub mod render;
pub mod wire;
