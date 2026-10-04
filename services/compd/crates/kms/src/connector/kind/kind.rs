//! Internal (eDP/LVDS/DSI) vs external classification, plus the
//! panel-orientation connector property -> Transform (feeds
//! `drm.output/output.physical`).

use smithay::backend::drm::DrmDevice;
use smithay::reexports::drm::control::{connector, Device};
use smithay::utils::Transform;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorKind {
    /// Laptop panel class: eDP, LVDS, DSI.
    InternalPanel,
    /// Everything routed through an external port.
    External,
    Unknown,
}

pub fn classify(info: &connector::Info) -> ConnectorKind {
    use connector::Interface;
    match info.interface() {
        Interface::EmbeddedDisplayPort | Interface::LVDS | Interface::DSI => {
            ConnectorKind::InternalPanel
        }
        Interface::Unknown => ConnectorKind::Unknown,
        _ => ConnectorKind::External,
    }
}

/// Read the "panel orientation" connector property, if the kernel exposes it.
pub fn panel_orientation(drm: &DrmDevice, info: &connector::Info) -> Option<Transform> {
    let props = drm.get_properties(info.handle()).ok()?;
    for (prop, value) in props.iter() {
        let Ok(prop_info) = drm.get_property(*prop) else {
            continue;
        };
        if prop_info.name().to_str() != Ok("panel orientation") {
            continue;
        }
        // Enum values per kernel: 0 Normal, 1 Upside Down, 2 Left Side Up, 3 Right Side Up.
        return match *value {
            0 => Some(Transform::Normal),
            1 => Some(Transform::_180),
            2 => Some(Transform::_90),
            3 => Some(Transform::_270),
            _ => None,
        };
    }
    None
}
