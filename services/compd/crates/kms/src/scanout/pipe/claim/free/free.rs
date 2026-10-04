//! Free-CRTC claim: pick a CRTC the connector can drive that is NOT already in
//! use by another pipe. Used to bring up a second output during a live monitor
//! switch (the active output keeps its CRTC for a clean revert). Sibling of
//! `pipe.claim` (which always returns the current/first CRTC).

use smithay::backend::drm::DrmDevice;
use smithay::reexports::drm::control::{connector, crtc, Device, ResourceHandles};

/// A CRTC usable by `connector` and not in `busy`. Walks the connector's encoders
/// for their `possible_crtcs`, then falls back to any CRTC in `res` not in `busy`.
pub fn claim_excluding(
    drm: &DrmDevice,
    connector: &connector::Info,
    res: &ResourceHandles,
    busy: &[crtc::Handle],
) -> Option<crtc::Handle> {
    for enc in connector.encoders() {
        let Ok(info) = drm.get_encoder(*enc) else { continue };
        if let Some(c) = res.filter_crtcs(info.possible_crtcs()).into_iter().find(|c| !busy.contains(c)) {
            return Some(c);
        }
    }
    res.crtcs().iter().copied().find(|c| !busy.contains(c))
}
