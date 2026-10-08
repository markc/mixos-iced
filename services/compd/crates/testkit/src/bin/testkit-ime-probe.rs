// SPDX-License-Identifier: MIT OR Apache-2.0
//! A real input-method-v2 keyboard grab with one finite protocol lifetime.
//! Usage: testkit-ime-probe HOLD_MS (100..=60000).
//! Readiness follows server keymap/repeat receipts and a display roundtrip;
//! release explicitly destroys the grab, then confirms another roundtrip.
//! No application control channel, synthetic seat state or input injection.
use std::{collections::BTreeMap, io::{self, Write}, os::fd::AsRawFd, time::{Duration, Instant}};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, protocol::{wl_registry, wl_seat}};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2::{self, ZwpInputMethodKeyboardGrabV2},
    zwp_input_method_manager_v2::{self, ZwpInputMethodManagerV2},
    zwp_input_method_v2::{self, ZwpInputMethodV2},
};

#[derive(Default)]
struct Probe {
    manager: Option<ZwpInputMethodManagerV2>,
    seats: BTreeMap<u32, (wl_seat::WlSeat, Option<String>)>,
    unavailable: bool,
    keymap: bool,
    repeat: bool,
}

fn say(message: &str) -> Result<(), String> {
    let mut output = io::stdout().lock();
    writeln!(output, "{message}").map_err(|error| error.to_string())?;
    output.flush().map_err(|error| error.to_string())
}

fn wait_readable(queue: &mut EventQueue<Probe>, remaining: Duration) -> Result<(), String> {
    queue.flush().map_err(|error| error.to_string())?;
    let Some(guard) = queue.prepare_read() else { return Ok(()); };
    let mut descriptor = libc::pollfd {
        fd: guard.connection_fd().as_raw_fd(),
        events: libc::POLLIN | libc::POLLERR | libc::POLLHUP,
        revents: 0,
    };
    let millis = remaining.as_millis().clamp(1, i32::MAX as u128) as libc::c_int;
    // SAFETY: the read guard owns the live fd, and descriptor is one valid
    // writable pollfd. This waits on that fd until one absolute deadline.
    let ready = unsafe { libc::poll(&mut descriptor, 1, millis) };
    if ready < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted { return Ok(()); }
        return Err(error.to_string());
    }
    if descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
        return Err("compositor disconnected".into());
    }
    if ready > 0 && descriptor.revents & libc::POLLIN != 0 {
        guard.read().map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn run() -> Result<(), String> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.len() != 1 { return Err("expected one bounded HOLD_MS argument".into()); }
    let hold_ms: u64 = arguments[0].parse().map_err(|_| "invalid HOLD_MS")?;
    if !(100..=60_000).contains(&hold_ms) { return Err("HOLD_MS outside 100..=60000".into()); }
    let connection = Connection::connect_to_env().map_err(|error| error.to_string())?;
    let mut queue = connection.new_event_queue();
    let handle = queue.handle();
    let _registry = connection.display().get_registry(&handle, ());
    let mut probe = Probe::default();
    queue.roundtrip(&mut probe).map_err(|error| error.to_string())?;
    queue.roundtrip(&mut probe).map_err(|error| error.to_string())?;
    let manager = probe.manager.as_ref().ok_or("no authorised input-method manager")?;
    let seats: Vec<_> = probe.seats.values().filter(|(_, name)| name.as_deref() == Some("seat0")).collect();
    if seats.len() != 1 { return Err("expected exactly one actual human seat0".into()); }
    let method = manager.get_input_method(&seats[0].0, &handle, ());
    queue.roundtrip(&mut probe).map_err(|error| error.to_string())?;
    if probe.unavailable { return Err("input method unavailable".into()); }
    let grab = method.grab_keyboard(&handle, ());
    queue.roundtrip(&mut probe).map_err(|error| error.to_string())?;
    if probe.unavailable || !probe.keymap || !probe.repeat {
        return Err("actual server grab receipts missing".into());
    }
    say(&format!("ime_grab_ready pid={} seat=seat0", std::process::id()))?;
    let deadline = Instant::now() + Duration::from_millis(hold_ms);
    loop {
        queue.dispatch_pending(&mut probe).map_err(|error| error.to_string())?;
        if probe.unavailable { return Err("input method retired while held".into()); }
        let now = Instant::now();
        if now >= deadline { break; }
        wait_readable(&mut queue, deadline - now)?;
    }
    grab.destroy();
    queue.roundtrip(&mut probe).map_err(|error| error.to_string())?;
    say(&format!("ime_grab_released pid={} seat=seat0", std::process::id()))?;
    method.destroy();
    queue.roundtrip(&mut probe).map_err(|error| error.to_string())?;
    Ok(())
}

impl Dispatch<wl_registry::WlRegistry, ()> for Probe {
    fn event(state: &mut Self, registry: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, handle: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_seat" => {
                    let seat = registry.bind(name, version.min(7), handle, name);
                    state.seats.insert(name, (seat, None));
                }
                "zwp_input_method_manager_v2" => {
                    state.manager = Some(registry.bind(name, version.min(1), handle, ()));
                }
                _ => {}
            }
        }
    }
}
impl Dispatch<wl_seat::WlSeat, u32> for Probe {
    fn event(state: &mut Self, _: &wl_seat::WlSeat, event: wl_seat::Event, id: &u32, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_seat::Event::Name { name } = event && let Some((_, actual_name)) = state.seats.get_mut(id) {
            *actual_name = Some(name);
        }
    }
}
impl Dispatch<ZwpInputMethodManagerV2, ()> for Probe {
    fn event(_: &mut Self, _: &ZwpInputMethodManagerV2, _: zwp_input_method_manager_v2::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<ZwpInputMethodV2, ()> for Probe {
    fn event(state: &mut Self, _: &ZwpInputMethodV2, event: zwp_input_method_v2::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let zwp_input_method_v2::Event::Unavailable = event { state.unavailable = true; }
    }
}
impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for Probe {
    fn event(state: &mut Self, _: &ZwpInputMethodKeyboardGrabV2, event: zwp_input_method_keyboard_grab_v2::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            // Dropping the owned keymap fd closes it. No key values/content
            // are retained or printed by this ownership fixture.
            zwp_input_method_keyboard_grab_v2::Event::Keymap { fd, size, .. } => {
                state.keymap = size > 0 && size <= 1024 * 1024 && fd.as_raw_fd() >= 0;
            }
            zwp_input_method_keyboard_grab_v2::Event::RepeatInfo { .. } => state.repeat = true,
            _ => {}
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("FAIL real input-method ownership: {error}");
        std::process::exit(1);
    }
}
