//! Which connector to drive. Policy: honor the highest-priority monitor preference
//! whose monitor is currently connected — `profiles` are in priority order, so the
//! FIRST profile is the default output. With no profiles, no profile identities, or
//! no match among the connected monitors, the policy is the original first-connected
//! behavior (so this is behavior-neutral when the user has set no default).

use crate::edid::identity::identity;
use crate::edid::parse::parse;
use render_gles::preference::output::profile::profile::OutputProfile;
use smithay::backend::drm::DrmDevice;
use smithay::reexports::drm::control::connector;
use std::sync::OnceLock;

/// `--connector NAME` pins the connector for this run,
/// ahead of every monitor preference. Kernel naming: `HDMI-A-1`, `eDP-1`, `DP-2`.
static FORCED: OnceLock<String> = OnceLock::new();

/// Pin the connector [`select`] must drive. Call before the backend is wired;
/// a second call is ignored.
pub fn force(name: String) {
    let _ = FORCED.set(name);
}

/// The kernel's name for a connector (`HDMI-A-1`): interface plus index.
pub fn connector_name(info: &connector::Info) -> String {
    format!("{}-{}", info.interface().as_str(), info.interface_id())
}

/// Pick the connector to drive. The first profile whose EDID identity
/// ("make model serial") matches a connected monitor wins; otherwise the first
/// connected connector is used. The EDID identity is the per-monitor key both the
/// in-compositor switch and the standalone settings-editor persist.
pub fn select(
    drm: &DrmDevice,
    connectors: Vec<connector::Info>,
    profiles: &[OutputProfile],
) -> Option<connector::Info> {
    let connected: Vec<connector::Info> = connectors
        .into_iter()
        .filter(|c| c.state() == connector::State::Connected)
        .collect();

    // A pinned connector wins outright; if it is not connected there is nothing
    // sensible to fall back to (the caller asked for THAT output), so say so and
    // return none.
    if let Some(want) = FORCED.get() {
        let found = connected.iter().position(|c| connector_name(c) == *want);
        if found.is_none() {
            let have: Vec<String> = connected.iter().map(connector_name).collect();
            error!("--connector {want}: not a connected connector (connected: {have:?})");
        }
        return found.and_then(|i| connected.into_iter().nth(i));
    }

    let chosen = profiles.iter().find_map(|p| {
        let want = p.identity.as_deref()?;
        connected.iter().position(|c| identity_key(drm, c) == want)
    });

    match chosen {
        Some(idx) => connected.into_iter().nth(idx),
        None => connected.into_iter().next(),
    }
}

/// The stable identity key ("make model serial") for a connector's monitor — the same
/// value both the in-compositor switch and the standalone settings editor key
/// preferences by. An unreadable EDID yields the unknown-monitor key, so it simply
/// never matches a real preference.
fn identity_key(drm: &DrmDevice, info: &connector::Info) -> String {
    let raw = parse::read(drm, info);
    let parsed = raw.as_ref().and_then(|r| parse::parse(r));
    identity::identity(
        parsed.as_ref(),
        &format!("{:?}-{}", info.interface(), info.interface_id()),
    )
    .key()
}
