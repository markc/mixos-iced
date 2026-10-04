//! The camera pinned to identity.
//!
//! The projection puts the camera's position at the centre of the pane it
//! draws (`Transform::to_logical`: `(pos - camera) * zoom + half + origin`).
//! Nothing moves the camera any more (`camera::system`), but a camera left at
//! position (0, 0) makes world (0, 0) the SCREEN CENTRE, while the comp
//! props' coordinates (`outputs.*`, `windows.*`, place, maximise, corners, region,
//! input targets) are output-logical with (0, 0) at the top-left.
//!
//! Pinned, every pane's camera sits at the centre of the rectangle it draws,
//! in output-logical coordinates, at zoom 1. The projection is then the
//! identity: a world point IS the output-logical point, and everything derived
//! from the camera (placement centred on it, the canvas cull rect,
//! the iced world transform) keeps working unchanged. [`pin`] runs every Bus
//! pass, so an output resize or a pane split is re-pinned before the next
//! input or frame.

use smithay::utils::{Physical, Point, Rectangle};

use crate::state::state::{Orchestrator, output_key};
use crate::viewport::layout::layout::compute;

/// Put every pane camera of every output at the centre of its own
/// rectangle (output-logical), zoom 1. Returns whether any camera moved.
pub fn pin(inner: &mut Orchestrator) -> bool {
    let outputs: Vec<(String, (i32, i32), f64)> = inner
        .space_state()
        .state
        .outputs()
        .filter_map(|output| {
            let mode = output.current_mode()?;
            Some((output_key(output), (mode.size.w, mode.size.h), output.current_scale().fractional_scale()))
        })
        .collect();
    let mut moved = false;
    for (key, (width, height), scale) in outputs {
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        let views = inner.output_views_mut();
        views.ensure(&key);
        let viewports = views.views_mut(&key);
        let bounds = Rectangle::<i32, Physical>::new(Point::from((0, 0)), (width, height).into());
        let centres: Vec<(u64, (f64, f64))> = compute(viewports, bounds)
            .regions
            .iter()
            .map(|region| {
                let rect = region.rect;
                (
                    region.slot,
                    (
                        (f64::from(rect.loc.x) + f64::from(rect.size.w) / 2.0) / scale,
                        (f64::from(rect.loc.y) + f64::from(rect.size.h) / 2.0) / scale,
                    ),
                )
            })
            .collect();
        for (slot, (x, y)) in centres {
            let Some(camera) = viewports.camera_of_mut(slot) else { continue };
            let transform = &mut camera.transform;
            if transform.position.x != x || transform.position.y != y || transform.zoom != 1.0 {
                transform.position = Point::from((x, y));
                transform.zoom = 1.0;
                moved = true;
            }
        }
    }
    moved
}

/// The pointer starts at the centre of the first output, once, as soon as an
/// output exists (left at world (0, 0) it would rest in the top-left hot
/// corner). A warp, not input: it goes straight to the smithay pointer,
/// so no hot corner is sampled, no human activity is recorded and no client
/// gets focus until the pointer really moves. Run after [`pin`].
pub fn centre_pointer_once(lp: &mut crate::state::Loop) {
    use smithay::input::pointer::MotionEvent;
    use smithay::utils::SERIAL_COUNTER;
    if lp.inner.comp.pointer_placed {
        return;
    }
    let space = &lp.inner.space_state().state;
    let Some(output) = space.outputs().next().cloned() else { return };
    let (Some(mode), Some(geometry)) = (output.current_mode(), space.output_geometry(&output)) else {
        return;
    };
    let scale = output.current_scale().fractional_scale();
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let physical = (f64::from(mode.size.w) / 2.0, f64::from(mode.size.h) / 2.0);
    let location = Point::from((
        f64::from(geometry.loc.x) + physical.0 / scale,
        f64::from(geometry.loc.y) + physical.1 / scale,
    ));
    lp.inner.pointer_mut().motion.x = physical.0;
    lp.inner.pointer_mut().motion.y = physical.1;
    if let Some(pointer) = lp.state.seat.seat.get_pointer() {
        let time = lp.inner.start_time.elapsed().as_millis() as u32;
        pointer.motion(&mut lp.state, None, &MotionEvent { location, serial: SERIAL_COUNTER.next_serial(), time });
        pointer.frame(&mut lp.state);
    }
    lp.inner.comp.pointer_placed = true;
    lp.state.schedule_redraw(dispatcher::state::state::RedrawReason::Cursor);
}
