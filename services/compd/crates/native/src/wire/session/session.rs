//! Session pause/resume wiring: registers the seat notifier source and maps
//! its events through the seat lifecycle protocols onto the hosted pipe.
//! (Ex wire.rs `start()` session closure — the only crate besides the other
//! wire.* siblings that names `Loop`, Law 4.)
//!
//! Completion-pass addition: held modifiers are cleared on PAUSE (the same
//! VT-switch stuck-modifier problem the winit path fixed; the keys-up events
//! are consumed by the other VT, so the compositor must forget them).

use crate::context::render::render::NativeRenderContext;
use smithay::backend::session::libseat::LibSeatSessionNotifier;
use smithay::reexports::calloop::EventLoop;
use smithay::utils::{Logical, Point};
use std::cell::RefCell;
use std::rc::Rc;
use world::state::state::StatusSession;
use world::state::Loop;
use world::environment::interface::lifecycle::lifecycle;

pub fn register(
    event_loop: &mut EventLoop<'static, Loop>,
    session_notifier: LibSeatSessionNotifier,
    ctx_rc: Rc<RefCell<NativeRenderContext>>,
    render_kick: impl Fn(Rc<RefCell<NativeRenderContext>>, &mut Loop) + Clone + 'static,
) {
    let session_context = ctx_rc;
    let session_loop_handle = event_loop.handle();

    event_loop
        .handle()
        .insert_source(session_notifier, move |event, _, state| {
            let mut ctx = session_context.borrow_mut();
            let ctx_ref = &mut *ctx;

            match event {
                smithay::backend::session::Event::PauseSession => {
                    state.inner.status_session = StatusSession::Paused;
                    state.inner.comp.hardware.note(world::comp::hardware::Kind::Paused, None);
                    world::comp::scenes::pointer_motion(state, None);
                    info!("Session paused (TTY switch away)");
                    // Drain copies already waiting behind a flip that can no
                    // longer complete, through the capture-only render path.
                    state.state.redraw.wake();

                    // Seat de-activating — stop and discard any active capture.
                    recorder::interface::interface::stop_and_discard(state);

                    // The keys-up for anything held at switch time will be
                    // consumed by the other VT — forget held modifiers now.
                    seat::modifier::clear::clear::clear_held_modifiers(state);

                    // Release every live pipe's CRTC/planes NOW, while the device is
                    // still active. Once `pause()` below flips it inactive, smithay's
                    // surface `Drop` skips its disabling commit
                    // (`AtomicDrmSurface::drop` early-returns on `!active`), so any pipe
                    // torn down from here on leaves its CRTC configured in the kernel.
                    // This is the last point a modeset can still be committed; the
                    // resume path re-enables via `queue_frame`.
                    for p in ctx_ref.outputs.iter() {
                        if let Some(o) = p.drm_output.as_ref() {
                            if let Err(e) =
                                kms::scanout::surface::output::output::clear(o)
                            {
                                warn!(
                                    "pause: clearing connector={:?} crtc={:?} failed: {e} — its \
                                     CRTC stays configured. EACCES/EPERM here means DRM master was \
                                     already revoked before the pause event reached us, in which \
                                     case this release point is too late to be effective.",
                                    p.connector, p.crtc
                                );
                            }
                        }
                    }

                    // Pause protocol: display first, then input (seat.lifecycle).
                    let manager = ctx_ref.drm_output_manager.clone();
                    let libinput = &mut ctx_ref.libinput_context;
                    kms::seat::lifecycle::pause::pause::pause(
                        || {
                            kms::scanout::surface::output::output::pause(
                                &mut manager.borrow_mut(),
                            )
                        },
                        || libinput.suspend(),
                    );

                    *state.inner.kernel.get_mut(&drivers::resume::base::VBLANK_SEEN_MUT) = false;

                    if let Some(token) = state.inner.kernel.get_mut(&drivers::resume::base::RESUME_WATCHDOG_MUT).take() {
                        state.loop_handle.remove(token);
                    }
                }
                smithay::backend::session::Event::ActivateSession => {
                    info!("Session activated");
                    state.inner.status_session = StatusSession::Active;
                    state.inner.comp.hardware.note(world::comp::hardware::Kind::Active, None);
                    crate::render::execute::diagnostics::activated();

                    // Resume protocol (seat.lifecycle): input, activate
                    // (forced reclaiming modeset), surface reset, buffer
                    // reset, remap. Step failures are the self-recovering
                    // class — the watchdog drives recovery.
                    {
                        let manager = ctx_ref.drm_output_manager.clone();
                        let space = &mut state.inner.space_state_mut().state;
                        // Remap EVERY live output back at its current global-space
                        // position (multi-output: not just the primary — a secondary
                        // monitor would otherwise stay unmapped/dark after a VT switch).
                        // Its existing geometry loc is the layout to restore.
                        let remap_list: Vec<(smithay::output::Output, Point<i32, Logical>)> = ctx_ref
                            .outputs
                            .iter()
                            .map(|p| {
                                let loc = space
                                    .output_geometry(&p.output)
                                    .map(|g| g.loc)
                                    .unwrap_or_else(|| Point::from((0, 0)));
                                (p.output.clone(), loc)
                            })
                            .collect();
                        let libinput = &mut ctx_ref.libinput_context;
                        // `Option` (per pipe) because a monitor switch briefly tears an
                        // output down before rebuilding. That can't overlap this session
                        // callback (calloop runs sources serially), but the resume path
                        // is the self-recovering class — skip a missing surface rather
                        // than panic. Direct `outputs` field access (not `pipe_mut()`)
                        // so this borrows only `outputs`, leaving the other `ctx_ref`
                        // fields the resume closures capture (libinput, …) borrowable.
                        let pipes = &mut ctx_ref.outputs;

                        kms::seat::lifecycle::resume::resume::resume(
                            kms::seat::lifecycle::resume::resume::ResumeSteps {
                                resume_input: || {
                                    libinput
                                        .resume()
                                        .map_err(|e| format!("libinput resume failed: {e:?}"))
                                },
                                activate_display: |force| {
                                    kms::scanout::surface::output::output::activate(
                                        &mut manager.borrow_mut(),
                                        force,
                                    )
                                },
                                // Reset EVERY live pipe's surface, not just the primary.
                                // (Their in-flight bookkeeping is cleared below, once
                                // the whole `Loop` is reachable again.)
                                reset_surface: || {
                                    let mut result = Ok(());
                                    for p in pipes.iter_mut() {
                                        // Whatever ran on the other VT may have
                                        // reprogrammed this connector's colorimetry
                                        // properties. HDR/PQ + BT.2020 is signalled ONCE
                                        // per pipe and deliberately never retried
                                        // (`render.execute`), and the flag is otherwise
                                        // only cleared when a pipe is BUILT — so without
                                        // this the panel keeps the other compositor's
                                        // colorspace and the whole framebuffer reads
                                        // wrong (heavy red cast) until a mode change
                                        // rebuilds the pipe.
                                        p.props_applied = false;
                                        if let Some(o) = p.drm_output.as_mut() {
                                            if let Err(e) = kms::scanout::surface::output::output::reset(o) {
                                                result = Err(e);
                                            }
                                        }
                                    }
                                    result
                                },
                                reset_buffers: || {},
                                remap_output: || {
                                    for (output, loc) in &remap_list {
                                        space.map_output(output, *loc);
                                    }
                                },
                            },
                        );
                    }
                    // The refresh the remap above needs, run out here where the whole
                    // `Loop` is reachable again (the closure holds only `space`). NOT
                    // `Space::refresh()`: its middle third re-derives `wl_output`
                    // membership from the window's Space position, which here is a
                    // WORLD coordinate — it would send every client a spurious
                    // leave/enter on each VT switch back, stalling the ones that render
                    // off frame callbacks. Done here rather than left to the next
                    // frame's `housekeeping` so a resume that stalls before it renders
                    // still leaves the space consistent.
                    state.inner.refresh_space();
                    // Any flip queued before the switch will never deliver a vblank:
                    // forget every pipe's flight so none is wedged out of the render
                    // loop. The schedule is incremental, so this is a plain reset —
                    // the next render re-creates each pipe as it reports.
                    state.state.redraw.clear();
                    drop(ctx);

                    // Reconcile the driven outputs against the live connector set.
                    // Drained here, onto a loop timer: the drain in `execute` is
                    // vblank-driven, and a display that came back dark never reaches
                    // it. Safety net — it re-syncs OUR view, it does not undo kernel
                    // state left behind by a teardown that happened while inactive.
                    *state.inner.kernel.get_mut(
                        &drivers::output::base::OUTPUT_RECONCILE_REQUEST_MUT,
                    ) = true;
                    crate::context::display::reconcile::reconcile::drain_reconcile(
                        state,
                        &session_context,
                    );

                    // Arm the watchdog: kick a full render every frame until a
                    // REAL vblank (flag set by wire.frame) arrives, then drop
                    // itself. Registration failure panics inside arm().
                    *state.inner.kernel.get_mut(&drivers::resume::base::VBLANK_SEEN_MUT) = false;
                    if let Some(token) = state.inner.kernel.get_mut(&drivers::resume::base::RESUME_WATCHDOG_MUT).take() {
                        state.loop_handle.remove(token);
                    }

                    let ctx = session_context.clone();
                    let kick = render_kick.clone();
                    let token = kms::seat::lifecycle::resume::resume::watchdog::arm(
                        &session_loop_handle,
                        |state: &mut Loop| (*state.inner.kernel.get(&drivers::resume::base::VBLANK_SEEN)),
                        |state: &mut Loop| {
                            *state.inner.kernel.get_mut(&drivers::resume::base::RESUME_WATCHDOG_MUT) = None;
                        },
                        move |state: &mut Loop| kick(ctx.clone(), state),
                    );
                    *state.inner.kernel.get_mut(&drivers::resume::base::RESUME_WATCHDOG_MUT) = Some(token);

                    // And the settle deadline on top: the resume watchdog above
                    // retires on the FIRST real vblank of any pipe; this waits for
                    // each live pipe's own (`watchdog.settle`).
                    {
                        let keys = crate::wire::frame::frame::live_keys(&session_context.borrow());
                        crate::wire::watchdog::settle::settle::arm(
                            &session_loop_handle,
                            &mut state.state.redraw,
                            keys,
                        );
                    }

                    // Take back the SHARED session environment, last, because it forks.
                    //
                    // `dbus-update-activation-environment` writes a per-USER environment
                    // that every compositor this user has on every VT shares, so whichever session
                    // pushed most recently owns `WAYLAND_DISPLAY` and `DISPLAY` for
                    // D-Bus- and systemd-activated launches. Re-pushing on activation
                    // makes that the session the user is actually looking at, which is the
                    // only reading of a per-user variable that can be right. It is also
                    // what makes deferring safe elsewhere: `push_session_env_if_active`
                    // drops a publish from a background session precisely because this
                    // will redo the whole set when that session comes forward.
                    //
                    // Values come from the compositor's own state, never read back from the session —
                    // following whatever last trampled these is the bug being fixed. Both
                    // are per-process and unambiguous: `loader.socket_name` is the socket
                    // this process created, `display::get()` the number its own Xwayland
                    // reported. Directly-spawned launches were never affected; they read
                    // the same two through `executor.install::base_env`.
                    let wayland_display = state.inner.loader.socket_name.to_string_lossy().into_owned();
                    let x_display = x11_wm::display::display::get()
                        .unwrap_or_default();
                    if let Err(err) = lifecycle::push_session_env(&[
                        ("WAYLAND_DISPLAY", wayland_display.as_str()),
                        ("DISPLAY", x_display.as_str()),
                    ]) {
                        warn!("could not re-publish the session environment on activation: {err:?}");
                    } else {
                        info!(
                            "session activated: republished WAYLAND_DISPLAY={wayland_display:?} DISPLAY={x_display:?}"
                        );
                    }
                }
            }
        })
        .unwrap();
}
