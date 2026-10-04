//! The DrmDeviceNotifier as a loop source + DrmEvent decoding: error vs
//! vblank-with-metadata. What a vblank MEANS (timing interpretation) belongs
//! to `backend.scanout/scanout.timing` — this crate only decodes.

use smithay::backend::drm::{DrmEvent, DrmEventMetadata, DrmEventTime};
use smithay::reexports::drm::control::crtc;
use std::time::Duration;

pub use smithay::backend::drm::DrmDeviceNotifier as DrmSource;

#[derive(Debug)]
pub enum DecodedDrmEvent {
    Error(String),
    VBlank {
        pipe: crtc::Handle,
        /// Monotonic timestamp from the kernel, when delivered as such.
        time: Option<Duration>,
        sequence: u64,
    },
}

pub fn decode(event: DrmEvent, meta: &Option<DrmEventMetadata>) -> DecodedDrmEvent {
    match event {
        DrmEvent::Error(error) => DecodedDrmEvent::Error(format!("{error:?}")),
        DrmEvent::VBlank(pipe) => DecodedDrmEvent::VBlank {
            pipe,
            time: meta.as_ref().and_then(|m| match m.time {
                DrmEventTime::Monotonic(t) => Some(t),
                _ => None,
            }),
            sequence: meta.as_ref().map(|m| m.sequence as u64).unwrap_or(0),
        },
    }
}
