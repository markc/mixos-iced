//! Native XWayland: the X server and the X11 window manager that drives it.
//!
//! Two connections, two event sources (see smithay's `xwayland` module). The
//! Xwayland process is an ordinary WAYLAND client of ours — its surfaces arrive
//! through the same compositor path as anyone else's — and separately we act as its
//! X11 window manager over a private socket. The first is what `XWayland::spawn`
//! gives us here; the second is `X11Wm::start_wm`, which can only be started once
//! the server signals ready.
//!
//! The window manager's sources go on a NESTED calloop loop whose data is
//! `Dispatch`, not `Loop`. That is forced rather than chosen: smithay's association
//! hook (`XWaylandShellState::new::<D>`) is a wayland PRE-COMMIT hook, so its `D` is
//! the wayland dispatch state — which makes `Dispatch: XwmHandler` the only possible
//! shape, and every `XwmHandler` callback therefore takes `&mut Dispatch`. Nesting
//! is cheap: calloop's `EventLoop` is itself pollable, so the outer loop watches its
//! fd and pumps it with a zero timeout, then drains the outboxes the callbacks
//! filled — exactly what the wayland source above it does.

use std::os::fd::AsFd;
use std::process::Stdio;
use std::time::Duration;

use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{EventLoop, Interest, LoopHandle, Mode, PostAction};
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::xwayland::{X11Wm, XWayland, XWaylandEvent};
use model::{error, info, warn};
use world::state::Loop;
use world::state::state::StatusSession;
use world::environment::interface::lifecycle::lifecycle;
use dispatcher::state::state::Dispatch;
use x11_wm::display::display;
use protocols::window::find::find::Shell;
use dispatcher::state::deferred::deferred::Deferred;
use smithay::reexports::wayland_server::Resource;
use smithay::xwayland::X11Surface;
use dispatcher::wire::trait_::wire_trait::WireTrait;

/// Spawn the X server and wire both of its sources into `event_loop`.
///
/// Failure is never fatal: a session without an X server is a session where X11 apps
/// do not run, which is strictly better than one that will not start.
///
/// Gated on the `xwayland.enabled` switch (`COMPD_XWAYLAND` > the
/// persisted per-socket file > on), read once here; a props set takes effect
/// at the next start. The switch gates the FIRST generation only: the one retry
/// is a recovery of a server this session already chose to run.
pub fn register(display_handle: &DisplayHandle, event_loop: &mut EventLoop<Loop>) {
    let switch = policy_host::xwayland::startup();
    if let Some(note) = switch.note {
        warn!("xwayland.enabled: {note}");
    }
    info!(
        "xwayland.enabled = {} (env {}, file {})",
        switch.enabled,
        policy_host::xwayland::ENV,
        switch.path.display()
    );
    if !switch.enabled {
        return;
    }
    start(display_handle.clone(), event_loop.handle());
}

/// One X server generation: the nested window-manager loop, its pump, and the
/// server itself. Called for the first generation by [`register`] and for the one
/// retry by [`after_failure`]. Any setup
/// failure is a failure of the generation and goes the same way as a crash.
fn start<'l>(dh: DisplayHandle, handle: LoopHandle<'l, Loop>) {
    policy_host::xwayland::note_starting();
    let mut xwm_loop = match EventLoop::<'static, Dispatch>::try_new() {
        Ok(loop_) => loop_,
        Err(err) => {
            error!("XWayland: could not create the X11 event loop: {err:?}");
            after_failure(&dh, &handle);
            return;
        }
    };
    let xwm_handle = xwm_loop.handle();

    // Pump the nested loop whenever it has something to deliver. `Mode::Level` so a
    // dispatch that leaves work behind is re-entered rather than dropped, and a zero
    // timeout so this never blocks the outer loop.
    //
    // Dispatch only — the `XwmHandler` callbacks queue onto the SAME outboxes the
    // wayland handlers use, and the outer loop drains them once per iteration after
    // every source has run. Draining here as well would make the order in which a
    // frame's X11 and wayland events are applied depend on which fd calloop polled
    // first, which is exactly the coupling the shared outboxes exist to remove.
    let pump = match xwm_loop.as_fd().try_clone_to_owned() {
        Ok(fd) => fd,
        Err(err) => {
            error!("XWayland: could not duplicate the X11 loop fd: {err:?}");
            after_failure(&dh, &handle);
            return;
        }
    };
    let (pump_dh, pump_handle) = (dh.clone(), handle.clone());
    let inserted = handle.insert_source(
        Generic::new(pump, Interest::READ, Mode::Level),
        move |_, _, state: &mut Loop| {
            let dispatched = xwm_loop.dispatch(Some(Duration::ZERO), &mut state.state);
            state.state.protocol_pending = true;

            // Did the server just die? Three signals, because none alone is certain: a
            // dispatch error is what a broken X connection surfaces as, the wayland
            // client vanishing is what a killed process always produces, and
            // `disconnected` is what an ORDERLY exit produces — smithay's X11 source
            // reports a closed connection as `Ok`, calls `XwmHandler::disconnected` and
            // removes itself, so a SIGTERM'd server fired neither of the other two and
            // left a dead `X11Wm`, ghost windows and an exported `DISPLAY` behind.
            //
            // Acting on it is not optional. The pump is `Mode::Level`, so a permanently
            // errored nested loop stays readable and would be re-dispatched forever —
            // a busy loop at 100% CPU rather than a degraded session.
            let client_gone = state
                .state
                .xwayland
                .client
                .as_ref()
                .is_some_and(|id| {
                    state
                        .state
                        .output
                        .display_handle
                        .backend_handle()
                        .get_client_data(id.clone())
                        .is_err()
                });
            if let Err(err) = &dispatched {
                warn!("X11 event loop dispatch failed: {err:?}");
            }
            let disconnected = state.state.xwayland.disconnected;
            if state.state.xwayland.xwm.is_some() && (dispatched.is_err() || client_gone || disconnected) {
                died(state);
                after_failure(&pump_dh, &pump_handle);
                // Removing drops this closure, and with it the nested loop and every
                // source `X11Wm::start_wm` registered on it.
                return Ok(PostAction::Remove);
            }
            Ok(PostAction::Continue)
        },
    );
    let pump_token = match inserted {
        Ok(token) => token,
        Err(err) => {
            error!("XWayland: could not watch the X11 event loop: {err:?}");
            after_failure(&dh, &handle);
            return;
        }
    };

    // The per-client scale is deliberately LEFT AT 1: `set_client_scale` is not called
    // on the Xwayland client, so X11 windows are handed unscaled logical pixels and
    // the camera does the rest. X has no per-window scale to publish and DPI-unaware
    // X clients resize themselves when a global one changes, which is exactly the
    // shrink/grow the satellite deployment had to pin down with `--force-scale 1
    // --ignore-fractional-scale`. Not calling it is the native spelling of those two
    // flags, and it is the default rather than an opt-out.
    //
    // `open_abstract_socket = true`: containerised and statically-linked X clients
    // (and anything using the classic `@/tmp/.X11-unix/X0` address) only ever try the
    // abstract socket, so leaving it closed makes them look like there is no X server.
    let spawned = XWayland::spawn(
        &dh,
        None,
        std::iter::empty::<(String, String)>(),
        // One extra argument, and it is a REMOVAL. Everything else smithay passes
        // (`-rootless`, `-terminate`, the wm/displayfd/listenfd wiring) is the whole
        // contract, and the opt-in features — `-enable-ei-portal`, `-shm`, `-glamor`,
        // `-dpi`, `-force-xrandr-emulation` — compd does not use. `-dpi` in particular
        // would undo the scale decision above by the other route: it changes the
        // resolution TOOLKITS read to size themselves, so X clients would lay out
        // larger in pixels and the camera would scale that again.
        //
        // RECORD off. The extension hands any X client a copy of the core protocol
        // stream for every other client on the display, device events included — so
        // one X app can read the keystrokes going to another. Wayland clients are out
        // of reach either way, and Xwayland only sees input while an X surface holds
        // focus, but "X11 apps cannot see each other's input" is the same explicit-
        // opt-in shape the wayland protocols here are held to, and RECORD is the one
        // channel of it that costs nothing to close: nothing in compd speaks it (our
        // x11rb is built without the `record` feature), and what breaks is keystroke
        // visualisers, `xtrace` and macro recorders.
        //
        // XTEST off for the same reason, from the other direction. RECORD is the READ
        // half of one capability and XTEST is the WRITE half: it lets an X client
        // synthesize key presses, buttons and motion into other X clients. Both are
        // confined to the X domain — Xwayland's fake input never reaches compd's pointer
        // or a wayland client, which is what `-enable-ei-portal` would change — so
        // removing them closes X-to-X snooping and X-to-X injection and nothing else.
        //
        // The usual objection is password-manager auto-type and `xdotool`. It does not
        // apply here: those are wayland tools on this desktop, and X11 support exists
        // for legacy applications rather than for driving the session.
        //
        // NOT a boundary, and it must not be read as one: XI2 raw events on the root
        // window remain a global input channel that no command line closes. This is
        // defence in depth.
        //
        // Neither removal touches ordinary input. Real events reach X clients through
        // the input devices Xwayland builds from the wayland seat, which is a separate
        // path from the XTEST virtual devices.
        [
            "-extension".to_string(),
            "RECORD".to_string(),
            "-extension".to_string(),
            "XTEST".to_string(),
        ],
        true,
        Stdio::null(),
        Stdio::null(),
        |_| (),
    );
    let (xwayland, client) = match spawned {
        Ok(pair) => pair,
        Err(err) => {
            error!("XWayland: could not spawn the X server: {err:?}");
            handle.remove(pump_token);
            after_failure(&dh, &handle);
            return;
        }
    };

    // Record which client is Xwayland NOW, not at `Ready`.
    //
    // Global visibility filters (`can_view` for `wp_tearing_control_v1` and
    // `wp_fractional_scale_v1`) are consulted when a client asks for the registry.
    // Xwayland connects and does that as soon as it starts; `Ready` fires only once the
    // X SERVER is up, which is far later. Recorded there, every filter would answer
    // "not Xwayland" for the one client it exists to exclude, and the globals would be
    // advertised and bound before the answer ever became correct.
    //
    // Safe here: `spawn` has inserted the client, and its registry request cannot be
    // dispatched until this function returns to the event loop.
    if protocols::xwm::base::set_filter_client(client.id()) {
        info!("global filters now name the retried XWayland client");
    }

    let (source_dh, source_handle) = (dh.clone(), handle.clone());
    let source = handle
        .insert_source(xwayland, move |event, _, state: &mut Loop| match event {
            XWaylandEvent::Ready { x11_socket, display_number } => {
                // E1: EWMH virtual desktops (`_NET_NUMBER_OF_DESKTOPS`,
                // `_NET_CURRENT_DESKTOP`, `_NET_WM_DESKTOP`).
                let config = smithay::xwayland::XwmConfig::default().with_ewmh_virtual_desktops(true);
                match X11Wm::start_wm_with_config(xwm_handle.clone(), &source_dh, x11_socket, client.clone(), config) {
                    Ok(wm) => {
                        state.state.xwayland.xwm = Some(wm);
                        // E7: a new server knows nothing compd mirrored into the
                        // last one; the next pass writes the desktops, every window
                        // and the stack afresh.
                        policy_host::x11::forget();
                        policy_host::xwayland::note_ready();
                        state.state.xwayland.client = Some(client.id());
                        // Only now is there an X server to point anything at: record the
                        // display for the launch executor's per-launch environment, and
                        // push it into the session so D-Bus- and systemd-activated
                        // services inherit it too.
                        display::set(display_number);
                        let value = format!(":{display_number}");
                        unsafe { std::env::set_var("DISPLAY", &value) };
                        // Shared env only while we are the session on screen; see
                        // `push_session_env_if_active`. Our own launches read
                        // `display::get()` either way.
                        lifecycle::push_session_env_if_active(
                            publishes_shared_env(state),
                            &[("DISPLAY", &value)],
                        );
                        // The per-socket descriptor (`compd/<socket>.xwayland.env`,
                        // DISPLAY + GENERATION, 0600): what launchers read to hand `DISPLAY`
                        // to an X client without any global being set.
                        if let Err(err) = policy_host::xwayland::publish_descriptor(display_number) {
                            warn!("XWayland DISPLAY descriptor not published: {err}");
                        }
                        info!("XWayland ready on :{display_number}");
                    }
                    // A server with no window manager is a failed generation, as
                    // a startup crash is: nothing would ever count it, and
                    // `xwayland.state` would read `starting` for the rest of the
                    // session. Its server is cut off (Xwayland exits with its
                    // wayland connection) so the retry is the only one running.
                    Err(err) => {
                        display::clear();
                        error!("XWayland X11 window manager failed to start: {err:?}");
                        source_dh.backend_handle().kill_client(
                            client.id(),
                            smithay::reexports::wayland_server::backend::DisconnectReason::ConnectionClosed,
                        );
                        source_handle.remove(pump_token);
                        after_failure(&source_dh, &source_handle);
                    }
                }
            }
            XWaylandEvent::Error => {
                display::clear();
                error!("XWayland crashed on startup");
                // The pump never saw a window manager, so it would never retire
                // itself; this generation's nested loop goes with it.
                source_handle.remove(pump_token);
                after_failure(&source_dh, &source_handle);
            }
        });
    if let Err(err) = source {
        error!("XWayland: could not watch the X server: {err:?}");
        handle.remove(pump_token);
        after_failure(&dh, &handle);
    }
}

/// A generation failed (crashed, exited, or never came up). The one retry
/// credit decides what follows: the first failure arms ONE restart after
/// `XWAYLAND_RETRY_DELAY_SECS` (a one-shot backstop, not a poll), every later one
/// leaves X11 down until compd restarts. Either way the Bus shows it:
/// `xwayland.display` null, `xwayland.state` `retrying` / `failed`, and
/// `xwayland.failures` counted.
fn after_failure<'l>(dh: &DisplayHandle, handle: &LoopHandle<'l, Loop>) {
    use policy::x11::{XWAYLAND_RETRY_DELAY_SECS, XwaylandRetryDecision};
    let decision = policy_host::xwayland::note_failure();
    let failures = policy_host::xwayland::failures();
    match decision {
        XwaylandRetryDecision::Retry => {
            error!("XWayland failed ({failures} this session); restarting it once in {XWAYLAND_RETRY_DELAY_SECS} s");
            let (retry_dh, retry_handle) = (dh.clone(), handle.clone());
            let armed = handle.insert_source(
                Timer::from_duration(Duration::from_secs(XWAYLAND_RETRY_DELAY_SECS)),
                move |_, _, _: &mut Loop| {
                    start(retry_dh.clone(), retry_handle.clone());
                    TimeoutAction::Drop
                },
            );
            if let Err(err) = armed {
                error!("XWayland: could not arm the restart: {err}; X11 stays down until compd restarts");
                policy_host::xwayland::note_gave_up();
            }
        }
        XwaylandRetryDecision::StayFailed => {
            error!("XWayland failed again ({failures} this session); X11 stays down until compd restarts");
        }
    }
}

/// May this session write the per-USER systemd and D-Bus activation environments?
///
/// Two conditions, and they exclude different things. NESTED never publishes: a dev
/// session under winit shares the host's bus and user manager (identically so inside a
/// container), so a publish repoints the host at a compositor that is not its session —
/// the same rule `main.rs` applies to `announce_session`. PAUSED does not publish because
/// the environment is per-user rather than per-session: a background session's X server
/// dying must not blank `DISPLAY` for the session actually on screen, whose own server is
/// fine. Deferring is safe because `wire.session` republishes the WHOLE set on activation.
fn publishes_shared_env(state: &Loop) -> bool {
    let nested = state
        .inner
        .kernel
        .try_get(&world::storage::state::state::NESTED)
        .copied()
        .unwrap_or(false);
    !nested && matches!(state.inner.status_session, StatusSession::Active)
}

/// The X server is gone. Retire everything that only made sense while it was up.
///
/// Called once per generation, from the pump, the moment the connection or the client
/// goes away. Nothing else will do it: smithay's `XWayland` source reports only a
/// STARTUP failure (it disables itself after `Ready`), so without this the session keeps
/// a dead `X11Wm` and every X11 path fails silently — configures, focus, stacking, the
/// clipboard — while the windows themselves sit on the canvas forever.
///
/// Does not respawn: the pump hands the failure to [`after_failure`], which spends
/// the single retry credit. A crash-looping X server is worse than
/// none, so the second death stays dead.
fn died(state: &mut Loop) {
    error!("XWayland exited; retiring its windows and X11 state");

    // The windows first, through the ORDINARY destroyed path. Queuing them rather than
    // reaping them here is what gets each one its `Destroyed` lifecycle event, and so
    // its placeholder: `refresh_alive` would eventually drop the dead elements from the
    // Space, but silently, leaving nothing behind for the user to reopen.
    let gone: Vec<X11Surface> = state
        .inner
        .all_world_spaces()
        .iter()
        .flat_map(|space| space.state.elements())
        .filter_map(|window| window.x11_surface().cloned())
        .collect();
    // The WITHDRAWN ones too. They already left the Space and already left a placeholder,
    // so they are not in the walk above and need no second teardown — but their record
    // claims they could still be mapped again, and with the server gone they cannot. It
    // is cleared wholesale rather than per window, since every entry belongs to the one
    // server that just died.
    let withdrawn = state.inner.withdrawn_x11.len();
    state.inner.withdrawn_x11.clear();
    let count = gone.len();
    for surface in gone {
        state.state.deferred.push(Deferred::WindowDestroyed {
            window: Shell::X11(surface),
            drag_discard: false,
        });
    }
    // Reached from the pump rather than from a dispatch, so mark by hand: the X server
    // dying is exactly the case where no further protocol traffic is coming to arm it.
    state.state.protocol_pending = true;
    // Dropping the `X11Wm` is what retires the POPUPS. They are not Space elements — an
    // X11 popup has no uuid and no slot — so the walk above cannot see one; but
    // `Drop for X11Wm` calls `handle_destroyed` on every window it holds, which makes
    // `X11Popup::alive` false and lets `PopupManager::cleanup` reap them next frame.
    state.state.xwayland.xwm = None;
    state.state.xwayland.client = None;
    // The global-visibility mirror (`xwayland_base::set_filter_client`) is NOT cleared,
    // and that is deliberate rather than an omission beside the two fields below. It
    // holds a `ClientId`, whose serial is a generation counter — the slot this client
    // occupied yields an unequal id when reused, so the stale entry matches no live
    // client and `is_xwayland` already answers `false` for everyone; the retried server
    // (E7) replaces it.
    state.state.xwayland.disconnected = false;
    // Bare X11 window IDs, and the only X11 state here that nothing else invalidates:
    // every `X11Surface` is now dead and every `wl_surface` gone, but these are `u32`s.
    // They resolve to nothing while no X server exists; they matter for the retried
    // server (E7), because X window ids are per-server and REUSED, and a stale one would
    // anchor a menu to an unrelated window.
    state.state.xwayland.map_position_parent_hover = None;
    state.state.xwayland.map_position_parent_focus = None;
    // A clipboard offer whose owner cannot answer. `send_selection` retires a stale one
    // on the first failed paste, but only if somebody pastes; doing it here means the
    // handover happens when the owner goes, not when someone next tries to use it.
    //
    // AFTER dropping the `X11Wm` above, and that ordering is the point:
    // `retire_x11_selection` re-claims the X selection for the persisted clipboard when
    // a window manager is still there, and here there is no X server left to claim it
    // from. What survives is the wayland-visible half — the capture taken when the X
    // client copied is promoted to the offer, so a copy made in an X app before the
    // server died still pastes into wayland apps afterwards.
    state.state.retire_x11_selection();
    // No focus index to clear: the surface→X11 mapping lives on each wl_surface and
    // `xwayland_focus::indexed` refuses a dead `X11Surface`, which every one of them
    // now is.

    // `DISPLAY` back to empty. The two routes are NOT symmetric and must not be made
    // so: the process-local pair is unconditional, because it is this session's own
    // state and its launches read it (`executor.install::base_env` calls
    // `display::get()`), while the shared publish below is conditional.
    display::clear();
    unsafe { std::env::set_var("DISPLAY", "") };
    // The descriptor names a server that is gone, so it goes with the
    // generation; the retry republishes it with the next GENERATION.
    if let Err(err) = policy_host::xwayland::remove_descriptor() {
        warn!("XWayland DISPLAY descriptor not removed: {err}");
    }
    // Retracted from the SHARED environment only if we are the active session. A
    // background session's X server dying must not blank `DISPLAY` for the session the
    // user is actually using, whose own server is fine — this is the destructive
    // direction of the per-user clobber, and the one worth being careful about.
    lifecycle::push_session_env_if_active(publishes_shared_env(state), &[("DISPLAY", "")]);
    info!("XWayland teardown complete; {count} X11 window(s) retired, {withdrawn} withdrawn record(s) cleared");
}
