//! testkit-input-probe: a minimal SHM toplevel that echoes the seat input it
//! receives.
//!
//! Gate client for compd's input and window verbs. Maps one xdg toplevel
//! (title, app id and size from the arguments), follows the compositor's
//! configure size, redraws on every frame callback with presentation
//! feedback, and prints one `PROBE ...` line per event:
//!
//! ```text
//! PROBE configure <w> <h>          PROBE enter <x> <y>
//! PROBE motion <x> <y>             PROBE button <code> <0|1> <x> <y>
//! PROBE key <evdev> <0|1>          PROBE keyboard_enter / keyboard_leave
//! PROBE presented <count>          PROBE close
//! ```
//!
//! Runs until `--seconds` elapse or the compositor closes it. With
//! `--hide-on-close` a close request unmaps the window (a null buffer)
//! instead, like a tray app, and prints `PROBE hidden`; with
//! `--remap-once-ms N` the first such hide is undone after N ms
//! (`PROBE remapped`).
//! `--translucent` uses premultiplied half-alpha ARGB with no opaque region;
//! the default XRGB buffer is opaque. `--ssd` requests server decorations.
//! `--colour RRGGBB` draws a fixed colour, premultiplied when translucent.
//! `--delay-size-commit WxH:MS` ACKs that size, retaining the old buffer for
//! the stated interval before committing the new one. Other sizes are immediate.
//! `--delay-state-commit WxH:MS` instead ACKs without any surface commit until
//! the delayed replacement buffer. Both delays require a resize of an already
//! mapped buffer and are bounded to 1..=60000 ms; the flags are mutually exclusive.
//! `--seats` lists and labels every seat's keyboard/pointer events.
//! `--popup` maps a persistent non-grabbing 20x20 xdg popup for tree visibility tests.
//! `--restore-overlay-on-leave` sends real xdg unset_fullscreen/unset_maximized
//! requests once, after an overlay configure and human keyboard focus loss.
//! `--idle-timeout-ms N` also enables this mode and subscribes to one
//! ext-idle-notify notification per seat. Seat names are whatever the
//! compositor advertises (compd: `seat0` for the human seat, `agent` for the
//! agent seat), quoted and escaped; e.g. `PROBE seat "agent" idled`.
//! `PROBE seats_ready` follows a sync after device/notification creation;
//! `PROBE seats_done` follows the final sync. Without either flag, the
//! single-seat bindings and output are used.
//!
//! `--version` / `-V` as the first argument prints the probe's name and
//! version and exits 0.

use std::{
    collections::BTreeMap,
    env,
    ffi::CString,
    fs::File,
    io::Write,
    os::{
        fd::{AsFd, AsRawFd, FromRawFd},
        unix::fs::FileExt,
    },
    process::ExitCode,
    time::{Duration, Instant},
};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
    protocol::{
        wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat,
        wl_shm, wl_shm_pool, wl_surface,
    },
};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1, ext_idle_notifier_v1,
};
use wayland_protocols::wp::presentation_time::client::{wp_presentation, wp_presentation_feedback};
use wayland_protocols::xdg::decoration::zv1::client::{
    zxdg_decoration_manager_v1, zxdg_toplevel_decoration_v1,
};
use wayland_protocols::xdg::shell::client::{
    xdg_popup, xdg_positioner, xdg_surface, xdg_toplevel, xdg_wm_base,
};

/// The program name: the window title, the memfd name and the `--version`
/// line.
const NAME: &str = "testkit-input-probe";

#[derive(Default)]
struct Probe {
    restore_overlay_on_leave: bool,
    overlay_fullscreen: bool,
    overlay_maximized: bool,
    toplevel: Option<xdg_toplevel::XdgToplevel>,
    all_seats: bool,
    idle_timeout_ms: Option<u32>,
    idle_notifier: Option<ext_idle_notifier_v1::ExtIdleNotifierV1>,
    seats: BTreeMap<u32, SeatProbe>,
    seat_error: Option<String>,
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    wm_base: Option<xdg_wm_base::XdgWmBase>,
    seat: Option<wl_seat::WlSeat>,
    presentation: Option<wp_presentation::WpPresentation>,
    decoration_manager: Option<zxdg_decoration_manager_v1::ZxdgDecorationManagerV1>,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    /// The latest unacknowledged configure: `(serial, width, height)`.
    pending_configure: Option<(u32, i32, i32)>,
    toplevel_size: (i32, i32),
    frame_done: bool,
    presented: u64,
    pointer_at: (f64, f64),
    closed: bool,
}

/// One seat's event lines. Events that arrive before the seat's name are
/// held back and emitted, in order, once the name is known.
#[derive(Default)]
struct SeatEvents {
    name: Option<String>,
    pending: Vec<String>,
}

impl SeatEvents {
    fn event(&mut self, event: String) -> Vec<String> {
        if let Some(name) = &self.name {
            vec![format!("seat {name:?} {event}")]
        } else {
            self.pending.push(event);
            Vec::new()
        }
    }

    fn named(&mut self, name: String) -> Vec<String> {
        self.name = Some(name);
        let pending = std::mem::take(&mut self.pending);
        pending
            .into_iter()
            .flat_map(|event| self.event(event))
            .collect()
    }
}

struct SeatProbe {
    seat: wl_seat::WlSeat,
    events: SeatEvents,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    notification: Option<ext_idle_notification_v1::ExtIdleNotificationV1>,
    pointer_at: (f64, f64),
    capabilities_received: bool,
    listed: bool,
}

impl SeatProbe {
    fn emit(&mut self, event: String) {
        for line in self.events.event(event) {
            say(&line);
        }
    }

    fn release(self) {
        if let Some(notification) = self.notification {
            notification.destroy();
        }
        if let Some(keyboard) = self.keyboard
            && keyboard.version() >= 3
        {
            keyboard.release();
        }
        if let Some(pointer) = self.pointer
            && pointer.version() >= 3
        {
            pointer.release();
        }
        if self.seat.version() >= 5 {
            self.seat.release();
        }
    }
}

impl Probe {
    fn restore_overlay(&mut self) {
        if !self.restore_overlay_on_leave || !(self.overlay_fullscreen || self.overlay_maximized) {
            return;
        }
        let Some(toplevel) = &self.toplevel else {
            return;
        };
        if self.overlay_fullscreen {
            toplevel.unset_fullscreen();
        }
        if self.overlay_maximized {
            toplevel.unset_maximized();
        }
        say("overlay_restore_requested");
        self.restore_overlay_on_leave = false;
    }
    fn prepare_seats(&mut self, qh: &QueueHandle<Self>) {
        for (id, seat) in &mut self.seats {
            if seat.notification.is_none()
                && let (Some(manager), Some(timeout)) = (&self.idle_notifier, self.idle_timeout_ms)
            {
                seat.notification =
                    Some(manager.get_idle_notification(timeout, &seat.seat, qh, *id));
            }
            if !seat.listed
                && seat.capabilities_received
                && seat.events.name.is_some()
                && (self.idle_timeout_ms.is_none() || seat.notification.is_some())
            {
                seat.emit(format!(
                    "bound keyboard={} pointer={} idle_timeout_ms={}",
                    u8::from(seat.keyboard.is_some()),
                    u8::from(seat.pointer.is_some()),
                    self.idle_timeout_ms
                        .map_or_else(|| "none".into(), |ms| ms.to_string())
                ));
                seat.listed = true;
            }
        }
    }
}

fn say(line: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "PROBE {line}");
    let _ = stdout.flush();
}

struct Options {
    restore_overlay_on_leave: bool,
    seats: bool,
    popup: bool,
    idle_timeout_ms: Option<u32>,
    title: String,
    app_id: String,
    width: i32,
    height: i32,
    duration: Duration,
    hide_on_close: bool,
    remap_once: Option<Duration>,
    translucent: bool,
    colour: Option<[u8; 3]>,
    ssd: bool,
    delay_size_commit: Option<(i32, i32, Duration)>,
    delay_state_commit: Option<(i32, i32, Duration)>,
}

fn options() -> Result<Options, String> {
    parse_options(env::args().skip(1))
}

fn parse_options(mut arguments: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        restore_overlay_on_leave: false,
        seats: false,
        popup: false,
        idle_timeout_ms: None,
        title: NAME.into(),
        app_id: "dev.mixos.InputProbe".into(),
        width: 320,
        height: 240,
        duration: Duration::from_secs(30),
        hide_on_close: false,
        remap_once: None,
        translucent: false,
        colour: None,
        ssd: false,
        delay_size_commit: None,
        delay_state_commit: None,
    };
    while let Some(argument) = arguments.next() {
        let mut value = || {
            arguments
                .next()
                .ok_or_else(|| format!("{argument} requires a value"))
        };
        match argument.as_str() {
            "--seats" => options.seats = true,
            "--popup" => options.popup = true,
            "--restore-overlay-on-leave" => options.restore_overlay_on_leave = true,
            "--idle-timeout-ms" => {
                options.idle_timeout_ms = Some(
                    value()?
                        .parse::<u32>()
                        .map_err(|error| format!("--idle-timeout-ms: {error}"))?,
                );
            }
            "--title" => options.title = value()?,
            "--app-id" => options.app_id = value()?,
            "--size" => {
                let size = value()?;
                let (width, height) = size
                    .split_once('x')
                    .ok_or_else(|| "--size expects WxH".to_string())?;
                options.width = width.parse().map_err(|error| format!("--size: {error}"))?;
                options.height = height.parse().map_err(|error| format!("--size: {error}"))?;
            }
            "--seconds" => {
                options.duration = Duration::from_secs(
                    value()?
                        .parse()
                        .map_err(|error| format!("--seconds: {error}"))?,
                );
            }
            "--hide-on-close" => options.hide_on_close = true,
            "--translucent" => options.translucent = true,
            "--colour" => options.colour = Some(parse_colour(&value()?)?),
            "--ssd" => options.ssd = true,
            "--delay-size-commit" | "--delay-state-commit" => {
                if options.delay_size_commit.is_some() || options.delay_state_commit.is_some() {
                    return Err("only one delayed commit flag is allowed".into());
                }
                let input = value()?;
                let (size, millis) = input
                    .split_once(':')
                    .ok_or("delayed commit expects WxH:MS")?;
                let (width, height) = size
                    .split_once('x')
                    .ok_or("delayed commit expects WxH:MS")?;
                let width: i32 = width.parse().map_err(|_| "invalid delayed width")?;
                let height: i32 = height.parse().map_err(|_| "invalid delayed height")?;
                let millis: u64 = millis.parse().map_err(|_| "invalid delayed milliseconds")?;
                if width <= 0 || height <= 0 || millis == 0 || millis > 60_000 {
                    return Err("delayed size must be positive, interval 1..=60000 ms".into());
                }
                let delay = Some((width, height, Duration::from_millis(millis)));
                if argument == "--delay-state-commit" {
                    options.delay_state_commit = delay;
                } else {
                    options.delay_size_commit = delay;
                }
            }
            "--remap-once-ms" => {
                options.remap_once = Some(Duration::from_millis(
                    value()?
                        .parse()
                        .map_err(|error| format!("--remap-once-ms: {error}"))?,
                ));
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if options.width <= 0 || options.height <= 0 {
        return Err("size must be positive".into());
    }
    Ok(options)
}

fn parse_colour(value: &str) -> Result<[u8; 3], String> {
    if value.len() != 6 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("--colour expects RRGGBB, got {value:?}"));
    }
    let hex = u32::from_str_radix(value, 16).map_err(|error| error.to_string())?;
    Ok([(hex >> 16) as u8, (hex >> 8) as u8, hex as u8])
}

impl Options {
    /// A delay never holds the initial mapping or an unchanged-size configure.
    /// The bool selects the existing ACK-with-old-buffer commit policy.
    fn commit_delay(
        &self,
        current: Option<(i32, i32)>,
        target: (i32, i32),
    ) -> Option<(Duration, bool)> {
        let current = current?;
        if current == target {
            return None;
        }
        let (delay, commit_old) = self
            .delay_state_commit
            .map(|delay| (delay, false))
            .or_else(|| self.delay_size_commit.map(|delay| (delay, true)))?;
        ((delay.0, delay.1) == target).then_some((delay.2, commit_old))
    }
    /// SHM's existing little-endian XRGB/ARGB byte order is BGRA.
    fn pixel(&self, frame: u32) -> [u8; 4] {
        if let Some([red, green, blue]) = self.colour {
            if self.translucent {
                [
                    (u32::from(blue) * 128 / 255) as u8,
                    (u32::from(green) * 128 / 255) as u8,
                    (u32::from(red) * 128 / 255) as u8,
                    0x80,
                ]
            } else {
                [blue, green, red, 0xff]
            }
        } else {
            let shade = (frame % 256) as u8;
            if self.translucent {
                [shade / 2, 0x40, (255 - shade) / 2, 0x80]
            } else {
                [shade, 0x80, 255 - shade, 0xff]
            }
        }
    }
}

/// One shared-memory buffer of a given size.
struct Canvas {
    backing: File,
    buffer: wl_buffer::WlBuffer,
    width: i32,
    height: i32,
}

fn canvas(
    shm: &wl_shm::WlShm,
    qh: &QueueHandle<Probe>,
    width: i32,
    height: i32,
    translucent: bool,
) -> Result<Canvas, String> {
    let bytes = (width * height * 4) as usize;
    let name = CString::new(NAME).unwrap();
    // SAFETY: name is valid and the successful descriptor is owned below.
    let raw = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if raw < 0 {
        return Err(format!(
            "memfd_create failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: memfd_create returned a new owned descriptor.
    let backing = unsafe { File::from_raw_fd(raw) };
    backing
        .set_len(bytes as u64)
        .map_err(|error| error.to_string())?;
    let pool = shm.create_pool(backing.as_fd(), bytes as i32, qh, ());
    let format = if translucent {
        wl_shm::Format::Argb8888
    } else {
        wl_shm::Format::Xrgb8888
    };
    let buffer = pool.create_buffer(0, width, height, width * 4, format, qh, ());
    pool.destroy();
    Ok(Canvas {
        backing,
        buffer,
        width,
        height,
    })
}

fn wait_readable(queue: &mut EventQueue<Probe>, timeout: Duration) -> Result<(), String> {
    queue
        .flush()
        .map_err(|error| format!("flush failed: {error}"))?;
    let Some(guard) = queue.prepare_read() else {
        return Ok(());
    };
    let mut descriptor = libc::pollfd {
        fd: guard.connection_fd().as_raw_fd(),
        events: libc::POLLIN | libc::POLLERR | libc::POLLHUP,
        revents: 0,
    };
    let timeout_ms = timeout.as_millis().clamp(1, i32::MAX as u128) as libc::c_int;
    // SAFETY: descriptor is valid writable storage for one pollfd and the
    // guard keeps the connection fd alive during poll.
    let ready = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
    if ready <= 0 {
        return Ok(());
    }
    if descriptor.revents & libc::POLLIN != 0 {
        guard
            .read()
            .map_err(|error| format!("read failed: {error}"))?;
        Ok(())
    } else if descriptor.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
        Err("compositor disconnected".into())
    } else {
        Ok(())
    }
}

fn run() -> Result<(), String> {
    let options = options()?;
    let connection = Connection::connect_to_env()
        .map_err(|error| format!("failed to connect to Wayland: {error}"))?;
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    let _registry = connection.display().get_registry(&qh, ());
    let mut probe = Probe {
        restore_overlay_on_leave: options.restore_overlay_on_leave,
        all_seats: options.seats || options.idle_timeout_ms.is_some(),
        idle_timeout_ms: options.idle_timeout_ms,
        ..Probe::default()
    };
    queue
        .roundtrip(&mut probe)
        .map_err(|error| format!("registry roundtrip failed: {error}"))?;
    let compositor = probe
        .compositor
        .clone()
        .ok_or("wl_compositor unavailable")?;
    let shm = probe.shm.clone().ok_or("wl_shm unavailable")?;
    let wm_base = probe.wm_base.clone().ok_or("xdg_wm_base unavailable")?;
    if probe.all_seats {
        if let Some(error) = probe.seat_error.take() {
            return Err(error);
        }
        if probe.seats.is_empty() {
            return Err("wl_seat unavailable".into());
        }
        if probe.idle_timeout_ms.is_some() && probe.idle_notifier.is_none() {
            return Err("--idle-timeout-ms requires ext_idle_notifier_v1".into());
        }
    } else {
        probe.seat.as_ref().ok_or("wl_seat unavailable")?;
    }
    queue
        .roundtrip(&mut probe)
        .map_err(|error| format!("seat roundtrip failed: {error}"))?;
    if probe.all_seats {
        // Device and idle-notification requests were emitted by the seat
        // callbacks. Sync those requests before announcing readiness.
        queue
            .roundtrip(&mut probe)
            .map_err(|error| format!("seat setup sync failed: {error}"))?;
        if let Some(error) = probe.seat_error.take() {
            return Err(error);
        }
        if probe.seats.values().any(|seat| !seat.listed) {
            return Err("seat missing name or capabilities".into());
        }
        say("seats_ready");
    }

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    probe.toplevel = Some(toplevel.clone());
    toplevel.set_title(options.title.clone());
    toplevel.set_app_id(options.app_id.clone());
    let _decoration = if options.ssd {
        let decoration = probe
            .decoration_manager
            .as_ref()
            .ok_or("--ssd requires zxdg_decoration_manager_v1")?
            .get_toplevel_decoration(&toplevel, &qh, ());
        decoration.set_mode(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        Some(decoration)
    } else {
        None
    };
    surface.commit();

    let deadline = Instant::now() + options.duration;
    let mut current: Option<Canvas> = None;
    let mut frame = 0_u32;
    probe.frame_done = true;
    let mut hidden = false;
    // After a remap nothing may be attached until the new configure is
    // acknowledged.
    let mut awaiting_configure = false;
    let mut remap_at: Option<Instant> = None;
    let mut remap_left = options.remap_once;
    let mut delayed_commit: Option<(i32, i32, Instant)> = None;
    // Keep the real non-grabbing popup and its shm backing alive with the root.
    let mut popup = None;
    while Instant::now() < deadline && (!probe.closed || options.hide_on_close) {
        if let Some(error) = probe.seat_error.take() {
            return Err(error);
        }
        if probe.closed && !hidden {
            probe.closed = false;
            hidden = true;
            delayed_commit = None;
            surface.attach(None, 0, 0);
            surface.commit();
            say("hidden");
            if let Some(after) = remap_left.take() {
                remap_at = Some(Instant::now() + after);
            }
        }
        if hidden && remap_at.is_some_and(|at| Instant::now() >= at) {
            remap_at = None;
            hidden = false;
            awaiting_configure = true;
            // A fresh initial commit; the configure it earns re-attaches.
            surface.commit();
            say("remapped");
        }
        if hidden {
            probe.pending_configure = None;
            wait_readable(&mut queue, Duration::from_millis(20))?;
            queue
                .dispatch_pending(&mut probe)
                .map_err(|error| format!("dispatch failed: {error}"))?;
            continue;
        }
        queue
            .dispatch_pending(&mut probe)
            .map_err(|error| format!("dispatch failed: {error}"))?;
        let mut dirty = false;
        if let Some((serial, width, height)) = probe.pending_configure.take() {
            xdg.ack_configure(serial);
            let width = if width > 0 { width } else { options.width };
            let height = if height > 0 { height } else { options.height };
            if delayed_commit.is_some_and(|(w, h, _)| (w, h) != (width, height)) {
                delayed_commit = None;
            }
            let resizing = current
                .as_ref()
                .is_none_or(|canvas| (canvas.width, canvas.height) != (width, height));
            let delay = options.commit_delay(
                current.as_ref().map(|canvas| (canvas.width, canvas.height)),
                (width, height),
            );
            if let Some((interval, commit_old)) = delay {
                if delayed_commit.is_none() {
                    delayed_commit = Some((width, height, Instant::now() + interval));
                    // The legacy delay commits ACKed state with the existing
                    // buffer. The state delay emits only the ACK until replacement.
                    if commit_old {
                        surface.commit();
                        say(&format!("deferred_commit {width} {height}"));
                    } else {
                        queue
                            .flush()
                            .map_err(|error| format!("flush held ACK failed: {error}"))?;
                        say(&format!("state_ack_held {width} {height} serial={serial}"));
                    }
                }
            } else if resizing {
                delayed_commit = None;
                current = Some(canvas(&shm, &qh, width, height, options.translucent)?);
                say(&format!("configure {width} {height}"));
                dirty = true;
            } else if delayed_commit.is_none() {
                dirty = true;
            }
            awaiting_configure = false;
        }
        if let Some((width, height, at)) = delayed_commit
            && Instant::now() >= at
        {
            delayed_commit = None;
            current = Some(canvas(&shm, &qh, width, height, options.translucent)?);
            say(&format!("configure {width} {height}"));
            dirty = true;
        }
        if let Some(canvas) = current.as_ref()
            && delayed_commit.is_none()
            && (dirty || (probe.frame_done && !awaiting_configure))
        {
            frame = frame.wrapping_add(1);
            let pixel = options.pixel(frame);
            let pixels = pixel.repeat((canvas.width * canvas.height) as usize);
            canvas
                .backing
                .write_all_at(&pixels, 0)
                .map_err(|error| error.to_string())?;
            surface.attach(Some(&canvas.buffer), 0, 0);
            surface.damage_buffer(0, 0, canvas.width, canvas.height);
            surface.frame(&qh, ());
            if let Some(presentation) = &probe.presentation {
                presentation.feedback(&surface, &qh, ());
            }
            surface.commit();
            if options.popup && popup.is_none() {
                // Process the parent's initial buffer before the popup commit.
                queue
                    .roundtrip(&mut probe)
                    .map_err(|error| error.to_string())?;
                let popup_surface = compositor.create_surface(&qh, ());
                let popup_xdg = wm_base.get_xdg_surface(&popup_surface, &qh, true);
                let positioner = wm_base.create_positioner(&qh, ());
                positioner.set_size(20, 20);
                positioner.set_anchor_rect(40, 40, 1, 1);
                let role = popup_xdg.get_popup(Some(&xdg), &positioner, &qh, ());
                positioner.destroy();
                popup_surface.commit();
                queue
                    .roundtrip(&mut probe)
                    .map_err(|error| error.to_string())?;
                let backing = self::canvas(&shm, &qh, 20, 20, false)?;
                backing
                    .backing
                    .write_all_at(&options.pixel(frame).repeat(400), 0)
                    .map_err(|error| error.to_string())?;
                popup_surface.attach(Some(&backing.buffer), 0, 0);
                popup_surface.damage_buffer(0, 0, 20, 20);
                popup_surface.commit();
                popup = Some((role, popup_xdg, popup_surface, backing));
                say("popup_mapped");
            }
            if options.delay_state_commit.is_some() && dirty {
                say(&format!("buffer_commit {} {}", canvas.width, canvas.height));
            }
            probe.frame_done = false;
        }
        wait_readable(&mut queue, Duration::from_millis(50))?;
    }
    if probe.all_seats {
        queue
            .roundtrip(&mut probe)
            .map_err(|error| format!("final seat sync failed: {error}"))?;
        say("seats_done");
    }
    if probe.closed {
        say("close");
    }
    say(&format!("presented {}", probe.presented));
    say("exit");
    if let Some((role, xdg, surface, _backing)) = popup {
        role.destroy();
        xdg.destroy();
        surface.destroy();
    }
    toplevel.destroy();
    let _ = queue.roundtrip(&mut probe);
    Ok(())
}

/// `--version` / `-V` as argv[1] only: `--title` and `--app-id` take free
/// strings with no `--` escape, so a value spelled `--version` must reach the
/// parser, not this check.
fn version_requested() -> bool {
    matches!(env::args().nth(1).as_deref(), Some("--version" | "-V"))
}

fn main() -> ExitCode {
    if version_requested() {
        println!("{NAME} {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{NAME} failed: {error}");
            ExitCode::FAILURE
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Probe {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::GlobalRemove { name } = &event
            && state.all_seats
            && let Some(mut seat) = state.seats.remove(name)
        {
            seat.emit("removed".into());
            seat.release();
            return;
        }
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_compositor" => {
                state.compositor = Some(registry.bind(name, version.min(6), qh, ()));
            }
            "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
            "xdg_wm_base" => state.wm_base = Some(registry.bind(name, version.min(6), qh, ())),
            "zxdg_decoration_manager_v1" => {
                state.decoration_manager = Some(registry.bind(name, 1, qh, ()));
            }
            "wl_seat" if state.all_seats => {
                if version < 2 {
                    state.seat_error = Some("--seats requires wl_seat v2 names".into());
                    return;
                }
                state.seats.insert(
                    name,
                    SeatProbe {
                        seat: registry.bind(name, version.min(7), qh, name),
                        events: SeatEvents::default(),
                        keyboard: None,
                        pointer: None,
                        notification: None,
                        pointer_at: (0.0, 0.0),
                        capabilities_received: false,
                        listed: false,
                    },
                );
                state.prepare_seats(qh);
            }
            "wl_seat" => state.seat = Some(registry.bind(name, version.min(7), qh, ())),
            "ext_idle_notifier_v1" if state.idle_timeout_ms.is_some() => {
                state.idle_notifier = Some(registry.bind(name, 1, qh, ()));
                state.prepare_seats(qh);
            }
            "wp_presentation" => {
                state.presentation = Some(registry.bind(name, version.min(2), qh, ()));
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, u32> for Probe {
    fn event(
        state: &mut Self,
        _: &wl_seat::WlSeat,
        event: wl_seat::Event,
        id: &u32,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let Some(seat) = state.seats.get_mut(id) else {
            return;
        };
        match event {
            wl_seat::Event::Name { name } => {
                for line in seat.events.named(name) {
                    say(&line);
                }
            }
            wl_seat::Event::Capabilities {
                capabilities: WEnum::Value(capabilities),
            } => {
                seat.capabilities_received = true;
                if capabilities.contains(wl_seat::Capability::Keyboard) {
                    if seat.keyboard.is_none() {
                        seat.keyboard = Some(seat.seat.get_keyboard(qh, *id));
                    }
                } else if let Some(keyboard) = seat.keyboard.take()
                    && keyboard.version() >= 3
                {
                    keyboard.release();
                }
                if capabilities.contains(wl_seat::Capability::Pointer) {
                    if seat.pointer.is_none() {
                        seat.pointer = Some(seat.seat.get_pointer(qh, *id));
                    }
                } else if let Some(pointer) = seat.pointer.take()
                    && pointer.version() >= 3
                {
                    pointer.release();
                }
                seat.listed = false;
            }
            _ => {}
        }
        state.prepare_seats(qh);
    }
}

impl Dispatch<wl_pointer::WlPointer, u32> for Probe {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(seat) = state.seats.get_mut(id) else {
            return;
        };
        let line = match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            } => {
                seat.pointer_at = (surface_x, surface_y);
                format!("enter {surface_x} {surface_y}")
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                seat.pointer_at = (surface_x, surface_y);
                format!("motion {surface_x} {surface_y}")
            }
            wl_pointer::Event::Leave { .. } => "leave".into(),
            wl_pointer::Event::Button {
                button,
                state: pressed,
                ..
            } => {
                let pressed = matches!(pressed, WEnum::Value(wl_pointer::ButtonState::Pressed));
                format!(
                    "button {button} {} {} {}",
                    u8::from(pressed),
                    seat.pointer_at.0,
                    seat.pointer_at.1
                )
            }
            _ => return,
        };
        seat.emit(line);
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, u32> for Probe {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let leaving = matches!(&event, wl_keyboard::Event::Leave { .. });
        let Some(seat) = state.seats.get_mut(id) else {
            return;
        };
        let line = match event {
            wl_keyboard::Event::Enter { .. } => "keyboard_enter".into(),
            wl_keyboard::Event::Leave { .. } => "keyboard_leave".into(),
            wl_keyboard::Event::Key {
                key,
                state: pressed,
                ..
            } => {
                let pressed = matches!(pressed, WEnum::Value(wl_keyboard::KeyState::Pressed));
                format!("key {key} {}", u8::from(pressed))
            }
            _ => return,
        };
        seat.emit(line);
        if leaving && seat.events.name.as_deref() == Some("seat0") {
            state.restore_overlay();
        }
    }
}

impl Dispatch<ext_idle_notification_v1::ExtIdleNotificationV1, u32> for Probe {
    fn event(
        state: &mut Self,
        _: &ext_idle_notification_v1::ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(seat) = state.seats.get_mut(id) else {
            return;
        };
        match event {
            ext_idle_notification_v1::Event::Idled => seat.emit("idled".into()),
            ext_idle_notification_v1::Event::Resumed => seat.emit("resumed".into()),
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Probe {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        else {
            return;
        };
        if capabilities.contains(wl_seat::Capability::Pointer) && state.pointer.is_none() {
            state.pointer = Some(seat.get_pointer(qh, ()));
        }
        if capabilities.contains(wl_seat::Capability::Keyboard) && state.keyboard.is_none() {
            state.keyboard = Some(seat.get_keyboard(qh, ()));
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Probe {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer_at = (surface_x, surface_y);
                say(&format!("enter {surface_x} {surface_y}"));
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer_at = (surface_x, surface_y);
                say(&format!("motion {surface_x} {surface_y}"));
            }
            wl_pointer::Event::Leave { .. } => say("leave"),
            wl_pointer::Event::Button {
                button,
                state: button_state,
                ..
            } => {
                let pressed =
                    matches!(button_state, WEnum::Value(wl_pointer::ButtonState::Pressed));
                say(&format!(
                    "button {button} {} {} {}",
                    u8::from(pressed),
                    state.pointer_at.0,
                    state.pointer_at.1
                ));
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Probe {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { .. } => say("keyboard_enter"),
            wl_keyboard::Event::Leave { .. } => {
                say("keyboard_leave");
                state.restore_overlay();
            }
            wl_keyboard::Event::Key {
                key,
                state: pressed,
                ..
            } => {
                let pressed = matches!(pressed, WEnum::Value(wl_keyboard::KeyState::Pressed));
                say(&format!("key {key} {}", u8::from(pressed)));
            }
            _ => {}
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Probe {
    fn event(
        _: &mut Self,
        wm_base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

// Popup configure acknowledgement never changes the toplevel's resize plan.
impl Dispatch<xdg_surface::XdgSurface, bool> for Probe {
    fn event(
        _: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &bool,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
        }
    }
}

wayland_client::delegate_noop!(Probe: ignore xdg_popup::XdgPopup);
wayland_client::delegate_noop!(Probe: ignore xdg_positioner::XdgPositioner);

impl Dispatch<xdg_surface::XdgSurface, ()> for Probe {
    fn event(
        state: &mut Self,
        _: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            state.pending_configure = Some((serial, state.toplevel_size.0, state.toplevel_size.1));
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for Probe {
    fn event(
        state: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Configure {
                width,
                height,
                states,
            } => {
                state.toplevel_size = (width, height);
                let configured = |wanted: xdg_toplevel::State| {
                    states.chunks_exact(4).any(|bytes| {
                        u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
                            == wanted as u32
                    })
                };
                state.overlay_fullscreen = configured(xdg_toplevel::State::Fullscreen);
                state.overlay_maximized = configured(xdg_toplevel::State::Maximized);
            }
            xdg_toplevel::Event::Close => state.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for Probe {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.frame_done = true;
        }
    }
}

impl Dispatch<wp_presentation_feedback::WpPresentationFeedback, ()> for Probe {
    fn event(
        state: &mut Self,
        _: &wp_presentation_feedback::WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_presentation_feedback::Event::Presented { .. } = event {
            state.presented += 1;
            if state.presented == 1 {
                say("presented 1");
            }
        }
    }
}

macro_rules! ignore_events {
    ($($interface:ty),+ $(,)?) => {
        $(
            impl Dispatch<$interface, ()> for Probe {
                fn event(
                    _: &mut Self,
                    _: &$interface,
                    _: <$interface as wayland_client::Proxy>::Event,
                    _: &(),
                    _: &Connection,
                    _: &QueueHandle<Self>,
                ) {
                }
            }
        )+
    };
}

ignore_events!(
    ext_idle_notifier_v1::ExtIdleNotifierV1,
    wl_compositor::WlCompositor,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_buffer::WlBuffer,
    wl_surface::WlSurface,
    wp_presentation::WpPresentation,
    zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
    zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_restore_on_leave_is_explicit_and_has_no_effect_without_a_role() {
        let plain = parse_options(std::iter::empty()).unwrap();
        assert!(!plain.restore_overlay_on_leave);
        let enabled =
            parse_options(["--restore-overlay-on-leave".into(), "--seats".into()].into_iter())
                .unwrap();
        assert!(enabled.restore_overlay_on_leave && enabled.seats);
        let mut probe = Probe {
            restore_overlay_on_leave: true,
            ..Probe::default()
        };
        probe.restore_overlay();
        assert!(
            probe.restore_overlay_on_leave,
            "ordinary focus loss cannot consume the overlay test"
        );
        probe.overlay_fullscreen = true;
        probe.restore_overlay();
        assert!(
            probe.restore_overlay_on_leave,
            "no actual xdg role means no request or receipt"
        );
    }

    #[test]
    fn delayed_state_commit_is_strict_and_only_holds_a_mapped_resize() {
        let parse =
            |value: &str| parse_options(["--delay-state-commit".into(), value.into()].into_iter());
        for value in [
            "0x600:1",
            "800x-1:1",
            "800x600:0",
            "800x600:60001",
            "800x600",
            "800:1",
            "800x600:bad",
        ] {
            assert!(parse(value).is_err(), "{value}");
        }
        let options = parse("800x600:60000").unwrap();
        assert_eq!(
            options.commit_delay(None, (800, 600)),
            None,
            "initial mapping must not stall"
        );
        assert_eq!(
            options.commit_delay(Some((800, 600)), (800, 600)),
            None,
            "same-size state changes are immediate"
        );
        assert_eq!(options.commit_delay(Some((320, 240)), (640, 480)), None);
        assert_eq!(
            options.commit_delay(Some((320, 240)), (800, 600)),
            Some((Duration::from_secs(60), false)),
            "held state must not commit the old buffer"
        );
        let legacy =
            parse_options(["--delay-size-commit".into(), "800x600:1000".into()].into_iter())
                .unwrap();
        assert_eq!(
            legacy.commit_delay(Some((320, 240)), (800, 600)),
            Some((Duration::from_secs(1), true))
        );
        for flags in [
            ["--delay-size-commit", "--delay-state-commit"],
            ["--delay-state-commit", "--delay-size-commit"],
            ["--delay-state-commit", "--delay-state-commit"],
        ] {
            assert!(
                parse_options(
                    [
                        flags[0].into(),
                        "800x600:1".into(),
                        flags[1].into(),
                        "800x600:1".into()
                    ]
                    .into_iter()
                )
                .is_err()
            );
        }
    }

    #[test]
    fn delayed_size_commit_is_bounded_and_does_not_change_default_timing() {
        assert!(
            parse_options(std::iter::empty())
                .unwrap()
                .delay_size_commit
                .is_none()
        );
        let parse = |value: &str| {
            parse_options(["--delay-size-commit".to_string(), value.to_string()].into_iter())
        };
        assert_eq!(
            parse("1280x736:3000").unwrap().delay_size_commit,
            Some((1280, 736, Duration::from_secs(3)))
        );
        for value in [
            "0x736:3000",
            "1280x-1:3000",
            "1280x736:0",
            "1280x736:60001",
            "1280x736",
            "1280:3000",
            "x736:10",
            "1280x736:bad",
        ] {
            assert!(parse(value).is_err(), "{value}");
        }
    }

    #[test]
    fn default_options_do_not_enable_seat_observation() {
        let options = parse_options(std::iter::empty()).unwrap();
        assert!(!options.seats);
        assert_eq!(options.idle_timeout_ms, None);
        assert_eq!(options.duration, Duration::from_secs(30));
        assert_eq!(options.app_id, "dev.mixos.InputProbe");
    }

    #[test]
    fn fixed_colour_validates_hex_and_preserves_animation_and_bgra() {
        let parse = |args: &[&str]| parse_options(args.iter().map(|s| (*s).to_string()));
        let mut options = parse(&[]).unwrap();
        assert_eq!(options.pixel(0), [0, 128, 255, 255]);
        options.translucent = true;
        assert_eq!(options.pixel(0), [0, 64, 127, 128]);
        assert_eq!(options.pixel(255), [127, 64, 0, 128]);
        options = parse(&["--colour", "FF8000"]).unwrap();
        assert_eq!(options.pixel(19), [0, 128, 255, 255]);
        options.translucent = true;
        assert_eq!(options.pixel(27), [0, 64, 128, 128]);
        assert_eq!(
            parse(&["--colour", "12aBf0"]).unwrap().colour,
            Some([18, 171, 240])
        );
        for value in ["", "fff", "fffffff", "12 456", "gggggg", "éabcd"] {
            assert!(parse(&["--colour", value]).is_err(), "{value:?}");
        }
        assert!(parse(&["--colour"]).is_err());
    }

    #[test]
    fn seat_and_idle_options_are_independent_and_validate_u32_timeout() {
        let parse = |args: &[&str]| parse_options(args.iter().map(|s| (*s).to_string()));
        assert!(parse(&["--seats"]).unwrap().seats);
        assert_eq!(
            parse(&["--idle-timeout-ms", "3000"])
                .unwrap()
                .idle_timeout_ms,
            Some(3000)
        );
        assert_eq!(
            parse(&["--idle-timeout-ms", "0"]).unwrap().idle_timeout_ms,
            Some(0)
        );
        for args in [
            vec!["--idle-timeout-ms"],
            vec!["--idle-timeout-ms", "-1"],
            vec!["--idle-timeout-ms", "4294967296"],
            vec!["--idle-timeout-ms", "invalid"],
        ] {
            assert!(parse(&args).is_err());
        }
    }

    #[test]
    fn early_events_wait_for_their_own_seat_name_in_order() {
        let mut human = SeatEvents::default();
        let mut agent = SeatEvents::default();
        assert!(human.event("key 30 1".into()).is_empty());
        assert!(agent.event("idled".into()).is_empty());
        assert!(agent.event("resumed".into()).is_empty());
        assert_eq!(
            agent.named("agent".into()),
            ["seat \"agent\" idled", "seat \"agent\" resumed"]
        );
        assert_eq!(human.named("seat0".into()), ["seat \"seat0\" key 30 1"]);
        assert_eq!(human.event("idled".into()), ["seat \"seat0\" idled"]);
    }

    #[test]
    fn seat_names_cannot_inject_extra_output_lines() {
        let mut events = SeatEvents::default();
        let _ = events.named("name\n\"other\"".into());
        assert_eq!(
            events.event("idled".into()),
            ["seat \"name\\n\\\"other\\\"\" idled"]
        );
    }
}
