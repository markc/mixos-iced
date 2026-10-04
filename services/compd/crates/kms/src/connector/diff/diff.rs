//! Snapshot diffing — the hotplug comparison seam. Real bodies (the math is
//! cheap); the *reaction* belongs to `native.plugin`/`native.device`.

use smithay::reexports::drm::control::connector;

#[derive(Debug, Clone, Default)]
pub struct ConnectorSnapshot {
    pub entries: Vec<(connector::Handle, connector::State)>,
}

impl ConnectorSnapshot {
    pub fn take(infos: &[connector::Info]) -> Self {
        Self {
            entries: infos.iter().map(|i| (i.handle(), i.state())).collect(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ConnectorDiff {
    pub connected: Vec<connector::Handle>,
    pub disconnected: Vec<connector::Handle>,
}

impl ConnectorDiff {
    pub fn is_empty(&self) -> bool {
        self.connected.is_empty() && self.disconnected.is_empty()
    }
}

pub fn diff(old: &ConnectorSnapshot, new: &ConnectorSnapshot) -> ConnectorDiff {
    let mut out = ConnectorDiff::default();
    for (handle, state) in &new.entries {
        let was = old
            .entries
            .iter()
            .find(|(h, _)| h == handle)
            .map(|(_, s)| *s);
        match (was, state) {
            (Some(connector::State::Connected), connector::State::Connected) => {}
            (_, connector::State::Connected) => out.connected.push(*handle),
            (Some(connector::State::Connected), _) => out.disconnected.push(*handle),
            _ => {}
        }
    }
    for (handle, state) in &old.entries {
        if *state == connector::State::Connected
            && !new.entries.iter().any(|(h, _)| h == handle)
        {
            out.disconnected.push(*handle);
        }
    }
    out
}
