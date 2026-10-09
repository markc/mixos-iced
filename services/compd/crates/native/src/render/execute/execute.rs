//! The frame executor: runs the compositor-issued FramePlan against the
//! hosted pipe. (Ex draw.scene/scene.rs `scene()`, now plan-driven — the pass
//! presence/ordering comes from `frames::draw::plan::frame`, not from a
//! local Status match. Pass KINDS are compositor vocabulary; this crate maps
//! each kind to its element source.)
//!
//! The Rc<RefCell<renderer>> borrow choreography is carried verbatim from the
//! original, including its documented reasoning about the bind+blit
//! double-borrow problem.
//!
//! Completion-pass semantics:
//! - frame flags come from the plane policy (`scanout.plane/plane.direct`),
//!   not a hardcoded DEFAULT;
//! - the post-scene tap fires only when the PLAN places it AND a subscriber
//!   is active (`ctx.tap_subscriptions`), which is also when the capture
//!   registry is consulted;
//! - queue failures panic outside the session-resume window (see
//!   `scanout.flip/flip.queue`);
//! - the executor reports a `FrameOutcome` so the pacing layer (`wire.frame`)
//!   can act on empty frames when the `flip-estimate` net is compiled in and
//!   enabled.

use render_gles::element::wrap::wrap::GlesElementWrapper;
use crate::context::render::render::NativeRenderContext;
use frames::draw::plan::frame::frame::{plan, FramePass};
use frames::draw::plan::tap::tap::POST_SCENE;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::backend::renderer::{Bind, RendererSuper};
use smithay::reexports::calloop::LoopHandle;
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Physical, Point, Rectangle, Scale, Transform};
use std::cell::RefCell;
use std::rc::Rc;
use world::state::state::{StateDRMBinding, StatusSession};
use world::state::Loop;
use dispatcher::frame::frame::{ElementMeta, SceneDispatch};
use graphics::capture::registry::{CaptureRegistry, OutputId};
use super::diagnostics;


/// Honor `RenderFrameResult::needs_sync()` before queueing to KMS: when smithay
/// can't hand the atomic commit a GPU fence (device lacks fencing, or the
/// render's SyncPoint isn't an exportable fd), it is *our* responsibility to
/// CPU-wait for render completion before `queue_frame`, or KMS may scan out a
/// half-rendered buffer. When fencing IS available (`needs_sync()==false`),
/// this is a no-op and smithay attaches our fence as the commit IN_FENCE — the
/// best (no-CPU-wait) path. Cheap insurance that keeps every renderer correct.
fn honor_needs_sync<B, F, E>(
    result: &smithay::backend::drm::compositor::RenderFrameResult<'_, B, F, E>,
) where
    B: smithay::backend::allocator::Buffer,
    F: smithay::backend::drm::Framebuffer,
{
    use smithay::backend::drm::compositor::PrimaryPlaneElement;
    if result.needs_sync() {
        if let PrimaryPlaneElement::Swapchain(ref element) = result.primary_element {
            if let Err(err) = element.sync.wait() {
                warn!("native: render fence wait interrupted before queue_frame: {err:?}");
            }
        }
    }
}

/// What this execute() call did, for the pacing layer.
#[derive(Debug)]
pub enum FrameOutcome {
    /// A frame was rendered and queued; a VBlank will follow.
    Queued,
    /// Nothing was queued (no damage, empty plan, paused, or the queue was
    /// deferred to the resume watchdog); frame callbacks already handled.
    Idle,
    /// Empty damage and the estimate-pacing net is active: NO frame
    /// callbacks were sent — `wire.frame` delivers them at the estimated
    /// next vblank.
    #[cfg(feature = "flip-estimate")]
    EmptyDeferred {
        output: smithay::output::Output,
        visible: Vec<smithay::desktop::Window>,
    },
}

/// Which outputs this `execute()` call may render. The per-vblank path passes
/// `Crtc(handle)` so ONLY the output that just flipped is re-rendered — it is
/// structurally impossible to produce a frame for a monitor that has not
/// vblanked, which is what decouples each monitor's refresh cadence. The ping /
/// kickstart / resume-watchdog paths pass `All` to (re)start every idle output.
#[derive(Debug, Clone, Copy)]
pub enum RenderScope {
    /// Every output that is idle (not mid-flip) — ping, kickstart, resume.
    All,
    /// Only the output whose CRTC just delivered a VBlank — per-monitor pacing.
    Crtc(smithay::reexports::drm::control::crtc::Handle),
}

/// Shortest rate-cap wait worth deferring for. A deferral costs a timerfd
/// wake-up plus a ping round-trip back through the event loop; under roughly a
/// millisecond that overhead exceeds the interval being enforced, so the cap
/// would cost more rate than it saves.
const CAP_DEFER_FLOOR: std::time::Duration = std::time::Duration::from_micros(1_000);

/// Restore the render/submit invariant after a frame that was rendered but will
/// NOT be queued.
///
/// `DrmCompositor` pushes a damage-history entry on every render that produced
/// damage (`renderer/damage/mod.rs`), but advances swapchain slot ages only in
/// `queue_frame`/`commit_frame` -> `swapchain.submitted`. The two must stay 1:1.
/// A render that is never queued leaves every OTHER slot's recorded age one lower
/// than its true age, so the tracker hands back LESS damage than that buffer
/// actually needs and stale regions survive — the "screen alternating between the
/// last two samples" artifact. Zeroing the ages discards the poisoned accounting:
/// the next render is a full redraw (age 0) and everything is consistent again.
///
/// ONLY for genuinely unsubmitted renders. An EMPTY render needs nothing: it
/// pushes no history and performs no submit, so it is already consistent. A
/// `queue_frame` whose DRM submit fails is also fine — `submitted()` ran first.
///
/// `FrameFlags::FORCE_PRESENT` (pre-emptive rendering) makes a would-be-empty
/// frame queueable, and needs nothing here either — it changes only
/// `PreparedFrame::is_empty`, not `plane_state.skip`, and `queue_frame` gates
/// `swapchain.submitted` on `skip`. So such a frame re-presents the current
/// framebuffer without pushing history OR advancing ages: still 1:1.
fn discard_unsubmitted_render(
    pipe: &crate::context::render::render::OutputPipe,
) {
    if let Some(o) = pipe.drm_output.as_ref() {
        o.with_compositor(|c| c.reset_buffer_ages());
    }
}

/// Where the redraw schedule lives in the loop data, for its deadline timers.
fn schedule_of(state: &mut Loop) -> &mut protocols::redraw::schedule::schedule::Schedule {
    &mut state.state.redraw
}

/// An EMPTY frame's frame callbacks, owed rather than sent.
///
/// Sending them at once lets a client that commits without damage loop as fast
/// as the CPU allows — commit, empty frame, callback, commit — since no flip and
/// so no vblank paces it. They go at the estimated next vblank instead, from a
/// one-shot timer that exists only because this frame owes them: nothing is
/// armed on a static screen, where no frame is rendered at all.
///
/// The cursor's callbacks too: a damage-less cursor or
/// drag-icon client would otherwise loop unpaced on its own. One slot per pipe:
/// a later empty frame joins the armed timer instead of arming a
/// second one that would deliver the next callback early; a queued flip cancels
/// it (`cancel_owed_frames`), since that frame's `present` sends the callbacks.
fn owe_frames(
    state: &mut Loop,
    pipe: &mut crate::context::render::render::OutputPipe,
    visible: Vec<smithay::desktop::Window>,
    delay: std::time::Duration,
) {
    if let Some(owed) = pipe.owed_frames.as_ref()
        && !owed.fired.get()
    {
        let mut windows = owed.windows.borrow_mut();
        for window in visible {
            if !windows.contains(&window) {
                windows.push(window);
            }
        }
        return;
    }
    let windows = Rc::new(RefCell::new(visible));
    let fired = Rc::new(std::cell::Cell::new(false));
    let output = pipe.output.clone();
    let (timer_windows, timer_fired, timer_output) = (windows.clone(), fired.clone(), output.clone());
    let armed = state.loop_handle.insert_source(
        smithay::reexports::calloop::timer::Timer::from_duration(delay),
        move |_, _, state: &mut Loop| {
            timer_fired.set(true);
            let visible = std::mem::take(&mut *timer_windows.borrow_mut());
            send_owed(state, &timer_output, &visible);
            smithay::reexports::calloop::timer::TimeoutAction::Drop
        },
    );
    match armed {
        Ok(token) => {
            pipe.owed_frames = Some(crate::context::render::render::OwedFrames { token, windows, fired });
        }
        Err(err) => {
            // Late beats never: without the timer the client would wait forever.
            warn!("native: owed frame-callback timer not armed ({err}); sending now");
            let visible = std::mem::take(&mut *windows.borrow_mut());
            send_owed(state, &output, &visible);
        }
    }
}

/// A flip was queued on this pipe: its `present` sends the frame callbacks, so
/// an owed-callback timer still armed would only deliver them a second time.
fn cancel_owed_frames(state: &Loop, pipe: &mut crate::context::render::render::OutputPipe) {
    if let Some(owed) = pipe.owed_frames.take()
        && !owed.fired.get()
    {
        state.loop_handle.remove(owed.token);
    }
}

fn send_owed(state: &mut Loop, output: &smithay::output::Output, visible: &[smithay::desktop::Window]) {
    frames::draw::present::callbacks::callbacks::send_window_frames(state, output, visible);
    frames::draw::present::callbacks::callbacks::send_layer_frames(state, output);
    frames::draw::present::cursor::cursor::send_frames(state, output);
}

// The GLES path unwraps its render, so there are no `render_frame` failures to
// count. `OutputPipe::render_failures` is left at zero.

/// Capture-only pass. No mode reconciliation, DRM compositor, properties,
/// swapchain, queue_frame, vblank bookkeeping or presentation feedback here.
/// Borrow only the existing render-node GLES context and replay the same scene
/// assembly as the KMS pass, scoped to each output's viewport, mode and scale.
fn capture_offscreen(ctx_rc: &Rc<RefCell<NativeRenderContext>>, state: &mut Loop) {
    let serve_plain_copies = matches!(state.inner.status_session, StatusSession::Paused);
    let Ok(ctx) = ctx_rc.try_borrow() else {
        screencopy::offscreen::fail_pending(serve_plain_copies);
        return;
    };
    if !ctx.outputs.iter().any(|pipe| {
        screencopy::offscreen::pending(&pipe.output, serve_plain_copies)
    }) {
        screencopy::offscreen::fail_pending(serve_plain_copies);
        return;
    }
    let Ok(mut binding) = ctx.gpu_binding.try_borrow_mut() else {
        screencopy::offscreen::fail_pending(serve_plain_copies);
        return;
    };
    let StateDRMBinding { gpus, primary } = &mut *binding;
    // Unlike single_renderer(), this cannot trigger device enumeration.
    let Some(renderer) = gpus.cached_renderer_mut(primary) else {
        screencopy::offscreen::fail_pending(serve_plain_copies);
        return;
    };
    let previous_output = state.inner.render_output.clone();
    for pipe in &ctx.outputs {
        let output = &pipe.output;
        if !screencopy::offscreen::pending(output, serve_plain_copies) {
            continue;
        }
        let size = pipe.mode.size;
        let scale = output.current_scale().fractional_scale();
        if size.w <= 0 || size.h <= 0 || !scale.is_finite() || scale <= 0.0 {
            screencopy::offscreen::fail_output(output, serve_plain_copies);
            continue;
        }
        let key = world::state::state::output_key(output);
        state.inner.render_output = Some(key.clone());
        state.inner.output_views_mut().ensure(&key);
        let prepared = frames::scene::scene::prepare(state, renderer, size);
        let scene = frames::scene::scene::scene(state, renderer, size, prepared);
        screencopy::offscreen::capture(
            renderer, &scene.Element, |element| element.is_cursor(), output, size, scale,
            serve_plain_copies,
        );
    }
    state.inner.render_output = previous_output;
    screencopy::offscreen::fail_pending(serve_plain_copies);
}

/// Window snapshots use only the cached render-node context, even while KMS
/// is active. They never wait for a free scanout pipe or a page flip.
fn capture_windows_offscreen(ctx_rc: &Rc<RefCell<NativeRenderContext>>, state: &Loop) {
    if !screencopy::file::pending_windows() {
        return;
    }
    let Ok(ctx) = ctx_rc.try_borrow() else {
        screencopy::file::fail_windows("render context unavailable");
        return;
    };
    let Ok(mut binding) = ctx.gpu_binding.try_borrow_mut() else {
        screencopy::file::fail_windows("GPU context unavailable");
        return;
    };
    let StateDRMBinding { gpus, primary } = &mut *binding;
    let Some(renderer) = gpus.cached_renderer_mut(primary) else {
        screencopy::file::fail_windows("no cached capture renderer");
        return;
    };
    for pipe in &ctx.outputs {
        let scale = pipe.output.current_scale().fractional_scale();
        screencopy::offscreen::capture_windows(renderer, &pipe.output, |renderer, target| {
            let window = world::window::draw::frame::scene::capture_window(state, target.id, target.generation)
                .map_err(|error| screencopy::file::ControlReply::WindowTarget { id: target.id, error })?;
            world::window::draw::frame::scene::capture(renderer, &window, scale)
                .map_err(screencopy::file::capture_failed)
        });
    }
    screencopy::file::fail_windows("no capture output available");
}

/// Whether [`execute`] skips every frame right now (session paused, panel
/// powered down, no live output) without ticking the iced registry. A producer
/// that re-requests a frame until one drains it must not request while this
/// holds: the skipped frame drains nothing and the request pings the loop
/// straight back here. compd pinned two cores this way after a VT switch away
/// with input still queued for a scene surface. Resume, DPMS-on and output
/// recovery each request their own frame, which drains what waited.
pub fn frames_parked(state: &Loop) -> bool {
    matches!(state.inner.status_session, StatusSession::Paused)
        || *state.inner.kernel.get(&drivers::lid::base::DISPLAY_OFF)
        || NO_OUTPUT_PARKED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Set by [`execute`]'s no-output gate, cleared by the first frame past it.
/// Read from the gate itself rather than from `DARK`: a failed flip drops a
/// pipe's `drm_output` without going dark, and only the render context knows.
static NO_OUTPUT_PARKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn execute(
    ctx_rc: Rc<RefCell<NativeRenderContext>>,
    loop_handle: LoopHandle<'static, Loop>,
    state: &mut Loop,
    scope: RenderScope,
) -> FrameOutcome {
    let _ = &loop_handle; // retained for parity with the original signature
    capture_windows_offscreen(&ctx_rc, state);
    if let StatusSession::Paused = state.inner.status_session {
        let _skip = ledger::frame_trace::span("kms_skip_paused", 0);
        capture_offscreen(&ctx_rc, state);
        frames::draw::present::parked::parked::arm(state, frames_parked);
        return FrameOutcome::Idle;
    }
    // DPMS-off gate: a page-flip would re-power the blanked connector, so skip
    // frame production entirely while the panel is powered down (lid/idle).
    if *state.inner.kernel.get(&drivers::lid::base::DISPLAY_OFF) {
        let _skip = ledger::frame_trace::span("kms_skip_dpms_off", 0);
        capture_offscreen(&ctx_rc, state);
        frames::draw::present::parked::parked::arm(state, frames_parked);
        return FrameOutcome::Idle;
    }

    // Drain any pending output-mode / output-switch transaction from the settings
    // window every render frame, so a provisional Apply and especially a Confirm/
    // Revert take effect promptly instead of waiting for the next libinput event
    // (the request channels are otherwise only drained on input — a still pointer
    // after clicking Keep would let the ~15s watchdog auto-revert). Runs before the
    // context borrow below; both are no-ops when no request is pending.
    crate::context::display::mode::mode::drain(state, &ctx_rc);
    crate::context::display::reconcile::reconcile::drain_reconcile(state, &ctx_rc);

    let mut ctx = ctx_rc.borrow_mut();
    let ctx_ref = &mut *ctx;
    // Skip the whole frame only if NO output is live (all in the transient monitor-
    // switch teardown window). Otherwise the per-output loop below skips just the
    // dark ones; every `outputs[idx].drm_output.as_*().unwrap()` is guarded per pipe.
    if ctx_ref.outputs.iter().all(|p| p.drm_output.is_none()) {
        let _skip = ledger::frame_trace::span("kms_skip_no_output", 0);
        NO_OUTPUT_PARKED.store(true, std::sync::atomic::Ordering::Relaxed);
        drop(ctx);
        capture_offscreen(&ctx_rc, state);
        frames::draw::present::parked::parked::arm(state, frames_parked);
        return FrameOutcome::Idle;
    }
    NO_OUTPUT_PARKED.store(false, std::sync::atomic::Ordering::Relaxed);
    // Plane assignment is decided BEFORE this frame's scene exists, so it follows
    // the policy resolved LAST frame. Deliberately not the config's potential:
    // that would strip hardware planes (and the hardware cursor with them) the
    // moment any selector is armed, including on a desktop that is merely waiting
    // for a target and never tears.
    let mut frame_flags = kms::scanout::plane::direct::direct::flags(
        protocols::tearing::gate::gate::tearing(),
    );
    // Pre-emptive rendering: never let a frame be reported empty, so the loop
    // flips every pass instead of parking — the tail of this function re-arms the
    // redraw latch only after a non-empty result. Default `Engaged` applies that
    // only while a section is governing; `Always` applies it unconditionally. See
    // `Config::preemptive`.
    //
    // FORCE_PRESENT and NOT `reset_buffer_ages` / `DRAW_ALL_ELEMENTS`: those two
    // force a full-screen redraw. Being pre-emptive must not mean doing more work
    // per frame — only doing it sooner — so damage stays honest and this changes
    // nothing but whether the prepared frame may be called empty.
    let preemptive = model::environment::tearing::config::config::get()
        .preemptive
        .forces(protocols::tearing::gate::gate::governed());
    if preemptive {
        frame_flags |= smithay::backend::drm::compositor::FrameFlags::FORCE_PRESENT;
    }

    let gpu_binding = ctx_ref.gpu_binding.clone();
    let mut binding = gpu_binding.borrow_mut();
    let StateDRMBinding { gpus, primary } = &mut *binding;

    // The capture registry is pre-created at startup (loader prewarm) from the
    // shared bevy context — never built mid-render. Its tap subscription, by
    // contrast, lives on this backend's render context (created during render),
    // so subscribe exactly once here: registry presence IS the tap (Law 5).
    if state.inner.kernel.get(&world::driver::capture::base::CAPTURE_REGISTRY).is_some()
        && !ctx_ref.tap_subscriptions.is_active(POST_SCENE)
    {
        ctx_ref.tap_subscriptions.subscribe(POST_SCENE);
    }

    // Wrap the renderer in Rc<RefCell> so capture closures can defer borrow
    // tracking to runtime, sidestepping the bind+blit double-borrow problem
    // at compile time.
    let gles_renderer = Rc::new(RefCell::new(gpus.single_renderer(primary).unwrap()));

    // ---- Per-output render loop -------------------------------------------------
    // The renderer + GPU binding above are shared (built once); `size`, the render
    // target, the scene and the page-flip are per output. Each lit CRTC is drawn and
    // flipped in turn on the one GLES renderer. Single-output = one iteration, so the
    // behaviour is unchanged. (Body left at its original indent for review clarity.)
    let mut any_queued = false;
    // wlr-screencopy readbacks this pass started, answered once every pipe's
    // frame is queued (below the loop).
    let mut screencopy: Vec<screencopy::Captures<smithay::backend::renderer::gles::GlesRenderer>> =
        Vec::new();
    #[cfg(feature = "flip-estimate")]
    let mut deferred: Option<FrameOutcome> = None;
    // One-time diagnostic: the actual multi-output set the render loop sees.
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        static LOGGED: AtomicBool = AtomicBool::new(false);
        if !LOGGED.swap(true, Ordering::Relaxed) {
            let zoom = state.inner.camera().transform.zoom;
            let cam = state.inner.camera().transform.position;
            let lw: Vec<f64> = ctx_ref
                .outputs
                .iter()
                .map(|p| {
                    let s = p.output.current_scale().fractional_scale();
                    p.mode.size.w as f64 / if s.abs() < 1e-6 { 1.0 } else { s }
                })
                .collect();
            let total: f64 = lw.iter().sum();
            info!(
                "MULTI-OUTPUT render: {} pipe(s), camera pos=({:.1},{:.1}) zoom={:.3} layout_total_w={:.0}",
                ctx_ref.outputs.len(),
                cam.x,
                cam.y,
                zoom,
                total,
            );
            for (i, p) in ctx_ref.outputs.iter().enumerate() {
                let props = p.output.physical_properties();
                let scale = p.output.current_scale().fractional_scale();
                let x_left: f64 = lw[..i].iter().sum();
                let center = x_left + lw[i] / 2.0;
                let off_x = (center - total / 2.0) / if zoom.abs() < 1e-6 { 1.0 } else { zoom };
                let geo = state.inner.space_state().state.output_geometry(&p.output);
                info!(
                    "  pipe[{}] crtc={:?} name={:?} edid={:?} mode={}x{} scale={:.2} live={} → render_offset_x={:.1} space_geometry={:?}",
                    i,
                    p.crtc,
                    p.output.name(),
                    format!("{} {} {}", props.make, props.model, props.serial_number),
                    p.mode.size.w,
                    p.mode.size.h,
                    scale,
                    p.drm_output.is_some(),
                    off_x,
                    geo,
                );
            }
        }
    }
    // Round-1 finding B: hold the wakes of requests made while this renders (a
    // source asking for a frame from inside every render), and decide after the
    // loop — immediately if a flip is coming, paced to the vblank if not.
    state.state.redraw.begin_render();
    // The earliest estimated vblank among this pass's empty frames (the pace for
    // a held wake when nothing was queued).
    let mut pace: Option<std::time::Duration> = None;
    for output_idx in 0..ctx_ref.outputs.len() {
        let trace_output = if ledger::frame_trace::enabled() {
            OutputId::from_key(&world::state::state::output_key(&ctx_ref.outputs[output_idx].output)).0
        } else { 0 };
        if ctx_ref.outputs[output_idx].drm_output.is_none() {
            let _skip = ledger::frame_trace::span("kms_skip_no_pipe", trace_output);
            continue;
        }
        // Per-monitor pacing: on a vblank, render ONLY the pipe whose CRTC flipped.
        // Any other output is driven by its OWN vblank — rendering it here would
        // couple its cadence to this one. (All = ping/kickstart/resume: render every
        // idle output.)
        if let RenderScope::Crtc(target) = scope {
            if ctx_ref.outputs[output_idx].crtc != target {
                let _skip = ledger::frame_trace::span("kms_skip_other_crtc", trace_output);
                continue;
            }
        }
        // Skip a pipe whose page-flip is still in flight: its `queued_frame` slot
        // is occupied and won't scan out until its own vblank. Re-rendering it now
        // (on some OTHER output's vblank) would only overwrite that pending frame
        // and burn a CPU render+sync — the coupling that dragged a high-refresh
        // output down to a slower neighbour's rate. Its own vblank clears this and
        // re-renders it. (Single output: its vblank clears it each frame → no skip.)
        let key = world::state::state::output_key(&ctx_ref.outputs[output_idx].output);
        if state.state.redraw.in_flight(&key) {
            let _skip = ledger::frame_trace::span("kms_defer_pending_flip", trace_output);
            continue;
        }
        // Already rendered for the current epoch: nothing has been requested of
        // this pipe since. A wake is a STALE signal — the ping only says "a
        // redraw was asked for since the last drain" — and the vblank path may
        // have rendered that request already (a commit that landed mid-flight is
        // rendered by the flip completion, and the ping it also armed then finds
        // an idle pipe). Without this, that wake re-rendered unchanged content,
        // and under a governing section `FORCE_PRESENT` flipped it: composites
        // above the paced client's commit rate, each a tear spent on nothing.
        // Same for the off-thread workers' pings. The epoch is the ground truth
        // the vblank path already trusts; every forced render bumps it
        // (`force_redraw`, `bump_redraw_epoch`), so rescues are unaffected.
        if !state.state.redraw.needs(&key) {
            let _skip = ledger::frame_trace::span("kms_skip_current_epoch", trace_output);
            continue;
        }
        // Rate cap / pacing gate: with async flips the completion event arrives
        // almost immediately, so nothing throttles the loop to the panel any
        // more. `min_interval` is the policy's ceiling (a multiple of refresh,
        // or the fixed target interval under `Paced`); holding the composite
        // back here is the whole point — an unpaced loop renders frames the beam
        // never reaches.
        //
        // This DEFERS, it does not drop. The caller already consumed the
        // `needs_redraw` latch and its lost-wakeup guard only covers `in_flight`
        // pipes, so a bare `continue` would leave nothing to wake the loop and
        // freeze the compositor until unrelated input scheduled a redraw. Arm a
        // one-shot timer for the remainder instead.
        {
            let pipe = &ctx_ref.outputs[output_idx];
            // The cap belongs to whichever section is in force, but the visible
            // set is not known until the scene is built — which happens after
            // this gate. So take the ceiling the PREVIOUS frame resolved from a
            // real scene, rather than resolving a fresh one from an approximate
            // `Scene` here (which got `TargetFocused` wrong). A one-frame lag on
            // a rate ceiling is immaterial; deferring the gate until after the
            // render is not, since the whole point is to skip the composite.
            let cap = pipe.cap_interval;
            // Measured START-to-START. Gating on the *end* of the previous frame
            // would enforce `min + composite`, not `min` — the cap and the render
            // would serialize instead of overlap, so even a cap far above the
            // achievable rate would slow the loop down.
            let held = match (cap, pipe.render_start) {
                (Some(min), Some(started)) => min.checked_sub(started.elapsed()),
                _ => None,
            }
            // Deferring costs a timerfd wake-up plus a ping round-trip through
            // the event loop. Below that cost the deferral is more expensive than
            // the interval it enforces — which is how a 20x cap (a 0.83ms
            // interval on 60Hz, i.e. nominally no cap at all) ended up throttling
            // harder than no cap. Round down to "render now" instead.
            .filter(|remaining| *remaining > CAP_DEFER_FLOOR);
            if let Some(remaining) = held {
                let now = std::time::Instant::now();
                // Re-arm only when no live timer covers this window; a deadline in
                // the past belongs to a timer that has already fired.
                if pipe.cap_wake.is_none_or(|deadline| deadline <= now) {
                    let token = loop_handle
                        .insert_source(
                            smithay::reexports::calloop::timer::Timer::from_duration(remaining),
                            move |_, _, state: &mut Loop| {
                                // A bare wake (not a request): the deferred
                                // request is still pending on this pipe's epoch,
                                // so the frame answers its own reasons, and only
                                // the unconditional ping restarts an otherwise
                                // idle cycle.
                                state.state.redraw.wake();
                                smithay::reexports::calloop::timer::TimeoutAction::Drop
                            },
                        )
                        .ok();
                    if token.is_some() {
                        ctx_ref.outputs[output_idx].cap_wake = Some(now + remaining);
                    } else {
                        // Without a wake-up the loop would stall; rendering one
                        // frame early is strictly better than freezing.
                        warn!("rate-cap timer registration failed; compositing uncapped this frame");
                        ctx_ref.outputs[output_idx].cap_wake = None;
                    }
                }
                if ctx_ref.outputs[output_idx].cap_wake.is_some() {
                    let _skip = ledger::frame_trace::span_with_detail(
                        "kms_defer_rate_cap", trace_output, remaining.as_micros() as u64,
                    );
                    continue;
                }
            }
        }
        // This pipe is being rendered for the CURRENT redraw epoch — stamp it so
        // neither its own vblank (`process_vblank`) nor a later `execute(All)` (the
        // epoch skip above) renders it again for the same request. Sampled once at
        // the top of the frame, so a redraw requested WHILE this renders leaves the
        // pipe behind and is serviced next time.
        state.state.redraw.rendering(&key);
        // The ced gate's stall check reads each frame update as a `comp_update`
        // span; this is one pipe's drawn frame, the twin of the nested span
        // (nested compose), held over this iteration's scene, render and queued
        // flip.
        let _frame = ledger::frame_trace::span("comp_update", 0);
        ctx_ref.outputs[output_idx].render_start = Some(std::time::Instant::now());
        // NO per-frame age reset here. Per-output damage state is already correct by
        // construction — one DrmCompositor, one damage tracker and one swapchain per
        // CRTC, nothing shared — and both renderers preserve the undamaged remainder
        // (Vulkan composites with LOAD_OP_LOAD + per-element scissors; GLES clears via
        // a scissored quad fill). Per-CRTC vblank pacing is exactly the case smithay is
        // designed for.
        //
        // The artifact this used to paper over was the render/submit invariant being
        // broken by the capture pre-render; see `discard_unsubmitted_render`. Resetting
        // ages every frame also REPLACES any in-flight slot with a fresh one, dropping
        // its GBM buffer — so it cost a full-resolution dmabuf reallocation and a
        // Vulkan re-import per output per frame, which is where the multi-monitor
        // frame-rate collapse came from.
        let size = ctx_ref.outputs[output_idx].mode.size;
        // Tell the rim which physical output this frame draws, so the focus/
        // coordinate accessors (`current_output()`) resolve THIS output's mode
        // size/scale. Cleared after the loop so the input path falls back to the
        // cursor's output.
        let output_key =
            world::state::state::output_key(&ctx_ref.outputs[output_idx].output);
        let output_scale = ctx_ref.outputs[output_idx].output.current_scale().fractional_scale();
        // Stable capture id for THIS monitor (EDID-derived, not the vec index) so
        // capture entries key the same way the rim's capture requests resolve them.
        let output_id = OutputId::from_key(&output_key);
        state.inner.render_output = Some(output_key.clone());
        // Ensure THIS output has its own view tree (own camera + panes) so the focus
        // accessors resolve THIS monitor's independent camera while drawing — each
        // screen is its own viewport. Use `ensure` (NOT `set_current`): the render
        // loop must not move `current` off the cursor's output (the input systems
        // read `current`); `render_output` above already drives the draw accessors.
        state.inner.output_views_mut().ensure(&output_key);

    // ---- set_output_size: scoped borrow_mut ----
    if let Some(registry) = &state.inner.kernel.get(&world::driver::capture::base::CAPTURE_REGISTRY) {
        let mut r = gles_renderer.borrow_mut();
        let _ = registry.set_output_size(
            &state.inner.environment.GPU.as_str(),
            r.as_mut(),
            output_id,
            size,
        );
        drop(r);
    }

    // The compositor decides what this frame contains (Law 5): the plan
    // places the tap; the subscription set says whether anyone is listening.
    // The session lock and picker passes are not part of this plan, and there is
    // no shader pipeline: the plan is the scene pass (+ its tap) and the renderer
    // keeps its default (no-bundle) facts.
    let frame_plan = plan(&state.inner.status);
    let render_scene = frame_plan.has_pass(FramePass::Scene);
    if !render_scene {
        let _skip = ledger::frame_trace::span("kms_skip_empty_plan", trace_output);
    }
    let tap_post_scene =
        frame_plan.has_tap(POST_SCENE) && ctx_ref.tap_subscriptions.is_active(POST_SCENE);

    // Connector property pass: colorimetry + link bit depth, once per pipe, after
    // smithay's first modeset has bound the connector (gated on a seen vblank so
    // the prop-only atomic commit references an ACTIVE connector). A TEST commit
    // validates first, so a rejected request can never blank the display.
    //
    // NOT gated on `hdr_active` any more. These are sticky properties inherited
    // from whoever owned the connector last — another VT's compositor, or an
    // earlier HDR session of our own. An SDR pipe must therefore actively reset
    // `Colorspace` to Default and clear the HDR metadata; leaving them alone is
    // what made SDR content render through BT.2020 (heavy red cast).
    if !ctx_ref.outputs[output_idx].props_applied && (*state.inner.kernel.get(&drivers::resume::base::VBLANK_SEEN)) {
        let depth = model::environment::config::base::get().depth;
        let want_bpc: u64 = if depth == 10 { 10 } else { 8 };
        let hdr_active = ctx_ref.outputs[output_idx].hdr_active;
        let conn = ctx_ref.outputs[output_idx].connector;
        let crtc = ctx_ref.outputs[output_idx].crtc;
        match crate::render::execute::hdr::apply_connector_props(
            &ctx_ref.drm_fd,
            conn,
            &ctx_ref.outputs[output_idx].hdr_caps,
            hdr_active,
            want_bpc,
        ) {
            Ok(o) => {
                ctx_ref.outputs[output_idx].props_applied = true;
                info!("connector properties applied (colorimetry + max bpc)");
            }
            Err(e) => {
                ctx_ref.outputs[output_idx].props_applied = true; // don't retry every frame
                if hdr_active {
                    ctx_ref.outputs[output_idx].hdr_active = false;
                    let c = &ctx_ref.outputs[output_idx].hdr_caps;
                    model::stats::registry::base::set_hdr_info(
                        false,
                        c.hdr_capable(),
                        "SDR",
                        c.hdr.max_luminance.unwrap_or(0.0),
                        c.colorimetry.bt2020_rgb,
                        "8-bit sRGB",
                    );
                }
            }
        }
    }

    let mut last_result_empty = true;
    // THIS frame's per-element render states, kept only so `collect_feedback` can
    // report `ZeroCopy` per surface. Cloned out of the `RenderFrameResult` because
    // that borrows the renderer and is dropped well before `present` runs; the map
    // is one entry per element, so this is cheap next to the frame it describes.
    let mut frame_states: Option<smithay::backend::renderer::element::RenderElementStates> = None;
    let mut visible_window: Vec<_> = Vec::new();

    // GLES composes and scans out; there is no Vulkan composite path.
    {
    // The scene is the only pass (no lock fade or lock-only pass).
    if render_scene {
            // ---- Build scene: scoped borrow_mut, dropped immediately. ----
            let scene = {
                let mut r = gles_renderer.borrow_mut();
                let prepared =
                    frames::scene::scene::prepare_kms(state, r.as_mut(), size, output_id.0);
                let _assembly = ledger::frame_trace::span("kms_scene_assembly", output_id.0);
                let scene =
                    frames::scene::scene::scene(state, r.as_mut(), size, prepared);
                drop(r);
                scene
            };

            diagnostics::iced_elements(
                &ctx_ref.outputs[output_idx].output.name(),
                &scene.Element,
            );
            let wrapped: Vec<GlesElementWrapper<_>> =
                scene.Element.iter().map(GlesElementWrapper).collect();

            // ---- render_frame: hold RefMut for the lifetime of scene_result. ----
            let mut r = gles_renderer.borrow_mut();
            let diag_frame = diagnostics::frame(output_idx);
            let frame_flags = if diag_frame.is_some() || screencopy::file::pending_output(&ctx_ref.outputs[output_idx].output) {
                frame_flags | smithay::backend::drm::compositor::FrameFlags::FORCE_PRESENT
            } else {
                frame_flags
            };
            let drm_trace = ledger::frame_trace::span("kms_drm_render_frame", output_id.0);
            let scene_result = ctx_ref
                .outputs[output_idx]
                .drm_output
                .as_mut()
                .unwrap()
                .render_frame(&mut *r, &wrapped, [0.0, 0.0, 0.0, 1.0], frame_flags)
                .unwrap();
            drop(drm_trace);
            let sync_trace = ledger::frame_trace::span("kms_render_sync", output_id.0);
            honor_needs_sync(&scene_result);
            drop(sync_trace);

            let scene_is_empty = scene_result.is_empty;

            if diagnostics::enabled() {
                use smithay::backend::drm::compositor::PrimaryPlaneElement;
                use smithay::backend::renderer::element::RenderElementPresentationState;
                use smithay::backend::renderer::Texture;
                for (order, element) in wrapped.iter().enumerate() {
                    let Some(texture) = diagnostics::iced_texture(element.0) else {
                        continue;
                    };
                    let id = element.id();
                    let (dump_id, first) = diagnostics::surface(id, output_idx);
                    if first {
                        let geometry = element.geometry(Scale::from(output_scale));
                        let render_state = scene_result.states.element_render_state(id.clone());
                        let plane = if scene_result.cursor_element.is_some_and(|e| e.id() == id) {
                            "cursor"
                        } else if scene_result.overlay_elements.iter().any(|e| e.id() == id) {
                            "overlay"
                        } else if matches!(&scene_result.primary_element, PrimaryPlaneElement::Element(e) if e.id() == id) {
                            "primary-scanout"
                        } else if render_state.is_some_and(|s| matches!(s.presentation_state, RenderElementPresentationState::Rendering { .. })) {
                            "primary-composited"
                        } else {
                            "unassigned"
                        };
                        info!(
                            "KMS_DIAG iced id={id:?} dump_id={dump_id} output={output_key} geometry_px=({},{},{}x{}) texture={}x{} order={order}/{} plane={plane} state={render_state:?}",
                            geometry.loc.x, geometry.loc.y, geometry.size.w, geometry.size.h,
                            texture.width(), texture.height(), wrapped.len(),
                        );
                    }
                    if let Some(frame) = diag_frame.as_ref() {
                        diagnostics::iced(r.as_mut(), texture, id, dump_id, frame);
                    }
                }
            }

            if let Some(frame) = diag_frame.as_ref() {
                // Same DRM primary-buffer + promoted-plane copy as screencopy,
                // independent of whether a client requested a capture.
                let copied = (|| -> Result<_, String> {
                    let mut texture = screencopy::offscreen_texture(r.as_mut(), size)?;
                    {
                        let mut target = r.bind(&mut texture)
                            .map_err(|err| format!("bind KMS diagnostic target: {err}"))?;
                        scene_result.blit_frame_result(
                            size, Transform::Normal, Scale::from(output_scale),
                            &mut *r, &mut target, [Rectangle::from_size(size)],
                            std::iter::empty::<Id>(),
                        ).map_err(|err| format!("copy KMS diagnostic frame: {err:?}"))?;
                    }
                    Ok(texture)
                })();
                match copied {
                    Ok(texture) => diagnostics::primary(r.as_mut(), &texture, frame),
                    Err(err) => warn!("KMS_DIAG primary frame={} output={output_key} error={err}", frame.number),
                }
                if frame.number < 3 {
                    state.state.redraw.request_gated(
                        protocols::redraw::schedule::schedule::RedrawReason::Rearm,
                    );
                }
            }

            // ---- Tap (post-scene): capture blit, inline with r held. ----
            // The safe pattern (carried from the original): extract everything
            // we need from scene_result BEFORE dropping r, perform the capture
            // INSIDE the same scope as r, then drop both together — a fresh
            // borrow_mut while scene_result is alive would alias.
            if tap_post_scene {
                if let Some(job) =
                    recorder::interface::render::window_render_job(state)
                {
                    // Window / world-region capture: render the captured windows
                    // directly into the entry (off-screen capable, chrome-free)
                    // with the GLES renderer that holds their buffers.
                    if let Some(mut dmabuf) = state
                        .inner.kernel.get(&world::driver::capture::base::CAPTURE_REGISTRY)
                        .as_ref()
                        .and_then(|reg| reg.entry_dmabuf(job.entry_id))
                    {
                        recorder::interface::render::draw_windows_into(
                            &mut *r,
                            &mut dmabuf,
                            job.size,
                            &job.windows,
                            job.scale,
                        );
                    }
                } else if let Some(registry) = &mut state.inner.kernel.get(&world::driver::capture::base::CAPTURE_REGISTRY) {
                    let entries = registry.entries_for_output(output_id);
                    let full_src = Rectangle::<i32, Physical>::from_loc_and_size((0, 0), size);

                    for (entry_id, mut entry_tex, entry_size, src_override) in entries {
                        // Region captures blit their sub-rect of the composed
                        // scene; full captures blit the whole framebuffer.
                        let src = src_override.unwrap_or(full_src);
                        let result: Result<(), _> = (|| {
                            let mut entry_fb = r.bind(&mut entry_tex).map_err(
                                graphics::capture::registry::registry::BlitErr::Bind,
                            )?;
                            scene_result
                                .blit_frame_result(
                                    entry_size,
                                    smithay::utils::Transform::Normal,
                                    Scale::from(output_scale),
                                    &mut *r,
                                    &mut entry_fb,
                                    [src],
                                    std::iter::empty::<Id>(),
                                )
                                .map(|_sync| ())
                                .map_err(
                                    graphics::capture::registry::registry::BlitErr::Blit,
                                )
                        })();
                        if let Err(e) = result {
                            warn!("capture blit failed: entry_id={entry_id:?} err={e:?}");
                        }
                    }
                }
            }

            // Bus capture copies the real KMS result, including promoted planes.
                // It never replays a potentially different scene on the active VT.
                let output = &ctx_ref.outputs[output_idx].output;
                if screencopy::file::pending_picture(output, true) {
                    let copied = (|| -> Result<(), String> {
                        let mut texture = screencopy::offscreen_texture(r.as_mut(), size)?;
                        let mut target = r
                            .bind(&mut texture)
                            .map_err(|err| format!("bind Bus capture: {err}"))?;
                        let sync = scene_result
                            .blit_frame_result(
                                size,
                                Transform::Normal,
                                Scale::from(output_scale),
                                &mut *r,
                                &mut target,
                                [Rectangle::from_size(size)],
                                std::iter::empty::<Id>(),
                            )
                            .map_err(|err| format!("copy KMS capture: {err:?}"))?;
                        sync.wait()
                            .map_err(|err| format!("wait KMS capture: {err:?}"))?;
                        screencopy::file::service(
                            r.as_mut(),
                            screencopy::Source {
                                framebuffer: target.as_ref(),
                                readback: screencopy::Readback {
                                    size,
                                    origin_bottom_left: false,
                                },
                            },
                            output,
                            screencopy::file::CaptureSource::Kms,
                            true,
                        );
                        Ok(())
                    })();
                    if let Err(err) = copied {
                        screencopy::file::fail_output(output, &err);
                    }
                }

                // ---- wlr-screencopy. ----
                // Read the frame KMS will receive: its primary buffer plus the
                // promoted planes. Replaying the scene is needed only when a
                // cursorless copy must remove a cursor baked into the primary.
                if screencopy::active() || screencopy::file::pending_picture(output, false) {
                    let output = ctx_ref.outputs[output_idx].output.clone();
                    let scale = output_scale;
                    let damage = screencopy::frame_damage(&output, size, scale, &scene.Element);
                    let mut due = screencopy::sources_due(&output, damage.as_deref());
                    due.cursorless |= screencopy::file::pending_picture(&output, false);
                    let cursor_ids: Vec<Id> = scene
                        .Element
                        .iter()
                        .filter(|element| element.is_cursor())
                        .map(|element| element.id().clone())
                        .collect();
                    let cursor_separate = cursor_ids.iter().all(|id| {
                    scene_result.cursor_element.is_some_and(|element| element.id() == id)
                        || scene_result.overlay_elements.iter().any(|element| element.id() == id)
                        || matches!(
                            &scene_result.primary_element,
                            smithay::backend::drm::compositor::PrimaryPlaneElement::Element(element)
                                if element.id() == id
                        )
                });
                    let mut render = |with_pointer: bool| {
                        let result = (|| -> Result<_, String> {
                            if !with_pointer && !cursor_separate {
                                return screencopy::render_offscreen(
                                    r.as_mut(),
                                    &scene.Element,
                                    |element| !element.is_cursor(),
                                    size,
                                    scale,
                                );
                            }
                            let mut texture = screencopy::offscreen_texture(r.as_mut(), size)?;
                            {
                                let mut target = r
                                    .bind(&mut texture)
                                    .map_err(|err| format!("bind KMS copy target: {err}"))?;
                                let sync = scene_result
                                    .blit_frame_result(
                                        size,
                                        Transform::Normal,
                                        Scale::from(scale),
                                        &mut *r,
                                        &mut target,
                                        [Rectangle::from_size(size)],
                                        cursor_ids.iter().filter(|_| !with_pointer).cloned(),
                                    )
                                    .map_err(|err| format!("copy KMS frame: {err:?}"))?;
                                sync.wait()
                                    .map_err(|err| format!("wait KMS frame: {err:?}"))?;
                            }
                            Ok(texture)
                        })();
                        result
                            .map_err(|err| warn!("screencopy: offscreen render failed ({err})"))
                            .ok()
                    };
                    let mut with_pointer = if due.cursor { render(true) } else { None };
                    let mut without_pointer = if due.cursorless { render(false) } else { None };
                    let gles: &mut smithay::backend::renderer::gles::GlesRenderer = r.as_mut();
                    let with_target = with_pointer.as_mut().and_then(|texture| {
                        gles.bind(texture)
                            .map_err(|err| warn!("screencopy: offscreen texture not bound ({err})"))
                            .ok()
                    });
                    let without_target = without_pointer.as_mut().and_then(|texture| {
                        gles.bind(texture)
                            .map_err(|err| warn!("screencopy: offscreen texture not bound ({err})"))
                            .ok()
                    });
                    let readback = screencopy::Readback {
                        size,
                        origin_bottom_left: false,
                    };
                    if let Some(framebuffer) = without_target.as_ref() {
                        screencopy::file::service(
                            gles,
                            screencopy::Source {
                                framebuffer,
                                readback,
                            },
                            &output,
                            screencopy::file::CaptureSource::Kms,
                            false,
                        );
                    } else if screencopy::file::pending_picture(&output, false) {
                        screencopy::file::fail_output_capture(
                            &output,
                            "cursorless KMS copy failed",
                        );
                    }
                    screencopy.push(screencopy::service(
                        gles,
                        with_target.as_ref().map(|framebuffer| screencopy::Source {
                            framebuffer,
                            readback,
                        }),
                        without_target
                            .as_ref()
                            .map(|framebuffer| screencopy::Source {
                                framebuffer,
                                readback,
                            }),
                        &output,
                        damage.as_deref(),
                    ));
                }
                drop(scene_result);
            drop(r);

            last_result_empty = scene_is_empty;
            visible_window = scene.visible_window;
        }

    }

    // All RefMut guards on the renderer have been dropped by this point.
    // ---- present THIS output: queue its page-flip (or send empty-frame callbacks).
        if !last_result_empty {
            if present(ctx_ref, state, visible_window, output_idx, frame_states.as_ref()) {
                any_queued = true;
            }
            // Damage arrived: any hold on parking is settled.
            ctx_ref.outputs[output_idx].settlement.reset();
            // No UNCONDITIONAL re-arm. Requesting the next frame after every
            // non-empty one would (with FORCE_PRESENT pinned on) keep the loop
            // flipping every vblank forever. Every continuation source asks for
            // its own frame: a silent request made during
            // this render is picked up by this pipe's vblank (`frame.rs` renders
            // iff `needs`), and producers that want a frame LATER arm a deadline
            // (below).
            //
            // A governing tearing/pacing section IS such a source: there the
            // loop is meant to free-run with a frame ready ahead of each flip
            // (`Config::preemptive`). So the re-arm stays exactly while
            // pre-emptive presenting is in force — never on an ordinary desktop.
            if preemptive {
                state.state.redraw.request_gated(
                    protocols::redraw::schedule::schedule::RedrawReason::Rearm,
                );
            }
        } else {
            let output = ctx_ref.outputs[output_idx].output.clone();
            let _skip = ledger::frame_trace::span("kms_skip_empty_damage", output_id.0);
            let key = world::state::state::output_key(&output);
            // Rendered, nothing to submit: still a frame the ledger accounts for.
            state.state.redraw.frame(&key, true);
            // The estimated next vblank of THIS pipe: no flip is coming to pace
            // anything that rides on this frame, so it stands in for one.
            let until_vblank = {
                let pipe = &ctx_ref.outputs[output_idx];
                let refresh = kms::scanout::timing::vblank::vblank::interval(&pipe.mode);
                pipe.last_vblank
                    .map(|anchor| {
                        kms::scanout::timing::vblank::vblank::until_next(
                            anchor,
                            std::time::Instant::now(),
                            refresh,
                        )
                    })
                    .unwrap_or(refresh)
                    // Never a zero pace: a vblank estimated "now" must not turn
                    // a paced wake back into an immediate one.
                    .max(std::time::Duration::from_millis(1))
            };
            pace = Some(pace.map_or(until_vblank, |p| p.min(until_vblank)));
            #[cfg(feature = "flip-estimate")]
            if ctx_ref.safety.estimate_pacing {
                // Estimate net active: hold the frame callbacks; `wire.frame`
                // delivers them at the estimated next vblank.
                frames::draw::present::callbacks::callbacks::housekeeping(state);
                deferred = Some(FrameOutcome::EmptyDeferred {
                    output: output.clone(),
                    visible: visible_window.clone(),
                });
            } else {
                owe_frames(state, &mut ctx_ref.outputs[output_idx], visible_window.clone(), until_vblank);
            }
            #[cfg(not(feature = "flip-estimate"))]
            owe_frames(state, &mut ctx_ref.outputs[output_idx], visible_window.clone(), until_vblank);
            // May this empty frame park the loop? Not while damage the
            // tracker cannot see yet is on its way; then ask for another frame at
            // the estimated vblank, bounded by the settlement cap.
            let holds = crate::render::park::park::Holds {
                dmabuf_import: !state.state.pending_dmabuf.is_empty(),
                resuming: !*state.inner.kernel.get(&drivers::resume::base::VBLANK_SEEN),
            };
            match ctx_ref.outputs[output_idx].settlement.observe(holds, std::time::Instant::now()) {
                crate::render::park::park::Verdict::Park => {}
                crate::render::park::park::Verdict::Hold => {
                    let handle = state.loop_handle.clone();
                    state.state.redraw.request_at(
                        &handle,
                        std::time::Instant::now() + until_vblank,
                        holds.reason(),
                        schedule_of,
                    );
                }
                crate::render::park::park::Verdict::Timeout => {
                    error!(
                        "native: empty frames held from parking for {:?} by {:?}; parking anyway",
                        crate::render::park::park::SETTLEMENT_CAP,
                        holds.names()
                    );
                }
            }
            // A request made during this render (a silent continuation) left the
            // pipe behind the epoch, and no vblank is coming to pick it up. Wake
            // for it at the estimated vblank, not now: a source that asks every
            // frame must not spin the loop on frames that come out empty.
            if state.state.redraw.needs(&key) {
                let handle = state.loop_handle.clone();
                state.state.redraw.wake_at(
                    &handle,
                    std::time::Instant::now() + until_vblank,
                    schedule_of,
                );
            }
        }
    } // ---- end per-output render loop ----

    // wlr-screencopy: every pipe's frame is queued; now map the readbacks and
    // answer the copies (mapping waits for the GPU, which must not delay a flip).
    if !screencopy.is_empty() {
        let mut r = gles_renderer.borrow_mut();
        let gles: &mut smithay::backend::renderer::gles::GlesRenderer = r.as_mut();
        for captures in screencopy.drain(..) {
            captures.finish(gles);
        }
    }

    // Drawing done → clear the render-output seam and release the shared GPU state.
    state.inner.render_output = None;
    drop(gles_renderer);
    drop(binding);
    drop(ctx);

    // Housekeeping runs *every* execute() call, damage or no.
    frames::draw::present::callbacks::callbacks::housekeeping(state);
    // Finding B: a wake held during the render. In-flight pipes are served by
    // their own vblank whatever happens here; this is about the IDLE pipes behind
    // the epoch. If this pass queued a flip, wake now — a sibling can act and the
    // flip paces the rest. If it queued nothing, every frame came out empty and
    // waking now would render them again at CPU rate: wake at the vblank instead.
    if state.state.redraw.end_render() && state.state.redraw.pending() {
        if any_queued {
            state.state.redraw.wake();
        } else {
            let handle = state.loop_handle.clone();
            let at = std::time::Instant::now() + pace.unwrap_or(std::time::Duration::from_millis(16));
            state.state.redraw.wake_at(&handle, at, schedule_of);
        }
    }
    // A producer that needs a frame at a later instant (an iced
    // animation's `RedrawRequest::At`) offered it during this frame; one timer
    // requests that frame when it comes, instead of a frame every vblank until then.
    if let Some(at) = graphics::bridge::publish::wake::wake::take_deadline() {
        let handle = state.loop_handle.clone();
        state.state.redraw.request_at(
            &handle,
            at,
            protocols::redraw::schedule::schedule::RedrawReason::Iced,
            schedule_of,
        );
    }
    // Capture's time-based keep-alive: its own deadline, its
    // own reason.
    if let Some(at) = graphics::bridge::publish::wake::wake::take_capture_deadline() {
        let handle = state.loop_handle.clone();
        state.state.redraw.request_at(
            &handle,
            at,
            protocols::redraw::schedule::schedule::RedrawReason::Capture,
            schedule_of,
        );
    }
    if any_queued {
        FrameOutcome::Queued
    } else {
        #[cfg(feature = "flip-estimate")]
        {
            deferred.unwrap_or(FrameOutcome::Idle)
        }
        #[cfg(not(feature = "flip-estimate"))]
        {
            FrameOutcome::Idle
        }
    }
}

/// Queue the rendered frame with presentation feedback and send frame
/// callbacks. (Ex scene.rs `refresh()`, recomposed from present.callbacks +
/// flip.queue.) Queue failure panics outside the session-resume window;
/// inside it the watchdog recovers and no frame callbacks are sent (the
/// original's abort shape). Returns whether a frame is in flight.
fn present(
    ctx_ref: &mut NativeRenderContext,
    state: &mut Loop,
    window_visible: Vec<smithay::desktop::Window>,
    output_idx: usize,
    states: Option<&smithay::backend::renderer::element::RenderElementStates>,
) -> bool {
    use kms::scanout::flip::queue::queue::{queue, QueueOutcome};
    let trace_output = if ledger::frame_trace::enabled() {
        OutputId::from_key(&world::state::state::output_key(&ctx_ref.outputs[output_idx].output)).0
    } else { 0 };

    // Resolve which policy section governs this frame, from what is actually on
    // screen. Recomputed every frame off the drawn set, so panning away from a
    // target — even a frozen one — restores normal scheduling by itself; there
    // is no latched state to get stuck in.
    let active = {
        use protocols::tearing::gate::gate;
        use protocols::tearing::liveness::liveness;
        use protocols::tearing::pacer::pacer;
        use model::environment::tearing::select::select::{Exclusivity, Scene};
        use smithay::wayland::seat::WaylandFocus;

        // Both halves of the tag live on the SURFACE, and both are gathered over the
        // whole surface TREE: Mesa attaches `wp_tearing_control` to the surface it
        // presents to, which for many native games is a subsurface under the toplevel
        // while the heuristic stamped the toplevel. `Verdict::is_target` then resolves
        // the two ONCE, at the window — the client's statement wherever it spoke, the
        // heuristic only for the silence.
        //
        // One tag for both sections: pacing is another configurable layer over the same
        // "does this window own the cadence" question, not a separate claim. What keeps
        // an explicit setting above a client is `Selector::Always`, which ignores the
        // tag entirely.
        //
        // Xwayland is not offered the protocol (`wire.tearing::can_view`), so the hint
        // is always absent for an X11 window and none here is ever second-hand.
        let tagged = |w: &smithay::desktop::Window| {
            use smithay::wayland::compositor::{with_surface_tree_downward, TraversalAction};
            let mut verdict = pacer::Verdict::default();
            if let Some(s) = w.wl_surface() {
                with_surface_tree_downward(
                    s.as_ref(),
                    (),
                    |_, _, _| TraversalAction::DoChildren(()),
                    |_, states, _| {
                        if let Some(tag) = states.data_map.get::<pacer::TearingTag>() {
                            verdict.absorb(tag);
                        }
                    },
                    |_, _, _| true,
                );
            }
            verdict.is_target()
        };
        let focus = state.state.seat.seat.get_keyboard().and_then(|kb| kb.current_focus());
        let scene = Scene {
            target_visible: window_visible.iter().any(tagged),
            target_focused: focus.as_ref().is_some_and(|f| {
                window_visible
                    .iter()
                    .filter(|w| tagged(w))
                    .any(|w| w.wl_surface().is_some_and(|s| s.as_ref() == f))
            }),
            any_focused: focus.is_some(),
            any_visible: !window_visible.is_empty(),
        };

        let cfg = model::environment::tearing::config::config::get();
        let active = outputs::tearing::resolve::resolve::active(&cfg, scene);

        // Stamp what the scene actually drew, for the gates that ask about
        // visibility. A stamp rather than a flag: the scene knows what it drew,
        // never what it didn't, so there is no moment at which every other
        // surface could be cleared.
        //
        // Skipped entirely for the gates that never read it — an ordinary
        // desktop (`None`) and `Focused` — since this is a `with_states` per
        // visible window per frame, and it would run for a stamp nothing looks
        // at. Resolved from THIS frame's exclusivity, which is known before the
        // gate is published, so the frame that first engages is already stamped.
        let frame = gate::advance_frame();
        let excl = active.exclusivity();
        if matches!(
            excl,
            Exclusivity::Exclusive | Exclusivity::ExclusiveFocused | Exclusivity::Visible
        ) {
            for w in &window_visible {
                if let Some(s) = w.wl_surface() {
                    smithay::wayland::compositor::with_states(s.as_ref(), |states| {
                        states.data_map.insert_if_missing(gate::VisibleSurface::default);
                        if let Some(v) = states.data_map.get::<gate::VisibleSurface>() {
                            v.stamp(frame);
                        }
                    });
                }
            }
        }

        // Translate the user-facing exclusivity into the gate the Wayland
        // dispatch enforces per commit. `Off` when the rule is not in force, so
        // the dispatch never has to know about policy, scenes or windows.
        let g = if excl.engaged(scene) {
            match excl {
                Exclusivity::None => gate::Gate::Off,
                Exclusivity::Exclusive => gate::Gate::Tagged,
                Exclusivity::ExclusiveFocused => gate::Gate::TaggedFocused,
                Exclusivity::Focused => gate::Gate::Focused,
                Exclusivity::Visible => gate::Gate::Visible,
            }
        } else {
            gate::Gate::Off
        };
        if gate::set(g) {
            info!("tearing: redraw gate = {g:?} ({excl:?}, scene={scene:?})");
            // The floor watchdog exists only to rescue a gated loop, so it lives
            // exactly as long as the gate does.
            if g == gate::Gate::Off {
                crate::wire::watchdog::watchdog::disarm(
                    &state.loop_handle,
                    &mut ctx_ref.watchdog,
                );
            } else {
                crate::wire::watchdog::watchdog::arm(
                    &state.loop_handle,
                    &mut ctx_ref.watchdog,
                );
            }
        }
        // Publish for the NEXT frame's plane assignment, and carry this frame's
        // rate ceiling forward for the next frame's cap gate.
        let refresh = kms::scanout::timing::vblank::vblank::interval(
            &ctx_ref.outputs[output_idx].mode,
        );
        ctx_ref.outputs[output_idx].cap_interval = active.min_interval(refresh);
        // The watchdog floor is a rate like any other, so it is resolved against
        // this output's refresh here — the layer that owns the timer knows
        // nothing about modes.
        protocols::tearing::floor::floor::set(cfg.floor_interval(refresh));
        // Published for the next frame's pre-emptive decision, beside the plane
        // one below and for the same reason — both are read before a scene exists.
        gate::set_governed(!matches!(
            active,
            outputs::tearing::resolve::resolve::Active::Default
        ));
        if gate::set_tearing(active.may_tear(refresh)) {
            info!(
                "tearing: planes {} for the next frame",
                if active.may_tear(refresh) { "OFF (composited)" } else { "ON (direct scanout)" }
            );
        }
        liveness::note_composite(&world::state::state::output_key(
            &ctx_ref.outputs[output_idx].output,
        ));
        active
    };

    let current_output = ctx_ref.outputs[output_idx].output.clone();
    let feedback = frames::draw::present::callbacks::callbacks::collect_feedback(
        &current_output,
        &window_visible,
        states,
    );

    // Per-frame tearing decision. The FrameFlags half (plane assignment on/off)
    // is a mode-level property owned by `plane.direct`; this is the flip half.
    // The two MUST agree: a promoted overlay or cursor plane combined with the
    // async flag is an illegal commit that the kernel rejects outright.
    let tear = {
        let now = std::time::Instant::now();
        let pipe = &mut ctx_ref.outputs[output_idx];
        let refresh = kms::scanout::timing::vblank::vblank::interval(&pipe.mode);
        // Time left in the current refresh interval, extrapolated from the last
        // anchored retrace. `None` until this pipe has flipped once, which
        // `tear_now` reads as "unknown timing → prefer the clean frame".
        let until_vblank = pipe.last_vblank.map(|anchor| {
            kms::scanout::timing::vblank::vblank::until_next(anchor, now, refresh)
        });
        // This frame is going out, so any armed cap wake-up is spent.
        pipe.cap_wake = None;
        // Gated on the SAME value that chose this frame's plane flags. They must
        // agree: an async flip on a frame that still has planes armed carries two
        // planes and the kernel rejects it. On the frame a target first appears
        // the flags are still from the previous resolution, so this yields one
        // ordinary vsync'd frame rather than a rejected commit.
        let tear = active.tear_now(until_vblank, refresh)
            && protocols::tearing::gate::gate::tearing();
        pipe.last_tear = tear;
        tear
    };

    let resuming = !(*state.inner.kernel.get(&drivers::resume::base::VBLANK_SEEN));
    // Scope the drm_output borrow so the `Failed` arm can tear the pipe down.
    let outcome = {
        let Some(drm_output) = ctx_ref.outputs[output_idx].drm_output.as_mut() else { return false };
        // Arm the flip mode for THIS commit. `queue_frame` submits synchronously
        // (the per-pipe `in_flight` guard keeps `pending_frame` empty), so the
        // set-then-queue ordering is race-free.
        drm_output.with_compositor(|c| c.set_tearing(tear));
        let trace = ledger::frame_trace::span("kms_queue_frame", trace_output);
        let outcome = queue(drm_output, Some(feedback), resuming);
        drop(trace);
        outcome
    };
    match outcome {
        QueueOutcome::Queued => {
            if ledger::frame_trace::enabled() {
                ctx_ref.outputs[output_idx].flip_trace = Some(ledger::frame_trace::wait_span(
                    "kms_submit_to_flip", trace_output,
                ));
            }
            // In flight: the render loop skips this pipe until its own vblank scans
            // the frame out, decoupling its cadence from the others' — and the
            // schedule wakes the loop for a request only while some pipe is idle.
            let key = world::state::state::output_key(&ctx_ref.outputs[output_idx].output);
            state.state.redraw.queued(&key);
            cancel_owed_frames(state, &mut ctx_ref.outputs[output_idx]);
            // A submitted frame: the ledger attributes it to what was pending.
            state.state.redraw.frame(&key, false);
        }
        QueueOutcome::DeferredToWatchdog => {
            let _skip = ledger::frame_trace::span("kms_defer_watchdog", trace_output);
            // Rendered but not submitted: same invariant break as the capture
            // pre-render, so discard the age accounting rather than let it skew.
            // (`Failed` below needs nothing — it drops the whole `drm_output`, and
            // the swapchain goes with it.)
            discard_unsubmitted_render(&ctx_ref.outputs[output_idx]);
            // No frame callbacks for this frame; the watchdog re-kicks.
            return false;
        }
        QueueOutcome::Failed => {
            let _skip = ledger::frame_trace::span("kms_skip_queue_failed", trace_output);
            // Fail-soft: this connector's flip failed → drop its scanout target so
            // the render loop skips it (it goes dark) while other outputs keep
            // running. Recovered on the next hotplug reconcile.
            ctx_ref.outputs[output_idx].drm_output = None;
            return false;
        }
    }

    // compd (integration batch D): this frame was handed to the display.
    world::comp::presentation::frame_queued(state, &current_output, &window_visible);
    frames::draw::present::callbacks::callbacks::send_window_frames(
        state,
        &current_output,
        &window_visible,
    );
    frames::draw::present::callbacks::callbacks::send_layer_frames(state, &current_output);
    frames::draw::present::cursor::cursor::send_frames(state, &current_output);
    true
}
