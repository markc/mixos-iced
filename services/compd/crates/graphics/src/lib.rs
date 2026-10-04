//! graphics: GPU plumbing beneath the renderers. `surface` runs wgpu on its GL
//! backend over the compositor's own EGL context so iced draws into textures
//! the GLES renderer composites; `capture` is the screen-capture engine (the
//! registry of dmabuf-backed capture entries, VA-API/NVENC/software encoders,
//! save and re-encode); `bridge` carries the publish/wake/retire handshakes
//! between off-thread producers and the compositor loop.

#[macro_use]
extern crate model;

#[macro_use]
pub mod capture;
pub mod bridge;
pub mod surface;
