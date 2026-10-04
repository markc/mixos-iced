use crate::pointer::input::native_press;
use smithay::backend::input::{ButtonState, Event, InputBackend, PointerButtonEvent};
use smithay::input::pointer::ButtonEvent;
use smithay::utils::{Physical, Point, SERIAL_COUNTER};
use world::state::{Loop, Transform};
use world::state::state::CoordinateTrait;
use world::surface::interface::hit::surface_under_filtered;
use world::window::interface::draw::visible::DrawWindow;

/// Pointer focus (the seat's and the iced registry's, and the local
/// coordinates both hold) only follows MOTION, but the scene can change under a
/// still cursor: a window maps, raises or moves, a panel or menu opens, a
/// workspace switches. The wayland button then went to the stale focus (a newly
/// mapped window had no `enter` and ignored it) and an iced button never saw the
/// cursor over it, so a click did nothing until the pointer was wiggled. Replay
/// the motion at the same location before every press, exactly as a wiggle
/// would; when nothing changed that is one redundant same-position motion.
/// Not mid-grab (the grab owns routing) and not while the focused client holds
/// an active lock or confine (the pointer is the client's; a replayed crossing
/// would release its lock).
fn refresh_pointer_focus(_loop: &mut Loop, time: u32) {
    let Some(pointer) = _loop.state.seat.seat.get_pointer() else { return };
    if pointer.is_grabbed() || crate::pointer::input::constraint::constraint_active(_loop) {
        return;
    }
    let location = pointer.current_location();
    crate::pointer::input::native_motion::dispatch::dispatch(
        _loop,
        time,
        SERIAL_COUNTER.next_serial(),
        pointer,
        location,
        None,
        false,
    );
}

pub fn button<I: InputBackend>(event: &<I as InputBackend>::PointerButtonEvent, _loop: &mut Loop) {
    // A release ends an edge pan that the host's implicit DRAG grab was feeding.
    // Before every early return below, so no path can leave the camera scrolling.
    // A relative pointer's continuous pan is untouched — see `extent::release_absolute`.
    if event.state() == ButtonState::Released {
        crate::pointer::input::extent::release_absolute(_loop);
    }

    // Shared by winit and udev: reserved corner squares take presses BEFORE
    // chrome, legacy corners, popup grabs, furniture or application routing.
    // Their matching releases stay consumed even after pointer departure.
    if world::comp::scenes::pointer_button(_loop, event.button_code(), event.state() == ButtonState::Pressed) {
        return;
    }

    // A primary release fires a caption button the press armed, if it lands on
    // the same button, and clears its pressed look either way (decor). It
    // consumes nothing: the release still reaches the seat below, which ends
    // the implicit click grab or a chrome move/resize grab.
    if event.state() == ButtonState::Released && event.button_code() == crate::pointer::input::chrome::BTN_LEFT {
        crate::pointer::input::chrome::release(_loop);
    }

    // A press on an engaged hot corner, and its release, are the corner's (`corner.clicked*`); neither reaches a client.
    if world::comp::corners::button(_loop, event.button_code(), event.state() == ButtonState::Pressed) {
        return;
    }

    // A popup grab (a menu) owns the pointer: forward the raw button so smithay's popup
    // grab routes it into the menu chain (or dismisses on an outside press), then STOP —
    // the world bus + native_press must not fight it. Scoped by `in_popup_grab` so the
    // DnD grab keeps its own path; the flag resets once the seat is no longer grabbed.
    {
        let pointer = _loop.state.seat.seat.get_pointer().unwrap();
        if !pointer.is_grabbed() {
            _loop.state.in_popup_grab = false;
        }
        if _loop.state.in_popup_grab && pointer.is_grabbed() {
            let serial = SERIAL_COUNTER.next_serial();
            pointer.button(
                &mut _loop.state,
                &ButtonEvent {
                    button: event.button_code(),
                    state: event.state(),
                    serial,
                    time: event.time_msec(),
                },
            );
            pointer.frame(&mut _loop.state);
            return;
        }
    }

    // Viewport separator drag: a press on a separator bar starts a resize; the
    // matching release ends it. Both consume the event (no window/canvas routing).
    let cursor_world = _loop.state.seat.seat.get_pointer().unwrap().current_location();
    let cursor_phys: Point<f64, Physical> = {
        let t: Transform = ((cursor_world.x, cursor_world.y), _loop.focus_pane_context()).into();
        t.into()
    };
    match event.state() {
        ButtonState::Pressed => {
            // Separator drag hit-tests against the current output's viewport layout, so
            // it needs the output's physical bounds; the drag STATE + math live in
            // `viewport.interaction` (keeps the Orchestrator slim).
            let bounds = {
                let (pw, ph) = _loop.size_ctx_all().screen_size_physical;
                smithay::utils::Rectangle::new(
                    smithay::utils::Point::from((0, 0)),
                    smithay::utils::Size::from((pw.round() as i32, ph.round() as i32)),
                )
            };
            if world::viewport::interaction::interaction::try_begin_separator(_loop.inner.output_views_mut(), bounds, cursor_phys) {
                return;
            }
            // Floating pane move (Super-drag) / resize (Super+Shift-drag) near an
            // edge. The canvas grab "tool" already encodes the held modifier
            // (incl. the nested-winit Super→Ctrl remap): Move vs Scale.
            use world::canvas::input::state::state::{CanvasGrab, TargetOption};
            let tool = match _loop.inner.canvas().Grab {
                CanvasGrab::Target(TargetOption::Move) => Some(false),
                CanvasGrab::Target(TargetOption::Scale) => Some(true),
                _ => None,
            };
            if let Some(resize) = tool {
                if world::viewport::interaction::interaction::try_begin_floating(_loop.inner.output_views_mut(), cursor_phys, resize) {
                    return;
                }
            }
        }
        ButtonState::Released => {
            if _loop.inner.output_views().separator_drag.is_some() {
                world::viewport::interaction::interaction::end_separator(_loop.inner.output_views_mut());
                return;
            }
            if _loop.inner.output_views().floating_drag.is_some() {
                world::viewport::interaction::interaction::end_floating(_loop.inner.output_views_mut());
                return;
            }
        }
    }

    // Click-to-activate: a press makes the pane under the cursor the keyboard
    // shortcut target (`active`). The `pointer` slot was set by the last motion.
    if event.state() == ButtonState::Pressed {
        let under_cursor = _loop.inner.viewports().pointer;
        _loop.inner.viewports_mut().active = under_cursor;
    }

    let pointer = &_loop.state.seat.seat.get_pointer().unwrap();
    {
        // World input bus first (phase 3); Pass falls through to legacy routing.
        let location = pointer.current_location();
        // Read the modality off the tracker rather than taking it as a parameter: the
        // seat delegate sets it from the real device before dispatching, and the touch
        // / pen emulation paths (`touch::emulate`, `tablet::tip`) reach this same
        // function through the `TouchEmu` backend AFTER their own delegate arm has
        // already recorded Touch / Pen. So it is correct for both the real and the
        // synthesized press without threading an argument through every call site.
        let ev = slots::input::event::base::InputEvent::PointerButton {
            button: event.button_code(),
            pressed: event.state() == ButtonState::Pressed,
            x: location.x,
            y: location.y,
            modality: _loop.inner.touch.modality,
        };
        if crate::input::drive::drive::route(_loop, ev)
            == slots::input::event::base::InputFlow::Consume
        {
            return;
        }
    }
    // After the world bus passed the press (a canvas tool that consumes it owns
    // the pointer too), before the hit-test that delivers it.
    if event.state() == ButtonState::Pressed {
        refresh_pointer_focus(_loop, event.time_msec());
    }
    let event = &event;

    let button_state = event.state();
    let keyboard = &_loop.state.seat.seat.get_keyboard().unwrap();

    // PRESS and RELEASE are both handled by `CanvasSystem::input` on the world bus
    // (route() above). A `Pass` from the bus on PRESS means the cursor is over a
    // window (the system cleared selection + declined to grab), so the click is
    // routed directly to that window here via `native_press`.
    //
    // Grabbed presses take the SAME gate — hit test included — with `grabbed`
    // passed down so each step in `input_received` decides its own mid-grab
    // behavior: focus/raise/iced belong to the grab owner and skip, while the
    // wayland button still delivers THROUGH the grab (smithay's implicit click
    // grab while another button is held, or a client DnD grab). Without that
    // delivery, chorded input lost every button after the first: FPS games'
    // right-click-aim held + left-click-fire never reached the client.
    if ButtonState::Pressed == button_state {
        let grabbed = pointer.is_grabbed();
        if let Some(hit) = surface_under_filtered(_loop, pointer.current_location(), &|hit| {
            if let Some(window) = hit.window() {
                return window.visible(_loop);
            };

            true
        }) {
            // It is directly over a window.
            native_press::press::input_received::<I>(
                pointer, event, _loop, hit, keyboard, button_state, grabbed,
            )
        }
    }
}
