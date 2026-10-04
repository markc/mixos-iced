//! A real implementation with working mechanism bodies, compiled under the
//! `native-scanout` cargo feature. While the hosted DrmOutputManager remains
//! the live presentation path, the assembly-time self-test exercises this
//! machine against the real device (TEST_ONLY atomic commits are validated
//! by the kernel without touching the screen), so a swap-over would replace
//! the hosted path with a proven implementation, not a stub.
//!
//! Acquire/submit pacing against the swapchain. Failure policy: allocation
//! failure or slot exhaustion is not self-recovering — slot exhaustion means
//! frames are being acquired faster than vblanks retire them, which is a
//! pacing bug in the caller, and the policy is crash over mystery stalls.

#[cfg(feature = "native-scanout")]
pub use gated::*;

#[cfg(feature = "native-scanout")]
mod gated {
    use crate::scanout::swapchain::slot::slot::{NativeSlot, NativeSwapchain};

    /// Acquire the next render slot.
    pub fn acquire(swapchain: &mut NativeSwapchain) -> NativeSlot {
        swapchain
            .acquire()
            .unwrap_or_else(|e| abort!("swapchain buffer allocation failed: {e}"))
            .unwrap_or_else(|| {
                abort!("swapchain slots exhausted: frames outpacing vblank retirement")
            })
    }

    /// Mark a slot as submitted for scanout (ages the others).
    pub fn submitted(swapchain: &mut NativeSwapchain, slot: &NativeSlot) {
        swapchain.submitted(slot);
    }
}
