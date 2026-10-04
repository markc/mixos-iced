//! `comp.input.*` single verbs.
//!
//! - HUMAN seat: an injected event goes through the engine's own pointer and keyboard
//!   handlers via seat's synthetic backend ([`seat::inject`]), so it
//!   meets the same bindings, corners, edge pan and focus policy as a device.
//!   A `{window}`-targeted button skips the device hit-test: the focus verb
//!   already applied the raise policy.
//! - AGENT seat (`agent`): events go straight to that seat's own
//!   smithay handles, hit-tested by the engine's `surface_under_filtered` in host-Space
//!   coordinates. The agent pointer is invisible and has no cursor; its
//!   position is what injection last put there. Refusals are policy
//!   `agent`'s, fed the facts read here.
//!
//! The full-mesh-access law: no verb here checks who is calling. What stays
//! is correctness: `{id, generation}` freshness, grab and continuity checks
//! (`target_changed`), and the bounds of the coordinates.
//!
//! Coordinates: `{output, x, y}` is output-local logical. The
//! human pointer lives in the engine's physical screen space (output-local × scale);
//! the agent pointer and `{window}` coordinates in the host Space's, which is
//! what `windows.*` and `outputs.*` report.
//!
//! `comp.input.sequence` runs on the compd host's driver through
//! [`run_step`]; [`clear_agent`] handles lost input authority;
//! `input.seats` is [`project_seats`].

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{Value, json};
use smithay::backend::input::{AxisSource, KeyState};
use smithay::desktop::{PopupKeyboardGrab, PopupPointerGrab};
use smithay::wayland::seat::WaylandFocus;
use smithay::input::keyboard::{FilterResult, Keycode, Keysym, xkb};
use smithay::input::pointer::{AxisFrame, ButtonEvent, ClickGrab, MotionEvent};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Point, SERIAL_COUNTER};
use smithay::wayland::input_method::InputMethodSeat as _;

use comp_model::reply::ControlReply;
use comp_model::request::{InputOp, KeySpec, PointerMoveTarget, PressAction, ScrollSource, WindowOp};
use comp_model::snapshot::{OutputSnapshot, SeatFocusSnapshot, SeatPointerSnapshot, SeatSnapshot, output_key};
use dispatcher::state::state::Dispatch;
use dispatcher::wire::trait_::surface_event::SurfaceHandle;
use dispatcher::wire::trait_::wire_trait::WireTrait;
use policy::agent::{
    AgentSeatFacts, AgentTarget, AgentTargetedFacts, AgentTargetedPlan, Hold, HumanTargetedFacts,
    HumanTargetedPlan, agent_preflight, agent_refusal, agent_targeted_preflight, human_targeted_preflight,
    release_order, target_unfocusable,
};
use surfaces::{SeatKind, SurfaceId, SurfaceRole};
use world::camera::transform::translate::transform::Transform;
use world::state::Loop;
use world::state::state::CoordinateTrait;
use world::surface::interface::hit::{SurfaceHit, surface_under_filtered};
use world::window::interface::draw::visible::DrawWindow;

/// `comp.input.*` (one verb). A refused target injects nothing.
pub fn input(lp: &mut Loop, op: &InputOp) -> ControlReply {
    if matches!(op, InputOp::ReleaseAll) {
        // Bare `release_all` cleans both seats' injected holds.
        release_injected(lp, SeatKind::Agent);
        let mut reply = payload(lp, SeatKind::Human, op, None);
        if let ControlReply::Body(body) = &mut reply {
            body["seat"] = json!("both");
        }
        return reply;
    }
    let (seat, op) = match op {
        InputOp::OnSeat { seat, op } => (*seat, op.as_ref()),
        _ => (SeatKind::Human, op),
    };
    let mut reply = payload(lp, seat, op, None);
    match &mut reply {
        ControlReply::Body(body) | ControlReply::Refused { detail: body, .. } => {
            body["seat"] = json!(seat.name());
        }
        _ => return ControlReply::WithInputSeat { seat, reply: Box::new(reply) },
    }
    reply
}

/// One step of a `comp.input.sequence` run: the single-verb path, with the
/// run as the owner of what it presses.
pub fn run_step(lp: &mut Loop, run: u64, op: &InputOp) -> ControlReply {
    lp.inner.comp.injection.current_run = Some(run);
    let reply = input(lp, op);
    lp.inner.comp.injection.current_run = None;
    reply
}

/// A run ended early (refused step, cleared, or its caller gone): give up
/// its holds and release the ones no other owner still holds.
pub fn release_run(lp: &mut Loop, run: u64) {
    let time = now_ms(lp);
    for seat in [SeatKind::Human, SeatKind::Agent] {
        let orphaned = lp.inner.comp.injection.holds_mut(seat).drop_owner(Some(run));
        release_holds(lp, seat, orphaned, time);
    }
}

/// A session lock is beginning: the client that still holds the keyboard gets
/// the releases of every held non-modifier key now, before the lock takes
/// focus away, so it does not come back to a stuck key. The host calls this
/// before `world::comp::session_lock::service`.
pub fn release_keys_for_lock(lp: &mut Loop) {
    if world::comp::session_lock::entering(lp) {
        seat::keyboard::input::keyboard::release_held_keys(lp);
    }
}

/// Input authority was lost (a VT switch / session pause): the agent seat
/// lets go of every key and button,
/// drops its grabs and focus, and forgets where its pointer was. The host
/// bumps the agent epoch and answers the agent's runs `input_cleared`.
pub fn clear_agent(lp: &mut Loop) {
    dismiss_agent_popups(lp);
    let Some(agent) = lp.state.seat.agent.clone() else { return };
    let time = now_ms(lp);
    if let Some(keyboard) = agent.get_keyboard() {
        keyboard.unset_grab(&mut lp.state);
        for key in keyboard.pressed_keys() {
            keyboard.input::<(), _>(
                &mut lp.state,
                key,
                KeyState::Released,
                SERIAL_COUNTER.next_serial(),
                time,
                |_, _, _| FilterResult::Forward,
            );
        }
        keyboard.set_focus(&mut lp.state, None, SERIAL_COUNTER.next_serial());
    }
    if let Some(pointer) = agent.get_pointer() {
        pointer.unset_grab_without_focus_restore(&mut lp.state, SERIAL_COUNTER.next_serial(), time);
        for button in pointer.current_pressed() {
            pointer.button(
                &mut lp.state,
                &ButtonEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                    button,
                    state: button_state(false),
                },
            );
        }
        let location = pointer.current_location();
        pointer.motion(
            &mut lp.state,
            None,
            &MotionEvent { location, serial: SERIAL_COUNTER.next_serial(), time },
        );
        pointer.frame(&mut lp.state);
    }
    let injection = &mut lp.inner.comp.injection;
    injection.agent = Default::default();
    injection.agent_pointer = None;
}

/// Inject one op on one seat, then reply with
/// the `input_seq` minted for it and where it went.
fn payload(lp: &mut Loop, seat: SeatKind, op: &InputOp, target_window: Option<(u64, u64)>) -> ControlReply {
    if seat == SeatKind::Agent {
        let facts = agent_seat_facts(lp);
        if let Err(reply) = agent_preflight(op, &facts) {
            return reply;
        }
    }
    let time = now_ms(lp);
    let injected_at_us = world::comp::injection::monotonic_us();
    let mut key_result: (Option<&'static str>, usize, Option<(u64, u64)>) = (None, 0, None);
    let mut button_delivery = None;
    let keyboard = match op {
        InputOp::OnSeat { .. } => return input(lp, op),
        InputOp::Targeted { id, generation, raise, op } => {
            return match seat {
                SeatKind::Agent => agent_targeted(lp, *id, *generation, *raise, op),
                SeatKind::Human => human_targeted(lp, *id, *generation, *raise, op),
            };
        }
        InputOp::PointerMove { target, corners } => {
            let moved = match seat {
                SeatKind::Agent => move_agent_pointer(lp, target, time),
                SeatKind::Human => move_human_pointer(lp, target, *corners, time),
            };
            if let Err(reply) = moved {
                return reply;
            }
            false
        }
        InputOp::PointerButton { button, action } => {
            let expected = target_window
                .or_else(|| (seat == SeatKind::Agent).then(|| delivery_target(lp, seat, false)).flatten());
            for pressed in press_states(*action) {
                if seat == SeatKind::Agent
                    && *pressed
                    && expected.is_some()
                    && delivery_target(lp, seat, false) != expected
                {
                    return ControlReply::refused("target_changed", json!({}));
                }
                button_delivery = delivery_target(lp, seat, false);
                match (seat, target_window) {
                    (SeatKind::Human, Some((id, generation))) => {
                        targeted_human_button(lp, id, generation, *button, *pressed, time);
                        button_delivery = delivery_target(lp, seat, false);
                    }
                    _ => inject_button(lp, seat, *button, *pressed, time),
                }
            }
            false
        }
        InputOp::PointerScroll { dx, dy, source, v120 } => {
            inject_scroll(lp, seat, *dx, *dy, *source, *v120, time);
            false
        }
        InputOp::Key { key, action, modifiers } => {
            let index = keymap_index(lp, seat);
            let Some((keycode, shifted)) = resolve_key(&index, key) else {
                return unknown_key(key);
            };
            let mut held = Vec::with_capacity(modifiers.len() + 1);
            for modifier in modifiers {
                let Some((modifier, _)) = resolve_key(&index, modifier) else {
                    return unknown_key(modifier);
                };
                held.push(modifier);
            }
            if shifted {
                let shift = KeySpec::Name("Shift_L".into());
                let Some((shift, _)) = resolve_key(&index, &shift) else {
                    return unknown_key(&shift);
                };
                if !held.contains(&shift) {
                    held.push(shift);
                }
            }
            let mut events = Vec::new();
            if *action != PressAction::Release {
                for modifier in &held {
                    events.push((*modifier, true, false));
                }
            }
            for pressed in press_states(*action) {
                events.push((keycode, *pressed, true));
            }
            if *action != PressAction::Press {
                for modifier in held.iter().rev() {
                    events.push((*modifier, false, false));
                }
            }
            let delivery = target_window
                .or_else(|| (seat == SeatKind::Agent).then(|| delivery_target(lp, seat, true)).flatten());
            key_result = inject_keys(lp, seat, events, delivery, time);
            true
        }
        InputOp::Text(text) => {
            // An input method holding the keyboard would compose the keys
            // into something else; typed text must arrive as sent.
            if seat_of(lp, seat).is_some_and(|handle| handle.input_method().keyboard_grabbed()) {
                return ControlReply::refused("ime_active", json!({}));
            }
            let index = keymap_index(lp, seat);
            let shift = resolve_key(&index, &KeySpec::Name("Shift_L".into()));
            let mut keys = Vec::with_capacity(text.len());
            for (position, character) in text.chars().enumerate() {
                // XKB's Return keysym reads back as carriage return.
                let lookup = if character == '\n' { '\r' } else { character };
                match index.by_char.get(&u32::from(lookup)) {
                    Some((keycode, false)) => keys.push((*keycode, None)),
                    Some((keycode, true)) if shift.is_some() => {
                        keys.push((*keycode, shift.map(|(shift, _)| shift)));
                    }
                    _ => {
                        return ControlReply::refused(
                            "unmappable",
                            json!({"char": character.to_string(), "index": position}),
                        );
                    }
                }
            }
            let mut events = Vec::new();
            for (keycode, shift) in keys {
                if let Some(shift) = shift {
                    events.push((shift, true, false));
                }
                events.push((keycode, true, true));
                events.push((keycode, false, true));
                if let Some(shift) = shift {
                    events.push((shift, false, false));
                }
            }
            let delivery = target_window
                .or_else(|| (seat == SeatKind::Agent).then(|| delivery_target(lp, seat, true)).flatten());
            key_result = inject_keys(lp, seat, events, delivery, time);
            true
        }
        InputOp::ReleaseAll => {
            release_injected(lp, seat);
            false
        }
    };
    let input_seq = lp.inner.comp.injection.next_seq();
    let target = if keyboard {
        key_result.2
    } else if target_window.is_some() {
        button_delivery
    } else {
        button_delivery.or_else(|| delivery_target(lp, seat, false))
    };
    // The window the input reached (its root
    // toplevel) times its first update committed afterwards, within a
    // second, as input-to-present. `input_seq` is minted just above, so it
    // is strictly increasing by construction.
    let window = target.and_then(|(id, _)| {
        let root = root_of(lp, SurfaceId(id));
        lp.inner
            .comp
            .registry
            .get(root)
            .filter(|record| record.role().managed_toplevel())
            .map(|record| (record.id().0, record.generation()))
    });
    lp.inner.comp.presentation.stats.mark_input(
        window,
        ledger::presentation_stats::InputMark { seat, input_seq, injected_at_us },
    );
    let pointer = pointer_position(lp, seat);
    let body = json!({
        "input_seq": input_seq,
        "injected_at_us": injected_at_us,
        "pointer": pointer.map(|(output, _, x, y)| json!({"output": output, "x": x, "y": y})),
        "target": target.map(|(id, generation)| json!({"id": id, "generation": generation})),
        "targeted": target_window.map(|(id, generation)| json!({"id": id, "generation": generation})),
        "completed_events": key_result.1,
    });
    match key_result.0 {
        Some(reason) => ControlReply::refused(reason, body),
        None => ControlReply::Body(body),
    }
}

// ── the human seat ───────────────────────────────────────────────────────────

/// The refusal ladder, then `comp.window.focus`
/// (with `raise`), then the op with the focus verified.
fn human_targeted(lp: &mut Loop, id: u64, generation: u64, raise: bool, op: &InputOp) -> ControlReply {
    let window = window_record(lp, id, generation).and_then(|sid| crate::control::window_of(lp, sid));
    let human = lp.state.seat.seat.clone();
    let facts = HumanTargetedFacts {
        region_select: false,
        session_lock: world::comp::session_lock::active(lp),
        exclusive_layer: exclusive_layer(lp),
        current_workspace: lp.inner.comp.current_workspace(),
        input_presentable: true,
        visible: window.as_ref().is_some_and(|window| crate::control::drawn(lp, window)),
        keyboard_grab: human.get_keyboard().is_some_and(|keyboard| keyboard.is_grabbed())
            || human.input_method().keyboard_grabbed(),
        pointer_grab: lp.inner.comp.interactive.is_some()
            || (human.get_pointer().is_some_and(|pointer| pointer.is_grabbed())
                && delivery_target(lp, SeatKind::Human, false) != Some((id, generation))),
    };
    let plan = human_targeted_preflight(&lp.inner.comp.registry, id, generation, op, &facts);
    match plan {
        Err(reply) => reply,
        Ok(HumanTargetedPlan::Release) => payload(lp, SeatKind::Human, op, Some((id, generation))),
        Ok(HumanTargetedPlan::FocusThenDeliver) => {
            let focused = crate::control::window(lp, &WindowOp::Focus { id, generation, raise });
            if !matches!(&focused, Some(ControlReply::Body(body)) if body["focused"] == true)
                || delivery_target(lp, SeatKind::Human, true) != Some((id, generation))
            {
                return target_unfocusable(id, generation, "focus_refused");
            }
            // No event-loop yield between the fence, the focus and the injection.
            payload(lp, SeatKind::Human, op, Some((id, generation)))
        }
    }
}

/// The button goes to the named window
/// without the device hit-test (which would raise it regardless of `raise`).
fn targeted_human_button(lp: &mut Loop, id: u64, generation: u64, button: u32, pressed: bool, time: u32) {
    let Some(pointer) = lp.state.seat.seat.get_pointer() else { return };
    if !pointer.is_grabbed() {
        let Some(sid) = window_record(lp, id, generation) else { return };
        let Some(window) = crate::control::window_of(lp, sid) else { return };
        let Some(surface) = window.wl_surface().map(|surface| surface.into_owned()) else { return };
        let Some(origin) = surface_origin(lp, &window) else { return };
        let location = pointer.current_location();
        pointer.motion(
            &mut lp.state,
            Some((surface, origin)),
            &MotionEvent { location, serial: SERIAL_COUNTER.next_serial(), time },
        );
        pointer.frame(&mut lp.state);
    }
    note_hold(lp, SeatKind::Human, Hold::Button(button), pressed);
    injected(lp, SeatKind::Human);
    pointer.button(
        &mut lp.state,
        &ButtonEvent {
            serial: SERIAL_COUNTER.next_serial(),
            time,
            button,
            state: button_state(pressed),
        },
    );
    pointer.frame(&mut lp.state);
}

/// A pointer move on the human seat, delivered through the engine's pointer
/// path (corners, edge pan, focus).
fn move_human_pointer(lp: &mut Loop, target: &PointerMoveTarget, corners: bool, time: u32) -> Result<(), ControlReply> {
    let moved = match target {
        PointerMoveTarget::Relative { dx, dy } => Moved::By(*dx, *dy),
        PointerMoveTarget::Output { output, x, y } => {
            let (_, row) = output_row(lp, output.as_deref(), *x, *y)?;
            // The cursor goes to that output (the engine keeps one cursor output).
            let name = row.name.clone();
            let key = lp
                .inner
                .space_state()
                .state
                .outputs()
                .find(|candidate| candidate.name() == name)
                .map(world::state::state::output_key);
            if key.is_some() {
                lp.inner.cursor_output = key;
            }
            Moved::To(Point::from((x * row.scale, y * row.scale)))
        }
        PointerMoveTarget::Window { id, generation, x, y, require_hit } => {
            let world = window_point(lp, *id, *generation, *x, *y)?;
            if *require_hit {
                check_hit(lp, *id, *x, *y, world)?;
            }
            let transform: Transform = (Point::<f64, Logical>::from(world), lp.size_ctx_all()).into();
            Moved::To(transform.into())
        }
    };
    // A region selection holds the seat: the move is its.
    if let Some(run) = lp.inner.comp.region.run.as_ref().filter(|run| run.result.is_none()) {
        let at = match moved {
            Moved::By(dx, dy) => (run.pointer.0 + dx, run.pointer.1 + dy),
            Moved::To(position) => (position.x / run.scale, position.y / run.scale),
        };
        world::comp::region::motion(lp, at);
        injected(lp, SeatKind::Human);
        return Ok(());
    }
    lp.inner.comp.corners.set_suppressed(!corners);
    match moved {
        Moved::By(dx, dy) => seat::inject::pointer_by(lp, dx, dy, time),
        Moved::To(position) => seat::inject::pointer_to(lp, position, time),
    }
    lp.inner.comp.corners.set_suppressed(false);
    injected(lp, SeatKind::Human);
    Ok(())
}

enum Moved {
    By(f64, f64),
    To(Point<f64, Physical>),
}

/// `{window}` + `require_hit`: the point must be on an output and reach that
/// window (`off_output` / `occluded` otherwise).
fn check_hit(lp: &Loop, id: u64, x: f64, y: f64, world: (f64, f64)) -> Result<(), ControlReply> {
    let (rows, _, _) = crate::project::project_outputs(lp);
    let on_output = rows.values().any(|row| {
        (f64::from(row.x)..f64::from(row.x) + f64::from(row.width)).contains(&world.0)
            && (f64::from(row.y)..f64::from(row.y) + f64::from(row.height)).contains(&world.1)
    });
    if !on_output {
        return Err(ControlReply::refused("off_output", json!({"id": id, "x": x, "y": y})));
    }
    let under = human_root_at(lp, world);
    if under.map(|(under, _)| under) != Some(id) {
        return Err(ControlReply::refused(
            "occluded",
            json!({
                "id": id,
                "under": under.map(|(id, generation)| json!({"id": id, "generation": generation})),
            }),
        ));
    }
    Ok(())
}

/// The root window record a human pointer at `world` would reach (client
/// content or its chrome).
fn human_root_at(lp: &Loop, world: (f64, f64)) -> Option<(u64, u64)> {
    let hit = surface_under_filtered(lp, Point::from(world), &|hit| visible_hit(lp, hit))?;
    let id = match hit.surface() {
        Some(surface) => record_of(lp, surface)?,
        None => SurfaceHandle::of_window(hit.window()?).and_then(|handle| lp.inner.comp.registry.id_for_handle(&handle))?,
    };
    let root = root_of(lp, id);
    let record = lp.inner.comp.registry.get(root)?;
    Some((record.id().0, record.generation()))
}

// ── the agent seat ───────────────────────────────────────────────────────────

/// A `{window}`-targeted op on the agent seat.
fn agent_targeted(lp: &mut Loop, id: u64, generation: u64, raise: bool, op: &InputOp) -> ControlReply {
    let Some(agent) = lp.state.seat.agent.clone() else {
        return ControlReply::Busy;
    };
    let keyboard_op = matches!(op, InputOp::Key { .. } | InputOp::Text(_));
    let window = window_record(lp, id, generation).and_then(|sid| crate::control::window_of(lp, sid));
    let surface = window.as_ref().and_then(|window| window.wl_surface().map(|surface| surface.into_owned()));
    let pointer_on_target = delivery_target(lp, SeatKind::Agent, false) == Some((id, generation));
    // Resolve every pointer check before changing either device's focus.
    let position = lp
        .inner
        .comp
        .injection
        .agent_pointer
        .filter(|_| pointer_on_target)
        .or_else(|| window.as_ref().and_then(|window| window_center(lp, window)));
    let root = window_record(lp, id, generation);
    let hit = match (keyboard_op, root, position) {
        (false, Some(root), Some(position)) => agent_hit(lp, Some(root), position).ok().flatten(),
        _ => None,
    };
    let facts = AgentTargetedFacts {
        session_lock: world::comp::session_lock::active(lp),
        target: surface.as_ref().and_then(|surface| agent_target(lp, surface)),
        keyboard_grabbed: agent.get_keyboard().is_some_and(|keyboard| keyboard.is_grabbed()),
        // A popup grab whose delivery target is the named window keeps its
        // keyboard focus.
        matching_popup: agent
            .get_keyboard()
            .and_then(|keyboard| keyboard.with_grab(|_, grab| grab.is::<PopupKeyboardGrab<Dispatch>>()))
            .unwrap_or(false)
            && delivery_target(lp, SeatKind::Agent, true) == Some((id, generation)),
        pointer_grabbed: agent.get_pointer().is_some_and(|pointer| pointer.is_grabbed()),
        pointer_on_target,
        hit: hit.as_ref().and_then(|(surface, _)| agent_target(lp, surface)),
    };
    let plan = agent_targeted_preflight(&lp.inner.comp.registry, id, generation, raise, op, &facts);
    match plan {
        Err(reply) => reply,
        Ok(AgentTargetedPlan::Release) => payload(lp, SeatKind::Agent, op, Some((id, generation))),
        Ok(AgentTargetedPlan::Deliver { focus_keyboard, move_pointer }) => {
            if focus_keyboard && let (Some(keyboard), Some(surface)) = (agent.get_keyboard(), surface) {
                keyboard.set_focus(&mut lp.state, Some(surface), SERIAL_COUNTER.next_serial());
            }
            if move_pointer && let (Some(hit), Some(position)) = (hit, position) {
                agent_motion(lp, Some(hit), position, now_ms(lp));
            }
            payload(lp, SeatKind::Agent, op, Some((id, generation)))
        }
    }
}

/// Move the agent pointer.
fn move_agent_pointer(lp: &mut Loop, target: &PointerMoveTarget, time: u32) -> Result<(), ControlReply> {
    let (hit, position) = resolve_agent_motion(lp, target)?;
    agent_motion(lp, hit, position, time);
    injected(lp, SeatKind::Agent);
    Ok(())
}

/// Where an agent move lands and what it hits,
/// without moving anything (the delivery and the coalescing preview share
/// it, so a discarded preview never changes focus).
fn resolve_agent_motion(lp: &Loop, target: &PointerMoveTarget) -> Result<AgentMotion, ControlReply> {
    let pointer = agent_pointer(lp).ok_or(ControlReply::Busy)?;
    let (root, position) = match target {
        PointerMoveTarget::Window { id, generation, x, y, .. } => {
            let sid = window_record(lp, *id, *generation).ok_or_else(|| stale(lp, *id, *generation))?;
            let window = crate::control::window_of(lp, sid);
            let surface = window.as_ref().and_then(|window| window.wl_surface().map(|surface| surface.into_owned()));
            policy::agent::validate_agent_surface(
                surface.as_ref().and_then(|surface| agent_target(lp, surface)),
                false,
                false,
            )?;
            (Some(sid), window_point(lp, *id, *generation, *x, *y)?)
        }
        PointerMoveTarget::Relative { dx, dy } => {
            let (x, y) = lp
                .inner
                .comp
                .injection
                .agent_pointer
                .ok_or_else(|| agent_refusal("no_pointer_target"))?;
            (None, (x + dx, y + dy))
        }
        PointerMoveTarget::Output { output, x, y } => {
            let (_, row) = output_row(lp, output.as_deref(), *x, *y)?;
            (None, (f64::from(row.x) + x, f64::from(row.y) + y))
        }
    };
    let click_grab = pointer
        .with_grab(|_, grab| grab.is::<ClickGrab<Dispatch>>())
        .unwrap_or(false);
    if pointer.is_grabbed() {
        if !click_grab {
            return Err(agent_refusal("pointer_grab"));
        }
        // The implicit button grab may continue within its own root.
        let start = pointer
            .grab_start_data()
            .and_then(|start| start.focus)
            .and_then(|(focus, _)| record_of(lp, &focus))
            .map(|id| root_of(lp, id));
        if start.is_none_or(|start| root.is_some_and(|root| root != start)) {
            return Err(agent_refusal("pointer_grab"));
        }
    }
    let hit = agent_hit(lp, root, position)?;
    if root.is_some() && hit.is_none() {
        return Err(agent_refusal("chrome_target"));
    }
    if let Some((surface, _)) = &hit {
        policy::agent::validate_agent_surface(agent_target(lp, surface), false, false)?;
    }
    Ok((hit, position))
}

/// Two adjacent agent moves with the same
/// coordinate contract that land on the same surface fold into one (relative
/// deltas add up; absolute targets keep the later one). Grabs are ordering
/// barriers. Previews only: nothing moves.
pub fn coalesce_agent_motion(lp: &Loop, previous: &InputOp, next: &InputOp) -> Option<InputOp> {
    if agent_pointer(lp).is_none_or(|pointer| pointer.is_grabbed()) {
        return None;
    }
    let motion = |op: &InputOp| match op {
        InputOp::OnSeat { seat: SeatKind::Agent, op } => match op.as_ref() {
            InputOp::PointerMove { target, .. } => Some(target.clone()),
            _ => None,
        },
        _ => None,
    };
    let previous = motion(previous)?;
    let next = motion(next)?;
    let combined = match (&previous, &next) {
        (PointerMoveTarget::Relative { dx, dy }, PointerMoveTarget::Relative { dx: nx, dy: ny }) => {
            PointerMoveTarget::Relative { dx: dx + nx, dy: dy + ny }
        }
        (
            PointerMoveTarget::Window { id, generation, require_hit, .. },
            PointerMoveTarget::Window { id: next_id, generation: next_generation, require_hit: next_hit, .. },
        ) if (id, generation, require_hit) == (next_id, next_generation, next_hit) => next.clone(),
        (PointerMoveTarget::Output { output, .. }, PointerMoveTarget::Output { output: next_output, .. })
            if output == next_output =>
        {
            next.clone()
        }
        _ => return None,
    };
    let (before, _) = resolve_agent_motion(lp, &previous).ok()?;
    let (after, _) = resolve_agent_motion(lp, &combined).ok()?;
    if before.as_ref()?.0 != after.as_ref()?.0 {
        return None;
    }
    Some(InputOp::OnSeat {
        seat: SeatKind::Agent,
        op: Box::new(InputOp::PointerMove { target: combined, corners: false }),
    })
}

/// What the agent pointer reaches at `position` (host Space): a client
/// surface and its origin, or nothing. Compositor chrome (SSD bars, iced) is
/// `chrome_target` unless a root was named (then only that root's surfaces
/// count).
/// The agent seat's hit: the surface under the point and its surface-local position.
type AgentHit = (WlSurface, Point<f64, Logical>);
/// Where an agent move lands (host Space) and what it hits there.
type AgentMotion = (Option<AgentHit>, (f64, f64));

fn agent_hit(
    lp: &Loop,
    root: Option<SurfaceId>,
    position: (f64, f64),
) -> Result<Option<AgentHit>, ControlReply> {
    let hit = surface_under_filtered(lp, Point::from(position), &|hit| {
        if !visible_hit(lp, hit) {
            return false;
        }
        match root {
            None => true,
            Some(root) => hit
                .surface()
                .and_then(|surface| record_of(lp, surface))
                .is_some_and(|id| root_of(lp, id) == root),
        }
    });
    match hit {
        None => Ok(None),
        Some(hit) => match (hit.surface(), hit.position_motion()) {
            (Some(surface), Some(origin)) => Ok(Some((surface.clone(), origin))),
            _ => Err(agent_refusal("chrome_target")),
        },
    }
}

fn agent_motion(lp: &mut Loop, focus: Option<(WlSurface, Point<f64, Logical>)>, position: (f64, f64), time: u32) {
    lp.inner.comp.injection.agent_pointer = Some(position);
    let Some(pointer) = agent_pointer(lp) else { return };
    pointer.motion(
        &mut lp.state,
        focus,
        &MotionEvent {
            location: Point::from(position),
            serial: SERIAL_COUNTER.next_serial(),
            time,
        },
    );
    pointer.frame(&mut lp.state);
}

/// The agent preflight's facts.
fn agent_seat_facts(lp: &Loop) -> AgentSeatFacts {
    let Some(agent) = lp.state.seat.agent.as_ref() else {
        return AgentSeatFacts::default();
    };
    let keyboard = agent.get_keyboard();
    let pointer = agent.get_pointer();
    AgentSeatFacts {
        session_lock: world::comp::session_lock::active(lp),
        keyboard_grab: keyboard
            .as_ref()
            .and_then(|keyboard| keyboard.with_grab(|_, grab| !grab.is::<PopupKeyboardGrab<Dispatch>>()))
            .unwrap_or(false),
        pointer_grab: pointer
            .as_ref()
            .and_then(|pointer| {
                pointer.with_grab(|_, grab| {
                    !grab.is::<PopupPointerGrab<Dispatch>>() && !grab.is::<ClickGrab<Dispatch>>()
                })
            })
            .unwrap_or(false),
        popup_pointer_grab: pointer
            .as_ref()
            .and_then(|pointer| pointer.with_grab(|_, grab| grab.is::<PopupPointerGrab<Dispatch>>()))
            .unwrap_or(false),
        keyboard_target: keyboard
            .as_ref()
            .and_then(|keyboard| keyboard.current_focus())
            .map(|surface| agent_target(lp, &surface)),
        pointer_target: pointer
            .as_ref()
            .and_then(|pointer| pointer.current_focus())
            .map(|surface| agent_target(lp, &surface)),
    }
}

/// The agent target facts for `surface` (its root's record).
fn agent_target(lp: &Loop, surface: &WlSurface) -> Option<AgentTarget> {
    let id = record_of(lp, surface)?;
    let root = root_of(lp, id);
    let record = lp.inner.comp.registry.get(root)?;
    let client = surface.client();
    let agent = lp.state.seat.agent.as_ref();
    let bound = |keyboard: bool| {
        let (Some(client), Some(agent)) = (client.as_ref(), agent) else {
            return false;
        };
        if keyboard {
            agent
                .get_keyboard()
                .is_some_and(|handle| handle.client_keyboards(client).next().is_some())
        } else {
            agent
                .get_pointer()
                .is_some_and(|handle| handle.client_pointers(client).next().is_some())
        }
    };
    Some(AgentTarget {
        x11: matches!(record.role(), SurfaceRole::X11 { .. }),
        root_mapped: record.mapped(),
        tree_mapped: tree_mapped(lp, id),
        // compd has no presentation ledger yet: a mapped surface is presentable
        // (as in `control::window_facts`).
        input_presentable: true,
        has_client: client.is_some(),
        keyboard_bound: bound(true),
        pointer_bound: bound(false),
    })
}

// ── delivery ─────────────────────────────────────────────────────────────────

fn inject_button(lp: &mut Loop, seat: SeatKind, button: u32, pressed: bool, time: u32) {
    // A region selection takes the human seat's buttons.
    if seat == SeatKind::Human && world::comp::region::button(lp, button, pressed) {
        injected(lp, seat);
        return;
    }
    if seat == SeatKind::Human && pressed {
        world::comp::injection::note_press(lp, true);
    }
    note_hold(lp, seat, Hold::Button(button), pressed);
    injected(lp, seat);
    match seat {
        SeatKind::Human => seat::inject::button(lp, button, pressed, time),
        SeatKind::Agent => {
            let Some(pointer) = agent_pointer(lp) else { return };
            pointer.button(
                &mut lp.state,
                &ButtonEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                    button,
                    state: button_state(pressed),
                },
            );
            pointer.frame(&mut lp.state);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn inject_scroll(
    lp: &mut Loop,
    seat: SeatKind,
    dx: Option<f64>,
    dy: Option<f64>,
    source: ScrollSource,
    v120: (Option<i32>, Option<i32>),
    time: u32,
) {
    injected(lp, seat);
    let source = match source {
        ScrollSource::Wheel => AxisSource::Wheel,
        ScrollSource::Finger => AxisSource::Finger,
        ScrollSource::Continuous => AxisSource::Continuous,
    };
    match seat {
        SeatKind::Human => seat::inject::scroll(
            lp,
            seat::inject::Scroll {
                time,
                horizontal: dx,
                vertical: dy,
                v120,
                source,
            },
        ),
        SeatKind::Agent => {
            let mut frame = AxisFrame::new(time).source(source);
            let mut carries_anything = false;
            for (axis, amount, v120) in [
                (smithay::backend::input::Axis::Horizontal, dx, v120.0),
                (smithay::backend::input::Axis::Vertical, dy, v120.1),
            ] {
                let Some(amount) = amount else { continue };
                let stops = amount == 0.0 && matches!(source, AxisSource::Finger | AxisSource::Continuous);
                if !stops && amount == 0.0 && v120.is_none_or(|value| value == 0) {
                    continue;
                }
                carries_anything = true;
                frame = frame.value(axis, amount);
                if let Some(value) = v120 {
                    frame = frame.v120(axis, value);
                }
                if stops {
                    frame = frame.stop(axis);
                }
            }
            if carries_anything && let Some(pointer) = agent_pointer(lp) {
                pointer.axis(&mut lp.state, frame);
                pointer.frame(&mut lp.state);
            }
        }
    }
}

/// Check continuity before every press; a failure
/// releases what this call pressed. Returns `(failure, completed events,
/// delivery target)`.
fn inject_keys(
    lp: &mut Loop,
    seat: SeatKind,
    events: Vec<(Keycode, bool, bool)>,
    target: Option<(u64, u64)>,
    time: u32,
) -> (Option<&'static str>, usize, Option<(u64, u64)>) {
    let mut pressed_here = BTreeSet::new();
    let mut completed = 0;
    let mut delivery = None;
    let mut payload_seen = false;
    let mut failure = None;
    for (keycode, pressed, required) in events {
        if pressed && target.is_some() && delivery_target(lp, seat, true) != target {
            failure = Some("target_changed");
            break;
        }
        let was_held = lp.inner.comp.injection.holds_mut(seat).owners_of(Hold::Key(keycode.raw())) > 0;
        let (handled, delivered) = inject_key(lp, seat, keycode, pressed, time);
        completed += 1;
        if required && (pressed || !payload_seen) {
            if delivered.is_some() {
                delivery = delivered;
            }
            payload_seen = true;
        }
        if pressed {
            if !was_held {
                pressed_here.insert(keycode.raw());
            }
            if required && !was_held && !handled {
                failure = Some("no_keyboard_target");
                break;
            }
        } else {
            pressed_here.remove(&keycode.raw());
        }
    }
    if failure.is_some() {
        release_holds(lp, seat, pressed_here.into_iter().map(Hold::Key).collect(), time);
    }
    (failure, completed, delivery)
}

/// One key edge on a seat. Returns whether something took it and the
/// window it was delivered to.
fn inject_key(lp: &mut Loop, seat: SeatKind, keycode: Keycode, pressed: bool, time: u32) -> (bool, Option<(u64, u64)>) {
    // A region selection takes the human seat's keys: handled, no target.
    if seat == SeatKind::Human && world::comp::region::key(lp, keycode.raw(), pressed) {
        injected(lp, seat);
        return (true, None);
    }
    if seat == SeatKind::Human && pressed {
        world::comp::injection::note_press(lp, false);
    }
    note_hold(lp, seat, Hold::Key(keycode.raw()), pressed);
    injected(lp, seat);
    match seat {
        SeatKind::Human => {
            // A compositor iced surface holding the keyboard (a scene dialog
            // takes it on show and drops the seat's wayland focus) gets the key
            // ahead of any client (seat `should_forward`): it is the
            // keyboard target, as Quoin's dialog layer is. Read
            // before the key, which may hide the dialog (Escape).
            let iced_held = iced_keyboard_held(lp);
            // A key a binding took (its press acted, or
            // its release was swallowed) is handled with or without a focused
            // client, and so is a bare modifier (a chord's prefix); a key the
            // binding filter took is delivered to no window.
            lp.inner.comp.bindings.last_edge = None;
            seat::inject::key(lp, keycode, pressed, time);
            if let Some(edge) = lp.inner.comp.bindings.last_edge
                && (edge.took || edge.modifier)
            {
                let delivered = if edge.took { None } else { delivery_target(lp, seat, true) };
                return (true, delivered);
            }
            if iced_held {
                return (true, scene_keyboard_target(lp));
            }
        }
        SeatKind::Agent => {
            if let Some(keyboard) = lp.state.seat.agent.as_ref().and_then(|agent| agent.get_keyboard()) {
                let state = if pressed { KeyState::Pressed } else { KeyState::Released };
                keyboard.input::<(), _>(
                    &mut lp.state,
                    keycode,
                    state,
                    SERIAL_COUNTER.next_serial(),
                    time,
                    |_, _, _| FilterResult::Forward,
                );
            }
        }
    }
    let focused = seat_of(lp, seat)
        .and_then(|handle| handle.get_keyboard())
        .is_some_and(|keyboard| keyboard.current_focus().is_some());
    (focused, delivery_target(lp, seat, true))
}

/// `release_all` on one seat: everything injection holds there, and only
/// that; the agent's popups are dismissed too.
fn release_injected(lp: &mut Loop, seat: SeatKind) {
    let holds = lp.inner.comp.injection.holds_mut(seat).take_all();
    let time = now_ms(lp);
    release_holds(lp, seat, holds, time);
    if seat == SeatKind::Agent {
        dismiss_agent_popups(lp);
    }
}

/// End the agent seat's popup grab and dismiss its popups.
fn dismiss_agent_popups(lp: &mut Loop) {
    if let Some(mut grab) = lp.state.agent_popup_grab.take() {
        grab.ungrab(smithay::desktop::PopupUngrabStrategy::All);
    }
    let Some(agent) = lp.state.seat.agent.clone() else { return };
    if let Some(keyboard) = agent.get_keyboard()
        && keyboard
            .with_grab(|_, grab| grab.is::<PopupKeyboardGrab<Dispatch>>())
            .unwrap_or(false)
    {
        keyboard.unset_grab(&mut lp.state);
    }
    if let Some(pointer) = agent.get_pointer()
        && pointer
            .with_grab(|_, grab| grab.is::<PopupPointerGrab<Dispatch>>())
            .unwrap_or(false)
    {
        let time = now_ms(lp);
        pointer.unset_grab_without_focus_restore(&mut lp.state, SERIAL_COUNTER.next_serial(), time);
    }
}

/// Release the given holds the seat still has pressed: keys newest code
/// first, then buttons (policy `release_order`).
fn release_holds(lp: &mut Loop, seat: SeatKind, holds: Vec<Hold>, time: u32) {
    let Some(handle) = seat_of(lp, seat) else { return };
    let pressed_keys = handle.get_keyboard().map(|keyboard| keyboard.pressed_keys()).unwrap_or_default();
    let pressed_buttons = handle.get_pointer().map(|pointer| pointer.current_pressed()).unwrap_or_default();
    for hold in release_order(&holds, |_| false) {
        match hold {
            Hold::Key(raw) if pressed_keys.contains(&Keycode::new(raw)) => {
                inject_key(lp, seat, Keycode::new(raw), false, time);
            }
            Hold::Button(button) if pressed_buttons.contains(&button) => {
                inject_button(lp, seat, button, false, time);
            }
            _ => {}
        }
    }
}

/// One injected event went through a seat: counted for a
/// sequence's yield and recorded as that seat's activity (an injected human
/// event counts as human input).
fn injected(lp: &mut Loop, seat: SeatKind) {
    let injection = &mut lp.inner.comp.injection;
    injection.events = injection.events.wrapping_add(1);
    match seat {
        // Human injection is human activity: the clock and every seat's idle
        // notification. The agent's moves only its own clock.
        SeatKind::Human => world::comp::injection::note_human_activity(lp),
        SeatKind::Agent => injection.note_activity(seat),
    }
}

/// Record an injected press or release, owned by the running sequence (or
/// `None`, a single verb).
fn note_hold(lp: &mut Loop, seat: SeatKind, hold: Hold, pressed: bool) {
    let injection = &mut lp.inner.comp.injection;
    let owner = injection.current_run;
    injection.holds_mut(seat).note(owner, hold, pressed);
}

// ── lookups ──────────────────────────────────────────────────────────────────

fn seat_of(lp: &Loop, seat: SeatKind) -> Option<smithay::input::Seat<Dispatch>> {
    match seat {
        SeatKind::Human => Some(lp.state.seat.seat.clone()),
        SeatKind::Agent => lp.state.seat.agent.clone(),
    }
}

fn agent_pointer(lp: &Loop) -> Option<smithay::input::pointer::PointerHandle<Dispatch>> {
    lp.state.seat.agent.as_ref().and_then(|agent| agent.get_pointer())
}

/// The registry record of a surface (an Xwayland surface resolves to its X
/// window's record).
fn record_of(lp: &Loop, surface: &WlSurface) -> Option<SurfaceId> {
    lp.inner.comp.registry.id_for_handle(&SurfaceHandle::resolve(surface))
}

/// A record's root (its parents walked up).
fn root_of(lp: &Loop, mut id: SurfaceId) -> SurfaceId {
    let registry = &lp.inner.comp.registry;
    for _ in 0..64 {
        match registry.get(id).and_then(|record| record.parent()) {
            Some(parent) => id = parent,
            None => break,
        }
    }
    id
}

/// The record and every ancestor are mapped and not dormant.
fn tree_mapped(lp: &Loop, mut id: SurfaceId) -> bool {
    let registry = &lp.inner.comp.registry;
    for _ in 0..64 {
        let Some(record) = registry.get(id) else { return false };
        if !record.mapped() || record.role() == SurfaceRole::Dormant {
            return false;
        }
        match record.parent() {
            Some(parent) => id = parent,
            None => return true,
        }
    }
    false
}

/// `{id, generation}` of the root of whatever the seat now delivers to:
/// keyboard focus for keys, pointer focus otherwise.
fn delivery_target(lp: &Loop, seat: SeatKind, keyboard: bool) -> Option<(u64, u64)> {
    let handle = seat_of(lp, seat)?;
    let surface = if keyboard {
        handle.get_keyboard()?.current_focus()
    } else {
        handle.get_pointer()?.current_focus()
    }?;
    let id = record_of(lp, &surface)?;
    if seat == SeatKind::Agent && !tree_mapped(lp, id) {
        return None;
    }
    lp.inner
        .comp
        .registry
        .get(root_of(lp, id))
        .filter(|record| seat == SeatKind::Human || record.mapped())
        .map(|record| (record.id().0, record.generation()))
}

/// Whether a compositor iced surface holds the human keyboard (the iced
/// registry's keyboard focus): keys go to it ahead of the seat's client.
fn iced_keyboard_held(lp: &Loop) -> bool {
    lp.inner.surface().registry.as_ref().is_some_and(|registry| registry.keyboard_focus().is_some())
}

/// The scene surface holding the keyboard as `{id, generation}` (its
/// comp.props row), when the iced focus is a scene's; `None` for another
/// compositor iced surface (it has no row).
fn scene_keyboard_target(lp: &Loop) -> Option<(u64, u64)> {
    let scenes = &lp.inner.comp.scenes;
    let id = scenes.focus?;
    scenes.rows.iter().find(|row| row.id == id).map(|row| (id.0, row.generation))
}

/// A live `{id, generation}` window's record.
fn window_record(lp: &Loop, id: u64, generation: u64) -> Option<SurfaceId> {
    lp.inner
        .comp
        .registry
        .resolve_window_target(id, Some(generation))
        .ok()
        .map(|record| record.id())
}

fn stale(lp: &Loop, id: u64, generation: u64) -> ControlReply {
    match lp.inner.comp.registry.resolve_window_target(id, Some(generation)) {
        Err(error) => ControlReply::WindowTarget { id, error },
        Ok(_) => ControlReply::Busy,
    }
}

/// Window-local `(x, y)` (from the window-geometry origin) in the host Space.
fn window_point(lp: &Loop, id: u64, generation: u64, x: f64, y: f64) -> Result<(f64, f64), ControlReply> {
    let sid = window_record(lp, id, generation).ok_or_else(|| stale(lp, id, generation))?;
    let origin = crate::control::window_of(lp, sid)
        .and_then(|window| lp.inner.host_space().state.element_location(&window))
        .ok_or(ControlReply::WindowTarget { id, error: surfaces::WindowTargetError::NotMapped })?;
    Ok((f64::from(origin.x) + x, f64::from(origin.y) + y))
}

/// The middle of a window's geometry, in the host Space.
fn window_center(lp: &Loop, window: &smithay::desktop::Window) -> Option<(f64, f64)> {
    let origin = lp.inner.host_space().state.element_location(window)?;
    let size = window.geometry().size;
    Some((f64::from(origin.x) + f64::from(size.w) / 2.0, f64::from(origin.y) + f64::from(size.h) / 2.0))
}

/// Where a window's root surface sits in the host Space (its geometry origin
/// less the geometry offset): the pointer-focus origin.
fn surface_origin(lp: &Loop, window: &smithay::desktop::Window) -> Option<Point<f64, Logical>> {
    let location = lp.inner.host_space().state.element_location(window)?;
    Some((location - window.geometry().loc).to_f64())
}

/// Re-run the human pointer's hit-test where it stands: stacking or visibility moved
/// what is under the cursor, so focus follows without the cursor moving.
pub fn retarget_pointer(lp: &mut Loop) {
    let Some(pointer) = lp.state.seat.seat.get_pointer() else { return };
    if pointer.is_grabbed() {
        return;
    }
    let location = pointer.current_location();
    let focus = surface_under_filtered(lp, location, &|hit| visible_hit(lp, hit))
        .and_then(|hit| Some((hit.surface()?.clone(), hit.position_motion()?)));
    let time = now_ms(lp);
    pointer.motion(
        &mut lp.state,
        focus,
        &MotionEvent { location, serial: SERIAL_COUNTER.next_serial(), time },
    );
    pointer.frame(&mut lp.state);
}

/// A hit the draw would show (hidden windows take no pointer).
fn visible_hit(lp: &Loop, hit: &SurfaceHit) -> bool {
    hit.window().is_none_or(|window| window.visible(lp))
}

/// `{output, x, y}`: the output's row (by key or name; `None` is the default
/// output) with `(x, y)` inside it (`unknown_output` / `out_of_bounds` otherwise).
fn output_row(lp: &Loop, output: Option<&str>, x: f64, y: f64) -> Result<(String, OutputSnapshot), ControlReply> {
    let (rows, _, _) = crate::project::project_outputs(lp);
    let row = match output {
        Some(requested) => rows.into_iter().find(|(key, row)| key == requested || row.name == requested),
        None => rows.into_iter().find(|(_, row)| row.default),
    };
    let Some((key, row)) = row else {
        return Err(ControlReply::refused("unknown_output", json!({"output": output})));
    };
    let (width, height) = (f64::from(row.width), f64::from(row.height));
    if !(0.0..width).contains(&x) || !(0.0..height).contains(&y) {
        return Err(ControlReply::refused(
            "out_of_bounds",
            json!({"output": key, "x": x, "y": y, "width": row.width, "height": row.height}),
        ));
    }
    Ok((key, row))
}

/// The seat's pointer as `(output key, output name, output-local x, y)`.
/// The human cursor is unreported while the session is paused.
fn pointer_position(lp: &Loop, seat: SeatKind) -> Option<(String, String, f64, f64)> {
    match seat {
        SeatKind::Human => {
            if matches!(lp.inner.status_session, world::state::state::StatusSession::Paused) {
                return None;
            }
            // The engine's cursor: physical pixels on the cursor's output.
            let key = lp.inner.cursor_output.clone();
            let space = &lp.inner.space_state().state;
            let output = space
                .outputs()
                .find(|output| key.as_ref() == Some(&world::state::state::output_key(output)))
                .or_else(|| space.outputs().next())?;
            let scale = output.current_scale().fractional_scale();
            let motion = lp.inner.pointer().motion;
            let name = output.name();
            Some((output_key(&name), name, motion.x / scale, motion.y / scale))
        }
        SeatKind::Agent => {
            let (x, y) = lp.inner.comp.injection.agent_pointer?;
            let (rows, _, _) = crate::project::project_outputs(lp);
            rows.into_iter().find_map(|(key, row)| {
                let local = (x - f64::from(row.x), y - f64::from(row.y));
                ((0.0..f64::from(row.width)).contains(&local.0) && (0.0..f64::from(row.height)).contains(&local.1))
                    .then(|| (key, row.name.clone(), local.0, local.1))
            })
        }
    }
}

/// The human cursor as `(output name, output-local x, y)` for
/// `pointer.changed`; `None` while it is off every output or the session is
/// paused.
pub fn human_pointer(lp: &Loop) -> Option<(String, f64, f64)> {
    pointer_position(lp, SeatKind::Human).map(|(_, name, x, y)| (name, x, y))
}

/// `input.seats`: each seat's advertised name, its
/// keyboard and pointer focus records, its pointer on an output (by name),
/// and its last input.
pub(crate) fn project_seats(lp: &Loop) -> BTreeMap<&'static str, SeatSnapshot> {
    use dispatcher::wayland::seat::factory::factory::{AGENT_SEAT, PRIMARY_SEAT};
    let focus = |surface: Option<WlSurface>| {
        surface
            .and_then(|surface| record_of(lp, &surface))
            .and_then(|id| lp.inner.comp.registry.get(id))
            .map(|record| SeatFocusSnapshot {
                id: record.id().0,
                generation: record.generation(),
            })
    };
    [(SeatKind::Human, PRIMARY_SEAT), (SeatKind::Agent, AGENT_SEAT)]
        .into_iter()
        .filter_map(|(kind, name)| {
            let seat = seat_of(lp, kind)?;
            Some((
                kind.name(),
                SeatSnapshot {
                    name,
                    keyboard_focus: focus(seat.get_keyboard().and_then(|keyboard| keyboard.current_focus())),
                    pointer_focus: focus(seat.get_pointer().and_then(|pointer| pointer.current_focus())),
                    // No coordinates under a session lock.
                    pointer: pointer_position(lp, kind)
                        .filter(|_| !world::comp::session_lock::active(lp))
                        .map(|(_, output, x, y)| SeatPointerSnapshot { output, x, y }),
                    last_input_us: lp.inner.comp.injection.last_input_us(kind),
                },
            ))
        })
        .collect()
}

/// An exclusive-keyboard layer surface on Overlay or Top (seat
/// `keyboard::exclusive_layer`).
pub(crate) fn exclusive_layer(lp: &Loop) -> bool {
    // The latch is the one answer: Top/Overlay, unconcealed, mapped.
    world::comp::latch::active(lp)
}

fn button_state(pressed: bool) -> smithay::backend::input::ButtonState {
    if pressed {
        smithay::backend::input::ButtonState::Pressed
    } else {
        smithay::backend::input::ButtonState::Released
    }
}

fn press_states(action: PressAction) -> &'static [bool] {
    match action {
        PressAction::Press => &[true],
        PressAction::Release => &[false],
        PressAction::Both => &[true, false],
    }
}

fn now_ms(lp: &Loop) -> u32 {
    lp.inner.start_time.elapsed().as_millis() as u32
}

fn unknown_key(key: &KeySpec) -> ControlReply {
    let key: Value = match key {
        KeySpec::Name(name) => json!(name),
        KeySpec::Evdev(code) => json!(code),
    };
    ControlReply::refused("unknown_key", json!({ "key": key }))
}

/// Keysym and character lookups against the seat's live keymap, verified
/// against an XKB state carrying the seat's locked modifiers and layout.
struct KeymapIndex {
    by_sym: HashMap<u32, (Keycode, bool)>,
    by_char: HashMap<u32, (Keycode, bool)>,
}

fn keymap_index(lp: &mut Loop, seat: SeatKind) -> KeymapIndex {
    let empty = || KeymapIndex {
        by_sym: HashMap::new(),
        by_char: HashMap::new(),
    };
    let Some(keyboard) = seat_of(lp, seat).and_then(|handle| handle.get_keyboard()) else {
        return empty();
    };
    keyboard.with_xkb_state(&mut lp.state, |context| {
        let xkb = context.xkb().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        build_keymap_index(&xkb)
    })
}

fn build_keymap_index(keyboard: &smithay::input::keyboard::Xkb) -> KeymapIndex {
    // SAFETY: the references (and the scratch states' keymap ref-counts) live
    // only inside this call, under the keyboard's xkb lock.
    let (keymap, live) = unsafe { (keyboard.keymap(), keyboard.state()) };
    let locked = live.serialize_mods(xkb::STATE_MODS_LOCKED);
    let layout = live.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE);
    let shift_index = keymap.mod_get_index(xkb::MOD_NAME_SHIFT);
    let shift = if shift_index == xkb::MOD_INVALID { 0 } else { 1 << shift_index };
    let mut index = KeymapIndex {
        by_sym: HashMap::new(),
        by_char: HashMap::new(),
    };
    let (min, max) = (keymap.min_keycode().raw(), keymap.max_keycode().raw());
    for uses_shift in [false, true] {
        if uses_shift && shift == 0 {
            continue;
        }
        let mut state = xkb::State::new(keymap);
        state.update_mask(if uses_shift { shift } else { 0 }, 0, locked, 0, 0, layout);
        for raw in min..=max {
            let keycode = Keycode::new(raw);
            let sym = state.key_get_one_sym(keycode);
            if sym == Keysym::NoSymbol {
                continue;
            }
            index.by_sym.entry(sym.raw()).or_insert((keycode, uses_shift));
            let character = xkb::keysym_to_utf32(sym);
            if character != 0 {
                index.by_char.entry(character).or_insert((keycode, uses_shift));
            }
        }
    }
    index
}

fn keysym_by_name(name: &str) -> Option<Keysym> {
    [xkb::KEYSYM_NO_FLAGS, xkb::KEYSYM_CASE_INSENSITIVE]
        .into_iter()
        .map(|flags| xkb::keysym_from_name(name, flags))
        .find(|sym| *sym != Keysym::NoSymbol)
}

/// A key as `(keycode, needs shift)`, without sending anything.
fn resolve_key(index: &KeymapIndex, key: &KeySpec) -> Option<(Keycode, bool)> {
    match key {
        KeySpec::Evdev(code) => Some((Keycode::new(code + 8), false)),
        KeySpec::Name(name) => index.by_sym.get(&keysym_by_name(name)?.raw()).copied(),
    }
}
