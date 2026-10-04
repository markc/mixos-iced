//! The gles implementation of the contract's display-bind + import
//! capabilities: legacy wl_drm bind (GLES-only, no Vulkan counterpart),
//! dmabuf format negotiation, client dmabuf import/validation, early import.
//! (Ex draw.state state.rs `Backend::bind_display` body.)
//!
//! Failure policy notes: the EGL bind failure log is ORIGINAL behavior by
//! design — the wl_drm bridge is optional (dmabuf+syncobj is the modern
//! path), so its absence is not a failure. Import returning false is the
//! validation contract (the protocol rejects the buffer), not a swallowed
//! error.

use crate::multigpu::factory::factory::{self, NativeGpuManager};
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::drm::DrmNode;
use smithay::backend::renderer::{ImportDma, ImportEgl};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::DisplayHandle;

/// Bind the legacy wl_drm acceleration bridge and return the dmabuf format
/// set for the linux-dmabuf global.
///
/// CHECK carried from the original: revisit disabling the EGL bridge in favor
/// of dmabuf+syncobj — it should skip only when syncobj is unsupported.
pub fn bind(
    gpus: &mut NativeGpuManager,
    primary: &DrmNode,
    display_handle: &DisplayHandle,
) -> FormatSet {
    let mut renderer = factory::single_renderer(gpus, primary);

    match renderer.bind_wl_display(display_handle) {
        Ok(_) => info!("EGL hardware-acceleration enabled"),
        Err(err) => warn!("Failed to initialize EGL hardware-acceleration: {err:?}"),
    }

    renderer.dmabuf_formats()
}

/// The (fourcc x modifier) set the renderer can sample from — what the
/// linux-dmabuf global should advertise (contract `supported_formats`).
pub fn texture_formats(gpus: &mut NativeGpuManager, primary: &DrmNode) -> FormatSet {
    let mut renderer = factory::single_renderer(gpus, primary);
    renderer
        .as_mut()
        .egl_context()
        .dmabuf_texture_formats()
        .clone()
}

/// Contract import capability: validate a client dmabuf by importing on the
/// primary node.
pub fn import_dmabuf(gpus: &mut NativeGpuManager, primary: &DrmNode, dmabuf: &Dmabuf) -> bool {
    let mut renderer = factory::single_renderer(gpus, primary);
    match renderer.import_dmabuf(dmabuf, None) {
        Ok(_) => true,
        Err(err) => {
            trace!("client dmabuf rejected by import validation: {err:?}");
            false
        }
    }
}

/// Contract early-import optimization (multi-GPU correctness + latency).
/// Best-effort by definition: render-time import is the authoritative path.
pub fn early_import(gpus: &mut NativeGpuManager, primary: &DrmNode, surface: &WlSurface) {
    if let Err(err) = gpus.early_import(*primary, surface) {
        trace!("early import skipped: {err:?}");
    }
}
