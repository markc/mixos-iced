//! GbmDevice from fd.
//!
//! Architecture note: GBM is the GL-path allocator. The vulkan path bypasses
//! this crate (its allocator is `vulkan.memory`; the cursor plane has its own
//! dumb-buffer allocator in `scanout.plane/plane.cursor`).
//! Failure policy: panic (original unwrap).

use smithay::backend::allocator::gbm::GbmDevice;
use smithay::backend::drm::DrmDeviceFd;

pub fn create(fd: DrmDeviceFd) -> GbmDevice<DrmDeviceFd> {
    GbmDevice::new(fd).expect("GbmDevice creation failed")
}
