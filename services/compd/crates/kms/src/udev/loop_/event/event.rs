//! Added/Changed/Removed typing + routing. Detects and types; the host's
//! `native.plugin/plugin.route` reacts.

use smithay::backend::udev::UdevEvent;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum DeviceEvent {
    Added { device_id: u64, path: PathBuf },
    Changed { device_id: u64 },
    Removed { device_id: u64 },
}

pub fn decode(event: UdevEvent) -> DeviceEvent {
    match event {
        UdevEvent::Added { device_id, path } => DeviceEvent::Added {
            device_id: device_id.into(),
            path,
        },
        UdevEvent::Changed { device_id } => DeviceEvent::Changed {
            device_id: device_id.into(),
        },
        UdevEvent::Removed { device_id } => DeviceEvent::Removed {
            device_id: device_id.into(),
        },
    }
}
