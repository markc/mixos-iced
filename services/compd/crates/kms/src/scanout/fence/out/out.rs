//! A real implementation with working mechanism bodies, compiled under the
//! `native-scanout` cargo feature and exercised by the assembly self-test
//! (TEST_ONLY atomic commits, kernel-validated on the real device).
//!
//! OUT_FENCE_PTR harvesting: the kernel writes a sync_file fd describing the
//! commit's completion into caller-owned memory. The slot owns that memory
//! (boxed, address-stable) for the lifetime of the commit and yields the fd
//! afterwards — presentation feedback for the explicit-sync era.

#[cfg(feature = "native-scanout")]
pub use gated::*;

#[cfg(feature = "native-scanout")]
mod gated {
    use crate::scanout::commit::build::build::PipelineProps;
    use smithay::reexports::drm::control::atomic::AtomicModeReq;
    use smithay::reexports::drm::control::{crtc, property};
    use std::os::unix::io::{FromRawFd, OwnedFd};

    /// Address-stable landing slot for the kernel-written fd.
    pub struct OutFenceSlot(Box<i64>);

    impl Default for OutFenceSlot {
        fn default() -> Self {
            Self::new()
        }
    }

    impl OutFenceSlot {
        pub fn new() -> Self {
            Self(Box::new(-1))
        }

        /// Whether the CRTC supports OUT_FENCE_PTR.
        pub fn supported(props: &PipelineProps) -> bool {
            props.crtc.has("OUT_FENCE_PTR")
        }

        /// Arm the slot on the request. The slot must outlive the commit
        /// ioctl: the kernel writes through this address during the call.
        pub fn arm(&mut self, req: &mut AtomicModeReq, pipe: crtc::Handle, props: &PipelineProps) {
            let ptr = &mut *self.0 as *mut i64 as u64;
            req.add_property(
                pipe,
                props.crtc.get("OUT_FENCE_PTR"),
                property::Value::UnsignedRange(ptr),
            );
        }

        /// Take the completion fence the commit produced (None if the commit
        /// failed or the slot was never armed).
        pub fn take(&mut self) -> Option<OwnedFd> {
            let fd = *self.0;
            *self.0 = -1;
            if fd >= 0 {
                Some(unsafe { OwnedFd::from_raw_fd(fd as i32) })
            } else {
                None
            }
        }
    }
}
