//! testkit-screenshot: a minimal wlr-screencopy client, for compd's pixel gates.
//!
//!   testkit-screenshot OUT.ppm [--region X,Y,W,H] [--with-damage] [--cursor]
//!
//! Connects to `$WAYLAND_DISPLAY`, captures the first `wl_output` (or a region
//! of it, output-local logical coordinates) through `zwlr_screencopy_manager_v1`,
//! and writes a binary PPM (P6). `--with-damage` uses `copy_with_damage` and
//! prints the damage boxes compd reported, one `damage x y w h` line each.
//! `--cursor` asks for the cursor in the copy (`overlay_cursor = 1`); without
//! it the copy excludes the cursor, as grim's default does.
//!
//! Exit codes: 0 written, 1 compd answered `failed`, 2 setup or protocol error.
//! Stands in for `grim` where grim is not installed; same protocol, no options
//! beyond what the gates use.

use std::fs::File;
use std::io::Write;
use std::os::fd::{AsFd, FromRawFd};
use std::os::unix::fs::FileExt;

use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_output::WlOutput, wl_registry, wl_registry::WlRegistry, wl_shm,
    wl_shm::WlShm, wl_shm_pool::WlShmPool,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1, zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1,
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

#[derive(Default)]
struct State {
    globals: Vec<(u32, String, u32)>,
    buffer: Option<(u32, u32, u32, u32)>,
    buffer_done: bool,
    y_invert: bool,
    damage: Vec<(u32, u32, u32, u32)>,
    ready: bool,
    failed: bool,
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

impl Dispatch<ZwlrScreencopyFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer {
                format,
                width,
                height,
                stride,
            } => {
                let format = match format {
                    WEnum::Value(f) => f as u32,
                    WEnum::Unknown(f) => f,
                };
                state.buffer = Some((format, width, height, stride));
            }
            zwlr_screencopy_frame_v1::Event::BufferDone => state.buffer_done = true,
            zwlr_screencopy_frame_v1::Event::Flags { flags } => {
                state.y_invert = matches!(flags, WEnum::Value(f) if f.contains(zwlr_screencopy_frame_v1::Flags::YInvert));
            }
            zwlr_screencopy_frame_v1::Event::Damage {
                x,
                y,
                width,
                height,
            } => state.damage.push((x, y, width, height)),
            zwlr_screencopy_frame_v1::Event::Ready { .. } => state.ready = true,
            zwlr_screencopy_frame_v1::Event::Failed => state.failed = true,
            _ => {}
        }
    }
}

delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlOutput);
delegate_noop!(State: ignore WlBuffer);
delegate_noop!(State: WlShmPool);
delegate_noop!(State: ZwlrScreencopyManagerV1);

fn die(code: i32, msg: &str) -> ! {
    eprintln!("testkit-screenshot: {msg}");
    std::process::exit(code);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out = None;
    let mut region = None;
    let mut with_damage = false;
    let mut cursor = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--region" => {
                let parts: Vec<i32> = args
                    .get(i + 1)
                    .unwrap_or_else(|| die(2, "--region needs X,Y,W,H"))
                    .split(',')
                    .map(|p| {
                        p.trim()
                            .parse()
                            .unwrap_or_else(|_| die(2, "--region needs four integers"))
                    })
                    .collect();
                if parts.len() != 4 {
                    die(2, "--region needs X,Y,W,H");
                }
                region = Some((parts[0], parts[1], parts[2], parts[3]));
                i += 2;
            }
            "--with-damage" => {
                with_damage = true;
                i += 1;
            }
            "--cursor" => {
                cursor = true;
                i += 1;
            }
            path if out.is_none() => {
                out = Some(path.to_string());
                i += 1;
            }
            other => die(2, &format!("unexpected argument {other}")),
        }
    }
    let out = out.unwrap_or_else(|| {
        die(
            2,
            "usage: testkit-screenshot OUT.ppm [--region X,Y,W,H] [--with-damage] [--cursor]",
        )
    });

    let conn = Connection::connect_to_env().unwrap_or_else(|e| die(2, &format!("connect: {e}")));
    let mut queue = conn.new_event_queue::<State>();
    let qh = queue.handle();
    let registry = conn.display().get_registry(&qh, ());
    let mut state = State::default();
    queue
        .roundtrip(&mut state)
        .unwrap_or_else(|e| die(2, &format!("roundtrip: {e}")));
    let find = |state: &State, interface: &str| {
        state
            .globals
            .iter()
            .find(|(_, name, _)| name == interface)
            .map(|(name, _, version)| (*name, *version))
            .unwrap_or_else(|| die(2, &format!("no {interface} global")))
    };
    let (name, _) = find(&state, "wl_shm");
    let shm: WlShm = registry.bind(name, 1, &qh, ());
    let (name, version) = find(&state, "wl_output");
    let output: WlOutput = registry.bind(name, version.min(4), &qh, ());
    let (name, version) = find(&state, "zwlr_screencopy_manager_v1");
    let manager: ZwlrScreencopyManagerV1 = registry.bind(name, version.min(3), &qh, ());

    let frame = match region {
        None => manager.capture_output(i32::from(cursor), &output, &qh, ()),
        Some((x, y, w, h)) => {
            manager.capture_output_region(i32::from(cursor), &output, x, y, w, h, &qh, ())
        }
    };
    // Version 3 ends the advertisement with buffer_done; earlier ones send one
    // `buffer` event, which the first round trip delivers.
    while !(state.failed || (state.buffer.is_some() && (state.buffer_done || version < 3))) {
        queue
            .blocking_dispatch(&mut state)
            .unwrap_or_else(|e| die(2, &format!("dispatch: {e}")));
    }
    if state.failed {
        die(1, "compd answered failed (no buffer)");
    }
    let (format, width, height, stride) = state.buffer.expect("checked above");
    if format != wl_shm::Format::Xrgb8888 as u32 && format != wl_shm::Format::Argb8888 as u32 {
        die(2, &format!("unsupported shm format {format:#x}"));
    }
    let size = (stride * height) as usize;
    let fd = unsafe { libc::memfd_create(c"testkit-screenshot".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        die(
            2,
            &format!("memfd_create: {}", std::io::Error::last_os_error()),
        );
    }
    let file = unsafe { File::from_raw_fd(fd) };
    file.set_len(size as u64)
        .unwrap_or_else(|e| die(2, &format!("size the pool: {e}")));
    let pool = shm.create_pool(file.as_fd(), size as i32, &qh, ());
    let shm_format = if format == wl_shm::Format::Argb8888 as u32 {
        wl_shm::Format::Argb8888
    } else {
        wl_shm::Format::Xrgb8888
    };
    let buffer = pool.create_buffer(
        0,
        width as i32,
        height as i32,
        stride as i32,
        shm_format,
        &qh,
        (),
    );
    if with_damage {
        frame.copy_with_damage(&buffer);
    } else {
        frame.copy(&buffer);
    }
    while !(state.ready || state.failed) {
        queue
            .blocking_dispatch(&mut state)
            .unwrap_or_else(|e| die(2, &format!("dispatch: {e}")));
    }
    if state.failed {
        die(1, "compd answered failed");
    }
    let mut bytes = vec![0u8; size];
    file.read_exact_at(&mut bytes, 0)
        .unwrap_or_else(|e| die(2, &format!("read the pool: {e}")));
    let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
    for row in 0..height {
        let src = if state.y_invert {
            height - 1 - row
        } else {
            row
        };
        let line = &bytes[(src * stride) as usize..(src * stride + width * 4) as usize];
        for px in line.chunks_exact(4) {
            // XRGB8888 / ARGB8888 in memory: B, G, R, X.
            ppm.extend_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    File::create(&out)
        .and_then(|mut f| f.write_all(&ppm))
        .unwrap_or_else(|e| die(2, &format!("write {out}: {e}")));
    for (x, y, w, h) in &state.damage {
        println!("damage {x} {y} {w} {h}");
    }
    println!("wrote {out} ({width}x{height})");
}
