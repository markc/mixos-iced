//! Connector <-> pipe mapping. 1:1 today; the multi-head seam.

use smithay::reexports::drm::control::{connector, crtc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipeAssignment {
    pub connector: connector::Handle,
    pub pipe: crtc::Handle,
}

pub fn assign(connector: connector::Handle, pipe: crtc::Handle) -> PipeAssignment {
    PipeAssignment { connector, pipe }
}
