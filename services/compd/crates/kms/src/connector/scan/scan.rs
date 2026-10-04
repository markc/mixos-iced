//! Resource handles -> connector infos; connected-state read.
//! Failure policy: a device that cannot enumerate resources cannot drive a
//! display — panic (original unwrapped both calls).

use smithay::backend::drm::DrmDevice;
use smithay::reexports::drm::control::{connector, Device, ResourceHandles};

pub fn resources(drm: &DrmDevice) -> ResourceHandles {
    drm.resource_handles().expect("resource_handles failed")
}

/// All connector infos, probed (`force = true`, as the original did).
pub fn connectors(drm: &DrmDevice, res: &ResourceHandles) -> Vec<connector::Info> {
    res.connectors()
        .iter()
        .map(|conn| {
            drm.get_connector(*conn, true)
                .expect("get_connector failed")
        })
        .collect()
}

pub fn is_connected(info: &connector::Info) -> bool {
    info.state() == connector::State::Connected
}
