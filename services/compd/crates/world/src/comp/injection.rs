//! Bus-injected input state on the compd host.
//!
//! The verbs themselves are policy-host `input`; this is what they keep
//! between calls:
//! - the `input_seq` mint and the injected-event count (a sequence's yield);
//! - each seat's injected holds, owned by the single verb (`None`) or the
//!   sequence run that pressed them, so `release_all` and an aborted run let
//!   go of what injection pressed and nothing a person holds;
//! - where the agent pointer is (the agent seat has no cursor of its own:
//!   its position exists only because injection put it there);
//! - each seat's last input and which seat moved last (`input.seats.*`,
//!   `input.last_origin`);
//! - the nested host passthrough gate (`input.host.passthrough`).
//!
//! [`human_input`] is the one call-out at the human-input point (scenegraph
//! `lifecycle::input`, every device event of both backends). It is plain
//! field work on `&mut Loop`: no lock, no atomic, no allocation.
//!
//! Human input does NOT clear the agent seat: the seats are
//! independent, and the agent is cleared only when input authority is lost
//! (a session lock, a VT switch). The compd host does that from the session
//! pause (`compd` `Bus::service`).

use std::collections::BTreeSet;

use policy::agent::Holds;
use surfaces::SeatKind;
use smithay::backend::input::{ButtonState, InputBackend, InputEvent, KeyState, KeyboardKeyEvent, PointerButtonEvent};

use crate::state::Loop;

#[derive(Debug)]
pub struct Injection {
    /// The last `input_seq` minted (0 = none yet).
    seq: u64,
    /// Events injected so far (a sequence yields every 256).
    pub events: u64,
    /// The sequence run whose step is being injected: the owner of what it
    /// presses (`None` = a single verb).
    pub current_run: Option<u64>,
    pub human: Holds,
    pub agent: Holds,
    /// The agent pointer, in host-Space (world) logical coordinates; `None`
    /// until the agent pointer is first moved.
    pub agent_pointer: Option<(f64, f64)>,
    /// Monotonic microseconds of each seat's last input (human, agent).
    pub last_input_us: [Option<u64>; 2],
    pub last_origin: Option<SeatKind>,
    /// `input.host.passthrough` exists (the nested backend).
    pub host_passthrough_available: bool,
    /// `input.host.passthrough`: host pointer and key input reach the seat.
    pub host_passthrough: bool,
    /// Host keys and buttons pressed while passthrough was on: their releases
    /// always pass, so turning passthrough off never strands one.
    host_held_keys: BTreeSet<u32>,
    host_held_buttons: BTreeSet<u32>,
}

impl Default for Injection {
    fn default() -> Self {
        Self {
            seq: 0,
            events: 0,
            current_run: None,
            human: Holds::default(),
            agent: Holds::default(),
            agent_pointer: None,
            last_input_us: [None, None],
            last_origin: None,
            host_passthrough_available: false,
            host_passthrough: true,
            host_held_keys: BTreeSet::new(),
            host_held_buttons: BTreeSet::new(),
        }
    }
}

impl Injection {
    /// The one mint for `input_seq`: never taken from a caller, so it is
    /// strictly increasing by construction.
    pub fn next_seq(&mut self) -> u64 {
        self.seq = self.seq.saturating_add(1);
        self.seq
    }

    pub fn holds_mut(&mut self, seat: SeatKind) -> &mut Holds {
        match seat {
            SeatKind::Human => &mut self.human,
            SeatKind::Agent => &mut self.agent,
        }
    }

    /// A seat delivered input (an injected human event counts as human).
    pub fn note_activity(&mut self, seat: SeatKind) {
        let slot = match seat {
            SeatKind::Human => 0,
            SeatKind::Agent => 1,
        };
        self.last_input_us[slot] = Some(monotonic_us());
        self.last_origin = Some(seat);
    }

    pub fn last_input_us(&self, seat: SeatKind) -> Option<u64> {
        match seat {
            SeatKind::Human => self.last_input_us[0],
            SeatKind::Agent => self.last_input_us[1],
        }
    }

    /// `props.set input.host.passthrough`.
    pub fn set_host_passthrough(&mut self, passthrough: bool) {
        self.host_passthrough = passthrough;
    }

    /// The host passthrough filter for one device event: with passthrough
    /// off, host pointer and key input is dropped, except the release of a
    /// key or button pressed while it was on.
    fn host_gate<I: InputBackend>(&mut self, event: &InputEvent<I>) -> bool {
        let open = self.host_passthrough;
        match event {
            InputEvent::Keyboard { event } => {
                let code = event.key_code().raw();
                match event.state() {
                    KeyState::Pressed => {
                        if open {
                            self.host_held_keys.insert(code);
                        }
                        open
                    }
                    KeyState::Released => self.host_held_keys.remove(&code) || open,
                }
            }
            InputEvent::PointerButton { event } => {
                let button = event.button_code();
                match event.state() {
                    ButtonState::Pressed => {
                        if open {
                            self.host_held_buttons.insert(button);
                        }
                        open
                    }
                    ButtonState::Released => self.host_held_buttons.remove(&button) || open,
                }
            }
            InputEvent::PointerMotion { .. }
            | InputEvent::PointerMotionAbsolute { .. }
            | InputEvent::PointerAxis { .. } => open,
            _ => true,
        }
    }
}

/// Monotonic microseconds (`CLOCK_MONOTONIC`).
pub fn monotonic_us() -> u64 {
    smithay::utils::Clock::<smithay::utils::Monotonic>::new().now().as_micros()
}

/// A human button or key press reached a client (the pointer's focus for a
/// button, the keyboard's for a key): the panel holders' press-time liveness
/// trigger. Only while Quoin has panels.
pub fn note_press(lp: &mut Loop, button: bool) {
    use smithay::reexports::wayland_server::Resource;
    if lp.inner.comp.panels.is_empty() {
        return;
    }
    let seat = &lp.state.seat.seat;
    let surface = if button {
        seat.get_pointer().and_then(|pointer| pointer.current_focus())
    } else {
        seat.get_keyboard().and_then(|keyboard| keyboard.current_focus())
    };
    let client = surface.and_then(|surface| surface.client()).map(|client| client.id());
    lp.inner.comp.panels.user_input.push(client);
}

/// The human-input point (scenegraph `lifecycle::input`): every device event
/// of both backends. Returns whether the event may proceed (the nested host
/// passthrough gate) and records human activity (and, while Quoin has
/// panels, presses) for those that do.
/// Human input happened (a device event, or an injection on the human
/// seat): the human clock moves, and every seat's ext-idle-notify
/// notification resets. Human
/// input wakes everyone. Agent input never calls this: an agent driving the
/// desktop neither wakes the screen nor counts as user activity.
pub fn note_human_activity(lp: &mut Loop) {
    lp.inner.comp.injection.note_activity(SeatKind::Human);
    dispatcher::idle::note_human_activity(&mut lp.state);
}

pub fn human_input<I: InputBackend>(lp: &mut Loop, event: &InputEvent<I>) -> bool {
    let injection = &mut lp.inner.comp.injection;
    if !injection.host_gate(event) {
        return false;
    }
    // A region selection holds the seat: it takes the event.
    if super::region::device_input(lp, event) {
        note_human_activity(lp);
        return false;
    }
    if !matches!(
        event,
        InputEvent::DeviceAdded { .. } | InputEvent::DeviceRemoved { .. } | InputEvent::Special(_)
    ) {
        note_human_activity(lp);
    }
    match event {
        InputEvent::PointerButton { event } if event.state() == ButtonState::Pressed => note_press(lp, true),
        InputEvent::Keyboard { event } if event.state() == KeyState::Pressed => note_press(lp, false),
        _ => {}
    }
    true
}
