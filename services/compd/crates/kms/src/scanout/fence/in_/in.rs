//! A real implementation with working mechanism bodies, compiled under the
//! `native-scanout` cargo feature and exercised by the assembly self-test
//! (TEST_ONLY atomic commits, kernel-validated on the real device).
//!
//! Acquire fences -> IN_FENCE_FD: materialization (syncobj -> sync_file) and
//! attachment onto the plane in an atomic request. The render side reaches
//! this through `vulkan.sync/sync.export` (timeline point -> syncobj) or,
//! once EGL native fences land, the gles equivalent.
//!
//! Carried caveat from the hosted compositor: IN_FENCE_FD makes commits fail
//! on the NVIDIA driver — the wiring site consults `has_in_fence` first.

#[cfg(feature = "native-scanout")]
pub use gated::*;

#[cfg(feature = "native-scanout")]
mod gated {
    use crate::scanout::commit::build::build::PipelineProps;
    use smithay::backend::drm::DrmDeviceFd;
    use smithay::reexports::drm::control::atomic::AtomicModeReq;
    use smithay::reexports::drm::control::{plane, property, syncobj, Device as ControlDevice};
    use std::os::unix::io::{AsRawFd, OwnedFd};

    /// A render-completion fence ready for IN_FENCE_FD.
    #[derive(Debug)]
    pub struct AcquireFence(pub OwnedFd);

    /// Materialize a (binary-semantics) syncobj as a sync_file acquire fence.
    pub fn from_syncobj(drm: &DrmDeviceFd, handle: syncobj::Handle) -> AcquireFence {
        AcquireFence(
            drm.syncobj_to_fd(handle, true)
                .unwrap_or_else(|e| abort!("acquire fence materialization failed: {e}")),
        )
    }

    /// Whether the plane supports IN_FENCE_FD (false on NVIDIA-class setups).
    pub fn has_in_fence(props: &PipelineProps) -> bool {
        props.plane.has("IN_FENCE_FD")
    }

    /// Attach the fence to the plane in `req`. The fence must outlive the
    /// commit ioctl (the kernel dups it during the call) — the caller holds
    /// it across submit.
    pub fn attach(
        req: &mut AtomicModeReq,
        plane: plane::Handle,
        props: &PipelineProps,
        fence: &AcquireFence,
    ) {
        req.add_property(
            plane,
            props.plane.get("IN_FENCE_FD"),
            property::Value::SignedRange(fence.0.as_raw_fd() as i64),
        );
    }
}
