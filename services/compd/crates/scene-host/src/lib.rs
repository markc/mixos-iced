//! Mix Scenes for compd: the host side of the Mix Scenes contract (Quoin's
//! `shell.scene.*` verbs), drawn with iced.
//!
//! - [`store`]: the transactional verb semantics (revisions, model
//!   authority, the page registry and the one dialog seat, notices, digest,
//!   refusals);
//! - [`host`]: request handling on compd's loop (provenance, owner
//!   attestation, receipts, the stale-connection fence, the departure sweep);
//! - [`port`]: the host's own Bus registration (`shell`) on a
//!   worker thread;
//! - [`render`] and [`view`]: placement and the iced surfaces.
//!
//! Opt-in: compd starts the host only when the `scene_host`
//! preference is on. The engine drives it through four calls, all on the
//! compositor thread: [`start`], [`service`] (post-dispatch, after the waker
//! fired), [`per_frame`] (each output's frame) and [`shutdown`].

use std::cell::RefCell;

use dispatcher::state::state::RedrawReason;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::utils::{Physical, Point, Size};
use world::state::Loop;

mod appearance;
mod description;
pub mod conf;
pub mod host;
mod icons;
mod images;
pub mod layout;
pub mod menu;
pub mod mount;
pub mod panels;
pub mod port;
mod preferences;
pub mod render;
pub mod seat;
mod state;
pub mod store;
pub mod templates;
pub mod verb;
pub mod view;

#[cfg(test)]
mod test_renderer;

pub use host::{Host, SceneHost, SceneSurface, Serviced, scene_surface_id};
pub use port::{DEFAULT_SERVICE, HostConfig, Waker};
pub use store::{Mounted, SceneStore};
pub use verb::SceneVerb;

thread_local! {
    /// The running host. Compositor-thread only: the request path evaluates
    /// bindings with thread-local Mix values, and the renderer needs the
    /// frame's GLES context.
    static HOST: RefCell<Option<SceneHost>> = const { RefCell::new(None) };
}

/// Start the host and its Bus worker. `waker` must wake the loop that calls
/// [`service`].
pub fn start(config: HostConfig, waker: Waker) -> Result<(), String> {
    let host = SceneHost::start(config, waker)?;
    HOST.with_borrow_mut(|slot| *slot = Some(host));
    Ok(())
}

/// Whether a host is running.
pub fn running() -> bool {
    HOST.with_borrow(Option::is_some)
}

/// Answer what the worker and the surfaces delivered; schedule a frame
/// (`RedrawReason::Publish`) when what is drawn may have changed.
pub fn service(lp: &mut Loop) -> Serviced {
    let serviced = HOST.with_borrow_mut(|slot| slot.as_mut().map(|host| host.service_port(lp)));
    let serviced = serviced.unwrap_or_default();
    if serviced.changed {
        lp.state.schedule_redraw(RedrawReason::Publish);
    }
    serviced
}

/// One output's frame, inside the GLES prepare pass: keep the scenes'
/// surfaces in step with the store.
pub fn per_frame(lp: &mut Loop, renderer: &mut GlesRenderer, size: Size<i32, Physical>) {
    let mut input_changed = false;
    let wake = HOST.with_borrow_mut(|slot| {
        let host = slot.as_mut()?;
        // The panel model first: an edge sliding in or out is placed
        // where it is now.
        let output = lp.inner.current_output();
        let name = output.name();
        let scale = output.current_scale().fractional_scale().max(0.1);
        host.ensure_output(
            lp.inner.current_output_key(),
            &name,
            (size.w as f32 / scale as f32, size.h as f32 / scale as f32),
        );
        let wake = host.tick_panels(&name);
        input_changed = host.render(lp, renderer, size);
        Some(wake)
    });
    if input_changed {
        world::comp::scenes::mark_geometry_dirty(lp);
    }
    schedule_panel_wake(lp, wake);
}

/// Real pointer motion on either backend, in physical output-local pixels.
/// The host feeds Quoin's core detector and panel membership inputs; only
/// exact dwell/conceal deadlines request a one-shot wake.
pub fn pointer_motion(lp: &mut Loop, point: Option<Point<f64, Physical>>) {
    let wake = HOST.with_borrow_mut(|slot| {
        let host = slot.as_mut()?;
        let output = lp.inner.active_output();
        let name = output.name();
        let size = output
            .current_transform()
            .transform_size(output.current_mode()?.size);
        let scale = output.current_scale().fractional_scale().max(0.1);
        host.ensure_output(
            lp.inner.active_output_key(),
            &name,
            (size.w as f32 / scale as f32, size.h as f32 / scale as f32),
        );
        let corners = lp.inner.comp.corners.config();
        let config = edges::CornerDetectorConfig::new(
            corners.deadzone_px as f32,
            std::time::Duration::from_millis(corners.dwell_ms),
            corners.velocity_max_px_s as f32,
        )
        .ok()?;
        let point = point.filter(|_| corners.enabled && !world::comp::session_lock::active(lp));
        host.host.panels.pointer(
            &name,
            point.map(|p| ((p.x / scale) as f32, (p.y / scale) as f32)),
            config,
        );
        Some(host.tick_panels(&name))
    });
    schedule_panel_wake(lp, wake);
}

/// Both pointer backends route buttons here before any surface hit-test.
pub fn pointer_button(lp: &mut Loop, button: u32, pressed: bool) -> bool {
    // Use output-local hardware coordinates, as motion does, rather than the
    // camera-transformed seat location. Resample even without a motion event.
    let point = lp.inner.pointer_mut().motion;
    pointer_motion(lp, Some(Point::from((point.x, point.y))));
    let shift = lp
        .state
        .seat
        .seat
        .get_keyboard()
        .is_some_and(|keyboard| keyboard.modifier_state().shift);
    let (consumed, wake) = HOST.with_borrow_mut(|slot| {
        let Some(host) = slot.as_mut() else {
            return (false, None);
        };
        let consumed = host.host.panels.pointer_button(button, pressed, shift);
        if consumed {
            // Opening/closing a menu on an empty/static edge still needs a
            // frame: the panel core may correctly answer Idle.
            lp.state.schedule_redraw(RedrawReason::Publish);
        }
        let name = lp.inner.active_output().name();
        (consumed, consumed.then(|| host.tick_panels(&name)))
    });
    schedule_panel_wake(lp, wake);
    consumed
}

fn schedule_panel_wake(lp: &mut Loop, wake: Option<panels::Wake>) {
    match wake {
        // An edge is moving: the next frame carries it on, and the frames stop
        // once it settles.
        Some(panels::Wake::Animate) => {
            PANEL_WAKE.set(None);
            lp.state.schedule_redraw(RedrawReason::Publish);
        }
        Some(panels::Wake::At(at)) => arm_panel_wake(lp, at),
        Some(panels::Wake::Idle) => PANEL_WAKE.set(None),
        None => {}
    }
}

thread_local! {
    /// The panel deadline a timer is armed for, so one deadline arms once.
    static PANEL_WAKE: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
}

/// A one-shot loop timer at a panel deadline (a grace or intro end): it
/// schedules the frame that ticks the model past it. Never a poll.
fn arm_panel_wake(lp: &mut Loop, at: std::time::Instant) {
    use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
    if PANEL_WAKE.get() == Some(at) {
        return;
    }
    PANEL_WAKE.set(Some(at));
    let armed = lp
        .loop_handle
        .insert_source(Timer::from_deadline(at), move |_, _, lp| {
            if PANEL_WAKE.get() == Some(at) {
                PANEL_WAKE.set(None);
                lp.state.schedule_redraw(RedrawReason::Publish);
            }
            TimeoutAction::Drop
        });
    if let Err(error) = armed {
        PANEL_WAKE.set(None);
        tracing::warn!("scene host: panel deadline timer not armed: {error}");
    }
}

/// The space the scene panels of `output` (compd's output key) reserve:
/// `(edge, logical px)` per DOCKED edge, as Quoin's exclusive zones.
/// Empty with no host. For the usable-area math.
pub fn exclusive_zones(output: &str) -> Vec<(seat::Edge, f32)> {
    HOST.with_borrow(|slot| {
        slot.as_ref()
            .map(|host| host.host.panels.zones(host.output_name(output)))
            .unwrap_or_default()
    })
}

/// The scene surfaces drawn on `output` (compd's output key), shaped as
/// layer surfaces for comp.props `surfaces` ([`SceneSurface::row`]). Empty
/// with no host.
pub fn surfaces(output: &str) -> Vec<SceneSurface> {
    HOST.with_borrow(|slot| {
        slot.as_ref()
            .map(|host| host.surfaces(host.output_name(output)))
            .unwrap_or_default()
    })
}

/// The comp.props id (`scene:<name>`) of the scene holding the keyboard: the
/// iced keyboard focus is one of its surfaces, so keys go to it and not to
/// the window the seat last focused. `None` otherwise; comp.props
/// `focus.keyboard` reports this ahead of the seat's surface.
pub fn focused_scene(state: &Loop) -> Option<String> {
    let handle = state.inner.surface().registry.as_ref()?.keyboard_focus()?;
    HOST.with_borrow(|slot| slot.as_ref()?.scene_of(handle).map(scene_surface_id))
}

/// Drain settings and deregister under a two-second budget, with a small
/// bounded runtime/completion margin.
pub fn shutdown() {
    if let Some(host) = HOST.with_borrow_mut(Option::take) {
        host.finish();
    }
}
