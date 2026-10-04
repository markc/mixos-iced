//! testkit-ssd-client: a minimal xdg toplevel that asks for server-side
//! decorations, for compd's chrome pixel gate.
//!
//!   testkit-ssd-client [--colour RRGGBB] [--size WxH] [--mode server|client|none]
//!       [--report-scale]
//!
//! Connects to `$WAYLAND_DISPLAY`, maps one xdg toplevel filled with one
//! opaque colour (default ff00ff), `--size` logical pixels (default 300x200;
//! a configure with a size wins), and negotiates xdg-decoration: `server`
//! (default) asks for server-side, `client` for client-side, `none` binds no
//! decoration object at all. No title, so nothing of the client's own shows in
//! a titlebar. Prints `mapped decoration=<mode compd answered>` once the first
//! buffer is committed after the initial configure, then serves configures
//! until killed. Exits 2 on a setup or protocol error.
//!
//! `--report-scale` binds wp_fractional_scale_v1 on the surface and prints
//! `preferred_scale <N>` (N/120, the protocol's unit) for every preferred scale
//! compd sends: the scale gates read what a client is told.
//! The buffer is still drawn at the logical size; only the report changes.

use std::fs::File;
use std::os::fd::{AsFd, FromRawFd};
use std::os::unix::fs::FileExt;

use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry, wl_registry::WlRegistry, wl_shm,
    wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::xdg::decoration::zv1::client::{
    zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
    zxdg_toplevel_decoration_v1::{self, ZxdgToplevelDecorationV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

#[derive(Default)]
struct State {
    globals: Vec<(u32, String, u32)>,
    /// The serial of a configure not yet acked.
    pending_serial: Option<u32>,
    /// The size the last toplevel configure asked for (0 = client's choice).
    size: Option<(i32, i32)>,
    /// What compd answered the decoration request with.
    mode: Option<&'static str>,
    closed: bool,
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            state.globals.push((name, interface, version));
        }
    }
}

impl Dispatch<XdgWmBase, ()> for State {
    fn event(
        _: &mut Self,
        base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for State {
    fn event(
        state: &mut Self,
        _: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            state.pending_serial = Some(serial);
        }
    }
}

impl Dispatch<XdgToplevel, ()> for State {
    fn event(
        state: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Configure { width, height, .. } => {
                state.size = Some((width, height))
            }
            xdg_toplevel::Event::Close => state.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<ZxdgToplevelDecorationV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZxdgToplevelDecorationV1,
        event: zxdg_toplevel_decoration_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_toplevel_decoration_v1::Event::Configure { mode } = event {
            state.mode = Some(match mode {
                WEnum::Value(zxdg_toplevel_decoration_v1::Mode::ServerSide) => "server",
                WEnum::Value(zxdg_toplevel_decoration_v1::Mode::ClientSide) => "client",
                _ => "unknown",
            });
        }
    }
}

delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlSurface);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlShmPool);
delegate_noop!(State: ignore WlBuffer);
delegate_noop!(State: ignore ZxdgDecorationManagerV1);
delegate_noop!(State: ignore WpFractionalScaleManagerV1);

impl Dispatch<WpFractionalScaleV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            println!("preferred_scale {scale}");
        }
    }
}

fn die(msg: &str) -> ! {
    eprintln!("testkit-ssd-client: {msg}");
    std::process::exit(2);
}

/// One XRGB8888 buffer of `colour`, `w`×`h`.
fn buffer(shm: &WlShm, qh: &QueueHandle<State>, w: i32, h: i32, colour: u32) -> WlBuffer {
    let stride = w * 4;
    let size = (stride * h) as usize;
    let fd = unsafe { libc::memfd_create(c"testkit-ssd-client".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        die(&format!(
            "memfd_create: {}",
            std::io::Error::last_os_error()
        ));
    }
    let file = unsafe { File::from_raw_fd(fd) };
    file.set_len(size as u64)
        .unwrap_or_else(|e| die(&format!("size the pool: {e}")));
    let row: Vec<u8> = (0..w).flat_map(|_| colour.to_le_bytes()).collect();
    for y in 0..h {
        file.write_all_at(&row, (y * stride) as u64)
            .unwrap_or_else(|e| die(&format!("fill the pool: {e}")));
    }
    let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
    let buffer = pool.create_buffer(0, w, h, stride, wl_shm::Format::Xrgb8888, qh, ());
    pool.destroy();
    buffer
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut colour = 0x00ff_00ffu32;
    let mut size = (300, 200);
    let mut mode = "server".to_string();
    let mut report_scale = false;
    let mut i = 0;
    while i < args.len() {
        let value = || {
            args.get(i + 1)
                .cloned()
                .unwrap_or_else(|| die("a flag needs a value"))
        };
        match args[i].as_str() {
            "--colour" => {
                colour = u32::from_str_radix(value().trim_start_matches('#'), 16)
                    .unwrap_or_else(|_| die("--colour needs RRGGBB"));
                i += 2;
            }
            "--size" => {
                let v = value();
                let (w, h) = v.split_once('x').unwrap_or_else(|| die("--size needs WxH"));
                size = (
                    w.parse().unwrap_or_else(|_| die("--size needs WxH")),
                    h.parse().unwrap_or_else(|_| die("--size needs WxH")),
                );
                i += 2;
            }
            "--mode" => {
                mode = value();
                if !["server", "client", "none"].contains(&mode.as_str()) {
                    die("--mode is server, client or none");
                }
                i += 2;
            }
            "--report-scale" => {
                report_scale = true;
                i += 1;
            }
            other => die(&format!("unexpected argument {other}")),
        }
    }

    let conn = Connection::connect_to_env().unwrap_or_else(|e| die(&format!("connect: {e}")));
    let mut queue = conn.new_event_queue::<State>();
    let qh = queue.handle();
    let registry = conn.display().get_registry(&qh, ());
    let mut state = State::default();
    queue
        .roundtrip(&mut state)
        .unwrap_or_else(|e| die(&format!("roundtrip: {e}")));
    let find = |state: &State, interface: &str| {
        state
            .globals
            .iter()
            .find(|(_, name, _)| name == interface)
            .map(|(name, _, version)| (*name, *version))
    };
    let need = |interface: &str| {
        find(&state, interface).unwrap_or_else(|| die(&format!("no {interface} global")))
    };
    let (name, version) = need("wl_compositor");
    let compositor: WlCompositor = registry.bind(name, version.min(4), &qh, ());
    let (name, _) = need("wl_shm");
    let shm: WlShm = registry.bind(name, 1, &qh, ());
    let (name, version) = need("xdg_wm_base");
    let base: XdgWmBase = registry.bind(name, version.min(2), &qh, ());

    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_app_id("testkit-ssd-client".into());
    let _decoration = if mode == "none" {
        None
    } else {
        let (name, _) = need("zxdg_decoration_manager_v1");
        let manager: ZxdgDecorationManagerV1 = registry.bind(name, 1, &qh, ());
        let decoration = manager.get_toplevel_decoration(&toplevel, &qh, ());
        decoration.set_mode(if mode == "server" {
            zxdg_toplevel_decoration_v1::Mode::ServerSide
        } else {
            zxdg_toplevel_decoration_v1::Mode::ClientSide
        });
        Some(decoration)
    };
    let _fractional = report_scale.then(|| {
        let (name, _) = need("wp_fractional_scale_manager_v1");
        let manager: WpFractionalScaleManagerV1 = registry.bind(name, 1, &qh, ());
        manager.get_fractional_scale(&surface, &qh, ())
    });
    // The initial commit: no buffer, asks for the initial configure.
    surface.commit();

    let mut mapped = false;
    loop {
        queue
            .blocking_dispatch(&mut state)
            .unwrap_or_else(|e| die(&format!("dispatch: {e}")));
        if state.closed {
            return;
        }
        if let Some(serial) = state.pending_serial.take() {
            xdg_surface.ack_configure(serial);
            let (w, h) = match state.size {
                Some((w, h)) if w > 0 && h > 0 => (w, h),
                _ => size,
            };
            let buffer = buffer(&shm, &qh, w, h, colour);
            surface.attach(Some(&buffer), 0, 0);
            surface.damage_buffer(0, 0, w, h);
            surface.commit();
            if !mapped {
                mapped = true;
                println!("mapped decoration={}", state.mode.unwrap_or("none"));
            }
        }
    }
}
