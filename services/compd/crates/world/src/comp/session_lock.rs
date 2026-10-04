//! The session lock's policy.
//!
//! The protocol lifecycle is dispatcher's (`wayland::sessionlock`); this
//! module is what the lock does to the rest of compd:
//! - [`service`], once per loop iteration: on entry the seat is torn down
//!   (grabs, focus, the region run, an interactive grab, hot corners), on
//!   exit the keyboard focus is restored; while locked the keyboard stays on
//!   a lock surface; `locked` is confirmed once every output presented;
//! - [`frame`], the scene's hook: a locked output draws an opaque blank and
//!   its lock surface, nothing else (no windows, layers, compositor UI or
//!   notifications);
//! - [`presented`], the present path's hook: lock surfaces get the frame
//!   callbacks; nothing else does;
//! - [`hit`], the hit-test hook: only lock surfaces take the pointer.

use dispatcher::wayland::sessionlock::Phase;
use dispatcher::wire::trait_::wire_trait::WireTrait;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::desktop::WindowSurfaceType;
use smithay::desktop::utils::{send_frames_surface_tree, under_from_surface_tree};
use smithay::input::pointer::MotionEvent;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Point, Rectangle, SERIAL_COUNTER, Size};
use smithay::wayland::shell::wlr_layer::Layer;

use crate::state::Loop;
use crate::surface::interface::core::hit::SurfaceHit;

/// The comp policy's view of the lock (in CompState).
#[derive(Debug, Default)]
pub struct LockPolicy {
    /// The phase the last [`service`] pass acted on.
    seen: Phase,
    /// The keyboard focus at lock entry, restored at unlock.
    prior_focus: Option<WlSurface>,
}

thread_local! {
    static BLANK_ID: Id = Id::new();
}

pub fn phase(lp: &Loop) -> Phase {
    lp.state.session_lock.phase()
}

/// A lock is in force (Locking, Locked or Orphaned).
pub fn active(lp: &Loop) -> bool {
    phase(lp).active()
}

/// Whether `surface` belongs to the lock.
pub fn is_lock_surface(lp: &Loop, surface: &WlSurface) -> bool {
    lp.state.session_lock.owns(surface)
}

/// A lock began that [`service`] has not acted on yet: the moment to release
/// held keys to the client that still has the keyboard (seat
/// `release_held_keys`, called by the host before `service`).
pub fn entering(lp: &Loop) -> bool {
    active(lp) && !lp.inner.comp.lock.seen.active()
}

/// One loop pass (compd's loop closure): transitions, the keyboard's place,
/// the `locked` confirmation.
pub fn service(lp: &mut Loop) {
    let now = phase(lp);
    let seen = lp.inner.comp.lock.seen;
    if now != seen {
        if !seen.active() && now.active() {
            enter(lp);
        } else if seen.active() && !now.active() {
            leave(lp);
        }
        lp.inner.comp.lock.seen = now;
        // `focus.session_lock` moved.
        lp.inner.comp.settings_changed("", "session.lock");
    }
    if !now.active() {
        return;
    }
    keep_keyboard_on_lock(lp);
    let outputs: Vec<String> = lp.inner.space_state().state.outputs().map(Output::name).collect();
    if lp.state.session_lock.confirm_if_presented(outputs.iter().map(String::as_str)) {
        lp.inner.comp.lock.seen = Phase::Locked;
        lp.inner.comp.settings_changed("", "session.lock");
    }
}

/// Nothing that was running for the
/// unlocked session survives into the lock.
fn enter(lp: &mut Loop) {
    let serial = SERIAL_COUNTER.next_serial();
    let time = lp.inner.start_time.elapsed().as_millis() as u32;
    // A region selection ends `locked`; an interactive grab is dropped.
    super::region::finish(lp, super::region::Outcome::Locked);
    lp.inner.comp.interactive = None;
    let seat = lp.state.seat.seat.clone();
    if let Some(keyboard) = seat.get_keyboard() {
        lp.inner.comp.lock.prior_focus = keyboard.current_focus();
        keyboard.unset_grab(&mut lp.state);
        keyboard.set_focus(&mut lp.state, None, serial);
    }
    // Every popup of every window and layer surface is dismissed (the lock
    // tears down what the unlocked session had open).
    let mut roots: Vec<WlSurface> = Vec::new();
    for space in lp.inner.all_world_spaces() {
        roots.extend(space.state.elements().filter_map(|window| {
            use smithay::wayland::seat::WaylandFocus;
            window.wl_surface().map(|surface| surface.into_owned())
        }));
    }
    for output in lp.inner.space_state().state.outputs() {
        let map = smithay::desktop::layer_map_for_output(output);
        roots.extend(map.layers().map(|layer| layer.wl_surface().clone()));
    }
    for root in &roots {
        let popups: Vec<_> = smithay::desktop::PopupManager::popups_for_surface(root).map(|(popup, _)| popup).collect();
        for popup in popups {
            let _ = smithay::desktop::PopupManager::dismiss_popup(root, &popup);
        }
    }
    if let Some(pointer) = seat.get_pointer() {
        pointer.unset_grab(&mut lp.state, serial, time);
        let location = pointer.current_location();
        pointer.motion(&mut lp.state, None, &MotionEvent { location, serial, time });
        pointer.frame(&mut lp.state);
    }
}

/// The keyboard goes back where it
/// was, if that surface is still there and is not a lock surface.
fn leave(lp: &mut Loop) {
    let prior = lp.inner.comp.lock.prior_focus.take();
    let Some(keyboard) = lp.state.seat.seat.get_keyboard() else { return };
    let focus = prior.filter(|surface| {
        use smithay::reexports::wayland_server::Resource;
        surface.is_alive()
    });
    keyboard.set_focus(&mut lp.state, focus, SERIAL_COUNTER.next_serial());
}

/// Locked keyboard arbitration: the keyboard is on a lock
/// surface (the first output's that has one) or on nothing.
fn keep_keyboard_on_lock(lp: &mut Loop) {
    let Some(keyboard) = lp.state.seat.seat.get_keyboard() else { return };
    let current = keyboard.current_focus();
    if current.as_ref().is_some_and(|surface| is_lock_surface(lp, surface)) {
        return;
    }
    let target = lp.state.session_lock.surfaces().next().map(|(_, lock)| lock.wl_surface().clone());
    if current == target {
        return;
    }
    keyboard.set_focus(&mut lp.state, target, SERIAL_COUNTER.next_serial());
}

/// The output a scene pass draws: its `output_key`, or the first output
/// for a single-output pass.
fn output_for(lp: &Loop, render_key: Option<&str>) -> Option<Output> {
    let space = &lp.inner.space_state().state;
    match render_key {
        Some(key) => space
            .outputs()
            .find(|output| crate::state::state::output_key(output).as_str() == key)
            .cloned(),
        None => space.outputs().next().cloned(),
    }
}

/// What a locked output draws: an opaque blank over the whole output and,
/// above it, the lock surface the lock has there.
pub struct LockFrame {
    pub blank: SolidColorRenderElement,
    /// The lock surface, its physical location on the output, its scale.
    pub surface: Option<(WlSurface, Point<i32, Physical>, f64)>,
}

/// The scene's hook: `None` while unlocked (draw as usual); while locked,
/// the only things this output may show. Records the lock frame built.
pub fn frame(lp: &Loop, render_key: Option<&str>, size: Size<i32, Physical>) -> Option<LockFrame> {
    if !active(lp) {
        return None;
    }
    let blank = SolidColorRenderElement::new(
        BLANK_ID.with(Clone::clone),
        Rectangle::new(Point::from((0, 0)), size),
        CommitCounter::default(),
        [0.0, 0.0, 0.0, 1.0],
        Kind::Unspecified,
    );
    let output = output_for(lp, render_key);
    let surface = output.as_ref().and_then(|output| {
        lp.state.session_lock.mark_built(&output.name());
        let scale = output.current_scale().fractional_scale();
        lp.state
            .session_lock
            .surface_for(&output.name())
            .map(|lock| (lock.wl_surface().clone(), Point::from((0, 0)), scale))
    });
    Some(LockFrame { blank, surface })
}

/// The present path's hook, for `output` that just presented: while locked,
/// its lock surface gets the frame callbacks (and nothing else does, so the
/// caller skips the layers) and the presented lock frame counts toward
/// `locked`. Returns whether a lock is in force.
pub fn presented(lp: &Loop, output: &Output) -> bool {
    if !active(lp) {
        return false;
    }
    let lock = &lp.state.session_lock;
    lock.mark_presented(&output.name());
    if let Some(surface) = lock.surface_for(&output.name()) {
        let time = lp.inner.start_time.elapsed();
        send_frames_surface_tree(surface.wl_surface(), output, time, None, |_, _| Some(output.clone()));
    }
    true
}

/// The hit-test hook: `None` while unlocked (hit as usual); while locked,
/// the lock surface (or its subsurface) under the point on that point's
/// output, or no hit at all.
pub fn hit(lp: &Loop, position_world: Point<f64, Logical>) -> Option<Option<SurfaceHit>> {
    if !active(lp) {
        return None;
    }
    let space = &lp.inner.space_state().state;
    let found = space.outputs().find_map(|output| {
        let geometry = space.output_geometry(output)?;
        if !geometry.to_f64().contains(position_world) {
            return None;
        }
        let lock = lp.state.session_lock.surface_for(&output.name())?;
        let local = position_world - geometry.loc.to_f64();
        let (surface, origin) = under_from_surface_tree(lock.wl_surface(), local, (0, 0), WindowSurfaceType::ALL)?;
        Some(SurfaceHit::Layer {
            Ice: Some(true),
            layer: Layer::Overlay,
            surface,
            position_space: (geometry.loc + origin).to_f64(),
        })
    });
    Some(found)
}
