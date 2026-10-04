//! Mode listing + the diagnostics dump (ex wire.rs mode logging).

use smithay::reexports::drm::control::{connector, Mode as DrmMode};

pub fn modes(info: &connector::Info) -> &[DrmMode] {
    info.modes()
}

pub fn dump(info: &connector::Info) {
    for (i, m) in modes(info).iter().enumerate() {
        info!(
            "  available mode[{}]: {}x{} @ {}Hz, type: {:?}",
            i,
            m.size().0,
            m.size().1,
            m.vrefresh(),
            m.mode_type(),
        );
    }
}
