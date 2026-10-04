//! Device -> Connector -> Pipe -> Output relationships, collection-shaped
//! from day one (single-device today is an instance count, not a shape),
//! plus the per-device connector snapshot the hotplug diff compares against.

use kms::connector::diff::diff::ConnectorSnapshot;
use smithay::backend::drm::DrmNode;
use smithay::output::Output;
use smithay::reexports::drm::control::{connector, crtc};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct ConnectorEntry {
    pub handle: connector::Handle,
    pub kind: kms::connector::kind::kind::ConnectorKind,
    pub pipe: Option<crtc::Handle>,
    pub output: Option<Output>,
}

#[derive(Debug, Clone)]
pub struct DeviceEntry {
    pub node: DrmNode,
    pub connectors: Vec<ConnectorEntry>,
    pub snapshot: ConnectorSnapshot,
}

#[derive(Debug, Default)]
pub struct Topology {
    devices: HashMap<u64, DeviceEntry>,
}

impl Topology {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_device(&mut self, dev_id: u64, node: DrmNode) {
        self.devices.entry(dev_id).or_insert(DeviceEntry {
            node,
            connectors: Vec::new(),
            snapshot: ConnectorSnapshot::default(),
        });
    }

    pub fn register_connector(&mut self, dev_id: u64, entry: ConnectorEntry) {
        if let Some(dev) = self.devices.get_mut(&dev_id) {
            dev.connectors.retain(|c| c.handle != entry.handle);
            dev.connectors.push(entry);
        }
    }

    pub fn set_snapshot(&mut self, dev_id: u64, snapshot: ConnectorSnapshot) {
        if let Some(dev) = self.devices.get_mut(&dev_id) {
            dev.snapshot = snapshot;
        }
    }

    pub fn snapshot(&self, dev_id: u64) -> Option<&ConnectorSnapshot> {
        self.devices.get(&dev_id).map(|d| &d.snapshot)
    }

    pub fn remove_device(&mut self, dev_id: u64) -> Option<DeviceEntry> {
        self.devices.remove(&dev_id)
    }

    pub fn device(&self, dev_id: u64) -> Option<&DeviceEntry> {
        self.devices.get(&dev_id)
    }

    /// Whether `handle` on `dev_id` is the connector driving an active output.
    pub fn is_active_connector(&self, dev_id: u64, handle: connector::Handle) -> bool {
        self.devices
            .get(&dev_id)
            .map(|d| {
                d.connectors
                    .iter()
                    .any(|c| c.handle == handle && c.output.is_some())
            })
            .unwrap_or(false)
    }
}
