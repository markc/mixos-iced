//! ext-idle-notify-v1.
//!
//! smithay's `IdleNotifierState` keeps the notifications, one list per
//! `wl_seat`, and arms each one's timeout as a one-shot calloop timer on the
//! compositor's loop (vendor patch: `IdleTimerLoop`, since compd's loop data
//! is `Wire`, not this `Dispatch`). Nothing here polls: a timer is armed when
//! a notification is created or activity resets it, and fires once.
//!
//! What counts as activity is the policy lane's call, made at its input
//! funnels (world `comp::injection`, policy-host `injected`): human input
//! resets every seat's notifications, the agent seat's never does, so an
//! agent driving the desktop neither wakes the screen nor reads as a user.

use smithay::wayland::idle_notify::{IdleNotifierHandler, IdleNotifierState};

use crate::state::state::Dispatch;

impl IdleNotifierHandler for Dispatch {
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self> {
        &mut self.idle_notifier
    }
}

/// Human activity: reset the idle timeout of every notification on BOTH seats
/// (`seat0` and `agent`); an idled one resumes.
pub fn note_human_activity(dispatch: &mut Dispatch) {
    let seats: Vec<_> = std::iter::once(dispatch.seat.seat.clone()).chain(dispatch.seat.agent.clone()).collect();
    for seat in &seats {
        dispatch.idle_notifier.notify_activity(seat);
    }
}

/// Hold (or release) idle for notifications that honour inhibitors. Nothing
/// inhibits yet (compd serves no idle-inhibit protocol); kept for when one does.
pub fn set_inhibited(dispatch: &mut Dispatch, inhibited: bool) {
    dispatch.idle_notifier.set_is_inhibited(inhibited);
}
