//! A real implementation with working mechanism bodies, compiled under the
//! `native-scanout` cargo feature. While the hosted DrmOutputManager remains
//! the live presentation path, the assembly-time self-test exercises this
//! machine against the real device (TEST_ONLY atomic commits are validated
//! by the kernel without touching the screen), so a swap-over would replace
//! the hosted path with a proven implementation, not a stub.
//!
//! Framebuffer lifetime across frames: a slot keeps its framebuffer for as
//! long as the buffer lives (AddFB2 once per buffer, not per frame), exactly
//! the caching discipline the hosted compositor applies. The slot's
//! UserDataMap carries the cached object; this crate owns that key.

#[cfg(feature = "native-scanout")]
pub use gated::*;

#[cfg(feature = "native-scanout")]
mod gated {
    use crate::scanout::framebuffer::import::import::{self, NativeFramebuffer};
    use crate::scanout::swapchain::slot::slot::NativeSlot;
    use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
    use smithay::backend::drm::DrmDeviceFd;
    use smithay::reexports::drm::control::framebuffer;
    use std::sync::Arc;

    /// The cached per-slot framebuffer (Arc: the commit borrows it while the
    /// slot owns it).
    struct CachedFramebuffer(Arc<NativeFramebuffer>);

    /// Framebuffer for a slot: cached on first sight, reused after.
    pub fn framebuffer_for(
        exporter: &GbmFramebufferExporter<DrmDeviceFd>,
        drm: &DrmDeviceFd,
        slot: &NativeSlot,
    ) -> Arc<NativeFramebuffer> {
        if let Some(cached) = slot.userdata().get::<CachedFramebuffer>() {
            return cached.0.clone();
        }
        let fb = Arc::new(import::import(exporter, drm, slot));
        slot.userdata().insert_if_missing(|| CachedFramebuffer(fb.clone()));
        fb
    }

    /// The kernel handle a commit references.
    pub fn handle(fb: &NativeFramebuffer) -> framebuffer::Handle {
        *AsRef::<framebuffer::Handle>::as_ref(fb)
    }
}
