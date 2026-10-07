// SPDX-License-Identifier: MIT OR Apache-2.0
//! MixOS Term — the lightweight frontend: the shared native application host
//! backend (D2/D3), drawing `term-core`'s grid through native tiny-skia
//! bands by default, or one persistent wgpu texture per visible pane (D7).
//!
//! Tabs and split panes at parity with bterm (T3): the same tab and pane
//! model (`term_core::tabs`), the same chords, and the same `term.*`
//! verbs — this binary registers the Bus name `term` and serves the core's
//! surface through `term_core::bus`, so a `term.pane.split` from
//! another node and a Ctrl+Shift+E at the keyboard produce the same tree.
//! Runtime font sizing is foot's (T4): Ctrl +/-/0 and Ctrl+wheel, keeping the
//! window and changing the cell count.
//!
//! The two things that are requirements rather than optimisations, because
//! they are what the whole lane is for: the grid is re-rasterised **by damaged
//! row**, and it is rasterised **into persistent storage per visible pane**
//! (four-row CPU bands or a whole-pane wgpu buffer). See `frame.rs` and
//! `term_core::raster::render_into`.

mod clipboard;
mod frame;
mod ime;
mod input;
mod layout;
#[cfg(test)]
mod native_tests;
mod presentation;
mod settings_bus;
mod strings;
mod theme;

#[cfg(feature = "wgpu")]
mod wgpu_grid;

#[cfg(all(feature = "tiny-skia", not(feature = "wgpu")))]
mod cpu_grid;

#[cfg(not(any(feature = "wgpu", feature = "tiny-skia")))]
compile_error!("term needs a renderer: enable the `tiny-skia` (default) or `wgpu` feature");

use ::bus::native_client::ConnState;
use application::Element;
use application::iced::widget::{Row, column, container, row, space};
use application::iced::{Length, Size, Subscription, Task};
use application::presentation::native::Ui;
use frame::Painter;
use input::Action;
use layout::{Node, Shape};
use presentation::{Content, LocalContext, PtyExtent, RasterKey};
use settings_bus::{Describe, Handle};
use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use term_core::{
    config as core_config,
    font::FontSize,
    native_lane::NativeLane,
    panes::{Geometry, SplitDir},
    session_fd,
    tabs::{self, CompletionNote, Removed, TabSet},
    version::version_request,
    wake::WakeFd,
};

const DISPLAY_NAME: &str = "MixOS Term";
/// The Bus name and verb namespace this frontend owns (D1). The Bevy frontend
/// is `bterm` / `bterm.*`, so both can run at once — which T5's A/B needs.
const SERVICE: &str = "term";

fn main() {
    // FIRST, before the inherited-fd quarantine, the config read, the Wayland
    // check and the window: Mark's contract (2026-09-21) is that `--version`
    // reports the version and the build hash and does nothing else, whether or
    // not a term is already running and whether or not there is a display.
    // `build_info!()` expands HERE so the sha is this crate's, not the core's.
    if let Some(text) = version_request(
        &std::env::args().collect::<Vec<_>>(),
        buildinfo::build_info!(),
    ) {
        println!("{text}");
        return;
    }
    session_fd::quarantine_inherited();
    if std::env::args().any(|arg| arg == "--help") {
        println!(
            "{DISPLAY_NAME}: tabbed Wayland Mix terminal (iced frontend)\n\
             Font: TERM_SPIKE_FONT=/path/to/font.ttf, TERM_FONT_PX=<6..48>\n\
             Keys: Ctrl+Shift+T/W new/close tab, Ctrl+PageUp/PageDown change tab,\n\
             \x20     Ctrl+Shift+E/O split side by side/stacked, Ctrl+Shift+X close pane,\n\
             \x20     Ctrl+Shift+arrows move focus, Ctrl+Shift+Q quit,\n\
             \x20     Tab / Shift+Tab switch split panes; Ctrl+Tab / Ctrl+Shift+Tab also cycle,\n\
             \x20     Right Shift+Left/Right previous / next tab (wrap),\n\
             \x20     Ctrl+Shift+C copy; Ctrl+Shift+V or Shift+Insert paste clipboard\n\
             \x20     Drag selects; double/triple click word/line; middle click pastes primary\n\
             \x20     Shift+mouse overrides application mouse reporting\n\
             \x20     Ctrl+plus/equal/minus/0 (and Ctrl+wheel) font size\n\
             \x20     Wheel scrolls; Shift forces history; Shift+PageUp/PageDown page history\n\
             \x20     Shift+Home/End history top/bottom (primary screen only)\n\
             \x20     Input returns to bottom; single-pane Tab or Ctrl+I goes to the shell\n\
             TERM_NOTIFY=0: no desktop notification when a pane's shell exits\n\
             --version: print version and build hash, and nothing else\n\
             --print-config: print resolved startup settings and exit\n\
             Bus: serves `{SERVICE}` / `{SERVICE}.*`; the Bevy frontend is `bterm`\n\
             Native lane: target-bound list/session/tabs/panes/snapshot/type, execute,\n\
             exec.result/cancel, task.submit/result/cancel, operation, props.get/set.\n\
             Requires local noded >= 0.16.8 native ingress (MIXOS_RUN=/run/mixos,\n\
             socket directory 0755); unavailable ingress leaves graphics working."
        );
        return;
    }
    let settings = resolve_config(
        core_config::load(Some(
            &::config::path(::config::Dir::Etc).join("term.conf.mix"),
        )),
        std::env::var("TERM_FONT_PX").ok().as_deref(),
        core_config::selected_term(),
    );
    if std::env::args().any(|arg| arg == "--print-config") {
        println!(
            "{}",
            serde_json::to_string_pretty(&settings).expect("validated config")
        );
        return;
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("term requires a native Wayland session");
        std::process::exit(1);
    }
    if let Err(error) = run(settings) {
        eprintln!("term: {error}");
        std::process::exit(1);
    }
}

fn resolve_config(
    mut config: core_config::Config,
    env_font: Option<&str>,
    term: &'static str,
) -> core_config::Settings {
    if let Some(px) = env_font
        .and_then(|value| value.parse().ok())
        .filter(|px| core_config::valid_font(*px))
    {
        config.font_px = px;
    }
    core_config::Settings { config, term }
}

fn run(settings: core_config::Settings) -> Result<(), String> {
    // Scale 1.0 to start; the raster is rebuilt at the surface's real
    // fractional scale on the first `Rescaled`, so glyphs are rasterised at
    // physical resolution and the compositor never upscales them.
    let painter = Painter::new(
        1.0,
        FontSize::new(settings.config.font_px),
        settings.config.cursor,
    )?;
    let local = LocalContext::new(1.0, settings.config.cursor)?;
    let baseline = settings.config.font_px;
    let bootstrap_raster = painter.bootstrap_raster();
    let tabs = Arc::new(Mutex::new(TabSet::starting(settings)));
    let (cleanup, reaper) = tabs::Cleanup::start().map_err(|e| format!("cleanup worker: {e}"))?;

    // One eventfd for the whole frontend: every PTY, resize, pane exit and
    // Bus mutation (including the native lane) coalesces onto it. The UI
    // subscription learns of all of them in one poll, without doing RPC work.
    let waker = Arc::new(Waker {
        fd: WakeFd::new().map_err(|e| format!("wake descriptor: {e}"))?,
        pending: AtomicBool::new(false),
        sender: Mutex::new(None),
        polling: AtomicBool::new(false),
    });
    tabs.lock().expect("tabs").set_wake(waker.fd.waker());
    WAKER
        .set(waker.clone())
        .map_err(|_| "wake descriptor installed twice".to_owned())?;
    let native = NativeLane::start_background(tabs.clone(), settings, cleanup.clone())
        .map_err(|e| format!("native startup worker: {e}"))?;

    // Completion notifications, exactly as bterm: a pane whose shell exits on
    // its own is reaped on the UI thread and handed to the Bus thread, which
    // emits interact.notify. TERM_NOTIFY=0 drops the sender, so notes are
    // never queued and the Bus task retires its receive branch.
    let notify_enabled = std::env::var("TERM_NOTIFY")
        .map(|value| value != "0")
        .unwrap_or(true);
    let (notify_tx, notify_rx) = tokio::sync::mpsc::unbounded_channel();
    // The desktop adapter: one supervised client serves the `term.*` lane AND
    // the shared settings traffic (no second connection); it starts without
    // blocking, so an offline bus leaves the terminal working. It publishes
    // into the same WakeFd every PTY does.
    let started = settings_bus::start(
        SERVICE,
        ::bus::client_helpers::resolve_noded_url(),
        tabs.clone(),
        cleanup.clone(),
        notify_rx,
        waker.fd.waker(),
        settings_bus::PreparationSeed {
            local,
            raster: bootstrap_raster,
        },
    )
    .map_err(|e| format!("Bus startup: {e}"))?;
    let ui = started
        .bootstrap
        .typography()
        .get("ui")
        .expect("UI typography");
    // A clone lives outside the State so teardown below can still quit the
    // adapter after the handle was moved in.
    let handle = started.handle.clone();
    let frames = started.frames.clone();

    let state = State {
        painter,
        tabs: tabs.clone(),
        cleanup: cleanup.clone(),
        notify: notify_enabled.then_some(notify_tx),
        tokens: started.bootstrap.tokens(),
        ui,
        chrome: layout::strip_height(1.0, ui),
        settings: started.ui,
        frames: started.frames,
        local,
        baseline,
        applied_raster: None,
        applied_context: None,
        #[cfg(test)]
        fixture_lane: None,
        bus: started.handle,
        describes: started.describes,
        waker,
        window: Size::new(900.0, 560.0),
        shape: Shape::default(),
        grids: HashMap::new(),
        modifiers: application::iced::keyboard::Modifiers::empty(),
        right_shift: std::cell::Cell::new(false),
        wheel: 0.0,
        scroll_wheel: 0.0,
        scroll_pane: None,
        pointer: std::cell::Cell::new(None),
        mouse: clipboard::MouseState::default(),
        paste_notice: None,
        last_redraw: None,
        ime_preedit: None,
        ime: ime::Composition::default(),
        keyboard_focus: true,
        force_paint: false,
        paint_requested: true,
    };

    let ui_font = ui.font;
    let result = application::start(
        (state, Task::none()),
        update,
        view,
        application::Window::new(
            format!("dev.mixos.{SERVICE}"),
            Size::new(900.0, 560.0),
            ui_font,
        ),
    )
    .title(DISPLAY_NAME)
    .subscription(subscription)
    .frame_presentation(frame_binding)
    .theme(application::iced::Theme::Dark)
    .style(|state: &State, _theme| application::iced::theme::Style {
        background_color: state.tokens.palette.surface,
        text_color: state.tokens.palette.text,
    })
    .run();

    // Same teardown ordering as bterm, and the design contract: tabs close
    // first, then the global Bus finishes (verb-lane drain, tracked replies,
    // 2 s cache flush and client close inside the adapter), then native
    // cleanup is released, then the cleanup worker and reaper join. The
    // settings Lane drops with the adapter thread, last.
    let removed = tabs.lock().expect("tabs").shutdown();
    frames.close();
    cleanup.submit(removed);
    handle.quit();
    let mut shutdown = Ok(());
    if let Err(fault) = handle.wait_done() {
        // The worker prints TERM_SHUTDOWN with its own faults on the normal
        // path; this is the rare one where it cannot confirm done at all —
        // a blocking resource job in the settings worker cannot be cancelled,
        // so its thread is left unjoined rather than hanging this shutdown.
        eprintln!("term: {fault}");
        shutdown = Err(fault);
    } else {
        let _ = started.worker.join();
    }
    let native = native
        .join()
        .map_err(|_| "native startup worker panicked")?;
    native.release_cleanup();
    drop(cleanup);
    let _ = reaper.join();
    let startup = native.startup_result();
    drop(native);
    startup?;
    shutdown?;
    result.map_err(|error| error.to_string())
}

type WakeSender = application::iced::futures::channel::mpsc::UnboundedSender<Message>;

struct Waker {
    fd: WakeFd,
    /// True between "the poll thread published a wake" and "the UI thread
    /// consumed it". Exact coalescing: the thread publishes only on the
    /// false->true edge, so a flood of PTY output cannot grow the queue, and
    /// nothing is ever dropped — a change landing after the UI thread cleared
    /// the flag publishes a fresh wake rather than being swallowed.
    pending: AtomicBool,
    /// Where the poll thread publishes. Swapped, not recreated, when iced
    /// rebuilds the subscription: a second poll thread would race the first
    /// for the same eventfd, and the loser's drain would silently eat wakes
    /// the winner never hears about (cold-review finding, 2026-09-21).
    sender: Mutex<Option<WakeSender>>,
    /// Set once, so exactly one thread ever owns the descriptor.
    polling: AtomicBool,
}

static WAKER: OnceLock<Arc<Waker>> = OnceLock::new();

struct State {
    tabs: Arc<Mutex<TabSet>>,
    cleanup: tabs::Cleanup,
    notify: Option<tokio::sync::mpsc::UnboundedSender<CompletionNote>>,
    painter: Painter,
    /// Window chrome. Tokens restyle the surface only; `ui`/`chrome` drive
    /// the one computed strip height (extent). Colours never reflow or
    /// re-rasterise.
    tokens: toolkit::Tokens,
    ui: toolkit::typography::TextStyle,
    chrome: f32,
    /// The settings session's UI half, reconciled on every wake against the
    /// generation sampled from the actual shared client.
    settings: Ui<Content, LocalContext>,
    /// One bounded observer for this actual window incarnation.
    frames: application::frames::Handle,
    local: LocalContext,
    baseline: f32,
    applied_raster: Option<RasterKey>,
    applied_context: Option<LocalContext>,
    #[cfg(test)]
    fixture_lane: Option<application::presentation::native::Lane<Content, LocalContext>>,
    /// The desktop adapter: one client for the verb lane and the settings lane.
    bus: Handle,
    /// Queued `app.describe` requests, answered on the UI thread after the
    /// settings reconcile; bounded upstream (the adapter refuses beyond 32).
    describes: tokio::sync::mpsc::Receiver<Describe>,
    waker: Arc<Waker>,
    /// Logical inner size of the window, as the compositor last reported it.
    window: Size,
    /// Tabs and the active pane tree as of the last wake — what `view` draws.
    shape: Shape,
    /// Columns and rows each visible pane's PTY has been told about. A pane
    /// missing here has not been sized yet, which forces its first resize.
    grids: HashMap<u64, PtyExtent>,
    /// Tracked for Ctrl+wheel: a mouse event carries no modifier state.
    modifiers: application::iced::keyboard::Modifiers,
    right_shift: std::cell::Cell<bool>,
    /// Fractional Ctrl+wheel travel not yet worth a font step.
    wheel: f32,
    /// History and zoom gestures never share fractional travel.
    scroll_wheel: f32,
    scroll_pane: Option<u64>,
    /// Window coordinates survive a tab change beneath a stationary pointer.
    pointer: std::cell::Cell<Option<application::iced::Point>>,
    mouse: clipboard::MouseState,
    paste_notice: Option<String>,
    last_redraw: Option<std::time::Instant>,
    ime_preedit: Option<application::iced::advanced::input_method::Preedit>,
    ime: ime::Composition,
    keyboard_focus: bool,
    force_paint: bool,
    paint_requested: bool,
}

#[derive(Debug, Clone)]
enum Message {
    /// Something in the core changed: PTY output, a resize, a pane exit, a
    /// Bus mutation.
    Wake,
    Paint(std::time::Instant),
    /// Keys to put on the PTY, from the widget tree — NOT from an event
    /// subscription, which drops them under load (see `keys.rs`).
    Keys(Vec<term_core::terminal::Key>),
    Ime(application::iced::advanced::input_method::Event),
    /// A chord the terminal answers itself (tabs, panes, font size).
    Action(Action),
    Tab {
        forward: bool,
        repeat: bool,
    },
    Modifiers(application::iced::keyboard::Modifiers),
    SelectTab(u64),
    Mouse(
        application::iced::mouse::Event,
        application::iced::Point,
        Instant,
    ),
    Paste(u64, Option<String>),
    Wheel(u64, application::iced::mouse::ScrollDelta),
    Pointer,
    Window(application::iced::window::Event),
    /// The window's device-pixel ratio, answered by the runtime.
    Scale(f32),
}

fn subscription(_state: &State) -> Subscription<Message> {
    Subscription::batch([
        Subscription::run(wakes),
        // WINDOW events only. Keys go through the widget tree instead,
        // because this path DROPS events under load — see `keys.rs`. Window
        // events survive it: they are rare, and a lost resize is corrected by
        // the next one. `listen_with` already filters RedrawRequested, so
        // this cannot feed itself.
        application::iced::event::listen_with(|event, _status, _window| match event {
            application::iced::Event::Window(event) => Some(Message::Window(event)),
            _ => None,
        }),
    ])
}

/// Sample only installed settings alongside the immutable view. A pending or
/// failed preparation keeps the last good stamp; bootstrap remains unstamped.
fn frame_binding(state: &State) -> Option<application::frames::FrameBinding> {
    state.settings.session().frame_stamp().map(|stamp| state.frames.binding(stamp))
}

/// Publishes a [`Message::Wake`] whenever the core's eventfd fires.
///
/// A dedicated thread blocking in `poll(2)` rather than an async descriptor:
/// the executor here is a futures thread pool with no reactor, and a terminal
/// that is idle must cost nothing — this thread is parked in the kernel until
/// the PTY actually writes.
fn wakes() -> impl application::iced::futures::Stream<Item = Message> {
    let (sender, receiver) = application::iced::futures::channel::mpsc::unbounded();
    let Some(waker) = WAKER.get().cloned() else {
        // Only reachable if the wiring order in `run` changes. The window
        // would come up and then never repaint, which reads as a hung shell
        // rather than a broken terminal — so say which it is.
        eprintln!(
            "term: wake descriptor not installed before the event loop; the grid cannot repaint"
        );
        return receiver;
    };
    // Re-arm before publishing anywhere: if a previous subscription was torn
    // down between the thread's `swap(true)` and the UI thread's clear, the
    // flag would be stuck true and every later wake silently suppressed.
    *waker.sender.lock().expect("wake sender") = Some(sender);
    waker.pending.store(false, Ordering::Release);
    // Also re-arm the descriptor: any wake the dropped receiver was holding
    // is gone, so ask for one unconditionally rather than wait for the next
    // PTY byte. Redraw checks per-pane damage before snapshotting or painting.
    waker.fd.waker()();
    if waker.polling.swap(true, Ordering::AcqRel) {
        return receiver; // The one poll thread is already running.
    }
    let spawned = std::thread::Builder::new()
        .name("term-wake".into())
        .spawn(move || {
            loop {
                let mut fds = libc::pollfd {
                    fd: waker.fd.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one live pollfd, blocking indefinitely.
                if unsafe { libc::poll(&mut fds, 1, -1) } < 0 {
                    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return;
                }
                // Ready-but-not-readable means the descriptor is broken, not
                // that a wake arrived: `drain` would return false and the loop
                // would re-poll instantly, spinning a core forever. Stop
                // instead, and say so — a terminal that stops repainting is a
                // visible failure; one that pins a core is a mystery.
                if fds.revents & libc::POLLIN == 0 {
                    eprintln!(
                        "term: wake descriptor failed (revents {}); repaints have stopped",
                        fds.revents
                    );
                    return;
                }
                // Drain BEFORE publishing, never after: a change that lands
                // while the UI thread reads the grid then leaves the
                // descriptor readable for the next turn (one redundant,
                // damage-free repaint) instead of being lost.
                if !waker.fd.drain() {
                    continue;
                }
                if waker.pending.swap(true, Ordering::AcqRel) {
                    continue; // A wake is already queued; this one coalesces.
                }
                let sender = waker.sender.lock().expect("wake sender").clone();
                match sender {
                    Some(sender) if sender.unbounded_send(Message::Wake).is_ok() => {}
                    // The receiver is gone. Leave `pending` false so the next
                    // subscription is not born latched shut, and keep polling:
                    // this thread owns the descriptor for the process's life.
                    _ => waker.pending.store(false, Ordering::Release),
                }
            }
        });
    if let Err(error) = spawned {
        waker_spawn_failed(&error);
    }
    receiver
}

fn waker_spawn_failed(error: &std::io::Error) {
    // Not a warning to carry on past: with no poll thread the grid never
    // repaints and the window is a frozen picture of the first frame, which
    // looks like a hung shell rather than a failed terminal.
    eprintln!("term: cannot start the wake thread ({error}); the grid would never repaint");
    std::process::exit(1);
}

fn update(state: &mut State, message: Message) -> Task<Message> {
    state.sync_ime();
    let task = update_message(state, message);
    state.sync_ime();
    task
}

fn update_message(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::Paint(at) => {
            if state.last_redraw != Some(at) {
                state.last_redraw = Some(at);
                state.repaint();
            }
        }
        Message::Wake => {
            state.paint_requested = true;
            // Clear the wake flag BEFORE draining: everything sync() drains —
            // PTY damage, the settings reconcile, describe requests — must
            // publish a fresh wake if it lands after this clear.
            state.waker.pending.store(false, Ordering::Release);
            return state.sync();
        }
        Message::Scale(scale) => state.rescale(scale),
        Message::Keys(keys) => state.send_keys(keys),
        Message::Tab { forward, repeat } => {
            // Widget events can be batched before messages are applied. The
            // preceding message may have switched tabs or changed the split.
            let split = {
                let tabs = state.tabs.lock().expect("tabs");
                if tabs.is_empty() {
                    return Task::none();
                }
                tabs.leaves().len() > 1
            };
            if split {
                if !repeat {
                    return state.act(Action::CyclePane { forward });
                }
            } else {
                state.send_keys(vec![term_core::terminal::Key::Tab]);
            }
        }
        Message::Ime(event) => {
            use application::iced::advanced::input_method::{Event, Preedit};
            // Closed acknowledges the disable even after window focus loss.
            if matches!(event, Event::Closed) {
                state.ime.closed();
                state.ime_preedit = None;
                return Task::none();
            }
            if !state.keyboard_focus {
                return Task::none();
            }
            match event {
                Event::Preedit(content, selection) => {
                    let tabs = state.tabs.lock().expect("tabs");
                    let active = (!tabs.is_empty()).then(|| tabs.active_tab().active_pane);
                    if !state.ime.preedit(active) {
                        state.ime_preedit = None;
                        return Task::none();
                    }
                    state.ime_preedit = Some(Preedit {
                        content,
                        selection,
                        text_size: None,
                    });
                }
                Event::Commit(text) => {
                    state.ime_preedit = None;
                    // Resolve and validate the target under the same lock;
                    // a Bus focus mutation cannot race the owner check.
                    let tabs = state.tabs.lock().expect("tabs");
                    let active = (!tabs.is_empty()).then(|| tabs.active_tab().active_pane);
                    if state.ime.commit(active) {
                        tabs.user_activity();
                        let terminal = tabs.active_terminal();
                        if let Err(error) = terminal
                            .lock()
                            .expect("terminal")
                            .keys(&input::text_keys(&text), Instant::now())
                        {
                            eprintln!("term input: {error}");
                        }
                    }
                }
                Event::Closed => unreachable!(),
                Event::Opened => state.ime.opened(),
            }
        }
        Message::Action(action) => return state.act(action),
        Message::Modifiers(modifiers) => {
            if modifiers != state.modifiers {
                state.scroll_wheel = 0.0;
            }
            // Letting go of Ctrl ends a Ctrl+wheel gesture: travel short of a
            // step must not carry into the next one and zoom early.
            if !modifiers.control() {
                state.wheel = 0.0;
            }
            state.modifiers = modifiers;
        }
        Message::SelectTab(id) => {
            state.cancel_mouse_gesture();
            let mut tabs = state.tabs.lock().expect("tabs");
            tabs.user_activity();
            tabs.select(id);
        }
        Message::Mouse(event, position, at) => return state.mouse_event(event, position, at),
        Message::Paste(id, text) => state.paste(id, text),
        Message::Pointer => {}
        Message::Wheel(id, delta) => {
            if state.modifiers.control() {
                let steps = input::wheel_steps(&mut state.wheel, delta);
                if steps != 0 {
                    state.zoom(|font| font.step_by(steps));
                }
            } else {
                state.scroll(id, delta);
            }
        }
        Message::Window(event) => match event {
            application::iced::window::Event::Opened { size, .. } => {
                state.resize(size);
                // Ask rather than wait: winit does not necessarily emit a
                // Rescaled for the scale a surface is BORN at, and a terminal
                // that renders one frame at the wrong scale is a terminal
                // that starts blurry.
                return application::iced::window::latest()
                    .and_then(application::iced::window::scale_factor)
                    .map(Message::Scale);
            }
            application::iced::window::Event::Resized(size) => state.resize(size),
            application::iced::window::Event::Rescaled(scale) => state.rescale(scale),
            application::iced::window::Event::Focused => {
                state.tabs.lock().expect("tabs").user_activity();
                state.keyboard_focus = true;
            }
            // A release that happens while another window has the keyboard
            // is never delivered; a latched Ctrl would turn every later wheel
            // into a zoom.
            application::iced::window::Event::Unfocused => {
                state.keyboard_focus = false;
                state.right_shift.set(false);
                state.ime.cancel();
                state.ime_preedit = None;
                state.cancel_mouse_gesture();
                state.modifiers = application::iced::keyboard::Modifiers::empty();
                state.wheel = 0.0;
                state.scroll_wheel = 0.0;
            }
            application::iced::window::Event::CloseRequested => {
                state.frames.close();
                return application::iced::exit();
            }
            _ => {}
        },
    }
    Task::none()
}

/// The keyboard, routed from the widget tree. A terminal chord wins over the
/// shell encoder — without that order, Ctrl+Shift+T would reach the PTY as a
/// Ctrl-T (`input::tests::a_tab_chord_would_otherwise_reach_the_shell_as_a_control_code`).
#[cfg(test)]
fn on_key(event: &application::iced::keyboard::Event) -> Option<Message> {
    on_key_screen(event, false)
}

#[cfg(test)]
fn on_key_screen(event: &application::iced::keyboard::Event, alternate: bool) -> Option<Message> {
    on_key_context(event, alternate, false)
}

fn on_key_context(
    event: &application::iced::keyboard::Event,
    alternate: bool,
    right_shift: bool,
) -> Option<Message> {
    match event {
        application::iced::keyboard::Event::KeyPressed {
            key,
            modified_key,
            physical_key,
            text,
            modifiers,
            repeat,
            ..
        } => {
            if matches!(
                key,
                application::iced::keyboard::Key::Named(
                    application::iced::keyboard::key::Named::Tab
                )
            ) && !modifiers.control()
                && !modifiers.alt()
                && !modifiers.logo()
            {
                return Some(Message::Tab {
                    forward: !modifiers.shift(),
                    repeat: *repeat,
                });
            }
            if let Some(action) = input::action_on_screen(
                input::navigation_action(key, *modifiers, right_shift)
                    .or_else(|| input::action_for(key, modified_key, *physical_key, *modifiers)),
                alternate,
            ) {
                // A repeat of a non-repeating chord is swallowed, not passed
                // through: it must not turn into a control code either.
                return (!*repeat || action.repeats()).then_some(Message::Action(action));
            }
            let keys = input::keys_for(key, text.as_deref(), *modifiers);
            (!keys.is_empty()).then_some(Message::Keys(keys))
        }
        application::iced::keyboard::Event::ModifiersChanged(modifiers) => {
            Some(Message::Modifiers(*modifiers))
        }
        _ => None,
    }
}

fn view(state: &State) -> Element<'_, Message> {
    let tokens = state.tokens;
    let ui = state.ui;
    let scale = state.painter.scale();
    let bounds = layout::content(state.window.width, state.window.height, state.chrome);
    let panes: Element<'_, Message> = match &state.shape.tree {
        Some(tree) => pane_tree(state, tree, bounds, scale),
        None => space().into(),
    };
    // The keyboard rides the widget tree, not a subscription: see `keys.rs`.
    // It wraps the strip as well, so a key pressed while the pointer is over
    // a tab still reaches the terminal.
    let pane_bounds = state
        .shape
        .tree
        .as_ref()
        .map(|tree| layout::panes(tree, bounds, scale))
        .unwrap_or_default();
    let ime_cursor = pane_bounds
        .iter()
        .find(|(id, _)| *id == state.shape.active_pane)
        .and_then(|(id, pane)| {
            let (col, row) = state
                .painter
                .existing(*id)
                .and_then(|frame| frame.lock().expect("frame").cursor())
                .unwrap_or((0, 0));
            let (cw, ch) = state.painter.logical_cell();
            let (columns, rows) = state.grids.get(id)?.grid();
            toolkit::GridGeometry {
                cell: Size::new(cw, ch),
                columns,
                rows,
                border: layout::border(scale),
            }
            .cursor_rect(
                application::iced::Point::new(pane.x, pane.y),
                (
                    u16::try_from(col).unwrap_or(u16::MAX),
                    u16::try_from(row).unwrap_or(u16::MAX),
                ),
            )
        });
    let hovered = move |position: application::iced::Point| {
        let (id, pane) = pane_bounds.iter().find(|(_, pane)| {
            position.x >= pane.x
                && position.x < pane.x + pane.w
                && position.y >= pane.y
                && position.y < pane.y + pane.h
        })?;
        let grid = state.grids.get(id)?.grid();
        let (col, row) = input::pointer_cell(
            application::iced::Point::new(position.x - pane.x, position.y - pane.y),
            layout::border(scale),
            state.painter.logical_cell(),
            grid,
        );
        Some((*id, col, row))
    };
    let last = std::cell::Cell::new(state.pointer.get().and_then(&hovered));
    let mouse = clipboard::MouseEvents::new(state);
    let mut content =
        toolkit::keys::keys(column![tab_strip(state, scale, ui), panes], move |event| {
            state
                .right_shift
                .set(input::right_shift_after(event, state.right_shift.get()));
            let message = on_key_context(event, false, state.right_shift.get());
            // Ordinary typing must not acquire an extra terminal/grid lock just
            // to decide who owns a scrollback chord.
            if matches!(message, Some(Message::Action(Action::Scroll(_)))) {
                let tabs = state.tabs.lock().expect("tabs");
                if !tabs.is_empty()
                    && tabs
                        .active_terminal()
                        .lock()
                        .expect("terminal")
                        .alternate_screen()
                {
                    return on_key_context(event, true, state.right_shift.get());
                }
            }
            message
        })
        .on_pointer(move |position| {
            let cell = hovered(position);
            pointer_message(&state.pointer, &last, cell, position)
        })
        .on_mouse(move |event, position| mouse.message(state, event, position))
        .input_method(
            match ime_cursor {
                Some(cursor) if state.keyboard_focus && state.ime.enabled() => {
                    application::iced::advanced::input_method::InputMethod::Enabled {
                        // Runtime composition overlay; only Commit goes to the PTY.
                        cursor,
                        purpose: application::iced::advanced::input_method::Purpose::Terminal,
                        preedit: state.ime_preedit.clone(),
                    }
                }
                _ => application::iced::advanced::input_method::InputMethod::Disabled,
            },
            |event| Message::Ime(event.clone()),
        );
    // Clean chrome redraws must not publish Paint: iced would rebuild the UI
    // and dispatch RedrawRequested a second time for no terminal change.
    if state.needs_paint() {
        content = content.on_redraw(state.last_redraw, Message::Paint);
    }
    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_theme| container::Style {
            background: Some(tokens.palette.surface.into()),
            ..container::Style::default()
        })
        .into()
}

type HoveredCell = Option<(u64, u16, u16)>;

fn pointer_message(
    pointer: &std::cell::Cell<Option<application::iced::Point>>,
    last: &std::cell::Cell<HoveredCell>,
    hovered: HoveredCell,
    position: application::iced::Point,
) -> Option<Message> {
    // Keep pixel coordinates even when no application update is needed. A queued
    // Pointer message must never overwrite a newer coalesced position.
    pointer.set(Some(position));
    (last.replace(hovered) != hovered).then_some(Message::Pointer)
}

/// One button per tab and a `+`, as bterm. Every colour is a design token;
/// the labels carry the complete prepared UI font, size and line height through
/// the shared TabBar style and its single layout path.
fn tab_strip(
    state: &State,
    scale: f32,
    ui: toolkit::typography::TextStyle,
) -> Element<'_, Message> {
    let tokens = state.tokens;
    let mut tabs = toolkit::TabBar::with_tab_labels(
        state
            .shape
            .tabs
            .iter()
            .map(|tab| (tab.id, toolkit::TabLabel::Text(tab.title.clone())))
            .collect(),
        Message::SelectTab,
    )
    .text_style(ui)
    .tab_width(Length::Shrink)
    .padding([layout::TAB_V_PADDING, 12.0])
    .spacing(4.0)
    .style(move |_, status| {
        let mut style = toolkit::theme::tab_bar::default(&toolkit::Theme::new(tokens), status);
        if status == toolkit::tab_bar::Status::Active {
            style.tab_label_background = tokens.palette.primary.into();
            style.text_color = tokens.palette.primary_text;
        } else if status == toolkit::tab_bar::Status::Disabled {
            style.tab_label_background = tokens.palette.card.into();
        }
        style
    });
    if let Some(active) = state.shape.tabs.iter().find(|tab| tab.active) {
        tabs = tabs.set_active_tab(&active.id);
    }
    let new_tab = toolkit::CenteredButton::new(ui.text("+"))
        .padding([layout::TAB_V_PADDING, 12.0])
        .on_press(Message::Action(Action::NewTab))
        .style(move |_, status| {
            let mut style = toolkit::theme::button::text(&toolkit::Theme::new(tokens), status);
            style.text_color = tokens.palette.muted_text;
            style
        });
    let mut strip = Row::new()
        .spacing(4.0)
        .padding([layout::STRIP_V_PADDING, 6.0]);
    // The labelled chrome: the Bus connection provenance while it is not
    // connected, and the settings-presentation kind while the appearance is
    // not Current.
    if let Some(label) = state.chrome_label() {
        strip = strip.push(ui.text(label).color(tokens.palette.muted_text));
    }
    if let Some(notice) = &state.paste_notice {
        strip = strip.push(ui.text(notice).color(tokens.palette.text));
    }
    strip = strip
        .push(tabs.scrollable().width(Length::Fill))
        .push(new_tab);
    container(strip)
        .width(Length::Fill)
        .height(Length::Fixed(layout::strip_height(scale, ui)))
        .style(move |_| container::Style {
            background: Some(tokens.palette.card.into()),
            ..Default::default()
        })
        .into()
}

/// The active tab's panes as nested rows and columns, split with the same
/// function that sized their PTYs (`layout::split`), so a pane's widget and
/// its grid always agree on its rectangle.
fn pane_tree<'a>(
    state: &'a State,
    node: &Node,
    bounds: Geometry,
    scale: f32,
) -> Element<'a, Message> {
    match node {
        Node::Leaf(id) => pane(state, *id, bounds, scale),
        Node::Split {
            dir,
            ratio,
            first,
            second,
        } => {
            let (a, b) = layout::split(*dir, *ratio, bounds, scale);
            let first = pane_tree(state, first, a, scale);
            let second = pane_tree(state, second, b, scale);
            match dir {
                SplitDir::Vertical => row![first, second].into(),
                SplitDir::Horizontal => column![first, second].into(),
            }
        }
    }
}

/// One pane: a border in the focus colour, the themed surface, and the grid
/// sized to its cells exactly, so the texture maps 1:1 to physical pixels and
/// the nearest sampler never resamples a glyph.
fn pane(state: &State, id: u64, bounds: Geometry, scale: f32) -> Element<'_, Message> {
    let tokens = state.tokens;
    let (cell_width, cell_height) = state.painter.logical_cell();
    let grid: Element<'_, Message> = match (state.painter.existing(id), state.grids.get(&id)) {
        (Some(frame), Some(&PtyExtent(cols, rows, _, _))) => renderer(state, id, frame)
            .width(Length::Fixed(f32::from(cols) * cell_width))
            .height(Length::Fixed(f32::from(rows) * cell_height))
            .into(),
        // Not sized or painted yet: sync sizes it, then the next redraw paints.
        _ => space().into(),
    };
    toolkit::TerminalPane::new(
        grid,
        Size::new(bounds.w, bounds.h),
        layout::border(scale),
        tokens,
    )
    .focus_ring(show_focus_ring(&state.shape, id))
    .on_scroll(move |delta| Message::Wheel(id, delta))
    .into()
}

/// A pane's border colour. The focus ring marks which pane keys go to, so it
/// shows only when there is a choice: a lone pane wears the plain border, as
/// foot shows nothing at all. The border's WIDTH never changes — that is what
/// keeps focus changes from resizing a PTY — only its colour.
fn show_focus_ring(shape: &Shape, id: u64) -> bool {
    id == shape.active_pane && shape.visible().len() > 1
}

#[cfg(feature = "wgpu")]
fn renderer(
    _state: &State,
    _id: u64,
    frame: Arc<Mutex<frame::Frame>>,
) -> application::iced::widget::Shader<Message, wgpu_grid::GridProgram> {
    application::iced::widget::shader(wgpu_grid::GridProgram::new(frame))
}

#[cfg(all(feature = "tiny-skia", not(feature = "wgpu")))]
fn renderer(state: &State, _id: u64, frame: Arc<Mutex<frame::Frame>>) -> cpu_grid::Grid {
    cpu_grid::view(&frame, state.painter.scale())
}

/// Apply a tab or pane chord to the tab set, returning what to tear down.
///
/// The same `TabSet` calls bterm's keyboard handler makes, so the two
/// frontends do the same thing for the same chord. Font chords are not tab
/// operations and never reach here.
fn apply(tabs: &mut TabSet, action: Action) -> Vec<Removed> {
    if tabs.is_empty() {
        return Vec::new();
    }
    tabs.user_activity();
    match action {
        Action::NewTab => {
            if let Err(error) = tabs.open() {
                eprintln!("new tab: {error}");
            }
            Vec::new()
        }
        Action::CloseTab => {
            let id = tabs.active_id();
            tabs.close(id).1.into_iter().collect()
        }
        Action::Quit => tabs.shutdown(),
        Action::Split(dir) => {
            if let Err(error) = tabs.split_active(dir) {
                eprintln!("split pane: {error}");
            }
            Vec::new()
        }
        Action::ClosePane => tabs.close_active().1.into_iter().collect(),
        Action::Focus(direction) => {
            tabs.focus_dir(direction);
            Vec::new()
        }
        Action::Cycle { forward } => {
            tabs.cycle(forward);
            Vec::new()
        }
        Action::CyclePane { forward } => {
            let ids: Vec<_> = tabs.leaves().iter().map(|pane| pane.id).collect();
            if let Some(id) = input::cycle_pane(&ids, tabs.active_tab().active_pane, forward) {
                tabs.focus(id);
            }
            Vec::new()
        }
        Action::Scroll(request) => {
            tabs.active_terminal()
                .lock()
                .expect("terminal")
                .scroll_view(request);
            Vec::new()
        }
        Action::FontIncrease
        | Action::FontDecrease
        | Action::FontReset
        | Action::Copy
        | Action::Paste => Vec::new(),
    }
}

impl State {
    fn scroll(&mut self, id: u64, delta: application::iced::mouse::ScrollDelta) {
        if self.scroll_pane != Some(id) {
            self.scroll_wheel = 0.0;
            self.scroll_pane = Some(id);
        }
        let Some(position) = self.pointer.get() else {
            return;
        };
        let Some(tree) = &self.shape.tree else {
            return;
        };
        let scale = self.painter.scale();
        let bounds = layout::content(self.window.width, self.window.height, self.chrome);
        let Some((_, pane)) = layout::panes(tree, bounds, scale)
            .into_iter()
            .find(|(pane, _)| *pane == id)
        else {
            return;
        };
        let Some(&grid) = self.grids.get(&id) else {
            return;
        };
        let lines =
            input::scroll_steps(&mut self.scroll_wheel, delta, self.painter.logical_cell().1);
        if lines == 0 {
            return;
        }
        let (col, row) = input::pointer_cell(
            application::iced::Point::new(position.x - pane.x, position.y - pane.y),
            layout::border(scale),
            self.painter.logical_cell(),
            grid.grid(),
        );
        let tabs = self.tabs.lock().expect("tabs");
        let Some(terminal) = tabs.pane_by_id(id) else {
            return;
        };
        tabs.user_activity();
        drop(tabs);
        let terminal = terminal.lock().expect("terminal");
        let mods = term_core::terminal::MouseModifiers {
            shift: self.modifiers.shift(),
            alt: self.modifiers.alt(),
            ctrl: self.modifiers.control(),
        };
        if !terminal.mouse_scroll(col, row, lines, mods) {
            let before = terminal.display_offset();
            terminal.scroll_wheel(lines, mods);
            self.paint_requested |= terminal.display_offset() != before;
        }
    }

    /// Everything a wake can mean, in one place: reconcile the settings lane
    /// first, reap exited shells, follow the tab set's shape (a Bus verb may
    /// have changed it), size any pane that needs it. Painting waits for the
    /// widget's redraw event.
    fn sync(&mut self) -> Task<Message> {
        // Settings first: reconcile the generation sampled from the actual
        // shared client, drain every queued lane event (the pending wake flag
        // was cleared BEFORE this ran), then adopt chrome and answer
        // describes — so every later reader sees live evidence, not a queued
        // lifecycle notice.
        let generation = self.bus.settings_generation();
        self.settings.reconcile(generation);
        let mut target = presentation::ActivationTarget {
            painter: &mut self.painter,
            tokens: &mut self.tokens,
            ui: &mut self.ui,
            chrome: &mut self.chrome,
            baseline: &mut self.baseline,
            applied_context: &mut self.applied_context,
            applied_key: &mut self.applied_raster,
            force_paint: &mut self.force_paint,
            paint_requested: &mut self.paint_requested,
            tabs: &self.tabs,
            shape: &mut self.shape,
            window: self.window,
            grids: &mut self.grids,
        };
        let changes = self
            .settings
            .drain_with(|| self.bus.settings_generation(), |p| target.activate(p));
        if !changes.is_empty() {
            // The settings activation receipt, same shape the other hosts
            // print: live evidence, cache state and fallback diagnostics.
            eprintln!(
                "TERM_SETTINGS {}",
                serde_json::json!({
                    "service": self.bus.service_name(),
                    "evidence": self.settings.session().host().consumer().evidence(),
                    "settings_cache": self.settings.session().cache_evidence(),
                    "fallback_diagnostics": self.settings.session().fallback_diagnostics(),
                })
            );
        }
        let (removed, notes) = self.tabs.lock().expect("tabs").reap_exited();
        self.sync_ime();
        self.cancel_hidden_gesture();
        self.cleanup.submit(removed);
        if let Some(notify) = &self.notify {
            for note in notes {
                let _ = notify.send(note);
            }
        }
        let shape = {
            let tabs = self.tabs.lock().expect("tabs");
            if tabs.is_empty() {
                if tabs.is_starting() {
                    return Task::none();
                }
                return application::iced::exit();
            }
            Shape::of(&tabs)
        };
        let visible = shape.visible();
        self.painter.retain(&visible);
        self.grids.retain(|id, _| visible.contains(id));
        self.shape = shape;
        self.relayout();
        self.answer_describes();
        Task::none()
    }

    /// The extent-change boundary, split out so tests can drive it directly:
    /// tokens never reflow; a changed strip height relayouts once.
    #[cfg(test)]
    fn apply_chrome(&mut self, ui: toolkit::typography::TextStyle, tokens: toolkit::Tokens) {
        self.tokens = tokens;
        self.ui = ui;
        let chrome = layout::strip_height(self.painter.scale(), ui);
        if self.chrome != chrome {
            self.chrome = chrome;
            self.relayout();
        }
    }

    /// Every queued `app.describe` is answered on the UI thread AFTER the
    /// settings reconcile above, so the evidence describes the live
    /// presentation. Bounded: the adapter refuses beyond 32 pending.
    fn answer_describes(&mut self) {
        while let Ok(describe) = self.describes.try_recv() {
            let value = self.describe();
            self.bus.reply(&describe, 0, value);
        }
    }

    /// The full describe evidence: canonical serialized consumer evidence and
    /// cache state, the ACTUAL served name (post-fallback), the computed
    /// chrome extent and the prepared UI typography — never a partial or
    /// fabricated picture.
    fn describe(&self) -> serde_json::Value {
        let service = self.bus.service_name();
        let ui = self.ui;
        let mut describe = serde_json::json!({
            "schema": "term.v1",
            "app_id": format!("dev.mixos.{SERVICE}"),
            "version": env!("CARGO_PKG_VERSION"),
            "transport": "native",
            "service": service,
            "verbs": [
                "term.tabs", "term.tab.new", "term.tab.select", "term.tab.close",
                "term.tab.title", "term.tab.move", "term.panes", "term.pane.split",
                "term.pane.select", "term.pane.close", "term.snapshot",
                "term.scroll", "term.type", "term.props.watch", "term.session",
                "app.describe"
            ],
            "chrome": {"height": self.chrome},
            "ui": {
                "font": ui.font.family.to_string(),
                "size": ui.size,
                "line_height": ui.line_height,
            },
        });
        describe["settings"] =
            serde_json::json!(self.settings.session().host().consumer().evidence());
        describe["settings_cache"] = serde_json::json!(self.settings.session().cache_evidence());
        describe["fallback_diagnostics"] =
            serde_json::json!(self.settings.session().fallback_diagnostics());
        let preparation = self.settings.preparation_evidence();
        describe["preparation"] = serde_json::json!({
            "desired": preparation.desired.get(),
            "applied": preparation.applied.map(|revision| revision.get()),
            "current": preparation.current,
            "fault": preparation.fault,
            "cell": self.painter.cell(),
            "scale": self.painter.scale(),
            "baseline_px": self.baseline,
            "desired_zoom_steps": self.local.zoom_steps,
            "applied_zoom_steps": self.applied_context.map(|context| context.zoom_steps),
            "desired_scale": self.local.scale,
            "raster": self.applied_raster.as_ref().map(|key| serde_json::json!({
                "source": match &key.source {
                    presentation::RasterSource::Bootstrap => serde_json::json!({"kind":"bootstrap"}),
                    presentation::RasterSource::Verified(groups) => serde_json::json!({"kind":"verified", "groups":groups}),
                },
                "weight":key.weight,
                "scale":f32::from_bits(key.scale),
                "logical_px":f32::from_bits(key.logical_px),
                "cursor":key.cursor,
            })),
        });
        describe
    }

    /// The label the chrome wears, sampled from the shared handle — the Bus
    /// connection provenance whenever it is not Connected (with the broker's
    /// refusal words when refused), and the settings-presentation kind while
    /// the appearance is not Current. Persistent UI evidence, independent of
    /// the transient log lines. No extra worker.
    fn chrome_label(&self) -> Option<String> {
        let connection = self.bus.connection();
        match connection.state {
            ConnState::Connected => self.settings_label(),
            ConnState::Connecting => Some(strings::label("bus-connecting")),
            ConnState::Disconnected | ConnState::ShuttingDown => {
                Some(strings::label("bus-disconnected"))
            }
            ConnState::Fatal => match connection.refused {
                Some(reason) => Some(strings::format("bus-refused", &[("reason", reason)])),
                None => Some(strings::label("bus-unavailable")),
            },
        }
    }

    /// The settings kind label: every non-Current presentation kind, from the
    /// Fluent catalogue, and nothing once the appearance is Current.
    fn settings_label(&self) -> Option<String> {
        use settings::fallback::PresentationKind;
        match self.settings.session().host().consumer().evidence().kind {
            Some(PresentationKind::Current) => None,
            Some(PresentationKind::Cached) => Some(strings::label("settings-cached")),
            Some(PresentationKind::Retained) => Some(strings::label("settings-retained")),
            Some(PresentationKind::LastGood) => Some(strings::label("settings-last-good")),
            Some(PresentationKind::Embedded) => Some(strings::label("settings-embedded")),
            None => Some(strings::label("settings-bootstrap")),
        }
    }

    fn needs_paint(&self) -> bool {
        self.paint_requested
            || self.force_paint
            || self
                .shape
                .visible()
                .iter()
                .any(|id| self.painter.existing(*id).is_none())
    }

    /// Rasterise every visible pane by its damaged rows (all rows, for a
    /// frame that was invalidated or is new).
    fn repaint(&mut self) {
        self.paint_requested = false;
        let terminals: Vec<_> = {
            let tabs = self.tabs.lock().expect("tabs");
            self.shape
                .visible()
                .into_iter()
                .filter_map(|id| tabs.pane_by_id(id).map(|terminal| (id, terminal)))
                .collect()
        };
        for (id, terminal) in terminals {
            let terminal = terminal.lock().expect("terminal");
            // Consume before capture: a write racing capture is either in
            // this snapshot, or publishes a token after its locked rearm.
            let dirty = terminal.take_damage();
            if !dirty && !self.force_paint && self.painter.existing(id).is_some() {
                continue;
            }
            let snapshot = terminal.grid_snapshot();
            drop(terminal);
            #[allow(unused_variables)]
            let painted = self
                .painter
                .repaint(id, &snapshot.screen, &snapshot.dirty_rows);
            #[cfg(all(feature = "tiny-skia", not(feature = "wgpu")))]
            if painted {
                let frame = self.painter.frame(id);
                cpu_grid::refresh(&frame);
            }
        }
        self.force_paint = false;
    }

    fn act(&mut self, action: Action) -> Task<Message> {
        match action {
            Action::Copy | Action::Paste => return self.clipboard_action(action),
            Action::FontIncrease => self.zoom(FontSize::increase),
            Action::FontDecrease => self.zoom(FontSize::decrease),
            Action::FontReset => self.zoom(FontSize::reset),
            _ => {
                if !matches!(action, Action::Scroll(_)) {
                    self.cancel_mouse_gesture();
                }
                let removed = apply(&mut self.tabs.lock().expect("tabs"), action);
                self.cleanup.submit(removed);
                if matches!(action, Action::Scroll(_)) {
                    self.paint_requested = true;
                }
                if action == Action::Quit {
                    return application::iced::exit();
                }
                // Every other mutation notifies the wake, and the next
                // `sync` picks up the new shape.
            }
        }
        Task::none()
    }

    /// foot's behaviour: the window keeps its size and the grid reflows to
    /// the new cell. Every visible pane is re-rasterised (they share the
    /// glyph cache) and every PTY is told its new size.
    fn zoom(&mut self, change: impl FnOnce(&mut FontSize) -> bool) {
        let mut font = match FontSize::from_steps(self.baseline, self.local.zoom_steps) {
            Ok(font) => font,
            Err(error) => {
                eprintln!("term: font zoom: {error}");
                return;
            }
        };
        if !change(&mut font) {
            return;
        }
        let next = LocalContext {
            zoom_steps: font.steps(),
            ..self.local
        };
        self.publish_context(next);
    }

    fn resize(&mut self, window: Size) {
        // An unsized grid must force a layout even when the size is
        // unchanged. Otherwise a compositor that grants exactly the requested
        // 900x560 on a scale-1 output takes BOTH early returns — this one and
        // `rescale`'s — and no pane is ever sized: a terminal window with
        // nothing in it, forever (cold-review finding, 2026-09-21).
        if self.window == window && !self.grids.is_empty() {
            return;
        }
        self.window = window;
        self.relayout();
    }

    fn rescale(&mut self, scale: f32) {
        match self.local.with_scale(scale) {
            Ok(next) => self.publish_context(next),
            Err(error) => eprintln!("term: output scale: {error}"),
        }
    }

    fn publish_context(&mut self, next: LocalContext) {
        match self
            .settings
            .set_context(next, self.bus.settings_generation())
        {
            Ok(_) => self.local = next,
            Err(error) => eprintln!("term: local preparation: {}", error.message),
        }
    }

    /// Compare complete PTY extents using the applied painter geometry.
    fn relayout(&mut self) {
        presentation::LayoutTarget {
            painter: &self.painter,
            tabs: &self.tabs,
            shape: &mut self.shape,
            window: self.window,
            chrome: self.chrome,
            grids: &mut self.grids,
            paint_requested: &mut self.paint_requested,
        }
        .relayout();
    }
    fn sync_ime(&mut self) {
        if !self.ime.has_owner() {
            return;
        }
        let tabs = self.tabs.lock().expect("tabs");
        let active = (!tabs.is_empty()).then(|| tabs.active_tab().active_pane);
        if self.ime.focus(active) {
            self.ime_preedit = None;
        }
    }

    fn send_keys(&mut self, keys: Vec<term_core::terminal::Key>) {
        if keys.is_empty() {
            return;
        }
        let tabs = self.tabs.lock().expect("tabs");
        if tabs.is_empty() {
            return;
        }
        tabs.user_activity();
        // The focused pane of the active tab.
        let terminal = tabs.active_terminal();
        drop(tabs);
        let terminal = terminal.lock().expect("terminal");
        let at = Instant::now();
        if let Err(error) = terminal.keys(&keys, at) {
            eprintln!("term input: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use term_core::panes::Direction;

    #[test]
    fn an_env_font_size_overrides_the_config_only_when_it_is_valid() {
        let base = core_config::Config {
            font_px: 13.0,
            ..core_config::Config::default()
        };
        assert_eq!(
            resolve_config(base, Some("18.5"), "xterm").config.font_px,
            18.5
        );
        for rejected in ["5.9", "48.1", "", "eighteen", "nan", "inf"] {
            assert_eq!(
                resolve_config(base, Some(rejected), "xterm").config.font_px,
                13.0,
                "accepted {rejected}"
            );
        }
        assert_eq!(resolve_config(base, None, "xterm").config.font_px, 13.0);
    }

    fn press(
        key: application::iced::keyboard::Key,
        modified: application::iced::keyboard::Key,
        modifiers: application::iced::keyboard::Modifiers,
        text: Option<&str>,
        repeat: bool,
    ) -> application::iced::keyboard::Event {
        application::iced::keyboard::Event::KeyPressed {
            key,
            modified_key: modified,
            physical_key: application::iced::keyboard::key::Physical::Unidentified(
                application::iced::keyboard::key::NativeCode::Unidentified,
            ),
            location: application::iced::keyboard::Location::Standard,
            modifiers,
            text: text.map(Into::into),
            repeat,
        }
    }

    fn character(c: &str) -> application::iced::keyboard::Key {
        application::iced::keyboard::Key::Character(c.into())
    }

    #[test]
    fn tab_is_deferred_and_right_shift_arrows_win_before_pty_encoding() {
        use application::iced::keyboard::{Key, Modifiers, key::Named};
        for alternate in [false, true] {
            for (key, modifiers, right_shift, action) in [
                (
                    Named::ArrowLeft,
                    Modifiers::SHIFT,
                    true,
                    Action::Cycle { forward: false },
                ),
                (
                    Named::ArrowRight,
                    Modifiers::SHIFT,
                    true,
                    Action::Cycle { forward: true },
                ),
            ] {
                let key = Key::Named(key);
                let event = press(key.clone(), key.clone(), modifiers, None, false);
                assert!(matches!(on_key_context(&event, alternate, right_shift),
                    Some(Message::Action(found)) if found == action));
                let event = press(key.clone(), key, modifiers, None, true);
                assert!(on_key_context(&event, alternate, right_shift).is_none());
            }
        }
        let key = Key::Named(Named::Tab);
        let event = press(key.clone(), key, Modifiers::empty(), Some("\t"), false);
        assert!(matches!(
            on_key_context(&event, false, false),
            Some(Message::Tab {
                forward: true,
                repeat: false
            })
        ));
        let key = Key::Named(Named::ArrowRight);
        let event = press(key.clone(), key, Modifiers::SHIFT, None, false);
        assert!(matches!(
            on_key_context(&event, false, false),
            Some(Message::Keys(_))
        ));
        let event = press(character("i"), character("i"), Modifiers::CTRL, None, false);
        let Some(Message::Keys(keys)) = on_key_context(&event, false, false) else {
            panic!("Ctrl+I must send a literal tab in a split");
        };
        assert_eq!(
            keys.into_iter()
                .flat_map(term_core::terminal::encode)
                .collect::<Vec<_>>(),
            b"\t"
        );
    }

    #[test]
    fn right_shift_tracks_physical_press_release_and_modifier_reset() {
        use application::iced::keyboard::{
            Event, Key, Location, Modifiers,
            key::{Code, Named, Physical},
        };
        let mut event = press(
            Key::Named(Named::Shift),
            Key::Named(Named::Shift),
            Modifiers::SHIFT,
            None,
            false,
        );
        if let Event::KeyPressed {
            physical_key,
            location,
            ..
        } = &mut event
        {
            *physical_key = Physical::Code(Code::ShiftRight);
            *location = Location::Right;
        }
        assert!(input::right_shift_after(&event, false));
        let left = press(
            Key::Named(Named::Shift),
            Key::Named(Named::Shift),
            Modifiers::SHIFT,
            None,
            false,
        );
        assert!(!input::right_shift_after(&left, false));
        assert!(input::right_shift_after(&left, true));
        let release = Event::KeyReleased {
            key: Key::Named(Named::Shift),
            modified_key: Key::Named(Named::Shift),
            physical_key: Physical::Code(Code::ShiftRight),
            location: Location::Right,
            modifiers: Modifiers::SHIFT, // left Shift is still down
        };
        assert!(!input::right_shift_after(&release, true));
        assert!(!input::right_shift_after(
            &Event::ModifiersChanged(Modifiers::empty()),
            true
        ));
        for extra in [Modifiers::CTRL, Modifiers::ALT, Modifiers::LOGO] {
            assert_eq!(
                input::navigation_action(
                    &Key::Named(Named::ArrowRight),
                    Modifiers::SHIFT | extra,
                    true
                ),
                None
            );
            assert_eq!(
                input::navigation_action(&Key::Named(Named::Tab), extra, false),
                None
            );
        }
    }

    #[test]
    fn clipboard_chords_win_before_pty_encoding_and_swallow_repeats() {
        use application::iced::keyboard::{Key, Modifiers, key::Named};
        for (key, mods, expected) in [
            (
                character("c"),
                Modifiers::CTRL | Modifiers::SHIFT,
                Action::Copy,
            ),
            (
                character("v"),
                Modifiers::CTRL | Modifiers::SHIFT,
                Action::Paste,
            ),
            (Key::Named(Named::Insert), Modifiers::SHIFT, Action::Paste),
        ] {
            for alternate in [false, true] {
                let event = press(key.clone(), key.clone(), mods, None, false);
                assert!(matches!(
                    on_key_screen(&event, alternate),
                    Some(Message::Action(action)) if action == expected
                ));
                let event = press(key.clone(), key.clone(), mods, None, true);
                assert!(on_key_screen(&event, alternate).is_none());
            }
        }
        let event = press(character("c"), character("c"), Modifiers::CTRL, None, false);
        let Some(Message::Keys(keys)) = on_key(&event) else {
            panic!("plain Ctrl+C must reach the shell");
        };
        assert_eq!(
            keys.into_iter()
                .flat_map(term_core::terminal::encode)
                .collect::<Vec<_>>(),
            [3]
        );
    }

    #[test]
    fn scroll_chords_and_repeats_never_reach_the_shell() {
        use application::iced::keyboard::{Key, Modifiers, key::Named};
        use term_core::terminal::ScrollRequest;
        for (named, request) in [
            (Named::PageUp, ScrollRequest::PageUp),
            (Named::PageDown, ScrollRequest::PageDown),
            (Named::Home, ScrollRequest::Top),
            (Named::End, ScrollRequest::Bottom),
        ] {
            for repeat in [false, true] {
                assert!(matches!(
                    on_key(&press(Key::Named(named), Key::Named(named), Modifiers::SHIFT, None, repeat)),
                    Some(Message::Action(Action::Scroll(actual))) if actual == request
                ));
                assert!(matches!(
                    on_key(&press(
                        Key::Named(named),
                        Key::Named(named),
                        Modifiers::empty(),
                        None,
                        repeat
                    )),
                    Some(Message::Keys(_))
                ));
                assert!(matches!(
                    on_key_screen(
                        &press(
                            Key::Named(named),
                            Key::Named(named),
                            Modifiers::SHIFT,
                            None,
                            repeat
                        ),
                        true
                    ),
                    Some(Message::Keys(_))
                ));
            }
        }
    }

    #[test]
    fn pointer_motion_emits_only_for_a_new_pane_or_cell() {
        let pointer = std::cell::Cell::new(None);
        let last = std::cell::Cell::new(None);
        assert!(
            pointer_message(
                &pointer,
                &last,
                Some((1, 0, 0)),
                application::iced::Point::new(2.0, 2.0)
            )
            .is_some()
        );
        for pixel in 3..8 {
            assert!(
                pointer_message(
                    &pointer,
                    &last,
                    Some((1, 0, 0)),
                    application::iced::Point::new(pixel as f32, 2.0)
                )
                .is_none()
            );
        }
        assert_eq!(pointer.get(), Some(application::iced::Point::new(7.0, 2.0)));
        // A narrower cell after zoom/layout uses the newest pixel, not x=2.
        assert_eq!(
            input::pointer_cell(pointer.get().unwrap(), 0.0, (4.0, 4.0), (80, 24)),
            (1, 0)
        );
        assert!(
            pointer_message(
                &pointer,
                &last,
                Some((1, 1, 0)),
                application::iced::Point::new(9.0, 2.0)
            )
            .is_some()
        );
        assert!(
            pointer_message(
                &pointer,
                &last,
                Some((2, 1, 0)),
                application::iced::Point::new(90.0, 2.0)
            )
            .is_some()
        );
        assert!(pointer_message(&pointer, &last, None, application::iced::Point::ORIGIN).is_some());
        assert!(pointer_message(&pointer, &last, None, application::iced::Point::ORIGIN).is_none());
    }

    /// `on_key` is the dispatcher that decides chord versus shell and
    /// filters repeats; review finding: nothing exercised it.
    #[test]
    fn the_dispatcher_puts_chords_before_the_shell_and_filters_repeats() {
        use application::iced::keyboard::Modifiers;
        let ctrl_shift = Modifiers::CTRL | Modifiers::SHIFT;
        let tab = |repeat| {
            on_key(&press(
                character("t"),
                character("T"),
                ctrl_shift,
                Some("T"),
                repeat,
            ))
        };

        assert!(matches!(tab(false), Some(Message::Action(Action::NewTab))));
        // A held Ctrl+Shift+T is one tab: the repeat is swallowed, and it is
        // NOT handed to the encoder, which would send Ctrl-T to the shell.
        assert!(
            tab(true).is_none(),
            "a repeated tab chord must be swallowed"
        );

        // Font steps repeat when held, as in foot.
        let grow = |repeat| {
            on_key(&press(
                character("="),
                character("="),
                Modifiers::CTRL,
                None,
                repeat,
            ))
        };
        assert!(matches!(
            grow(false),
            Some(Message::Action(Action::FontIncrease))
        ));
        assert!(matches!(
            grow(true),
            Some(Message::Action(Action::FontIncrease))
        ));

        // Not a chord: the shell gets it, repeats included.
        for repeat in [false, true] {
            assert!(matches!(
                on_key(&press(character("a"), character("a"), Modifiers::empty(), Some("a"), repeat)),
                Some(Message::Keys(keys)) if keys.len() == 1
            ));
        }
        // Ctrl+T without Shift is the shell's Ctrl-T, not a chord.
        assert!(matches!(
            on_key(&press(
                character("t"),
                character("t"),
                Modifiers::CTRL,
                None,
                false
            )),
            Some(Message::Keys(_))
        ));
        // Modifier state is tracked for Ctrl+wheel.
        assert!(matches!(
            on_key(&application::iced::keyboard::Event::ModifiersChanged(Modifiers::CTRL)),
            Some(Message::Modifiers(modifiers)) if modifiers.control()
        ));
    }

    #[test]
    fn the_focus_ring_shows_only_when_there_is_more_than_one_pane() {
        let lone = Shape {
            tabs: Vec::new(),
            tree: Some(Node::Leaf(7)),
            active_pane: 7,
        };
        assert!(!show_focus_ring(&lone, 7));

        let split = Shape {
            tree: Some(Node::Split {
                dir: SplitDir::Vertical,
                ratio: 0.5,
                first: Box::new(Node::Leaf(7)),
                second: Box::new(Node::Leaf(8)),
            }),
            ..lone
        };
        assert!(show_focus_ring(&split, 7));
        assert!(!show_focus_ring(&split, 8));
    }

    /// A real `State` with a PTY-backed tab set, no window and no Bus: the
    /// settings session owns an explicit fixture binding, and the adapter
    /// handle is a sink, so no live lane is behind the fixture.
    fn test_state() -> (State, std::thread::JoinHandle<()>) {
        let (cleanup, reaper) = tabs::Cleanup::start().expect("cleanup worker");
        let waker = Arc::new(Waker {
            fd: WakeFd::new().expect("eventfd"),
            pending: AtomicBool::new(false),
            sender: Mutex::new(None),
            polling: AtomicBool::new(false),
        });
        let tabs = Arc::new(Mutex::new(layout::test_tabs()));
        tabs.lock().unwrap().set_wake(waker.fd.waker());
        let consumer = settings::consumer::Consumer::for_app(
            settings::Binding {
                instance: "fixture".into(),
                profile: "default".into(),
            },
            "term",
        )
        .unwrap();
        let painter = Painter::for_test(1.0, FontSize::new(13.0), core_config::Cursor::Underline)
            .expect("a monospace font");
        let raster = painter.bootstrap_raster();
        let local = LocalContext::new(1.0, core_config::Cursor::Underline).unwrap();
        let (settings, lane) = application::presentation::native::bridge(
            application::presentation::native::Session::with_context(consumer, local),
            application::presentation::native::Worker::contextual_with_host(
                move |appearance, snapshot, local| {
                    presentation::prepare(appearance, snapshot, local, &raster)
                },
                appearance::resources::ResourceHost::new(assets::Lookup::new()),
            ),
        );
        let bootstrap = appearance::settings::bootstrap().unwrap();
        let ui = bootstrap.typography().get("ui").expect("UI typography");
        let (describe_tx, describes) = tokio::sync::mpsc::channel(32);
        drop(describe_tx); // no adapter behind the fixture
        let state = State {
            painter,
            tabs,
            cleanup,
            notify: None,
            tokens: bootstrap.tokens(),
            ui,
            chrome: layout::strip_height(1.0, ui),
            settings,
            frames: application::frames::Handle::new(),
            local,
            baseline: 13.0,
            applied_raster: None,
            applied_context: None,
            fixture_lane: Some(lane),
            bus: Handle::sink(),
            describes,
            waker,
            window: Size::new(900.0, 560.0),
            shape: Shape::default(),
            grids: HashMap::new(),
            modifiers: application::iced::keyboard::Modifiers::empty(),
            right_shift: std::cell::Cell::new(false),
            wheel: 0.0,
            scroll_wheel: 0.0,
            scroll_pane: None,
            pointer: std::cell::Cell::new(None),
            mouse: clipboard::MouseState::default(),
            paste_notice: None,
            last_redraw: None,
            ime_preedit: None,
            ime: ime::Composition::default(),
            keyboard_focus: true,
            force_paint: false,
            paint_requested: true,
        };
        (state, reaper)
    }

    /// Complete the actual contextual worker and activate through State::sync.
    /// No direct builder call or elapsed-time assumption substitutes for it.
    fn finish_preparation(state: &mut State) {
        static FONTS: std::sync::Once = std::sync::Once::new();
        FONTS.call_once(|| {
            toolkit::fonts::install(
                toolkit::fonts::FontSet::new().sans(
                    include_bytes!("../../../vendor/font/Inter-VariableFont_opsz,wght.ttf")
                        .as_slice(),
                ),
                None,
            )
            .unwrap();
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut lane = state.fixture_lane.take().expect("real fixture lane");
        runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while !state.settings.preparation_evidence().current {
                    assert_ne!(
                        lane.drive().await,
                        application::presentation::native::Progress::UiClosed
                    );
                    let _ = state.sync();
                    assert!(
                        state.settings.preparation_evidence().fault.is_none(),
                        "{:?}",
                        state.settings.preparation_evidence().fault
                    );
                }
            })
            .await
            .expect("contextual preparation did not finish");
        });
        state.fixture_lane = Some(lane);
    }

    /// The design boundary, colour side: a token change restyles the chrome
    /// on the next redraw without touching PTY geometry or any pane's raster.
    #[test]
    fn colour_changes_restyle_without_reflow_or_raster_replacement() {
        use term_core::terminal::Terminal;
        let (mut state, reaper) = test_state();
        let _ = state.sync();
        let _ = state.act(Action::Split(SplitDir::Vertical));
        let _ = state.sync();
        let visible = state.shape.visible();
        assert_eq!(visible.len(), 2);
        // Quiet fixture content, so no PTY damage races the assertions.
        for pane in &visible {
            let terminal = state.tabs.lock().unwrap().pane_by_id(*pane).unwrap();
            *terminal.lock().unwrap() = Terminal::from_test_vt(8, 3, b"abcdefgh");
        }
        let _ = update(&mut state, Message::Paint(std::time::Instant::now()));
        let generation = |state: &State, id| {
            state
                .painter
                .existing(id)
                .unwrap()
                .lock()
                .unwrap()
                .generation()
        };
        let grids = state.grids.clone();
        let force_paint = state.force_paint;
        let before = [
            generation(&state, visible[0]),
            generation(&state, visible[1]),
        ];
        let mut tokens = state.tokens;
        tokens.palette.surface = tokens.palette.text;
        state.apply_chrome(state.ui, tokens);
        assert_eq!(
            state.grids, grids,
            "a colour change must not reflow the PTYs"
        );
        assert_eq!(
            state.force_paint, force_paint,
            "a colour change must not replace the raster"
        );
        let _ = update(&mut state, Message::Paint(std::time::Instant::now()));
        assert_eq!(
            [
                generation(&state, visible[0]),
                generation(&state, visible[1])
            ],
            before,
            "a colour change must not repaint any pane"
        );
        let removed = state.tabs.lock().unwrap().shutdown();
        state.cleanup.submit(removed);
        drop(state);
        reaper.join().unwrap();
    }

    /// The design boundary, extent side. The window is first tuned so the pane
    /// interior sits mid-bucket (rows·cell + cell/2), which makes every leg
    /// exact whatever the cell metrics: a sub-cell growth moves the pixel
    /// chrome without moving the integer rows (the PTY grid quantises into
    /// cells), a growth crossing the cell boundary resizes each PTY and its
    /// content follows, and repeating the same extent resizes nothing.
    #[test]
    fn extent_changes_relayout_once_and_unchanged_extents_stay_put() {
        use term_core::terminal::Terminal;
        let (mut state, reaper) = test_state();
        let _ = state.sync();
        let _ = state.act(Action::Split(SplitDir::Vertical));
        let _ = state.sync();
        let visible = state.shape.visible();
        assert_eq!(visible.len(), 2);
        // Quiet fixture content, so no PTY damage races the assertions.
        for pane in &visible {
            let terminal = state.tabs.lock().unwrap().pane_by_id(*pane).unwrap();
            *terminal.lock().unwrap() = Terminal::from_test_vt(8, 3, b"abcdefgh");
        }
        // Mid-bucket window: interior = rows·cell + cell/2, so a growth of up
        // to half a cell provably stays inside the same quantisation bucket.
        let scale = state.painter.scale();
        let ch = state.painter.logical_cell().1;
        let rows = f32::from(state.grids[&visible[0]].1);
        let interior = rows * ch + ch / 2.0;
        state.resize(Size::new(
            state.window.width,
            state.chrome + 2.0 * layout::border(scale) + interior,
        ));
        assert_eq!(
            state.grids[&visible[0]].1 as f32, rows,
            "the tuned window stays in the same row bucket"
        );
        let base = state.ui.line_height.unwrap_or(state.ui.size * 1.4);
        let grow = |ui: &mut toolkit::typography::TextStyle, delta: f32| {
            ui.line_height = Some(base + delta);
        };

        // A sub-cell growth moves the pixel extent only: the chrome value
        // changes while every pane's integer rows stay put.
        let grids = state.grids.clone();
        let chrome = state.chrome;
        let mut ui = state.ui;
        grow(&mut ui, ch / 4.0);
        state.apply_chrome(ui, state.tokens);
        assert!(
            state.chrome > chrome,
            "a sub-cell extent change must move the pixel chrome"
        );
        assert_eq!(
            state.grids, grids,
            "a sub-cell extent change stays inside the row bucket"
        );

        // Crossing the cell boundary resizes each PTY once, and the pane
        // content follows the new grid.
        let grids = state.grids.clone();
        grow(&mut ui, 2.0 * ch);
        state.apply_chrome(ui, state.tokens);
        for id in &visible {
            assert!(
                state.grids[id].1 < grids[id].1,
                "crossing the cell boundary must shrink pane {id}"
            );
            let terminal = state.tabs.lock().unwrap().pane_by_id(*id).unwrap();
            assert_eq!(
                terminal.lock().unwrap().screen(false).rows,
                state.grids[id].1 as usize,
                "pane {id} content must follow the relaid-out grid"
            );
        }

        // The same extent again: no resize.
        let settled = state.grids.clone();
        state.apply_chrome(ui, state.tokens);
        assert_eq!(
            state.grids, settled,
            "an unchanged extent must not relayout"
        );
        let removed = state.tabs.lock().unwrap().shutdown();
        state.cleanup.submit(removed);
        drop(state);
        reaper.join().unwrap();
    }

    /// The stored chrome extent follows a rescale: after `set_scale` succeeds
    /// the strip is recomputed at the new scale BEFORE the reflow, so the
    /// pane area and the IME origin read the same extent the live view
    /// computes. No settings wake is involved — nothing may hide a stale
    /// extent behind a later reconcile.
    #[test]
    fn a_rescale_updates_the_stored_chrome_before_reflow() {
        let (mut state, reaper) = test_state();
        let _ = state.sync();
        finish_preparation(&mut state);
        state.rescale(2.0);
        assert_eq!(
            state.painter.scale(),
            1.0,
            "old complete raster remains active while preparing"
        );
        finish_preparation(&mut state);
        let scale = state.painter.scale();
        assert_eq!(scale, 2.0);
        let live = layout::strip_height(scale, state.ui);
        assert_eq!(
            state.chrome, live,
            "the stored extent follows the new scale"
        );
        let bounds = layout::content(state.window.width, state.window.height, state.chrome);
        assert_eq!(
            bounds.y, live,
            "the pane and IME origin starts below the live strip extent"
        );
        for id in state.shape.visible() {
            assert!(
                state.grids.contains_key(&id),
                "pane {id} was relaid out at the new scale"
            );
        }
        let removed = state.tabs.lock().unwrap().shutdown();
        state.cleanup.submit(removed);
        drop(state);
        reaper.join().unwrap();
    }

    #[test]
    fn ime_preedit_is_local_and_commit_sends_one_complete_sequence() {
        use application::iced::advanced::input_method::Event;
        let (mut state, reaper) = test_state();
        let _ = state.sync();
        let terminal = state.tabs.lock().unwrap().active_terminal();
        *terminal.lock().unwrap() = term_core::terminal::Terminal::from_test_vt(8, 3, b"");
        let read = terminal.lock().unwrap().listener.test_input_reader();
        let text = "👩‍💻🇦🇺👍🏽❤️e\u{301}";
        let _ = update(&mut state, Message::Ime(Event::Opened));
        let _ = update(
            &mut state,
            Message::Ime(Event::Preedit(text.into(), Some(0..text.len()))),
        );
        assert!(state.ime_preedit.is_some());
        assert_eq!(read(), None);
        let _ = update(
            &mut state,
            Message::Ime(Event::Commit(format!("\u{1b}{text}\u{3}"))),
        );
        assert_eq!(read().as_deref(), Some(text.as_bytes()));
        assert_eq!(read(), None);
        assert!(state.ime_preedit.is_none());
        state.right_shift.set(true);
        let _ = update(
            &mut state,
            Message::Window(application::iced::window::Event::Unfocused),
        );
        assert!(!state.right_shift.get());
        let _ = update(&mut state, Message::Ime(Event::Commit(text.into())));
        assert_eq!(read(), None);
        let removed = state.tabs.lock().unwrap().shutdown();
        state.cleanup.submit(removed);
        drop(state);
        reaper.join().unwrap();
    }

    #[test]
    fn tab_uses_live_panes_after_a_tab_switch_without_a_layout_wake() {
        use term_core::{panes::SplitDir, terminal::Terminal};
        let (mut state, reaper) = test_state();
        let _ = state.sync();
        let _ = state.act(Action::Split(SplitDir::Vertical));
        let _ = state.sync();
        assert_eq!(state.shape.visible().len(), 2);
        let _ = state.act(Action::NewTab);
        let terminal = state.tabs.lock().unwrap().active_terminal();
        *terminal.lock().unwrap() = Terminal::from_test_vt(8, 3, b"");
        let read = terminal.lock().unwrap().listener.test_input_reader();
        // Shape still describes the split tab, but this tab has one pane.
        for repeat in [false, true] {
            let _ = update(
                &mut state,
                Message::Tab {
                    forward: true,
                    repeat,
                },
            );
            assert_eq!(read().as_deref(), Some(&b"\t"[..]));
        }
        let _ = state.sync();
        assert_eq!(state.shape.visible().len(), 1);
        let _ = state.act(Action::Cycle { forward: false });
        let ids: Vec<_> = state
            .tabs
            .lock()
            .unwrap()
            .leaves()
            .iter()
            .map(|pane| pane.id)
            .collect();
        assert_eq!(ids.len(), 2);
        let before = state.tabs.lock().unwrap().active_tab().active_pane;
        let _ = update(
            &mut state,
            Message::Tab {
                forward: true,
                repeat: false,
            },
        );
        let after = state.tabs.lock().unwrap().active_tab().active_pane;
        assert_eq!(Some(after), input::cycle_pane(&ids, before, true));
        assert_ne!(after, before);
        let _ = update(
            &mut state,
            Message::Tab {
                forward: true,
                repeat: true,
            },
        );
        assert_eq!(state.tabs.lock().unwrap().active_tab().active_pane, after);
        let _ = update(
            &mut state,
            Message::Tab {
                forward: false,
                repeat: false,
            },
        );
        assert_eq!(state.tabs.lock().unwrap().active_tab().active_pane, before);
        assert!(read().is_none());
        let removed = state.tabs.lock().unwrap().shutdown();
        let _ = update(
            &mut state,
            Message::Tab {
                forward: true,
                repeat: false,
            },
        );
        state.cleanup.submit(removed);
        drop(terminal);
        drop(state);
        reaper.join().unwrap();
    }

    #[test]
    fn ime_focus_switch_and_owner_close_drop_queued_commit_before_wake() {
        use application::iced::advanced::input_method::Event;
        use term_core::terminal::Terminal;
        for change in ["pane", "tab", "close"] {
            let (mut state, reaper) = test_state();
            let _ = state.sync();
            let (owner, original_tab, first, second, second_id) = {
                let mut tabs = state.tabs.lock().unwrap();
                let owner = tabs.active_tab().active_pane;
                let original_tab = tabs.active_id();
                let first = tabs.active_terminal();
                let second_id = tabs.split_active(SplitDir::Vertical).unwrap();
                let second = tabs.active_terminal();
                tabs.focus(owner);
                (owner, original_tab, first, second, second_id)
            };
            *first.lock().unwrap() = Terminal::from_test_vt(8, 3, b"");
            *second.lock().unwrap() = Terminal::from_test_vt(8, 3, b"");
            let read_first = first.lock().unwrap().listener.test_input_reader();
            let _ = update(&mut state, Message::Ime(Event::Opened));
            let _ = update(
                &mut state,
                Message::Ime(Event::Preedit("draft".into(), None)),
            );
            // Same mutation routes used by the Bus, without delivering Wake.
            let target = {
                let mut tabs = state.tabs.lock().unwrap();
                match change {
                    "pane" => {
                        assert!(tabs.focus(second_id));
                    }
                    "tab" => {
                        tabs.open().unwrap();
                    }
                    _ => {
                        let removed = tabs.close_active().1.unwrap();
                        state.cleanup.submit(vec![removed]);
                    }
                }
                tabs.active_terminal()
            };
            if change == "tab" {
                *target.lock().unwrap() = Terminal::from_test_vt(8, 3, b"");
            }
            let read_target = target.lock().unwrap().listener.test_input_reader();
            let _ = update(&mut state, Message::Ime(Event::Commit("stale".into())));
            assert_eq!(read_first(), None, "{change}");
            assert_eq!(read_target(), None, "{change}");
            assert!(state.ime_preedit.is_none());
            assert!(!state.ime.enabled());
            let _ = update(&mut state, Message::Ime(Event::Closed));
            let _ = update(&mut state, Message::Ime(Event::Opened));
            let _ = update(&mut state, Message::Ime(Event::Preedit("new".into(), None)));
            let _ = update(&mut state, Message::Ime(Event::Commit("new".into())));
            assert_eq!(read_target().as_deref(), Some(b"new".as_slice()));
            // Explicit tab selection must clear preedit before its Wake too.
            if change == "tab" {
                let _ = update(
                    &mut state,
                    Message::Ime(Event::Preedit("again".into(), None)),
                );
                let _ = update(&mut state, Message::SelectTab(original_tab));
                assert!(!state.ime.enabled());
                assert!(state.ime_preedit.is_none());
                assert_eq!(state.tabs.lock().unwrap().active_tab().active_pane, owner);
            }
            let removed = state.tabs.lock().unwrap().shutdown();
            state.cleanup.submit(removed);
            drop(state);
            reaper.join().unwrap();
        }
    }

    #[test]
    fn mouse_drag_uses_event_positions_and_release_outside_the_pane() {
        use application::iced::{
            Point,
            mouse::{Button, Event},
        };
        use term_core::terminal::Terminal;
        let (mut state, reaper) = test_state();
        let _ = state.sync();
        let id = state.shape.active_pane;
        let terminal = state.tabs.lock().unwrap().pane_by_id(id).unwrap();
        *terminal.lock().unwrap() = Terminal::from_test_vt(8, 3, b"abcdefgh\r\nijklmnop");
        state
            .grids
            .insert(id, PtyExtent::new((8, 3), state.painter.cell()));
        let (cw, ch) = state.painter.logical_cell();
        let border = layout::border(state.painter.scale());
        let top = layout::strip_height(state.painter.scale(), state.ui) + border;
        let start = Point::new(border + cw * 0.1, top + ch * 0.5);
        let end = Point::new(border + cw * 3.9, start.y);
        let events = clipboard::MouseEvents::new(&state);
        // Coalesced hover is deliberately ahead of the queued button press.
        state.pointer.set(Some(end));
        let press = events
            .message(&state, &Event::ButtonPressed(Button::Left), Some(start))
            .unwrap();
        let motion = events
            .message(&state, &Event::CursorMoved { position: end }, Some(end))
            .unwrap();
        let _ = update(&mut state, press);
        let _ = update(&mut state, motion);
        assert_eq!(
            terminal.lock().unwrap().selection_text().as_deref(),
            Some("abcd")
        );
        // Moving back to the original anchor clears the range again.
        let _ = state.mouse_event(
            Event::CursorMoved { position: start },
            start,
            Instant::now(),
        );
        assert_eq!(terminal.lock().unwrap().selection_text(), None);
        // An unmatched release must not lose the original grab.
        assert!(
            events
                .message(&state, &Event::ButtonReleased(Button::Right), Some(end))
                .is_none()
        );
        let outside = Point::new(state.window.width + 20.0, start.y);
        let release = events
            .message(&state, &Event::ButtonReleased(Button::Left), Some(outside))
            .unwrap();
        let primary = update(&mut state, release);
        assert!(primary.units() > 0, "a completed selection writes PRIMARY");
        assert_eq!(
            terminal.lock().unwrap().selection_text().as_deref(),
            Some("abcdefgh")
        );
        assert!(state.clipboard_action(Action::Copy).units() > 0);

        let later = Instant::now() + std::time::Duration::from_secs(1);
        let _ = state.mouse_event(Event::ButtonPressed(Button::Left), start, later);
        assert_eq!(
            state
                .mouse_event(Event::ButtonReleased(Button::Left), start, later)
                .units(),
            0
        );
        assert_eq!(terminal.lock().unwrap().selection_text(), None);
        assert_eq!(state.clipboard_action(Action::Copy).units(), 0);
        let removed = state.tabs.lock().unwrap().shutdown();
        state.cleanup.submit(removed);
        drop(terminal);
        drop(state);
        reaper.join().unwrap();
    }

    #[test]
    fn cancelled_reported_gestures_release_the_original_pane_once() {
        use application::iced::{
            Point,
            mouse::{Button, Event},
        };
        use term_core::terminal::Terminal;
        let (mut state, reaper) = test_state();
        let _ = state.sync();
        let original = state.shape.active_pane;
        let original_tab = state.tabs.lock().unwrap().active_id();
        let terminal = state.tabs.lock().unwrap().pane_by_id(original).unwrap();
        *terminal.lock().unwrap() = Terminal::from_test_vt(8, 3, b"\x1b[?1002;1006h");
        let input = terminal.lock().unwrap().listener.test_input_reader();
        state
            .grids
            .insert(original, PtyExtent::new((8, 3), state.painter.cell()));
        let (cw, ch) = state.painter.logical_cell();
        let border = layout::border(state.painter.scale());
        let start = Point::new(
            border + cw * 0.1,
            layout::strip_height(state.painter.scale(), state.ui) + border + ch * 0.5,
        );
        let end = Point::new(start.x + cw * 3.0, start.y + ch);
        let begin = |state: &mut State| {
            let _ = state.mouse_event(Event::ButtonPressed(Button::Left), start, Instant::now());
            assert_eq!(input().unwrap(), b"\x1b[<0;1;1M");
            let _ = state.mouse_event(Event::CursorMoved { position: end }, end, Instant::now());
            assert_eq!(input().unwrap(), b"\x1b[<32;4;2M");
        };
        begin(&mut state);
        state.modifiers = application::iced::keyboard::Modifiers::SHIFT;
        let _ = update(
            &mut state,
            Message::Window(application::iced::window::Event::Unfocused),
        );
        assert_eq!(
            input().unwrap(),
            b"\x1b[<0;4;2m",
            "release uses press modifiers and last cell"
        );
        let _ = state.mouse_event(Event::ButtonReleased(Button::Left), start, Instant::now());
        assert!(input().is_none(), "late release is not duplicated");

        // Both supported and unknown new buttons terminate the stale grab.
        for button in [Button::Left, Button::Right, Button::Other(8)] {
            begin(&mut state);
            let events = clipboard::MouseEvents::new(&state);
            let next = events
                .message(&state, &Event::ButtonPressed(button), Some(start))
                .unwrap();
            let _ = update(&mut state, next);
            assert_eq!(input().unwrap(), b"\x1b[<0;4;2m");
            if button != Button::Other(8) {
                let code = if button == Button::Left { 0 } else { 2 };
                assert_eq!(input().unwrap(), format!("\x1b[<{code};1;1M").as_bytes());
                let release = events
                    .message(&state, &Event::ButtonReleased(button), Some(start))
                    .unwrap();
                let _ = update(&mut state, release);
                assert_eq!(input().unwrap(), format!("\x1b[<{code};1;1m").as_bytes());
            }
            assert!(input().is_none());
        }

        begin(&mut state);
        let tree = state.shape.tree.take();
        let _ = state.mouse_event(Event::ButtonReleased(Button::Left), start, Instant::now());
        assert_eq!(
            input().unwrap(),
            b"\x1b[<0;4;2m",
            "missing hit still releases"
        );
        state.shape.tree = tree;

        // A tab switch releases immediately, before the next layout sync.
        let second = state.tabs.lock().unwrap().open().unwrap();
        state.tabs.lock().unwrap().select(original_tab);
        begin(&mut state);
        let _ = update(&mut state, Message::SelectTab(second));
        assert_eq!(input().unwrap(), b"\x1b[<0;4;2m");
        state.tabs.lock().unwrap().select(original_tab);

        // External tab mutations are noticed by sync as well.
        begin(&mut state);
        state.tabs.lock().unwrap().select(second);
        let _ = state.sync();
        assert_eq!(input().unwrap(), b"\x1b[<0;4;2m");
        state.tabs.lock().unwrap().select(original_tab);
        let _ = state.sync();
        begin(&mut state);
        // Remove from the tree before sync: Press retains the terminal until
        // its release, rather than trying to resolve the now-absent pane ID.
        let removed = state.tabs.lock().unwrap().close_active().1.unwrap();
        let _ = state.sync();
        assert_eq!(input().unwrap(), b"\x1b[<0;4;2m");
        state.cleanup.submit(vec![removed]);
        state.cancel_mouse_gesture();
        assert!(input().is_none());
        let removed = state.tabs.lock().unwrap().shutdown();
        state.cleanup.submit(removed);
        drop(terminal);
        drop(state);
        reaper.join().unwrap();
    }

    #[test]
    fn cancelled_local_drag_clears_and_a_new_pane_selection_is_exclusive() {
        use application::iced::{
            Point,
            mouse::{Button, Event},
        };
        use term_core::terminal::{SelectionSide, SelectionType, Terminal};
        let (mut state, reaper) = test_state();
        let _ = state.sync();
        let first = state.shape.active_pane;
        let second = state
            .tabs
            .lock()
            .unwrap()
            .split_active(SplitDir::Vertical)
            .unwrap();
        let _ = state.sync();
        let terminals: Vec<_> = [first, second]
            .into_iter()
            .map(|id| {
                let terminal = state.tabs.lock().unwrap().pane_by_id(id).unwrap();
                *terminal.lock().unwrap() = Terminal::from_test_vt(8, 3, b"abcdefgh");
                state
                    .grids
                    .insert(id, PtyExtent::new((8, 3), state.painter.cell()));
                terminal
            })
            .collect();
        terminals[0].lock().unwrap().selection_start(
            0,
            0,
            SelectionSide::Left,
            SelectionType::Lines,
        );
        assert!(terminals[0].lock().unwrap().selection_text().is_some());
        let scale = state.painter.scale();
        let bounds = layout::content(state.window.width, state.window.height, state.chrome);
        let rect = layout::panes(state.shape.tree.as_ref().unwrap(), bounds, scale)
            .into_iter()
            .find(|(id, _)| *id == second)
            .unwrap()
            .1;
        let (cw, ch) = state.painter.logical_cell();
        let start = Point::new(
            rect.x + layout::border(scale) + cw * 0.1,
            rect.y + layout::border(scale) + ch * 0.5,
        );
        let end = Point::new(start.x + cw * 3.8, start.y);
        let _ = state.mouse_event(Event::ButtonPressed(Button::Left), start, Instant::now());
        assert_eq!(terminals[0].lock().unwrap().selection_text(), None);
        let _ = state.mouse_event(Event::CursorMoved { position: end }, end, Instant::now());
        assert_eq!(
            terminals[1].lock().unwrap().selection_text().as_deref(),
            Some("abcd")
        );
        let _ = update(
            &mut state,
            Message::Window(application::iced::window::Event::Unfocused),
        );
        assert_eq!(terminals[1].lock().unwrap().selection_text(), None);
        assert_eq!(
            state
                .mouse_event(Event::ButtonReleased(Button::Left), end, Instant::now())
                .units(),
            0
        );
        let removed = state.tabs.lock().unwrap().shutdown();
        state.cleanup.submit(removed);
        drop(terminals);
        drop(state);
        reaper.join().unwrap();
    }

    #[test]
    fn oversized_paste_has_a_visible_notice_and_sends_nothing() {
        use term_core::terminal::Terminal;
        let (mut state, reaper) = test_state();
        let _ = state.sync();
        let id = state.shape.active_pane;
        let terminal = state.tabs.lock().unwrap().pane_by_id(id).unwrap();
        *terminal.lock().unwrap() = Terminal::from_test_vt(8, 3, b"");
        let input = terminal.lock().unwrap().listener.test_input_reader();
        state.paste(id, Some("x".repeat(16 * 1024 * 1024 + 1)));
        assert_eq!(
            state.paste_notice.as_deref(),
            Some("Paste exceeds 16 MiB limit")
        );
        assert!(input().is_none());
        state.paste(id, Some("ok".into()));
        assert_eq!(input().unwrap(), b"ok");
        assert!(state.paste_notice.is_none());
        let removed = state.tabs.lock().unwrap().shutdown();
        state.cleanup.submit(removed);
        drop(terminal);
        drop(state);
        reaper.join().unwrap();
    }

    #[test]
    fn mouse_reporting_owns_buttons_unless_shift_started_the_gesture() {
        use application::iced::{
            Point,
            keyboard::Modifiers,
            mouse::{Button, Event},
        };
        use term_core::terminal::Terminal;
        let (mut state, reaper) = test_state();
        let _ = state.sync();
        let id = state.shape.active_pane;
        let terminal = state.tabs.lock().unwrap().pane_by_id(id).unwrap();
        // No PTY sender: a failed report must still NEVER fall back to paste.
        *terminal.lock().unwrap() = Terminal::from_test_vt(8, 3, b"\x1b[?9hword");
        state
            .grids
            .insert(id, PtyExtent::new((8, 3), state.painter.cell()));
        let position = Point::new(
            4.0,
            layout::strip_height(state.painter.scale(), state.ui) + 4.0,
        );
        let now = Instant::now();
        assert_eq!(
            state
                .mouse_event(Event::ButtonPressed(Button::Middle), position, now)
                .units(),
            0
        );
        assert_eq!(
            state
                .mouse_event(Event::ButtonReleased(Button::Middle), position, now)
                .units(),
            0
        );
        let _ = state.mouse_event(Event::ButtonPressed(Button::Left), position, now);
        let _ = state.mouse_event(Event::ButtonReleased(Button::Left), position, now);
        assert_eq!(terminal.lock().unwrap().selection_text(), None);
        state.modifiers = Modifiers::SHIFT;
        assert!(
            state
                .mouse_event(Event::ButtonPressed(Button::Middle), position, now)
                .units()
                > 0
        );
        let _ = state.mouse_event(Event::ButtonReleased(Button::Middle), position, now);
        let _ = state.mouse_event(Event::ButtonPressed(Button::Left), position, now);
        // Releasing Shift during this local gesture must not hand it to the app.
        state.modifiers = Modifiers::empty();
        let end = Point::new(
            position.x + state.painter.logical_cell().0 * 3.8,
            position.y,
        );
        assert!(
            state
                .mouse_event(Event::ButtonReleased(Button::Left), end, now)
                .units()
                > 0
        );
        assert!(terminal.lock().unwrap().selection_text().is_some());
        let removed = state.tabs.lock().unwrap().shutdown();
        state.cleanup.submit(removed);
        drop(terminal);
        drop(state);
        reaper.join().unwrap();
    }

    /// Review finding: a zoom used to relayout and then only WAKE, so the
    /// next `view` laid the new grid size over the old surface — one
    /// stretched frame per step. After the pre-draw paint, every visible frame
    /// must be exactly its grid in the new cell size.
    #[test]
    fn a_zoom_repaints_before_the_next_draw() {
        let (mut state, reaper) = test_state();
        assert!(frame_binding(&state).is_none(), "bootstrap has no installed stamp");
        let _ = state.sync();
        finish_preparation(&mut state);
        let installed = frame_binding(&state).expect("actual settings activation");
        let _ = state.act(Action::Split(SplitDir::Vertical));
        let _ = state.sync();
        assert_eq!(state.shape.visible().len(), 2);

        let before = state.painter.cell();
        state.zoom(|font| font.step_by(6));
        let pending = frame_binding(&state).unwrap();
        assert!(installed.same_presentation(&pending), "pending context retains the drawn stamp and owner");
        assert_eq!(
            state.painter.cell(),
            before,
            "pending zoom retains the applied raster"
        );
        finish_preparation(&mut state);
        let replaced = frame_binding(&state).unwrap();
        assert_ne!(replaced.stamp, installed.stamp);
        assert!(replaced.observer.same_owner(&installed.observer));
        let _ = update(&mut state, Message::Paint(std::time::Instant::now()));
        let cell = state.painter.cell();
        assert_ne!(cell, before, "six steps must change the cell");
        for id in state.shape.visible() {
            let (cols, rows) = state.grids[&id].grid();
            let frame = state
                .painter
                .existing(id)
                .expect("a visible pane has a frame");
            let frame = frame.lock().unwrap();
            assert_eq!(
                (frame.surface().width(), frame.surface().height()),
                (u32::from(cols) * cell.0, u32::from(rows) * cell.1),
                "pane {id} still holds the pre-zoom surface"
            );
        }

        let removed = state.tabs.lock().unwrap().shutdown();
        state.cleanup.submit(removed);
        drop(state);
        reaper.join().unwrap();
    }

    #[test]
    fn wakes_coalesce_at_redraw_and_leave_the_clean_neighbour_untouched() {
        let (mut state, reaper) = test_state();
        let _ = state.sync();
        let _ = state.act(Action::Split(SplitDir::Vertical));
        let _ = state.sync();
        let ids = state.shape.visible();
        assert_eq!(ids.len(), 2);
        // Let startup output settle, bounded by a deadline; consume it only
        // at redraws, exactly as the running app does.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut quiet = 0;
        while quiet < 5 {
            assert!(
                std::time::Instant::now() < deadline,
                "PTY startup did not settle"
            );
            let _ = update(&mut state, Message::Paint(std::time::Instant::now()));
            std::thread::sleep(std::time::Duration::from_millis(10));
            let tabs = state.tabs.lock().unwrap();
            let changed = ids
                .iter()
                .any(|id| tabs.pane_by_id(*id).unwrap().lock().unwrap().take_damage());
            quiet = if changed { 0 } else { quiet + 1 };
            if changed {
                state.force_paint = true;
            }
        }
        let generation = |state: &State, id| {
            state
                .painter
                .existing(id)
                .unwrap()
                .lock()
                .unwrap()
                .generation()
        };
        assert!(
            !state.needs_paint(),
            "clean chrome redraws must not request Paint"
        );
        let _ = update(&mut state, Message::Wake);
        assert!(state.needs_paint(), "a wake must arm the next redraw");
        let _ = update(&mut state, Message::Paint(std::time::Instant::now()));
        assert!(!state.needs_paint(), "painting consumes the request");
        let before = [generation(&state, ids[0]), generation(&state, ids[1])];
        let terminal = state.tabs.lock().unwrap().pane_by_id(ids[0]).unwrap();
        let (cols, rows) = state.grids[&ids[0]].grid();
        terminal.lock().unwrap().resize(cols - 1, rows, 0, 0);
        for _ in 0..20 {
            let _ = update(&mut state, Message::Wake);
        }
        assert_eq!(
            [generation(&state, ids[0]), generation(&state, ids[1])],
            before,
            "wakes must not paint intermediate states"
        );
        let at = std::time::Instant::now();
        let _ = update(&mut state, Message::Paint(at));
        assert_eq!(generation(&state, ids[0]), before[0] + 1);
        assert_eq!(
            generation(&state, ids[1]),
            before[1],
            "clean neighbour repainted"
        );
        let _ = update(&mut state, Message::Paint(at));
        assert_eq!(
            generation(&state, ids[0]),
            before[0] + 1,
            "redraw retry painted twice"
        );
        let neighbour = state.tabs.lock().unwrap().pane_by_id(ids[1]).unwrap();
        assert!(
            neighbour
                .lock()
                .unwrap()
                .grid_snapshot()
                .dirty_rows
                .iter()
                .all(|dirty| !dirty),
            "unchanged cursor dirtied the clean neighbour"
        );
        let removed = state.tabs.lock().unwrap().shutdown();
        state.cleanup.submit(removed);
        drop(state);
        reaper.join().unwrap();
    }

    /// Review finding: wheel travel short of a step survived letting go of
    /// Ctrl, so the next Ctrl+wheel gesture zoomed early.
    #[test]
    fn releasing_ctrl_forgets_partial_wheel_travel() {
        use application::iced::keyboard::Modifiers;
        use application::iced::mouse::ScrollDelta;
        let (mut state, reaper) = test_state();
        let _ = state.sync();
        finish_preparation(&mut state);
        let start = state.painter.font().current();
        let travel = |fraction: f32| {
            Message::Wheel(
                0,
                ScrollDelta::Pixels {
                    x: 0.0,
                    y: input::PIXELS_PER_STEP * fraction,
                },
            )
        };

        let _ = update(&mut state, Message::Modifiers(Modifiers::CTRL));
        let _ = update(&mut state, travel(0.75));
        assert_eq!(
            state.painter.font().current(),
            start,
            "three quarters is not a step"
        );
        let _ = update(&mut state, Message::Modifiers(Modifiers::empty()));
        let _ = update(&mut state, Message::Modifiers(Modifiers::CTRL));
        let _ = update(&mut state, travel(0.5));
        assert_eq!(
            state.painter.font().current(),
            start,
            "a new gesture of half a step zoomed: the old three quarters carried over"
        );
        let _ = update(&mut state, travel(0.5));
        assert_eq!(state.local.zoom_steps, 1);
        finish_preparation(&mut state);
        assert_ne!(
            state.painter.font().current(),
            start,
            "a whole step within one gesture zooms"
        );

        let removed = state.tabs.lock().unwrap().shutdown();
        state.cleanup.submit(removed);
        drop(state);
        reaper.join().unwrap();
    }

    #[test]
    fn wheel_targets_the_hovered_pane_without_focus_or_zoom_travel_leaking() {
        use application::iced::keyboard::Modifiers;
        use application::iced::mouse::ScrollDelta;
        let (mut state, reaper) = test_state();
        let _ = state.sync();
        let left = state.shape.active_pane;
        let _ = state.act(Action::Split(SplitDir::Vertical));
        let _ = state.sync();
        let right = state.shape.active_pane;
        assert_ne!(left, right);
        let terminal = state.tabs.lock().unwrap().pane_by_id(left).unwrap();
        fill_history(&terminal);
        state
            .pointer
            .set(Some(application::iced::Point::new(10.0, 40.0)));
        let half = ScrollDelta::Pixels {
            x: 0.0,
            y: state.painter.logical_cell().1 / 2.0,
        };
        let _ = update(&mut state, Message::Wheel(left, half));
        assert_eq!(state.scroll_pane, Some(left));
        assert_eq!(state.scroll_wheel, 0.5);
        assert_eq!(active_pane(&state.tabs.lock().unwrap()), right);
        let _ = update(&mut state, Message::Wheel(left, half));
        assert_eq!(
            terminal.lock().unwrap().display_offset(),
            1,
            "two half-cell deltas scroll the hovered pane"
        );
        let other = state.tabs.lock().unwrap().pane_by_id(right).unwrap();
        assert_eq!(other.lock().unwrap().display_offset(), 0);
        assert_eq!(active_pane(&state.tabs.lock().unwrap()), right);
        let _ = update(
            &mut state,
            Message::Wheel(left, ScrollDelta::Lines { x: 0.0, y: -1.0 }),
        );
        assert_eq!(terminal.lock().unwrap().display_offset(), 0);
        let _ = update(&mut state, Message::Modifiers(Modifiers::CTRL));
        let _ = update(
            &mut state,
            Message::Wheel(
                left,
                ScrollDelta::Pixels {
                    x: 0.0,
                    y: input::PIXELS_PER_STEP / 2.0,
                },
            ),
        );
        assert_eq!(state.wheel, 0.5);
        assert_eq!(state.scroll_wheel, 0.0);
        let _ = update(&mut state, Message::Modifiers(Modifiers::empty()));
        let _ = update(&mut state, Message::Wheel(left, half));
        assert_eq!(state.wheel, 0.0);
        assert_eq!(state.scroll_wheel, 0.5);
        // A different pane starts a new accumulation, even without movement
        // (for example a tab change beneath the pointer).
        let _ = update(&mut state, Message::Wheel(right, half));
        assert_eq!(state.scroll_pane, Some(right));
        assert_eq!(state.scroll_wheel, 0.5);
        let removed = state.tabs.lock().unwrap().shutdown();
        state.cleanup.submit(removed);
        drop(state);
        reaper.join().unwrap();
    }

    fn fill_history(terminal: &Arc<Mutex<term_core::terminal::Terminal>>) {
        let text = (0..120)
            .map(|n| format!("history-{n}\r\n"))
            .collect::<String>();
        let mut terminal = terminal.lock().unwrap();
        let screen = terminal.grid_snapshot().screen;
        // Replace the live pane with an isolated grid, keeping its dimensions.
        // Dropping the old terminal stops its reader; shell startup output can
        // never race the fixture or the subsequent pixel comparisons.
        *terminal =
            term_core::terminal::Terminal::from_test_vt(screen.cols, screen.rows, text.as_bytes());
    }

    #[test]
    fn viewport_redraw_repaints_fully_and_matches_fresh_pixels() {
        use term_core::terminal::ScrollRequest;
        let (mut state, reaper) = test_state();
        let _ = state.sync();
        let _ = state.act(Action::Split(SplitDir::Vertical));
        let _ = state.sync();
        let id = state.shape.active_pane;
        for pane in state.shape.visible() {
            let terminal = state.tabs.lock().unwrap().pane_by_id(pane).unwrap();
            fill_history(&terminal);
        }
        let terminal = state.tabs.lock().unwrap().pane_by_id(id).unwrap();
        let _ = update(&mut state, Message::Paint(std::time::Instant::now()));
        let neighbour = state
            .shape
            .visible()
            .into_iter()
            .find(|pane| *pane != id)
            .unwrap();
        let generation = |state: &State, pane| {
            state
                .painter
                .existing(pane)
                .unwrap()
                .lock()
                .unwrap()
                .generation()
        };
        let neighbour_generation = generation(&state, neighbour);
        state.pointer.set(Some(application::iced::Point::new(
            state.window.width - 10.0,
            40.0,
        )));
        for request in [
            ScrollRequest::PageUp,
            ScrollRequest::Top,
            ScrollRequest::Bottom,
        ] {
            assert!(!state.needs_paint());
            let before = generation(&state, id);
            // Wheel, keyboard action, and the core entry used by term.scroll.
            match request {
                ScrollRequest::PageUp => {
                    let _ = update(
                        &mut state,
                        Message::Wheel(
                            id,
                            application::iced::mouse::ScrollDelta::Lines { x: 0.0, y: 3.0 },
                        ),
                    );
                    assert!(state.needs_paint(), "wheel must arm redraw");
                }
                ScrollRequest::Top => {
                    let _ = update(&mut state, Message::Action(Action::Scroll(request)));
                    assert!(state.needs_paint(), "scroll chord must arm redraw");
                }
                _ => terminal.lock().unwrap().scroll_view(request),
            }
            // The same coalesced wake path serves Bus viewport changes.
            for _ in 0..3 {
                let _ = update(&mut state, Message::Wake);
            }
            assert!(state.needs_paint());
            assert_eq!(generation(&state, id), before, "wakes must not paint");
            let screen = terminal.lock().unwrap().screen(false);
            if request == ScrollRequest::Bottom {
                assert_eq!(terminal.lock().unwrap().display_offset(), 0);
                assert!(screen.cursor_visible);
            } else {
                assert!(terminal.lock().unwrap().display_offset() > 0);
                assert!(!screen.cursor_visible);
            }
            // Retain old CPU band handles across the viewport change.
            #[cfg(all(feature = "tiny-skia", not(feature = "wgpu")))]
            let _retained = cpu_grid::view(&state.painter.frame(id), state.painter.scale());
            let at = std::time::Instant::now();
            let _ = update(&mut state, Message::Paint(at));
            assert!(!state.needs_paint());
            assert_eq!(generation(&state, id), before + 1);
            assert_eq!(generation(&state, neighbour), neighbour_generation);
            let mut fresh = Painter::for_test(
                state.painter.scale(),
                state.painter.font(),
                core_config::Cursor::Underline,
            )
            .unwrap();
            fresh.repaint(id, &screen, &vec![true; screen.rows]);
            assert_eq!(
                state.painter.frame(id).lock().unwrap().surface().rgba(),
                fresh.frame(id).lock().unwrap().surface().rgba(),
                "the next redraw must repaint the whole viewport, including all bands",
            );
            let _ = update(&mut state, Message::Paint(at));
            assert_eq!(
                generation(&state, id),
                before + 1,
                "redraw retry painted twice"
            );
        }
        let removed = state.tabs.lock().unwrap().shutdown();
        state.cleanup.submit(removed);
        drop(state);
        reaper.join().unwrap();
    }

    fn active_pane(tabs: &TabSet) -> u64 {
        tabs.active_tab().active_pane
    }

    /// T3 parity at the model: each chord drives the tab set the way bterm's
    /// keyboard handler does, on a real PTY-backed `TabSet`.
    #[test]
    fn chords_drive_the_tab_set_like_bterm() {
        let mut tabs = layout::test_tabs();
        let first_tab = tabs.active_id();
        let left = active_pane(&tabs);

        assert!(apply(&mut tabs, Action::Split(SplitDir::Vertical)).is_empty());
        assert_eq!(tabs.leaves().len(), 2);
        let right = active_pane(&tabs);
        assert_ne!(right, left, "a split focuses the new pane");

        apply(&mut tabs, Action::CyclePane { forward: true });
        assert_eq!(active_pane(&tabs), left);
        apply(&mut tabs, Action::CyclePane { forward: false });
        assert_eq!(active_pane(&tabs), right);
        assert_eq!(tabs.active_id(), first_tab);

        apply(&mut tabs, Action::Focus(Direction::Left));
        assert_eq!(active_pane(&tabs), left);
        apply(&mut tabs, Action::Focus(Direction::Right));
        assert_eq!(active_pane(&tabs), right);

        apply(&mut tabs, Action::NewTab);
        assert_eq!(tabs.list().len(), 2);
        assert_ne!(tabs.active_id(), first_tab, "a new tab is selected");
        apply(&mut tabs, Action::Cycle { forward: true });
        assert_eq!(
            tabs.active_id(),
            first_tab,
            "cycling wraps back to the first tab"
        );

        let removed = apply(&mut tabs, Action::ClosePane);
        assert_eq!(removed.len(), 1, "the closed pane's terminal is torn down");
        assert_eq!(tabs.leaves().len(), 1);
        assert_eq!(active_pane(&tabs), left, "focus falls to the sibling");

        let removed = apply(&mut tabs, Action::CloseTab);
        assert_eq!(removed.len(), 1);
        assert_eq!(tabs.list().len(), 1);

        assert!(
            apply(&mut tabs, Action::FontIncrease).is_empty(),
            "not a tab operation"
        );
        assert_eq!(tabs.list().len(), 1);

        let removed = apply(&mut tabs, Action::Quit);
        assert!(!removed.is_empty());
        assert!(tabs.is_empty());
        assert!(
            apply(&mut tabs, Action::NewTab).is_empty(),
            "nothing opens after quit"
        );
        assert!(tabs.is_empty());
    }
}
