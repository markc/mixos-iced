//! `comp.region.select`, the modal part.
//!
//! While a selection runs, the HUMAN seat belongs to it: every button and key
//! is consumed (Escape or the right button cancels; a left drag selects, a
//! click or a line re-arms), and pointer motion moves the cursor with NO
//! client focus, so no client sees enter, motion or a button. Releases of what
//! was pressed during the run are swallowed after it ends. Device input reaches
//! this through the human-input hook; injected human input (policy-host
//! `input`) through the same calls. Coordinates are output-local logical
//! (the cursor's physical screen position over the output's scale).
//!
//! The verb, its timers and its reply are the host's (policy-host `region`,
//! compd). The overlay is [`overlay`]: plain solids only, drawn only while a
//! selection runs, plus the clean-frame record the selected reply waits for
//! (a selection is never claimed without a frame free of its overlay).
//!
//! Cancellation state: an identified selection carries an [`Identity`]
//! (compositor instance, owner capability, generation). The per-owner
//! retirement map below is bounded ([`OWNER_LIMIT`]) and persistent for the
//! compositor process lifetime, so a cancel-before-select or a reordered
//! mesh delivery can never resurrect a retired generation. A cancel acts on
//! a run only on exact identity equality; the policy host decides which
//! outcome that yields.

use std::collections::{BTreeMap, HashSet};
use std::time::Instant;

use smithay::backend::input::{
    AbsolutePositionEvent, ButtonState, InputBackend, InputEvent, KeyState, KeyboardKeyEvent,
    PointerButtonEvent, PointerMotionEvent,
};
use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::input::pointer::MotionEvent;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Physical, Point, Rectangle, SERIAL_COUNTER, Size};

use dispatcher::state::state::RedrawReason;

use crate::camera::transform::translate::transform::Transform;
use crate::state::Loop;
use crate::state::state::CoordinateTrait;

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Output-local logical `[x, y, width, height]`.
    Selected([i32; 4]),
    Cancelled(&'static str),
    Timeout,
    Busy,
    /// Refused with this error (the output changed under the run).
    Refused(&'static str),
    /// A session lock began (the run ends `locked`).
    Locked,
}

/// One identified selection's caller identity: the compositor process
/// instance it was sent to, the owner capability and the positive capture
/// generation. `comp.region.cancel` matches on all three fields exactly;
/// the terminal reply echoes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub instance: String,
    pub owner: String,
    pub generation: u64,
}

/// How many owner capabilities one compositor process remembers. At the
/// limit an unknown owner is refused before anything starts or retires;
/// known owners stay serviceable. Never silently evicted: an evicted owner
/// could let an old delayed select resurrect.
pub const OWNER_LIMIT: usize = 4096;

/// One owner capability's retirement state, kept for the compositor
/// process lifetime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Owner {
    /// The highest generation ever seen from this owner. A select at or
    /// below it is retired and can never start again; cancels and releases
    /// only ever raise it.
    pub watermark: u64,
    /// The generation an accepted select reserved, while its run lives.
    pub active: Option<u64>,
    /// The last generation that ran and finished: a late cancel for it
    /// answers `already_finished`.
    pub finished: Option<u64>,
}

/// What a reservation attempt found (the policy host names the refusal).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reserve {
    Accepted,
    /// The generation is at or below the owner's watermark: it cannot
    /// start now or later.
    Retired,
    /// The owner is unknown and the map is at [`OWNER_LIMIT`]: nothing was
    /// created or changed.
    Capacity,
}

/// What a cancel that did not match the current run did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retire {
    /// The identity is now retired: it cannot start later. The current
    /// run, if any, is untouched.
    Retired,
    /// The identity already completed.
    AlreadyFinished,
    /// The owner is unknown and the map is at [`OWNER_LIMIT`]: nothing was
    /// created or changed.
    Capacity,
}

#[derive(Debug)]
pub struct Run {
    pub id: u64,
    /// The output's generation when the run started.
    pub generation: u64,
    /// The output being selected on (smithay name).
    pub output: String,
    /// The output the caller named, if any (a re-armed run goes back to it).
    pub requested: Option<String>,
    /// The output's logical size.
    pub bounds: (f64, f64),
    pub scale: f64,
    /// Where the left button went down (output-local logical).
    pub start: Option<(f64, f64)>,
    pub pointer: (f64, f64),
    pub deadline: Instant,
    /// The latest a selected reply may wait for its clean frame.
    pub reply_deadline: Instant,
    pub result: Option<Outcome>,
    /// A frame was built without the overlay after the run finished.
    pub clean: bool,
    /// The keyboard focus the run displaced, restored when it finishes.
    prior_focus: Option<WlSurface>,
    /// The caller's selection identity, for `comp.region.cancel` exact
    /// matching and the terminal reply echo; a legacy select has none.
    pub identity: Option<Identity>,
}

#[derive(Debug, Default)]
pub struct Region {
    pub run: Option<Run>,
    next_id: u64,
    /// Keys and buttons pressed while a run held the seat: their releases are
    /// swallowed, even after it ends.
    keys: HashSet<u32>,
    buttons: HashSet<u32>,
    /// Per-owner retirement state, bounded by [`OWNER_LIMIT`] and kept for
    /// the process lifetime.
    owners: BTreeMap<String, Owner>,
}

impl Region {
    /// A run holds the seat (not yet finished).
    pub fn suspended(&self) -> bool {
        self.run.as_ref().is_some_and(|run| run.result.is_none())
    }

    pub fn next_id(&mut self) -> u64 {
        self.next_id = self.next_id.wrapping_add(1);
        self.next_id
    }

    /// Swallow the release of keys already down when the run started.
    pub fn hold_keys(&mut self, keys: impl IntoIterator<Item = u32>) {
        self.keys.extend(keys);
    }

    /// Reserve `generation` for `owner` before a run begins. The owner's
    /// watermark then covers the generation, so a retried or reordered
    /// select at or below it is retired instead of starting a second
    /// operation. A new owner at the limit is refused without any change.
    pub fn reserve(&mut self, owner: &str, generation: u64) -> Reserve {
        let Some(state) = self.owners.get_mut(owner) else {
            if self.owners.len() >= OWNER_LIMIT {
                return Reserve::Capacity;
            }
            self.owners.insert(
                owner.to_string(),
                Owner {
                    watermark: generation,
                    active: Some(generation),
                    finished: None,
                },
            );
            return Reserve::Accepted;
        };
        if generation <= state.watermark {
            return Reserve::Retired;
        }
        state.watermark = generation;
        state.active = Some(generation);
        Reserve::Accepted
    }

    /// The run for `owner`/`generation` ended: the reservation is spent and
    /// the generation is recorded as finished (a late cancel answers
    /// `already_finished`).
    pub fn release(&mut self, owner: &str, generation: u64) {
        let Some(state) = self.owners.get_mut(owner) else {
            return;
        };
        if state.active == Some(generation) {
            state.active = None;
        }
        state.finished = Some(generation);
        state.watermark = state.watermark.max(generation);
    }

    /// Retire an identity that is not the current run: it can never start
    /// later, and the current run is untouched. A newer generation retires
    /// an older active run's generation without cancelling the run.
    pub fn retire(&mut self, owner: &str, generation: u64) -> Retire {
        let Some(state) = self.owners.get_mut(owner) else {
            if self.owners.len() >= OWNER_LIMIT {
                return Retire::Capacity;
            }
            self.owners.insert(
                owner.to_string(),
                Owner {
                    watermark: generation,
                    active: None,
                    finished: None,
                },
            );
            return Retire::Retired;
        };
        if state.finished == Some(generation) {
            return Retire::AlreadyFinished;
        }
        state.watermark = state.watermark.max(generation);
        Retire::Retired
    }

    /// Whether the current run belongs exactly to `owner`'s `generation`.
    /// The compositor instance is fenced by the caller before this is
    /// consulted, so only the two per-owner fields need to match.
    pub fn run_is(&self, owner: &str, generation: u64) -> bool {
        self.run
            .as_ref()
            .and_then(|run| run.identity.as_ref())
            .is_some_and(|identity| identity.owner == owner && identity.generation == generation)
    }
}

/// Begin holding the seat for `run`: the keyboard leaves its client
/// (remembered for afterwards), the
/// pointer leaves every surface, and keys already down are swallowed when
/// they come up.
pub fn begin(lp: &mut Loop, mut run: Run) {
    let seat = lp.state.seat.seat.clone();
    let time = lp.inner.start_time.elapsed().as_millis() as u32;
    if let Some(keyboard) = seat.get_keyboard() {
        run.prior_focus = keyboard.current_focus();
        keyboard.set_focus(&mut lp.state, None, SERIAL_COUNTER.next_serial());
        let held: Vec<u32> = keyboard
            .pressed_keys()
            .into_iter()
            .map(|key| key.raw())
            .collect();
        lp.inner.comp.region.hold_keys(held);
    }
    if let Some(pointer) = seat.get_pointer() {
        let location = pointer.current_location();
        pointer.motion(
            &mut lp.state,
            None,
            &MotionEvent {
                location,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(&mut lp.state);
    }
    lp.inner.comp.corners.reset();
    lp.inner.comp.region.run = Some(run);
    lp.state.schedule_redraw(RedrawReason::Capture);
}

/// End the run with `outcome`: the seat goes
/// back (keyboard focus restored if its surface still lives). A selected run
/// still waits for a clean frame before the host replies.
pub fn finish(lp: &mut Loop, outcome: Outcome) {
    let Some(run) = lp.inner.comp.region.run.as_mut() else {
        return;
    };
    if run.result.is_some() {
        return;
    }
    run.result = Some(outcome);
    run.clean = false;
    let prior = run.prior_focus.take().filter(|surface| {
        use smithay::reexports::wayland_server::Resource;
        surface.is_alive()
    });
    if let Some(keyboard) = lp.state.seat.seat.get_keyboard() {
        keyboard.set_focus(&mut lp.state, prior, SERIAL_COUNTER.next_serial());
    }
    lp.state.schedule_redraw(RedrawReason::Capture);
}

/// The displayed output-local logical rectangle between two
/// points, rounded outwards and clipped to the output. A click or a line is
/// not a region.
pub fn normalise(bounds: (f64, f64), a: (f64, f64), b: (f64, f64)) -> Option<[i32; 4]> {
    if ![a.0, a.1, b.0, b.1].iter().all(|value| value.is_finite()) {
        return None;
    }
    let (ax, ay) = (a.0.clamp(0.0, bounds.0), a.1.clamp(0.0, bounds.1));
    let (bx, by) = (b.0.clamp(0.0, bounds.0), b.1.clamp(0.0, bounds.1));
    if ax == bx || ay == by {
        return None;
    }
    let (x, y) = (ax.min(bx).floor(), ay.min(by).floor());
    let (right, bottom) = (
        ax.max(bx).ceil().min(bounds.0),
        ay.max(by).ceil().min(bounds.1),
    );
    if right > f64::from(i32::MAX) || bottom > f64::from(i32::MAX) {
        return None;
    }
    let rect = [x as i32, y as i32, (right - x) as i32, (bottom - y) as i32];
    (rect[2] > 0 && rect[3] > 0).then_some(rect)
}

/// The pointer moved to `position` (output-local logical) during a run: the
/// cursor follows with no client focus.
pub fn motion(lp: &mut Loop, position: (f64, f64)) {
    let Some(run) = lp.inner.comp.region.run.as_mut() else {
        return;
    };
    let position = (
        position.0.clamp(0.0, run.bounds.0),
        position.1.clamp(0.0, run.bounds.1),
    );
    run.pointer = position;
    let physical = Point::<f64, Physical>::from((position.0 * run.scale, position.1 * run.scale));
    lp.inner.pointer_mut().motion.x = physical.x;
    lp.inner.pointer_mut().motion.y = physical.y;
    let ctx = lp.size_ctx_all();
    let transform: Transform = (physical, ctx).into();
    let location = transform.into_storage_point_f64();
    let time = lp.inner.start_time.elapsed().as_millis() as u32;
    if let Some(pointer) = lp.state.seat.seat.get_pointer() {
        pointer.motion(
            &mut lp.state,
            None,
            &MotionEvent {
                location,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(&mut lp.state);
    }
    lp.state.schedule_redraw(RedrawReason::Cursor);
}

/// A button during a run, or the release of one pressed during it. Returns
/// whether the region consumed it.
pub fn button(lp: &mut Loop, button: u32, pressed: bool) -> bool {
    let region = &mut lp.inner.comp.region;
    if !region.suspended() {
        // A release of a button the run took is swallowed after it ends.
        return !pressed && region.buttons.remove(&button);
    }
    if pressed {
        region.buttons.insert(button);
    } else {
        region.buttons.remove(&button);
    }
    let Some(run) = region.run.as_mut() else {
        return true;
    };
    let decided = match (button, pressed) {
        (0x111, true) => Some(Outcome::Cancelled("right_button")),
        (0x110, true) => {
            let p = run.pointer;
            if p.0 >= 0.0 && p.1 >= 0.0 && p.0 < run.bounds.0 && p.1 < run.bounds.1 {
                run.start = Some(p);
            }
            None
        }
        (0x110, false) => match run
            .start
            .map(|start| normalise(run.bounds, start, run.pointer))
        {
            Some(Some(rect)) => Some(Outcome::Selected(rect)),
            Some(None) => {
                // A click or a line keeps the selection armed for another drag.
                run.start = None;
                if let Some(requested) = run.requested.clone() {
                    run.output = requested;
                }
                None
            }
            None => None,
        },
        _ => None,
    };
    match decided {
        Some(outcome) => finish(lp, outcome),
        None => lp.state.schedule_redraw(RedrawReason::Capture),
    }
    true
}

/// A key during a run (raw xkb keycode), or the release of one pressed
/// during it. Escape cancels. Returns whether the region consumed it.
pub fn key(lp: &mut Loop, keycode: u32, pressed: bool) -> bool {
    let region = &mut lp.inner.comp.region;
    if !region.suspended() {
        return !pressed && region.keys.remove(&keycode);
    }
    if pressed {
        region.keys.insert(keycode);
    } else {
        region.keys.remove(&keycode);
    }
    // xkb keycode 9 is evdev KEY_ESC (1) + 8.
    if keycode == 9 && pressed {
        finish(lp, Outcome::Cancelled("escape"));
    }
    true
}

/// The human-input hook's call: a device event during (or just after) a run.
/// Returns whether the region consumed it.
pub fn device_input<I: InputBackend>(lp: &mut Loop, event: &InputEvent<I>) -> bool {
    match event {
        InputEvent::Keyboard { event } => key(
            lp,
            event.key_code().raw(),
            event.state() == KeyState::Pressed,
        ),
        InputEvent::PointerButton { event } => button(
            lp,
            event.button_code(),
            event.state() == ButtonState::Pressed,
        ),
        _ if !lp.inner.comp.region.suspended() => false,
        InputEvent::PointerMotionAbsolute { event } => {
            let (width, height) = lp.size_ctx_all().screen_size_physical;
            let size = smithay::utils::Size::<i32, smithay::utils::Logical>::from((
                width.round() as i32,
                height.round() as i32,
            ));
            let at = event.position_transformed(size);
            let scale = lp
                .inner
                .comp
                .region
                .run
                .as_ref()
                .map_or(1.0, |run| run.scale);
            motion(lp, (at.x / scale, at.y / scale));
            true
        }
        InputEvent::PointerMotion { event } => {
            let Some(run) = lp.inner.comp.region.run.as_ref() else {
                return true;
            };
            let delta = event.delta();
            let at = (run.pointer.0 + delta.x, run.pointer.1 + delta.y);
            motion(lp, at);
            true
        }
        // Scroll and touch reach nothing while a selection holds the seat.
        InputEvent::PointerAxis { .. }
        | InputEvent::TouchDown { .. }
        | InputEvent::TouchMotion { .. }
        | InputEvent::TouchUp { .. } => true,
        _ => false,
    }
}

thread_local! {
    static OVERLAY_IDS: [Id; 5] = [Id::new(), Id::new(), Id::new(), Id::new(), Id::new()];
}

const OUTLINE: [f32; 4] = [0.30, 0.60, 1.00, 1.0];
const FILL: [f32; 4] = [0.30, 0.60, 1.00, 0.18];
/// The outline's thickness, logical pixels.
const LINE: f64 = 2.0;

/// The region overlay for one frame (physical screen rectangles): a thin
/// outline round the output while a selection is armed, the selection's
/// outline and a translucent fill while dragging, nothing otherwise. After a
/// run finishes, the first frame built without it is recorded as the clean
/// frame its reply waits for.
pub fn overlay(lp: &mut Loop, _size: Size<i32, Physical>) -> Vec<SolidColorRenderElement> {
    let Some(run) = lp.inner.comp.region.run.as_mut() else {
        return Vec::new();
    };
    if run.result.is_some() {
        run.clean = true;
        return Vec::new();
    }
    let scale = run.scale;
    let px = |value: f64| (value * scale).round() as i32;
    let line = px(LINE).max(1);
    let (x, y, w, h) = match run.start {
        Some(start) => {
            let (left, top) = (start.0.min(run.pointer.0), start.1.min(run.pointer.1));
            let (right, bottom) = (start.0.max(run.pointer.0), start.1.max(run.pointer.1));
            (
                px(left),
                px(top),
                px(right - left).max(1),
                px(bottom - top).max(1),
            )
        }
        None => (0, 0, px(run.bounds.0), px(run.bounds.1)),
    };
    let rect = |x: i32, y: i32, w: i32, h: i32| {
        Rectangle::new(Point::from((x, y)), Size::from((w.max(1), h.max(1))))
    };
    let edges = [
        rect(x, y, w, line),
        rect(x, y + h - line, w, line),
        rect(x, y, line, h),
        rect(x + w - line, y, line, h),
    ];
    OVERLAY_IDS.with(|ids| {
        let mut elements: Vec<SolidColorRenderElement> = edges
            .into_iter()
            .zip(ids.iter())
            .map(|(edge, id)| {
                SolidColorRenderElement::new(
                    id.clone(),
                    edge,
                    CommitCounter::default(),
                    OUTLINE,
                    Kind::Unspecified,
                )
            })
            .collect();
        if run.start.is_some() {
            elements.push(SolidColorRenderElement::new(
                ids[4].clone(),
                rect(x, y, w, h),
                CommitCounter::default(),
                FILL,
                Kind::Unspecified,
            ));
        }
        elements
    })
}

impl Region {
    /// No key or button the region took is still down (a new run may start).
    pub fn idle(&self) -> bool {
        self.run.is_none() && self.keys.is_empty() && self.buttons.is_empty()
    }
}

impl Run {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: u64,
        generation: u64,
        output: String,
        requested: Option<String>,
        bounds: (f64, f64),
        scale: f64,
        pointer: (f64, f64),
        deadline: Instant,
        reply_deadline: Instant,
        identity: Option<Identity>,
    ) -> Self {
        Self {
            id,
            generation,
            output,
            requested,
            bounds,
            scale,
            start: None,
            pointer,
            deadline,
            reply_deadline,
            result: None,
            clean: false,
            prior_focus: None,
            identity,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "a3f9c2d1-4e7b-4a1c-9d8e-5f6b7c8d9e0f";

    fn owner(region: &Region, owner: &str) -> Owner {
        *region.owners.get(owner).expect("known owner")
    }

    #[test]
    fn reserve_accepts_then_retires_the_same_generation_forever() {
        let mut region = Region::default();
        assert_eq!(region.reserve(OWNER, 1), Reserve::Accepted);
        assert_eq!(owner(&region, OWNER).active, Some(1));
        // The same select again, while reserved: never a second operation.
        assert_eq!(region.reserve(OWNER, 1), Reserve::Retired);
        region.release(OWNER, 1);
        assert_eq!(owner(&region, OWNER).active, None);
        // Even after the run ended, the generation stays retired.
        assert_eq!(region.reserve(OWNER, 1), Reserve::Retired);
        // The next generation is admitted.
        assert_eq!(region.reserve(OWNER, 2), Reserve::Accepted);
        assert_eq!(owner(&region, OWNER).watermark, 2);
    }

    #[test]
    fn cancel_before_select_retires_the_delayed_select() {
        let mut region = Region::default();
        assert_eq!(region.retire(OWNER, 5), Retire::Retired);
        // The delayed select at the retired generation never starts.
        assert_eq!(region.reserve(OWNER, 5), Reserve::Retired);
        assert_eq!(region.reserve(OWNER, 6), Reserve::Accepted);
    }

    #[test]
    fn newer_cancel_retires_but_never_cancels_an_older_active_run() {
        let mut region = Region::default();
        assert_eq!(region.reserve(OWNER, 2), Reserve::Accepted);
        // A newer-generation cancel: retired, and the active run is untouched.
        assert_eq!(region.retire(OWNER, 3), Retire::Retired);
        let state = owner(&region, OWNER);
        assert_eq!(state.active, Some(2));
        assert_eq!(state.watermark, 3);
        // An older cancel does not lower the watermark either.
        assert_eq!(region.retire(OWNER, 1), Retire::Retired);
        assert_eq!(owner(&region, OWNER).watermark, 3);
        region.release(OWNER, 2);
    }

    #[test]
    fn late_cancels_answer_already_finished_and_never_touch_a_newer_run() {
        let mut region = Region::default();
        assert_eq!(region.reserve(OWNER, 1), Reserve::Accepted);
        region.release(OWNER, 1);
        assert_eq!(region.retire(OWNER, 1), Retire::AlreadyFinished);
        // A newer run starts; the replayed old cancel must not stop it.
        assert_eq!(region.reserve(OWNER, 2), Reserve::Accepted);
        assert_eq!(region.retire(OWNER, 1), Retire::AlreadyFinished);
        assert_eq!(owner(&region, OWNER).active, Some(2));
        region.release(OWNER, 2);
    }

    #[test]
    fn capacity_refuses_unknown_owners_and_keeps_known_ones_serviceable() {
        let mut region = Region::default();
        for index in 0..OWNER_LIMIT {
            assert_eq!(
                region.reserve(&format!("owner-{index}"), 1),
                Reserve::Accepted
            );
        }
        // An unknown owner is refused before anything starts or retires.
        assert_eq!(region.reserve("new-owner", 1), Reserve::Capacity);
        assert_eq!(region.retire("new-owner", 1), Retire::Capacity);
        assert_eq!(region.owners.len(), OWNER_LIMIT, "nothing was created");
        // Known owners and their cancellation stay serviceable at the limit.
        assert_eq!(region.retire("owner-0", 9), Retire::Retired);
        assert_eq!(region.reserve("owner-1", 2), Reserve::Accepted);
    }

    #[test]
    fn watermarks_never_wrap_or_lower_at_the_maximum() {
        let mut region = Region::default();
        assert_eq!(region.reserve(OWNER, u64::MAX), Reserve::Accepted);
        region.release(OWNER, u64::MAX);
        // The maximum generation stays retired: nothing can be above it.
        assert_eq!(region.reserve(OWNER, u64::MAX), Reserve::Retired);
        assert_eq!(region.retire(OWNER, u64::MAX), Retire::AlreadyFinished);
        // An old cancel after the maximum cannot wrap the watermark down.
        assert_eq!(region.retire(OWNER, 1), Retire::Retired);
        assert_eq!(owner(&region, OWNER).watermark, u64::MAX);
    }
}
