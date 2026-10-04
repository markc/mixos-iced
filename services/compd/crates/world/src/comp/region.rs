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

use std::collections::HashSet;
use std::time::Instant;

use smithay::backend::input::{
    AbsolutePositionEvent, ButtonState, InputBackend, InputEvent, KeyState, KeyboardKeyEvent, PointerButtonEvent,
    PointerMotionEvent,
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
}

#[derive(Debug, Default)]
pub struct Region {
    pub run: Option<Run>,
    next_id: u64,
    /// Keys and buttons pressed while a run held the seat: their releases are
    /// swallowed, even after it ends.
    keys: HashSet<u32>,
    buttons: HashSet<u32>,
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
        let held: Vec<u32> = keyboard.pressed_keys().into_iter().map(|key| key.raw()).collect();
        lp.inner.comp.region.hold_keys(held);
    }
    if let Some(pointer) = seat.get_pointer() {
        let location = pointer.current_location();
        pointer.motion(&mut lp.state, None, &MotionEvent { location, serial: SERIAL_COUNTER.next_serial(), time });
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
    let Some(run) = lp.inner.comp.region.run.as_mut() else { return };
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
    let (right, bottom) = (ax.max(bx).ceil().min(bounds.0), ay.max(by).ceil().min(bounds.1));
    if right > f64::from(i32::MAX) || bottom > f64::from(i32::MAX) {
        return None;
    }
    let rect = [x as i32, y as i32, (right - x) as i32, (bottom - y) as i32];
    (rect[2] > 0 && rect[3] > 0).then_some(rect)
}

/// The pointer moved to `position` (output-local logical) during a run: the
/// cursor follows with no client focus.
pub fn motion(lp: &mut Loop, position: (f64, f64)) {
    let Some(run) = lp.inner.comp.region.run.as_mut() else { return };
    let position = (position.0.clamp(0.0, run.bounds.0), position.1.clamp(0.0, run.bounds.1));
    run.pointer = position;
    let physical = Point::<f64, Physical>::from((position.0 * run.scale, position.1 * run.scale));
    lp.inner.pointer_mut().motion.x = physical.x;
    lp.inner.pointer_mut().motion.y = physical.y;
    let ctx = lp.size_ctx_all();
    let transform: Transform = (physical, ctx).into();
    let location = transform.into_storage_point_f64();
    let time = lp.inner.start_time.elapsed().as_millis() as u32;
    if let Some(pointer) = lp.state.seat.seat.get_pointer() {
        pointer.motion(&mut lp.state, None, &MotionEvent { location, serial: SERIAL_COUNTER.next_serial(), time });
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
    let Some(run) = region.run.as_mut() else { return true };
    let decided = match (button, pressed) {
        (0x111, true) => Some(Outcome::Cancelled("right_button")),
        (0x110, true) => {
            let p = run.pointer;
            if p.0 >= 0.0 && p.1 >= 0.0 && p.0 < run.bounds.0 && p.1 < run.bounds.1 {
                run.start = Some(p);
            }
            None
        }
        (0x110, false) => match run.start.map(|start| normalise(run.bounds, start, run.pointer)) {
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
        InputEvent::Keyboard { event } => key(lp, event.key_code().raw(), event.state() == KeyState::Pressed),
        InputEvent::PointerButton { event } => button(lp, event.button_code(), event.state() == ButtonState::Pressed),
        _ if !lp.inner.comp.region.suspended() => false,
        InputEvent::PointerMotionAbsolute { event } => {
            let (width, height) = lp.size_ctx_all().screen_size_physical;
            let size = smithay::utils::Size::<i32, smithay::utils::Logical>::from((width.round() as i32, height.round() as i32));
            let at = event.position_transformed(size);
            let scale = lp.inner.comp.region.run.as_ref().map_or(1.0, |run| run.scale);
            motion(lp, (at.x / scale, at.y / scale));
            true
        }
        InputEvent::PointerMotion { event } => {
            let Some(run) = lp.inner.comp.region.run.as_ref() else { return true };
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
            (px(left), px(top), px(right - left).max(1), px(bottom - top).max(1))
        }
        None => (0, 0, px(run.bounds.0), px(run.bounds.1)),
    };
    let rect = |x: i32, y: i32, w: i32, h: i32| Rectangle::new(Point::from((x, y)), Size::from((w.max(1), h.max(1))));
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
            .map(|(edge, id)| SolidColorRenderElement::new(id.clone(), edge, CommitCounter::default(), OUTLINE, Kind::Unspecified))
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
        }
    }
}
