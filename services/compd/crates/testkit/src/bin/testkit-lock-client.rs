//! testkit-lock-client: a minimal ext-session-lock-v1 client, for compd's session
//! lock gate.
//!
//!   testkit-lock-client [--hold SECONDS]
//!
//! Connects to `$WAYLAND_DISPLAY`, locks the session, and gives every
//! `wl_output` a lock surface filled with an opaque 0x182438 (the lock-probe
//! colour, so a screencopy can tell the lock surface from compd's black
//! blank). It answers every configure with `ack_configure` and a buffer of the
//! configured size, binds every seat's keyboard and logs each key it receives.
//! It unlocks on SIGUSR1, or after `--hold` seconds (default 60).
//!
//! Prints, one per line: `locked` when the compositor confirms the lock,
//! `key <evdev code> pressed|released` for each key it is sent, `unlocked`
//! after it unlocks. Exits 0 after unlocking; 3 on `finished` (the lock was
//! refused or lost before it was confirmed); 2 on a setup or protocol error.

use std::fs::File;
use std::io::Write;
use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_keyboard, wl_keyboard::WlKeyboard, wl_output::WlOutput,
    wl_registry, wl_registry::WlRegistry, wl_seat::WlSeat, wl_shm, wl_shm::WlShm, wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1,
    ext_session_lock_surface_v1::{self, ExtSessionLockSurfaceV1},
    ext_session_lock_v1::{self, ExtSessionLockV1},
};

/// The lock-probe colour, as XRGB8888 in memory (B, G, R, X).
const COLOUR: [u8; 4] = [0x38, 0x24, 0x18, 0xff];

static UNLOCK: AtomicBool = AtomicBool::new(false);

extern "C" fn on_usr1(_: libc::c_int) {
    UNLOCK.store(true, Ordering::SeqCst);
}

#[derive(Default)]
struct State {
    globals: Vec<(u32, String, u32)>,
    locked: bool,
    finished: bool,
    /// Configures waiting for a buffer: (surface index, width, height).
    configures: Vec<(usize, u32, u32)>,
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(state: &mut Self, _: &WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            state.globals.push((name, interface, version));
        }
    }
}

impl Dispatch<ExtSessionLockV1, ()> for State {
    fn event(state: &mut Self, _: &ExtSessionLockV1, event: ext_session_lock_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            ext_session_lock_v1::Event::Locked => {
                state.locked = true;
                println!("locked");
            }
            ext_session_lock_v1::Event::Finished => state.finished = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtSessionLockSurfaceV1, usize> for State {
    fn event(
        state: &mut Self,
        surface: &ExtSessionLockSurfaceV1,
        event: ext_session_lock_surface_v1::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_session_lock_surface_v1::Event::Configure { serial, width, height } = event {
            surface.ack_configure(serial);
            state.configures.push((*index, width, height));
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(_: &mut Self, _: &WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_keyboard::Event::Key { key, state, .. } = event {
            let state = match state {
                WEnum::Value(wl_keyboard::KeyState::Pressed) => "pressed",
                _ => "released",
            };
            println!("key {key} {state}");
        }
    }
}

delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlShmPool);
delegate_noop!(State: ignore WlBuffer);
delegate_noop!(State: ignore WlSurface);
delegate_noop!(State: ignore WlSeat);
delegate_noop!(State: ignore WlOutput);
delegate_noop!(State: ignore ExtSessionLockManagerV1);

fn fail(message: &str) -> ! {
    eprintln!("testkit-lock-client: {message}");
    std::process::exit(2);
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// A `width` x `height` shm buffer filled with [`COLOUR`].
fn buffer(shm: &WlShm, qh: &QueueHandle<State>, width: i32, height: i32) -> WlBuffer {
    let stride = width * 4;
    let size = stride * height;
    let fd = unsafe { libc::memfd_create(c"testkit-lock-client".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        fail(&format!("memfd_create: {}", std::io::Error::last_os_error()));
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    let pixels: Vec<u8> = COLOUR.repeat((width * height) as usize);
    if let Err(error) = file.write_all(&pixels) {
        fail(&format!("fill the shm pool: {error}"));
    }
    let pool = shm.create_pool(file.as_fd(), size, qh, ());
    let buffer = pool.create_buffer(0, width, height, stride, wl_shm::Format::Xrgb8888, qh, ());
    pool.destroy();
    buffer
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let hold = Duration::from_secs(arg(&args, "--hold").and_then(|s| s.parse().ok()).unwrap_or(60));
    unsafe {
        libc::signal(libc::SIGUSR1, on_usr1 as *const () as libc::sighandler_t);
    }

    let connection = Connection::connect_to_env().unwrap_or_else(|error| fail(&format!("connect: {error}")));
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    let registry = connection.display().get_registry(&qh, ());
    let mut state = State::default();
    queue.roundtrip(&mut state).unwrap_or_else(|error| fail(&format!("roundtrip: {error}")));

    let bind = |interface: &str| state.globals.iter().find(|(_, i, _)| i == interface).map(|(n, _, v)| (*n, *v));
    let (compositor, shm, manager) = match (bind("wl_compositor"), bind("wl_shm"), bind("ext_session_lock_manager_v1")) {
        (Some(compositor), Some(shm), Some(manager)) => (compositor, shm, manager),
        _ => fail("compd advertised no wl_compositor / wl_shm / ext_session_lock_manager_v1"),
    };
    let compositor: WlCompositor = registry.bind(compositor.0, compositor.1.min(4), &qh, ());
    let shm: WlShm = registry.bind(shm.0, 1, &qh, ());
    let manager: ExtSessionLockManagerV1 = registry.bind(manager.0, 1, &qh, ());
    let named = |interface: &str| -> Vec<(u32, u32)> {
        state
            .globals
            .iter()
            .filter(|(_, i, _)| i == interface)
            .map(|(name, _, version)| (*name, *version))
            .collect()
    };
    let (seats, outputs) = (named("wl_seat"), named("wl_output"));
    let _keyboards: Vec<(WlSeat, WlKeyboard)> = seats
        .into_iter()
        .map(|(name, version)| {
            let seat: WlSeat = registry.bind(name, version.min(7), &qh, ());
            let keyboard = seat.get_keyboard(&qh, ());
            (seat, keyboard)
        })
        .collect();
    let outputs: Vec<WlOutput> = outputs
        .into_iter()
        .map(|(name, version)| registry.bind(name, version.min(4), &qh, ()))
        .collect();
    if outputs.is_empty() {
        fail("no wl_output to lock");
    }

    let lock = manager.lock(&qh, ());
    // Learn a refusal before asking for surfaces: compd treats lock surfaces
    // from a lock object it did not accept as a protocol error.
    queue.roundtrip(&mut state).unwrap_or_else(|error| fail(&format!("roundtrip: {error}")));
    if state.finished {
        eprintln!("testkit-lock-client: finished");
        std::process::exit(3);
    }
    let surfaces: Vec<(WlSurface, ExtSessionLockSurfaceV1)> = outputs
        .iter()
        .enumerate()
        .map(|(index, output)| {
            let surface = compositor.create_surface(&qh, ());
            let lock_surface = lock.get_lock_surface(&surface, output, &qh, index);
            (surface, lock_surface)
        })
        .collect();
    connection.flush().unwrap_or_else(|error| fail(&format!("flush: {error}")));

    let deadline = Instant::now() + hold;
    let mut buffers: Vec<Option<WlBuffer>> = (0..surfaces.len()).map(|_| None).collect();
    while !UNLOCK.load(Ordering::SeqCst) && Instant::now() < deadline {
        queue.dispatch_pending(&mut state).unwrap_or_else(|error| fail(&format!("dispatch: {error}")));
        if state.finished {
            eprintln!("testkit-lock-client: finished");
            std::process::exit(3);
        }
        for (index, width, height) in std::mem::take(&mut state.configures) {
            let (width, height) = (width.max(1) as i32, height.max(1) as i32);
            let new = buffer(&shm, &qh, width, height);
            if let Some(old) = buffers[index].replace(new) {
                old.destroy();
            }
            let (surface, _) = &surfaces[index];
            surface.attach(buffers[index].as_ref(), 0, 0);
            surface.damage_buffer(0, 0, width, height);
            surface.commit();
        }
        connection.flush().unwrap_or_else(|error| fail(&format!("flush: {error}")));
        // Wait for events, waking every 100 ms to see SIGUSR1 or the deadline.
        if let Some(guard) = queue.prepare_read() {
            let mut fd = libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 };
            let ready = unsafe { libc::poll(&mut fd, 1, 100) };
            if ready > 0 {
                guard.read().unwrap_or_else(|error| fail(&format!("read: {error}")));
            }
        }
    }
    if !state.locked {
        fail("unlock requested before the compositor confirmed the lock");
    }
    lock.unlock_and_destroy();
    for (surface, lock_surface) in surfaces {
        lock_surface.destroy();
        surface.destroy();
    }
    let _ = queue.roundtrip(&mut state);
    println!("unlocked");
}
