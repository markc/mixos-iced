//! testkit-layer-client: a minimal wlr-layer-shell client, for compd's panel-holder
//! gates.
//!
//!   testkit-layer-client --namespace TOKEN [--edge top|bottom|left|right]
//!                    [--size N] [--layer background|bottom|top|overlay]
//!                    [--exclusive N] [--keyboard none|exclusive|on_demand]
//!                    [--conceal-on-usr1]
//!
//! Connects to `$WAYLAND_DISPLAY`, binds every `wl_seat` (as Quoin does, so
//! the agent seat sees a bound client), and maps one layer surface on the
//! default output: anchored to `--edge` and stretched along it, `--size`
//! logical pixels thick (default 32), namespace `TOKEN` (the panel token),
//! reserving an exclusive zone of `--exclusive` pixels (default 0: none),
//! with `--keyboard` interactivity (default none).
//! It answers every configure with `ack_configure` (a live client: the panel
//! liveness probe is a re-sent configure), and commits a frame for every
//! frame callback it receives, so compd's per-surface commit count is a
//! frame-callback oracle: it stops growing while the layer is concealed.
//!
//! Prints `mapped` once the first buffer is committed. Exits 0 on
//! `closed`, 2 on a setup or protocol error.
//!
//! `--conceal-on-usr1` makes it a panel OWNER that conceals itself, as Quoin
//! does on a `panel.command` reveal:false: on SIGUSR1 it attaches a null
//! buffer (the layer unmaps), stops committing frames and prints `concealed`.
//! compd must then see an owner-applied conceal and enforce nothing.

use std::fs::File;
use std::os::fd::{AsFd, FromRawFd};
use std::sync::atomic::{AtomicBool, Ordering};

use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_pointer::WlPointer, wl_registry, wl_registry::WlRegistry, wl_seat::WlSeat, wl_shm, wl_shm::WlShm,
    wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

#[derive(Default)]
struct State {
    globals: Vec<(u32, String, u32)>,
    /// The size the last configure gave, once one came.
    configured: Option<(u32, u32)>,
    /// A frame callback fired: commit the next frame.
    frame_due: bool,
    closed: bool,
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(state: &mut Self, _: &WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            state.globals.push((name, interface, version));
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        state: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer.ack_configure(serial);
                state.configured = Some((width, height));
            }
            zwlr_layer_surface_v1::Event::Closed => state.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<WlCallback, ()> for State {
    fn event(state: &mut Self, _: &WlCallback, event: wl_callback::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_callback::Event::Done { .. } = event {
            state.frame_due = true;
        }
    }
}

delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlShmPool);
delegate_noop!(State: ignore WlBuffer);
delegate_noop!(State: ignore WlSurface);
delegate_noop!(State: ignore WlSeat);
delegate_noop!(State: ignore WlPointer);
delegate_noop!(State: ignore ZwlrLayerShellV1);

fn fail(message: &str) -> ! {
    eprintln!("testkit-layer-client: {message}");
    std::process::exit(2);
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// An opaque `width` x `height` shm buffer.
fn buffer(shm: &WlShm, qh: &QueueHandle<State>, width: i32, height: i32) -> WlBuffer {
    let stride = width * 4;
    let size = stride * height;
    let fd = unsafe { libc::memfd_create(c"testkit-layer-client".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        fail(&format!("memfd_create: {}", std::io::Error::last_os_error()));
    }
    let file = unsafe { File::from_raw_fd(fd) };
    if let Err(error) = file.set_len(size as u64) {
        fail(&format!("size the shm pool: {error}"));
    }
    let pool = shm.create_pool(file.as_fd(), size, qh, ());
    let buffer = pool.create_buffer(0, width, height, stride, wl_shm::Format::Xrgb8888, qh, ());
    pool.destroy();
    buffer
}

/// Set by SIGUSR1 under `--conceal-on-usr1`; taken by the main loop.
static CONCEAL: AtomicBool = AtomicBool::new(false);

extern "C" fn on_usr1(_: libc::c_int) {
    CONCEAL.store(true, Ordering::SeqCst);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let conceal_on_usr1 = args.iter().any(|a| a == "--conceal-on-usr1");
    if conceal_on_usr1 {
        // SAFETY: the handler only stores to an atomic, which is
        // async-signal-safe.
        unsafe {
            libc::signal(libc::SIGUSR1, on_usr1 as extern "C" fn(libc::c_int) as libc::sighandler_t);
        }
    }
    let Some(namespace) = arg(&args, "--namespace") else {
        fail("--namespace TOKEN is required");
    };
    let edge = arg(&args, "--edge").unwrap_or_else(|| "top".into());
    let size: u32 = arg(&args, "--size").and_then(|s| s.parse().ok()).unwrap_or(32).max(1);
    let exclusive: i32 = arg(&args, "--exclusive").and_then(|s| s.parse().ok()).unwrap_or(0);
    let keyboard = match arg(&args, "--keyboard").as_deref() {
        None | Some("none") => KeyboardInteractivity::None,
        Some("exclusive") => KeyboardInteractivity::Exclusive,
        Some("on_demand") => KeyboardInteractivity::OnDemand,
        Some(other) => fail(&format!("unknown --keyboard {other}")),
    };
    let layer = match arg(&args, "--layer").as_deref() {
        None | Some("top") => Layer::Top,
        Some("overlay") => Layer::Overlay,
        Some("bottom") => Layer::Bottom,
        Some("background") => Layer::Background,
        Some(other) => fail(&format!("unknown --layer {other}")),
    };
    let (anchor, horizontal) = match edge.as_str() {
        "top" => (Anchor::Top | Anchor::Left | Anchor::Right, true),
        "bottom" => (Anchor::Bottom | Anchor::Left | Anchor::Right, true),
        "left" => (Anchor::Left | Anchor::Top | Anchor::Bottom, false),
        "right" => (Anchor::Right | Anchor::Top | Anchor::Bottom, false),
        other => fail(&format!("unknown --edge {other}")),
    };

    let connection = Connection::connect_to_env().unwrap_or_else(|error| fail(&format!("connect: {error}")));
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    let registry = connection.display().get_registry(&qh, ());
    let mut state = State::default();
    queue.roundtrip(&mut state).unwrap_or_else(|error| fail(&format!("roundtrip: {error}")));

    let bind = |interface: &str| state.globals.iter().find(|(_, i, _)| i == interface).map(|(n, _, v)| (*n, *v));
    let (compositor, shm, shell) = match (bind("wl_compositor"), bind("wl_shm"), bind("zwlr_layer_shell_v1")) {
        (Some(compositor), Some(shm), Some(shell)) => (compositor, shm, shell),
        _ => fail("compd advertised no wl_compositor / wl_shm / zwlr_layer_shell_v1"),
    };
    let compositor: WlCompositor = registry.bind(compositor.0, compositor.1.min(4), &qh, ());
    let shm: WlShm = registry.bind(shm.0, 1, &qh, ());
    let shell: ZwlrLayerShellV1 = registry.bind(shell.0, shell.1.min(4), &qh, ());
    // Every seat, each with a pointer: bound on both the primary and the agent seat.
    let seats: Vec<(u32, u32)> = state
        .globals
        .iter()
        .filter(|(_, interface, _)| interface == "wl_seat")
        .map(|(name, _, version)| (*name, *version))
        .collect();
    let _seats: Vec<(WlSeat, WlPointer)> = seats
        .into_iter()
        .map(|(name, version)| {
            let seat: WlSeat = registry.bind(name, version.min(7), &qh, ());
            let pointer = seat.get_pointer(&qh, ());
            (seat, pointer)
        })
        .collect();

    let surface = compositor.create_surface(&qh, ());
    let layer_surface = shell.get_layer_surface(&surface, None, layer, namespace, &qh, ());
    layer_surface.set_anchor(anchor);
    layer_surface.set_exclusive_zone(exclusive);
    layer_surface.set_keyboard_interactivity(keyboard);
    if horizontal {
        layer_surface.set_size(0, size);
    } else {
        layer_surface.set_size(size, 0);
    }
    surface.commit();

    let mut mapped = false;
    let mut concealed = false;
    // One buffer per configured size, re-attached every frame.
    let mut current: Option<((i32, i32), WlBuffer)> = None;
    while !state.closed {
        if let Err(error) = queue.blocking_dispatch(&mut state) {
            // A SIGUSR1 interrupts the poll; that is the wake, not a failure.
            if !(conceal_on_usr1 && CONCEAL.load(Ordering::SeqCst)) {
                fail(&format!("dispatch: {error}"));
            }
        }
        if conceal_on_usr1 && !concealed && CONCEAL.swap(false, Ordering::SeqCst) {
            concealed = true;
            surface.attach(None, 0, 0);
            surface.commit();
            let _ = connection.flush();
            println!("concealed");
        }
        if concealed {
            continue;
        }
        let Some((width, height)) = state.configured else { continue };
        let (width, height) = (width.max(1) as i32, height.max(1) as i32);
        if !mapped || state.frame_due {
            state.frame_due = false;
            if current.as_ref().is_none_or(|(size, _)| *size != (width, height)) {
                if let Some((_, old)) = current.take() {
                    old.destroy();
                }
                current = Some(((width, height), buffer(&shm, &qh, width, height)));
            }
            let Some((_, buffer)) = current.as_ref() else { continue };
            surface.attach(Some(buffer), 0, 0);
            surface.damage_buffer(0, 0, width, height);
            surface.frame(&qh, ());
            surface.commit();
            if !mapped {
                mapped = true;
                println!("mapped");
            }
        }
    }
    layer_surface.destroy();
    surface.destroy();
    let _ = connection.flush();
}
