//! Classifies incoming devices and routes them to the owning stack. Today's
//! udev watch only carries the DRM subsystem, so everything classifies as a
//! display/GPU device; the seam exists for the day the watch widens.

use kms::gpu::registry::node::node::NodeRegistry;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceClass {
    /// A GPU/display device already in the registry.
    KnownGpu,
    /// A GPU/display device we have not seen.
    NewGpu,
    /// Not ours to handle (future: input-only, sound, ...).
    Unknown,
}

pub fn classify(dev_id: u64, _path: Option<&Path>, registry: &NodeRegistry) -> DeviceClass {
    if registry.contains(dev_id) {
        DeviceClass::KnownGpu
    } else {
        DeviceClass::NewGpu
    }
}
