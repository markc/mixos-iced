//! Frame pacing wiring: the redraw ping source, the DRM vblank source, and
//! the idle kickstart. (Ex wire.rs `start()` — ping, vblank closure, idle.)
//!
//! The Law-7 timing nets wire in here, each under its DOUBLE gate (cargo
//! feature compiles the mechanism in; the live `ctx.safety` enable activates
//! it):
//! - `timing-throttle`: re-time vblanks buggy drivers deliver early;
//! - `flip-estimate`:   deliver frame callbacks for empty-damage frames at
//!                      the estimated next vblank instead of immediately;
//! - `timing-predict`:  refine that estimate with a presentation clock
//!                      (implies `flip-estimate`).

use crate::context::render::render::NativeRenderContext;
use crate::render::execute::execute::FrameOutcome;
use smithay::backend::drm::DrmDeviceNotifier;
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::calloop::ping::make_ping;
use std::cell::RefCell;
use std::os::fd::AsFd;
use std::rc::Rc;
use std::time::Duration;
use world::state::Loop;
use world::state::state::StatusSession;

#[cfg(feature = "flip-estimate")]
type EstimateSlot = Rc<RefCell<Option<smithay::reexports::calloop::RegistrationToken>>>;
#[cfg(feature = "timing-predict")]
type PredictClock = Rc<RefCell<kms::scanout::timing::predict::predict::PresentationClock>>;

pub fn register(
    event_loop: &mut EventLoop<'static, Loop>,
    state: &mut Loop,
    drm_notifier: DrmDeviceNotifier,
    ctx_rc: Rc<RefCell<NativeRenderContext>>,
) {
    let refresh = kms::scanout::timing::vblank::vblank::interval(&ctx_rc.borrow().pipe().mode);

    #[cfg(feature = "flip-estimate")]
    let estimate_slot: EstimateSlot = Rc::new(RefCell::new(None));
    #[cfg(feature = "timing-predict")]
    let predict_clock: PredictClock = Rc::new(RefCell::new(
        kms::scanout::timing::predict::predict::PresentationClock::new(refresh),
    ));

    // ---- Redraw ping: fired by schedule_redraw while the vblank cycle is idle.
    let (redraw_ping, redraw_ping_source) = make_ping().unwrap();
    state.state.redraw.set_ping(redraw_ping.clone());
    // The off-thread background wakes the compositor through this: an undamaged
    // frame queues no flip, so no vblank arrives and the loop stops. While the
    // background is the only thing animating, its publish is the only event that
    // can restart it.
    //
    // Not while a redraw gate is engaged: under exclusivity the tagged client is
    // the sole cadence source, and a producer's publish must not wake the loop —
    // the wake would cash in whatever the latch held and render off-cadence. The
    // `published` flag itself is still set (`notify_offthread_published`), so the
    // next commit-driven composite samples the buffer, and the flag survives
    // until the gate lifts and the loop free-runs again.
    graphics::bridge::publish::wake::wake::set_offthread_waker(std::sync::Arc::new(move || {
        if !protocols::tearing::gate::gate::engaged() {
            redraw_ping.ping();
        }
    }));

    // The stall rescue: a deadline that exists only while work is
    // outstanding on a live pipe — a request it has not rendered, or a flip it is
    // still waiting on — and acts only if no flip at all happens before it. See
    // `wire.watchdog/watchdog.idle`.
    let ctx_rescue = ctx_rc.clone();
    let ctx_outstanding = ctx_rc.clone();
    let rescue = crate::wire::watchdog::idle::idle::IdleRescue::new(
        move |key: &str, in_flight: bool| {
            let Ok(mut ctx) = ctx_rescue.try_borrow_mut() else {
                return;
            };
            for pipe in ctx.outputs.iter_mut() {
                if world::state::state::output_key(&pipe.output) == key {
                    if let Some(o) = pipe.drm_output.as_mut() {
                        if in_flight {
                            // A lost completion leaves smithay holding the flip as
                            // pending, and while it does `queue_frame` submits
                            // nothing. After 2 s the flip almost certainly happened
                            // and only its event was lost, so complete it the way a
                            // vblank would: the pending frame becomes current (its
                            // buffer is the one on screen and must not be reused),
                            // the old current is released. Its presentation
                            // feedback is discarded.
                            if let Err(e) = o.with_compositor(|c| c.frame_submitted()) {
                                warn!("native: dropping the lost flip on {key} failed: {e}");
                            }
                        }
                        o.reset_buffers();
                    }
                }
            }
        },
        move |state: &Loop| {
            if *state.inner.kernel.get(&drivers::lid::base::DISPLAY_OFF) {
                return Vec::new();
            }
            let Ok(ctx) = ctx_outstanding.try_borrow() else {
                return Vec::new();
            };
            live_keys(&ctx)
                .into_iter()
                .filter(|key| state.state.redraw.in_flight(key) || state.state.redraw.needs(key))
                .collect()
        },
    );

    let context_ping = ctx_rc.clone();
    let loop_handle_ping = event_loop.handle();
    let rescue_ping = rescue.clone();
    #[cfg(feature = "flip-estimate")]
    let estimate_ping = estimate_slot.clone();
    #[cfg(feature = "timing-predict")]
    let predict_ping = predict_clock.clone();
    event_loop
        .handle()
        .insert_source(redraw_ping_source, move |_, _, state| {
            // We were pinged because something called schedule_redraw while
            // the VBlank cycle was idle. Run the executor to restart the cycle.
            // The background publishing counts as needing a redraw: it is a
            // producer the damage tracker cannot see until we sample it.
            //
            // GATED like `schedule_redraw_post_vblank`, and for the same reason:
            // under exclusive pacing the tagged client is the sole continuation
            // source, and the background sustaining the loop is precisely what
            // that gate exists to stop. Short-circuited, so the flag survives the
            // gate and the publish is not lost when exclusivity lifts.
            let published = !protocols::tearing::gate::gate::engaged()
                && graphics::bridge::publish::wake::wake::take_offthread_published();
            // A publish is a redraw request with no commit behind it, so nothing
            // has moved the epoch: mark the pipes stale or the executor's
            // epoch-current skip would (correctly) find nothing to do.
            if published {
                state.state.redraw.request_silent_for(
                    protocols::redraw::schedule::schedule::RedrawReason::Background,
                );
            }
            // No output live: nothing renders, but the control plane must still
            // move. Pumped here, per request, rather than by a repeating dark tick.
            let dark = context_ping
                .try_borrow()
                .is_ok_and(|ctx| ctx.outputs.iter().all(|p| p.drm_output.is_none()));
            if dark {
                world::pump::dark::dark::pump(state);
            }
            // Render iff some pipe is idle and behind the epoch. A wake is a stale
            // signal — it only says a request happened since the last drain — so
            // the schedule decides; in-flight pipes are served by their own vblank.
            // A VT switch can leave the old flip in flight. Capture while
            // paused bypasses that schedule: no vblank will release the pipe.
            // Window captures also bypass it on the active VT: their offscreen
            // target needs no free scanout pipe.
            if state.state.redraw.pending()
                || screencopy::file::pending_windows()
                || matches!(state.inner.status_session, StatusSession::Paused)
                || *state.inner.kernel.get(&drivers::lid::base::DISPLAY_OFF)
            {
                let outcome = crate::render::execute::execute::execute(
                    context_ping.clone(),
                    loop_handle_ping.clone(),
                    state,
                    crate::render::execute::execute::RenderScope::All,
                );
                handle_outcome(
                    outcome,
                    &loop_handle_ping,
                    refresh,
                    #[cfg(feature = "flip-estimate")]
                    &estimate_ping,
                    #[cfg(feature = "timing-predict")]
                    &predict_ping,
                    #[cfg(feature = "timing-predict")]
                    state.inner.start_time.elapsed(),
                );
            }
            rescue_ping.observe(&loop_handle_ping, state);
        })
        .unwrap();

    // ---- VBlank: decode -> (throttle gate) -> interpret -> feedback ->
    //      conditional render.
    let context_drm = ctx_rc.clone();
    let loop_handle_vblank = event_loop.handle();
    let rescue_vblank = rescue.clone();
    #[cfg(feature = "flip-estimate")]
    let estimate_vblank = estimate_slot.clone();
    #[cfg(feature = "timing-predict")]
    let predict_vblank = predict_clock.clone();
    #[cfg(feature = "timing-throttle")]
    let throttle = Rc::new(RefCell::new(
        kms::scanout::timing::throttle::throttle::VblankThrottle::new(),
    ));
    event_loop
        .handle()
        .insert_source(drm_notifier, move |event, event_meta, state| {
            use kms::loop_::notifier::notifier::{DecodedDrmEvent, decode};

            match decode(event, event_meta) {
                DecodedDrmEvent::Error(error) => {
                    // The hosted compositor surfaces device errors here; the
                    // session lifecycle owns pause/resume, so an error outside
                    // it is not self-recovering.
                    abort!("DRM device error: {error}");
                }
                DecodedDrmEvent::VBlank {
                    pipe: crtc,
                    time,
                    sequence,
                } => {
                    // Measure event arrival, before pause/throttle gates and
                    // before rendering the next frame. CPU during this wait is
                    // unrelated event-loop work, so it is recorded as zero.
                    if let Some(pipe) = context_drm
                        .borrow_mut()
                        .outputs
                        .iter_mut()
                        .find(|p| p.crtc == crtc)
                        && let Some(span) = pipe.flip_trace.take()
                    {
                        span.finish();
                    }
                    if let StatusSession::Paused = state.inner.status_session {
                        return;
                    }

                    // A vblank event is a completed page
                    // flip, i.e. a frame on the glass. READY=1 on the first one; a
                    // no-op afterwards and outside systemd.
                    if slots::sdnotify::ready_once("first frame presented (kms)") {
                        info!("sd_notify: READY=1 after the first page flip");
                    }

                    // Law-7 throttle gate: buggy-driver early vblanks are
                    // re-timed; the deferred delivery re-enters process_vblank.
                    #[cfg(feature = "timing-throttle")]
                    if context_drm.borrow().safety.vblank_throttle {
                        let stamp_now = state.inner.start_time.elapsed();
                        let ctx_for_deliver = context_drm.clone();
                        let handle_for_deliver = loop_handle_vblank.clone();
                        #[cfg(feature = "flip-estimate")]
                        let est_for_deliver = estimate_vblank.clone();
                        #[cfg(feature = "timing-predict")]
                        let pred_for_deliver = predict_vblank.clone();
                        let rescue_for_deliver = rescue_vblank.clone();
                        let deferred = throttle.borrow_mut().throttle(
                            &loop_handle_vblank,
                            refresh,
                            time.unwrap_or(stamp_now),
                            move |state: &mut Loop| {
                                process_vblank(
                                    &ctx_for_deliver,
                                    &handle_for_deliver,
                                    state,
                                    time,
                                    sequence,
                                    crtc,
                                    refresh,
                                    &rescue_for_deliver,
                                    #[cfg(feature = "flip-estimate")]
                                    &est_for_deliver,
                                    #[cfg(feature = "timing-predict")]
                                    &pred_for_deliver,
                                );
                            },
                        );
                        if deferred {
                            return;
                        }
                    }

                    process_vblank(
                        &context_drm,
                        &loop_handle_vblank,
                        state,
                        time,
                        sequence,
                        crtc,
                        refresh,
                        &rescue_vblank,
                        #[cfg(feature = "flip-estimate")]
                        &estimate_vblank,
                        #[cfg(feature = "timing-predict")]
                        &predict_vblank,
                    );
                }
            }
        })
        .unwrap();

    // ---- Kickstart the very first frame to initiate the cycle.
    let context_init = ctx_rc;
    // The exclusive-pacing floor watchdog is NOT registered here: it is armed on
    // the transition into gate engagement and dropped on the way out, by
    // `wire.watchdog`. See that crate for why.
    //
    // The post-activation settle deadline does belong here, as a safeguard: the
    // kickstart below is a single idle render. It waits for each live pipe's own
    // first flip and retires (`watchdog.settle`).
    {
        let keys = live_keys(&context_init.borrow());
        let handle = state.loop_handle.clone();
        crate::wire::watchdog::settle::settle::arm(&handle, &mut state.state.redraw, keys);
    }

    let loop_handle_init = event_loop.handle();
    #[cfg(feature = "flip-estimate")]
    let estimate_init = estimate_slot;
    #[cfg(feature = "timing-predict")]
    let predict_init = predict_clock;
    let rescue_init = rescue;
    event_loop.handle().insert_idle(move |state| {
        let outcome = crate::render::execute::execute::execute(
            context_init,
            loop_handle_init.clone(),
            state,
            crate::render::execute::execute::RenderScope::All,
        );
        handle_outcome(
            outcome,
            &loop_handle_init,
            refresh,
            #[cfg(feature = "flip-estimate")]
            &estimate_init,
            #[cfg(feature = "timing-predict")]
            &predict_init,
            #[cfg(feature = "timing-predict")]
            state.inner.start_time.elapsed(),
        );
        rescue_init.observe(&loop_handle_init, state);
    });
}

/// The output keys of the pipes that are scanning out.
pub fn live_keys(ctx: &NativeRenderContext) -> Vec<String> {
    ctx.outputs
        .iter()
        .filter(|p| p.drm_output.is_some())
        .map(|p| world::state::state::output_key(&p.output))
        .collect()
}

/// One vblank: feedback for the completed frame, predict-clock update,
/// pending-estimate disarm (a real vblank supersedes the estimate), and the
/// conditional re-render.
#[allow(clippy::too_many_arguments)]
fn process_vblank(
    ctx_rc: &Rc<RefCell<NativeRenderContext>>,
    loop_handle: &LoopHandle<'static, Loop>,
    state: &mut Loop,
    time: Option<Duration>,
    sequence: u64,
    crtc: smithay::reexports::drm::control::crtc::Handle,
    refresh: Duration,
    rescue: &crate::wire::watchdog::idle::idle::IdleRescue,
    #[cfg(feature = "flip-estimate")] estimate_slot: &EstimateSlot,
    #[cfg(feature = "timing-predict")] predict_clock: &PredictClock,
) {
    process_vblank_inner(
        ctx_rc,
        loop_handle,
        state,
        time,
        sequence,
        crtc,
        refresh,
        #[cfg(feature = "flip-estimate")]
        estimate_slot,
        #[cfg(feature = "timing-predict")]
        predict_clock,
    );
    // Whatever the vblank did, re-read what is still owed: a completed flip with
    // nothing behind it drops the stall deadline.
    rescue.observe(loop_handle, state);
}

#[allow(clippy::too_many_arguments)]
fn process_vblank_inner(
    ctx_rc: &Rc<RefCell<NativeRenderContext>>,
    loop_handle: &LoopHandle<'static, Loop>,
    state: &mut Loop,
    time: Option<Duration>,
    sequence: u64,
    crtc: smithay::reexports::drm::control::crtc::Handle,
    refresh: Duration,
    #[cfg(feature = "flip-estimate")] estimate_slot: &EstimateSlot,
    #[cfg(feature = "timing-predict")] predict_clock: &PredictClock,
) {
    *state
        .inner
        .kernel
        .get_mut(&drivers::resume::base::VBLANK_SEEN_MUT) = true;

    let mut ctx = ctx_rc.borrow_mut();

    // Route the VBlank to the pipe whose CRTC flipped. If NO pipe matches, this is a
    // LATE flip completion from a CRTC whose pipe was just pruned (a monitor
    // deactivate / hotplug removed the pipe and freed its CRTC while a flip was still
    // queued on it). There is nothing to account it against — DROP it. Never fall
    // back to `outputs[0]`: clearing the primary's `in_flight` and popping its
    // feedback for someone else's flip corrupts the primary's flip state, causing a
    // double-queue that fails the primary's scanout and tears it down (both-black).
    let Some(idx) = ctx.outputs.iter().position(|p| p.crtc == crtc) else {
        return;
    };

    // This pipe's flip completed → it is no longer in flight. The re-render below
    // (if it lags the epoch) will now redraw THIS output; other pipes still in
    // flight stay skipped until their own vblank, so each output paces to its
    // own refresh.
    let key = world::state::state::output_key(&ctx.outputs[idx].output);
    // A real flip: completes the flight AND counts as this pipe's progress
    // (`Schedule::flips`, read by the stall rescue and the settle deadline).
    state.state.redraw.vblank(&key);
    // Round-1 finding G: the connector property pass (colorimetry + max bpc)
    // runs in the executor only once a vblank has been seen, and on a static
    // screen nothing else would ask for that frame — so inherited BT.2020/PQ
    // could persist. Owe this pipe exactly one frame for it; bounded, since the
    // pass marks `props_applied` whether it succeeds or not.
    // Per pipe: a global silent request would leave idle
    // sibling pipes behind the epoch with nothing to wake them.
    if !ctx.outputs[idx].props_applied {
        state.state.redraw.request_pipe_silent(
            &key,
            protocols::redraw::schedule::schedule::RedrawReason::Output,
        );
    }
    // Phase reference for the tearing policy's "time until the next vblank".
    //
    // Anchored to the retrace the kernel timestamped, NOT to when we observed
    // the event: an async (tearing) flip completes mid-scanout, so its event
    // arrival is not a vblank at all, and anchoring on it would corrupt the
    // phase. `anchor` recovers the true instant by measuring our dispatch delay
    // against CLOCK_MONOTONIC — the clock DRM stamps events with, and the one
    // `Instant` reads, which is why the two can be related at all. Note this
    // must NOT use `start_time.elapsed()`: that is a since-launch clock, a
    // different epoch entirely.
    ctx.outputs[idx].last_vblank = Some(kms::scanout::timing::vblank::vblank::anchor(
        std::time::Instant::now(),
        time,
        kms::scanout::timing::vblank::vblank::monotonic_now(),
    ));

    // 1. Pop presentation feedback for the frame that just hit screen. No output
    //    during a monitor-switch teardown window → nothing to pop.
    let pending_feedback = match ctx.outputs[idx].drm_output.as_mut() {
        Some(o) => kms::scanout::flip::feedback::feedback::pop(o),
        None => None,
    };

    // Per-output present rate for the FPS overlay: count only real page-flip
    // completions on THIS pipe (a dropped frame — a vblank with no new buffer —
    // doesn't increment), keyed by output. The overlay samples the delta.
    if matches!(pending_feedback, Some(Some(_))) {
        let key = world::state::state::output_key(&ctx.outputs[idx].output);
        model::stats::registry::base::present(&key);
    }

    // A torn frame has no predictable next refresh — it was applied mid-scanout
    // rather than at a retrace — so report `Unknown` rather than the panel's
    // fixed interval, which would be a wrong prediction rather than a missing one.
    let tore = ctx.outputs[idx].last_tear;
    let refresh_rate = if tore {
        smithay::wayland::presentation::Refresh::Unknown
    } else {
        kms::scanout::timing::vblank::vblank::refresh_interval(&ctx.outputs[idx].mode)
    };
    // Per-output refresh interval — the pacing (empty-frame estimate delay) must
    // use the interval of the output that ACTUALLY flipped, not the global primary
    // `refresh`. Otherwise a high-refresh output is paced at a slower neighbour's
    // rate. `refresh` (the primary's, from register()) is retained only for the
    // throttle gate above, which is feature-gated off in the shipping build.
    let this_refresh = kms::scanout::timing::vblank::vblank::interval(&ctx.outputs[idx].mode);
    // MSC for presentation feedback. The page-flip event's own sequence is the cheap
    // source and is used whenever it carries one; a driver that leaves it 0 is repaired
    // from the CRTC here, while `ctx` still holds the device fd. See
    // `scanout.timing/timing.sequence` for why 0 is the tell and what it costs clients.
    let sequence = kms::scanout::timing::sequence::sequence::resolve(
        ctx.drm_fd.as_fd(),
        crtc.into(),
        sequence,
    );
    // compd (integration batch D): the output whose queued frame this vblank completes.
    let presented_output = pending_feedback
        .as_ref()
        .map(|_| ctx.outputs[idx].output.clone());
    drop(ctx);

    let stamp = kms::scanout::timing::vblank::vblank::interpret(
        time,
        sequence,
        state.inner.start_time.elapsed(),
    );

    #[cfg(feature = "timing-predict")]
    predict_clock.borrow_mut().presented(stamp.time);

    // A real vblank supersedes any pending estimated delivery.
    #[cfg(feature = "flip-estimate")]
    if let Some(token) = estimate_slot.borrow_mut().take() {
        kms::scanout::flip::estimate::estimate::disarm(loop_handle, token);
    }

    // 2. Fire presentation callbacks for that completed frame.
    if let Some(Some(mut feedback)) = pending_feedback {
        kms::scanout::flip::feedback::feedback::presented(
            &mut feedback,
            stamp.time,
            refresh_rate,
            stamp.sequence,
            frames::draw::present::callbacks::callbacks::hw_flip_kind(tore),
        );
    }
    // compd (integration batch D): that frame reached the screen.
    if let Some(output) = presented_output {
        world::comp::presentation::presented(
            state,
            &output,
            stamp.time,
            (!tore).then_some(this_refresh),
            frames::draw::present::callbacks::callbacks::hw_flip_kind(tore).bits(),
            stamp.sequence,
        );
    }

    // 3. If anything has requested a redraw since THIS pipe last rendered, render
    //    now — but ONLY this output (the one that flipped). Other outputs are
    //    re-rendered on their OWN vblanks, so a fast monitor is never paced by a
    //    slow one.
    //
    // Per pipe, via the schedule: this output renders iff it lags the request
    // epoch. (A single global latch here once let this output's vblank swallow a
    // redraw another output was still waiting for, and it did not repaint until
    // some unrelated caller re-armed it.)
    let was_needed = state.state.redraw.needs(&key);
    if was_needed {
        let outcome = crate::render::execute::execute::execute(
            ctx_rc.clone(),
            loop_handle.clone(),
            state,
            crate::render::execute::execute::RenderScope::Crtc(crtc),
        );
        handle_outcome(
            outcome,
            loop_handle,
            this_refresh,
            #[cfg(feature = "flip-estimate")]
            estimate_slot,
            #[cfg(feature = "timing-predict")]
            predict_clock,
            #[cfg(feature = "timing-predict")]
            stamp.time,
        );
    }
}

/// Act on the executor's outcome. Without the `flip-estimate` net this is a
/// no-op (Queued/Idle carry no pacing obligation).
#[allow(unused_variables)]
fn handle_outcome(
    outcome: FrameOutcome,
    loop_handle: &LoopHandle<'static, Loop>,
    refresh: Duration,
    #[cfg(feature = "flip-estimate")] estimate_slot: &EstimateSlot,
    #[cfg(feature = "timing-predict")] predict_clock: &PredictClock,
    #[cfg(feature = "timing-predict")] now: Duration,
) {
    match outcome {
        FrameOutcome::Queued | FrameOutcome::Idle => {}
        #[cfg(feature = "flip-estimate")]
        FrameOutcome::EmptyDeferred { output, visible } => {
            // Delay: predicted next presentation when the predict net is in,
            // one refresh interval otherwise.
            #[cfg(feature = "timing-predict")]
            let delay = predict_clock
                .borrow()
                .next_presentation(now)
                .saturating_sub(now);
            #[cfg(not(feature = "timing-predict"))]
            let delay = refresh;

            let mut slot = estimate_slot.borrow_mut();
            if let Some(token) = slot.take() {
                kms::scanout::flip::estimate::estimate::disarm(loop_handle, token);
            }
            let token = kms::scanout::flip::estimate::estimate::arm(
                loop_handle,
                delay,
                move |state: &mut Loop| {
                    frames::draw::present::callbacks::callbacks::send_window_frames(
                        state, &output, &visible,
                    );
                    frames::draw::present::callbacks::callbacks::send_layer_frames(state, &output);
                    frames::draw::present::cursor::cursor::send_frames(state, &output);
                },
            );
            *slot = Some(token);
        }
    }
}
