//! Plane policy -> FrameFlags: the single source of the flags every render
//! hands the hosted compositor (which performs the actual plane assignment
//! under delegation). `plane.direct` is the on/off policy surface over this.

use smithay::backend::drm::compositor::FrameFlags;

#[derive(Debug, Clone, Copy)]
pub struct PlanePolicy {
    /// Allow scanning buffers out directly on planes (primary/overlay/cursor).
    pub allow_direct_scanout: bool,
}

pub fn frame_flags(policy: PlanePolicy) -> FrameFlags {
    if policy.allow_direct_scanout {
        FrameFlags::DEFAULT
    } else {
        FrameFlags::empty()
    }
}
