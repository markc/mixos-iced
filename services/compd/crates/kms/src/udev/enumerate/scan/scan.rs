//! UdevBackend device-list snapshot. Detection only — never reaction.
//! Failure policy: no udev means no devices means no compositor — panic.

use smithay::backend::udev::UdevBackend;
use std::path::PathBuf;

/// One snapshot of the seat's DRM devices: (dev_t, device path).
pub fn snapshot(seat: &str) -> Vec<(u64, PathBuf)> {
    let backend = UdevBackend::new(seat).expect("udev backend creation failed");
    backend
        .device_list()
        .map(|(dev_id, path)| (dev_id, path.to_path_buf()))
        .collect()
}
