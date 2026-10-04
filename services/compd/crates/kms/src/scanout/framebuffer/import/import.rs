//! A real implementation with working mechanism bodies, compiled under the
//! `native-scanout` cargo feature. While the hosted DrmOutputManager remains
//! the live presentation path, the assembly-time self-test exercises this
//! machine against the real device (TEST_ONLY atomic commits are validated
//! by the kernel without touching the screen), so a swap-over would replace
//! the hosted path with a proven implementation, not a stub.
//!
//! Render buffer -> DRM framebuffer (AddFB2): the import half of the
//! framebuffer story, over the same exporter type the hosted manager uses
//! (`GbmFramebufferExporter` — modifier-aware AddFB2 with the legacy
//! fallback handled inside).

#[cfg(feature = "native-scanout")]
pub use gated::*;

#[cfg(feature = "native-scanout")]
mod gated {
    use smithay::backend::allocator::gbm::GbmBuffer;
    use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
    use smithay::backend::drm::exporter::{ExportBuffer, ExportFramebuffer};
    use smithay::backend::drm::DrmDeviceFd;

    /// Our name for the imported framebuffer object (owns the kernel handle;
    /// drops rmfb).
    pub type NativeFramebuffer =
        <GbmFramebufferExporter<DrmDeviceFd> as ExportFramebuffer<GbmBuffer>>::Framebuffer;

    /// Import an allocator buffer as a scanout framebuffer.
    pub fn import(
        exporter: &GbmFramebufferExporter<DrmDeviceFd>,
        drm: &DrmDeviceFd,
        buffer: &GbmBuffer,
    ) -> NativeFramebuffer {
        exporter
            .add_framebuffer(drm, ExportBuffer::Allocator(buffer), false)
            .unwrap_or_else(|e| abort!("framebuffer import (AddFB2) failed: {e:?}"))
            .unwrap_or_else(|| abort!("buffer not eligible for framebuffer import"))
    }
}
