//! Device plug/unplug wiring: registers the retained udev watcher and routes
//! typed events to the host's reaction (`native.plugin/plugin.route`).
//! NEW capability of the restructure: previously the UdevBackend was dropped
//! after the initial lookup and hotplug events never flowed.

use kms::gpu::registry::node::node::NodeRegistry;
use crate::context::render::render::NativeRenderContext;
use crate::context::topology::topology::Topology;
use kms::udev::loop_::watch::watch::UdevWatch;
use smithay::reexports::calloop::EventLoop;
use std::cell::RefCell;
use std::rc::Rc;
use world::state::Loop;

/// No output is live. While dark the per-frame world
/// dispatch does not run, so a world-channel event from an emitter that does not
/// also request a redraw would sit undelivered; compd's per-iteration idle hook
/// now dispatches the worlds on every pass, dark or lit. Set on `WentDark`,
/// cleared on `Recovered`.
static DARK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the native backend currently has no live output.
pub fn dark() -> bool {
    DARK.load(std::sync::atomic::Ordering::Relaxed)
}

/// React to a reconcile's output-presence transition. EVERY reconcile routes its
/// result here — the udev hotplug path and the settings-driven one
/// (`display.reconcile::drain_reconcile`) — so a settings
/// change that darkens or recovers the display sets [`DARK`] and fires the same
/// lifecycle event as a hotplug.
pub fn apply_output_change(state: &mut Loop, change: world::event::output::output::OutputChange) {
    use world::event::output::output::OutputChange;
    // Fire the output-presence lifecycle event (event-driven — once per real
    // transition) on the ACTIVE world's router: that is the world whose systems
    // are dispatched. Background worlds re-read the snapshot token on enable.
    let active = state.inner.worlds.active_id();
    world::event::output::output::broadcast(state.inner.worlds.get_mut(active).channels(), change);
    match change {
        OutputChange::WentDark => {
            DARK.store(true, std::sync::atomic::Ordering::Relaxed);
            // Capturing requires an output — stop any in-progress capture the
            // moment the display goes away. (Capture is a Loop-level hook, not a
            // World system, so the emitter stops it directly.)
            recorder::interface::interface::stop_and_discard(state);
            // Deliver the event just broadcast (and anything else queued) now: the
            // per-frame dispatch does not run while dark. No-poll law: no repeating
            // timer for the dark window; the per-iteration idle hook dispatches the
            // worlds on every pass.
            world::pump::dark::dark::pump(state);
        }
        OutputChange::Recovered => DARK.store(false, std::sync::atomic::Ordering::Relaxed),
        _ => {}
    }
}

pub fn register(
    event_loop: &mut EventLoop<'static, Loop>,
    watch: UdevWatch,
    registry: Rc<RefCell<NodeRegistry>>,
    topology: Rc<RefCell<Topology>>,
    ctx_rc: Rc<RefCell<NativeRenderContext>>,
) {
    event_loop
        .handle()
        .insert_source(watch, move |event, _, state| {
            let decoded = kms::udev::loop_::event::event::decode(event);
            info!("udev event received: {decoded:?}");
            let rank = render_gles::preference::gpu::rank::rank::get();
            let reconcile = crate::plugin::route::route::route(
                decoded,
                &mut registry.borrow_mut(),
                &mut topology.borrow_mut(),
                &rank,
                &ctx_rc,
            );
            // A topology change on the driven device → drive the best connected
            // output per preferences (fail over / recover), or go dark + wait. Runs
            // in this udev dispatch (not the vblank callback), so the modeset is safe.
            if reconcile {
                if let Some(change) = crate::context::display::reconcile::reconcile::reconcile(state, &ctx_rc) {
                    apply_output_change(state, change);
                }
            }
            // Refresh the rim's display snapshot after any topology change so the
            // lid policy sees current external/internal presence.
            let active = ctx_rc.borrow().pipe().connector;
            let ctx = ctx_rc.borrow();
            let manager = ctx.drm_output_manager.borrow();
            let snap = crate::context::display::base::compute(
                manager.device(),
                active,
            );
            drop(manager);
            drop(ctx);
            *state
                .inner
                .kernel
                .get_mut(&drivers::lid::base::DISPLAY_SNAPSHOT_MUT) =
                snap;
        })
        .unwrap();
}
