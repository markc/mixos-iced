//! A real implementation with working mechanism bodies, compiled under the
//! `native-scanout` cargo feature. While the hosted DrmOutputManager remains
//! the live presentation path, the assembly-time self-test exercises this
//! machine against the real device (TEST_ONLY atomic commits are validated
//! by the kernel without touching the screen), so a swap-over would replace
//! the hosted path with a proven implementation, not a stub.
//!
//! Buffer slot ownership and age tracking for the native scanout path —
//! the per-pipe swapchain. Hosts smithay's allocator-level `Swapchain`
//! primitive (allocator machinery, NOT DrmCompositor) under our typing,
//! exactly as `surface.output` hosts the pipe objects.

#[cfg(feature = "native-scanout")]
pub use gated::*;

#[cfg(feature = "native-scanout")]
mod gated {
    use smithay::backend::allocator::gbm::{GbmAllocator, GbmBuffer};
    use smithay::backend::allocator::{Slot, Swapchain};
    use smithay::backend::allocator::{Fourcc, Modifier};
    use smithay::backend::drm::DrmDeviceFd;

    /// Our name for the native scanout swapchain (GL-path allocator; the
    /// vulkan path substitutes its own allocator at the same position).
    pub type NativeSwapchain = Swapchain<GbmAllocator<DrmDeviceFd>>;
    pub type NativeSlot = Slot<GbmBuffer>;

    pub fn create(
        allocator: GbmAllocator<DrmDeviceFd>,
        size: (u32, u32),
        fourcc: Fourcc,
        modifiers: Vec<Modifier>,
    ) -> NativeSwapchain {
        Swapchain::new(allocator, size.0, size.1, fourcc, modifiers)
    }

    /// Buffer age of a slot (damage-tracking input for partial redraws).
    pub fn age(slot: &NativeSlot) -> u8 {
        slot.age()
    }

    /// Drop all buffers (mode change / resume reset).
    pub fn reset(swapchain: &mut NativeSwapchain) {
        swapchain.reset_buffers();
    }
}
