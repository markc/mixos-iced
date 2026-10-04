//! GbmAllocator flags policy + framebuffer-exporter construction.

use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
use smithay::backend::drm::{DrmDeviceFd, DrmNode};

/// The buffer-flag policy for scanout-capable render buffers.
pub fn buffer_flags() -> GbmBufferFlags {
    GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT
}

pub fn allocator(gbm: GbmDevice<DrmDeviceFd>) -> GbmAllocator<DrmDeviceFd> {
    GbmAllocator::new(gbm, buffer_flags())
}

pub fn exporter(
    gbm: GbmDevice<DrmDeviceFd>,
    node: DrmNode,
) -> GbmFramebufferExporter<DrmDeviceFd> {
    GbmFramebufferExporter::new(gbm, node.into())
}
