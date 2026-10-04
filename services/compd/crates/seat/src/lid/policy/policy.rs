use world::state::Loop;
use drivers::lid::base::{
    DisplayRequest, DisplaySnapshot, LidPosition, DISPLAY_REQUEST_MUT, DISPLAY_SNAPSHOT,
    LID_POSITION, LID_POSITION_MUT,
};

/// Decide and dispatch the reaction to a lid switch toggle.
///
/// Reads the kernel-written `DisplaySnapshot` to choose between *suspend* (the
/// internal panel is the only display) and *keep running on the external*
/// (docked). The decision is written as a `DisplayRequest` the kernel loop
/// drains; this function performs no DRM/session work itself (wrong layer).
///
/// `lid_open` is derived from the libinput switch state by the caller
/// (`SwitchState::Off` ⇒ open).
pub fn on_lid_toggle(loop_: &mut Loop, lid_open: bool) {
    let position = if lid_open {
        LidPosition::Open
    } else {
        LidPosition::Closed
    };

    // Debounce: ignore repeated identical positions (libinput re-emits state).
    if *loop_.inner.kernel.get(&LID_POSITION) == Some(position) {
        return;
    }
    *loop_.inner.kernel.get_mut(&LID_POSITION_MUT) = Some(position);

    let snapshot: DisplaySnapshot = *loop_.inner.kernel.get(&DISPLAY_SNAPSHOT);

    let request = match position {
        // Lid opened: restore the internal panel.
        LidPosition::Open => {
            if snapshot.external_present {
                DisplayRequest::SwitchToInternal
            } else {
                DisplayRequest::PanelOn
            }
        }
        // Lid closed: stay on the external if docked, otherwise suspend.
        LidPosition::Closed => {
            if snapshot.external_present {
                DisplayRequest::SwitchToExternal
            } else {
                DisplayRequest::Suspend
            }
        }
    };

    info!(
        "lid {:?}: external_present={} -> {:?}",
        position, snapshot.external_present, request
    );
    *loop_.inner.kernel.get_mut(&DISPLAY_REQUEST_MUT) = Some(request);
    // Wake the control-plane ping so `display.apply` drains this off the loop,
    // input-independently (a lid switch may arrive with no follow-up input).
    loop_.inner.ping_control();
}
