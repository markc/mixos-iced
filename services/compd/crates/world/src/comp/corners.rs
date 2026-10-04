//! Hot corners on the compd host.
//!
//! [`Corners`] is the engine-free half: policy's detector and press
//! ownership on one output at a time, in output-local logical coordinates,
//! with the topics it earned queued for the Bus. [`sample`] and [`button`]
//! are the two call-outs seat makes from the pointer path, BEFORE
//! anything is forwarded to a client or to the canvas edge pan. The dwell timer is
//! the host's: it arms a one-shot at [`Corners::next_deadline`] and calls
//! [`Corners::tick`], so nothing polls.
//!
//! Outputs are named by their smithay name here; the Bus host turns the name
//! into the `o_<slug>` key when it publishes.
//!
//! Corner supremacy over pointer constraints: when a corner engages, the
//! human pointer's active
//! constraint is deactivated so a confined game cannot trap the cursor out of
//! the corner; a constraint broken (or, in the engine's `new_constraint`,
//! declined) while a corner is engaged is re-activated by the first real motion
//! sample outside every corner, never by a detector reset. A LOCKED pointer
//! never moves and so never reaches a corner (a known limit).
//!
//! Not here yet: the hover/flash affordance, with the furniture drawing work.

use std::time::Instant;

use policy::corner::{
    Corner, CornerConfig, CornerDetector, CornerEvent, CornerEvents, CornerPresses, CornerTopic, PressFacts,
};
use smithay::utils::{Physical, Point};
use smithay::wayland::pointer_constraints::with_pointer_constraint;

use crate::state::Loop;

/// The corner state of the compd host.
#[derive(Debug)]
pub struct Corners {
    config: CornerConfig,
    detector: CornerDetector,
    presses: CornerPresses,
    /// The output the detector samples (smithay name) and its logical size.
    region: Option<(String, (f64, f64))>,
    /// The last sample, output-local logical: the dwell timer and a
    /// release's re-sample sample here again.
    last: Option<(f64, f64)>,
    clock: Instant,
    topics: Vec<CornerTopic>,
    /// Bumped when the configuration changes (a set, or the first entry
    /// ending discovery): the edge pass and `compd.truth` must see it.
    changes: u64,
    /// An injected move with `corners: false` is running: sampling resets
    /// instead (an engaged or dwelling corner is still left).
    suppressed: bool,
}

impl Default for Corners {
    fn default() -> Self {
        let config = CornerConfig::default();
        Self {
            config,
            detector: CornerDetector::new(config, (0.0, 0.0)),
            presses: CornerPresses::default(),
            region: None,
            last: None,
            clock: Instant::now(),
            topics: Vec::new(),
            changes: 0,
            suppressed: false,
        }
    }
}

impl Corners {
    pub fn config(&self) -> CornerConfig {
        self.config
    }

    /// `comp.input.pointer.move {corners: false}` (comp
    /// `InjectionState::suppress_corners`), for the duration of one move.
    pub fn set_suppressed(&mut self, suppressed: bool) {
        self.suppressed = suppressed;
    }

    /// Configuration changes so far (see the field).
    pub fn changes(&self) -> u64 {
        self.changes
    }

    /// The output the detector is on (smithay name).
    pub fn output(&self) -> Option<&str> {
        self.region.as_ref().map(|(name, _)| name.as_str())
    }

    pub fn engaged(&self) -> Option<Corner> {
        self.detector.engaged_corner()
    }

    /// The hotspot the pointer is in, dwelled or not.
    pub fn contact(&self) -> Option<Corner> {
        self.detector.contact_corner()
    }

    /// Buttons the corners own (pressed on an engaged corner, not yet released).
    pub fn owned_buttons(&self) -> Vec<u32> {
        self.presses.owned().collect()
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.clock.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// When the pending dwell completes; `None` while nothing is pending.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.detector
            .next_deadline_ms()
            .map(|ms| self.clock + std::time::Duration::from_millis(ms))
    }

    /// The topics earned since the last call, in order.
    pub fn take_topics(&mut self) -> Vec<CornerTopic> {
        std::mem::take(&mut self.topics)
    }

    /// One pointer sample on `output` (logical `size`), at `local` (output
    /// logical) after an attempted motion of `attempted` (the outward push
    /// a pinned cursor still makes). Returns whether the pointer is in a
    /// hotspot, dwelled or not: corner bands win over the canvas edge pan.
    pub fn sample(
        &mut self,
        output: &str,
        size: (f64, f64),
        local: (f64, f64),
        attempted: (f64, f64),
        session_lock: bool,
    ) -> bool {
        if session_lock {
            self.reset();
            return false;
        }
        if self
            .region
            .as_ref()
            .is_none_or(|(name, region_size)| name != output || *region_size != size)
        {
            self.reset();
            self.region = Some((output.to_string(), size));
            let events = self.detector.reconfigure(self.config, size);
            self.emit(events);
        }
        let events = self.detector.sample(self.now_ms(), local, attempted);
        self.emit(events);
        self.presses.pointer_moved(local, self.config.deadzone_px);
        self.last = Some(local);
        self.detector.contact_corner().is_some()
    }

    /// The dwell timer fired: sample the resting pointer again.
    pub fn tick(&mut self) {
        let (Some((output, size)), Some(local)) = (self.region.clone(), self.last) else {
            return;
        };
        self.sample(&output, size, local, (0.0, 0.0), false);
    }

    /// The pointer left every output (or corners were suppressed): leave
    /// any engaged corner and cancel owed clicks; ownership stays.
    pub fn reset(&mut self) {
        self.presses.cancel_all();
        let events = self.detector.reset();
        self.emit(events);
        self.region = None;
        self.last = None;
    }

    /// A button went down. Returns whether the corners consume it (the press,
    /// and later its release, never reach a client).
    pub fn press(&mut self, button: u32, modifiers: Vec<&'static str>, session_lock: bool) -> bool {
        let engaged = self.region.as_ref().and_then(|(output, _)| {
            Some((
                output.clone(),
                self.detector.engaged_corner()?,
                self.detector.engaged_dwell_ms()?,
            ))
        });
        self.presses.consume_press(
            button,
            PressFacts {
                session_lock,
                engaged,
                position: self.last.unwrap_or_default(),
                modifiers,
            },
        )
    }

    /// A button came up. Returns whether the corners consume it; a click still
    /// owed is re-checked against the corner at the pointer first.
    pub fn release(&mut self, button: u32) -> bool {
        if !self.presses.owns(button) {
            return false;
        }
        if self.presses.owes_click(button) {
            self.tick();
        }
        if let Some(topics) = self.presses.consume_release(button) {
            self.topics.extend(topics);
        }
        true
    }

    /// `props.set input.corners.*`: a change
    /// to a detection leaf ends any engagement.
    pub fn set_config(&mut self, config: CornerConfig) {
        if config == self.config {
            return;
        }
        self.config = config;
        self.changes = self.changes.wrapping_add(1);
        let size = self.region.as_ref().map_or((0.0, 0.0), |(_, size)| *size);
        let events = self.detector.reconfigure(config, size);
        self.emit(events);
    }

    fn emit(&mut self, events: CornerEvents) {
        let Some((output, _)) = self.region.clone() else {
            return;
        };
        for event in events.into_iter().flatten() {
            match event {
                CornerEvent::Left { .. } => self.presses.cancel_all(),
                // The first reveal of any kind ends the discovery flash.
                CornerEvent::Entered { .. } if self.config.discovery => {
                    self.config.discovery = false;
                    self.changes = self.changes.wrapping_add(1);
                }
                CornerEvent::Entered { .. } => {}
            }
        }
        self.topics.extend(CornerTopic::from_events(&output, events));
    }
}

/// The seat motion call-out: `position` is the cursor in the physical
/// screen space of the output it is on (`screen`, that output's physical
/// size), `attempted` the physical motion it tried to make. Returns whether
/// the pointer is in a corner hotspot.
pub fn sample(lp: &mut Loop, position: Point<f64, Physical>, screen: (f64, f64), attempted: (f64, f64)) -> bool {
    if lp.inner.comp.corners.suppressed {
        lp.inner.comp.corners.reset();
        return false;
    }
    let Some((name, scale)) = cursor_output(lp) else {
        lp.inner.comp.corners.reset();
        return false;
    };
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let locked = super::session_lock::active(lp);
    let was_engaged = lp.inner.comp.corners.engaged().is_some();
    let hot = lp.inner.comp.corners.sample(
        &name,
        (screen.0 / scale, screen.1 / scale),
        (position.x / scale, position.y / scale),
        (attempted.0 / scale, attempted.1 / scale),
        locked,
    );
    let engaged = lp.inner.comp.corners.engaged().is_some();
    lp.state.seat.corner_engaged = engaged;
    if engaged && !was_engaged {
        break_constraint(lp);
        lp.state.seat.constraint_deferred = true;
    } else if !engaged && lp.state.seat.constraint_deferred {
        lp.state.seat.constraint_deferred = false;
        activate_pending_constraint(lp);
    }
    hot
}

/// The human pointer's focused
/// surface's ACTIVE constraint is deactivated (the corner wins).
fn break_constraint(lp: &mut Loop) {
    let Some(pointer) = lp.state.seat.seat.get_pointer() else { return };
    let Some(surface) = pointer.current_focus() else { return };
    // The engine's own deactivate (vendored smithay's takes the state, the
    // surface and the pointer), which also routes the unlock hint as every
    // other deactivation does. A no-op without an active constraint.
    lp.state.deactivate_constraint_for(&surface, &pointer);
}

/// The focused surface's
/// pending constraint is activated, under the same conditions the engine's
/// `new_constraint` activates one (the surface has the keyboard, the hand
/// tool is not suspending constraints).
fn activate_pending_constraint(lp: &mut Loop) {
    if lp.state.seat.constraints_suspended {
        return;
    }
    let Some(pointer) = lp.state.seat.seat.get_pointer() else { return };
    let Some(surface) = pointer.current_focus() else { return };
    if !lp.state.seat.is_keyboard_focused(&surface) {
        return;
    }
    with_pointer_constraint(&surface, &pointer, |constraint| {
        if let Some(constraint) = constraint
            && !constraint.is_active()
        {
            constraint.activate();
        }
    });
}

/// The seat button call-out. Returns whether the corners consumed the button.
pub fn button(lp: &mut Loop, button: u32, pressed: bool) -> bool {
    if !pressed {
        return lp.inner.comp.corners.release(button);
    }
    // Read at the press; the release may hold different modifiers.
    let modifiers = lp
        .state
        .seat
        .seat
        .get_keyboard()
        .map(|keyboard| {
            let state = keyboard.modifier_state();
            [
                (state.shift, "shift"),
                (state.ctrl, "ctrl"),
                (state.alt, "alt"),
                (state.logo, "super"),
            ]
            .into_iter()
            .filter_map(|(active, name)| active.then_some(name))
            .collect()
        })
        .unwrap_or_default();
    let locked = super::session_lock::active(lp);
    lp.inner.comp.corners.press(button, modifiers, locked)
}

/// The output the cursor is on: its smithay name and scale.
fn cursor_output(lp: &Loop) -> Option<(String, f64)> {
    let space = &lp.inner.space_state().state;
    let key = lp.inner.cursor_output.clone();
    let output = space
        .outputs()
        .find(|output| key.as_ref() == Some(&crate::state::state::output_key(output)))
        .or_else(|| space.outputs().next())?;
    Some((output.name(), output.current_scale().fractional_scale()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (f64, f64) = (1_000.0, 800.0);

    fn entered(topics: &[CornerTopic]) -> Vec<(String, Corner)> {
        topics
            .iter()
            .filter_map(|topic| match topic {
                CornerTopic::Entered { output, corner, .. } => Some((output.clone(), *corner)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn outward_push_engages_and_a_click_is_owned() {
        let mut corners = Corners::default();
        assert!(corners.sample("DP-1", SIZE, (5.0, 5.0), (0.0, 0.0), false), "in the hotspot");
        assert!(corners.engaged().is_none(), "not dwelled yet");
        assert!(corners.next_deadline().is_some(), "a dwell is pending");
        // Same position, a later sample with an outward push: early engage.
        std::thread::sleep(std::time::Duration::from_millis(2));
        corners.sample("DP-1", SIZE, (5.0, 5.0), (0.0, 0.0), false);
        std::thread::sleep(std::time::Duration::from_millis(2));
        corners.sample("DP-1", SIZE, (5.0, 5.0), (-3.0, 0.0), false);
        assert_eq!(corners.engaged(), Some(Corner::TopLeft));
        assert_eq!(entered(&corners.take_topics()), [("DP-1".to_string(), Corner::TopLeft)]);
        assert!(corners.press(0x110, Vec::new(), false), "an engaged press is consumed");
        assert!(corners.release(0x110));
        let topics = corners.take_topics();
        assert!(matches!(topics.as_slice(), [CornerTopic::Clicked { .. }, CornerTopic::ClickedV2 { .. }]));
        assert!(!corners.release(0x110), "not owned any more");
    }

    #[test]
    fn leaving_the_hotspot_leaves_and_cancels_the_owed_click() {
        let mut corners = Corners::default();
        corners.sample("DP-1", SIZE, (995.0, 795.0), (0.0, 0.0), false);
        std::thread::sleep(std::time::Duration::from_millis(2));
        corners.sample("DP-1", SIZE, (995.0, 795.0), (0.0, 0.0), false);
        std::thread::sleep(std::time::Duration::from_millis(2));
        corners.sample("DP-1", SIZE, (995.0, 795.0), (2.0, 2.0), false);
        assert_eq!(corners.engaged(), Some(Corner::BottomRight));
        assert!(corners.press(0x110, Vec::new(), false));
        assert!(!corners.sample("DP-1", SIZE, (500.0, 400.0), (0.0, 0.0), false));
        let topics = corners.take_topics();
        assert!(topics.iter().any(|topic| matches!(topic, CornerTopic::Left { corner: Corner::BottomRight, .. })));
        assert!(corners.release(0x110), "still owned");
        assert!(corners.take_topics().is_empty(), "but the click was cancelled");
    }

    #[test]
    fn config_changes_count_and_the_first_entry_ends_discovery() {
        let mut corners = Corners::default();
        let config = CornerConfig { discovery: true, ..CornerConfig::default() };
        corners.set_config(config);
        corners.set_config(config);
        assert_eq!(corners.changes(), 1, "an unchanged set is not a change");
        corners.sample("DP-1", SIZE, (5.0, 5.0), (0.0, 0.0), false);
        std::thread::sleep(std::time::Duration::from_millis(2));
        corners.sample("DP-1", SIZE, (5.0, 5.0), (0.0, 0.0), false);
        std::thread::sleep(std::time::Duration::from_millis(2));
        corners.sample("DP-1", SIZE, (5.0, 5.0), (-1.0, 0.0), false);
        assert_eq!(corners.engaged(), Some(Corner::TopLeft));
        assert!(!corners.config().discovery);
        assert_eq!(corners.changes(), 2);
    }

    #[test]
    fn an_unengaged_press_is_not_consumed_and_another_output_resets() {
        let mut corners = Corners::default();
        assert!(!corners.press(0x110, Vec::new(), false));
        assert!(!corners.release(0x110));
        corners.sample("DP-1", SIZE, (5.0, 5.0), (0.0, 0.0), false);
        assert_eq!(corners.output(), Some("DP-1"));
        corners.sample("HDMI-A-1", SIZE, (500.0, 400.0), (0.0, 0.0), false);
        assert_eq!(corners.output(), Some("HDMI-A-1"));
        assert!(corners.contact().is_none());
    }
}
