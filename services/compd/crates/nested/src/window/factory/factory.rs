//! Winit window/backend construction + the dev output. (Ex winit wire.rs
//! `new()`, moved.)

use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit::{self as WinitBackend, WinitEventLoop, WinitGraphicsBackend};
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::utils::Transform;
use std::sync::atomic::{AtomicU64, Ordering};

/// The nested output's scale chosen by compd (`--scale S`),
/// as `f64` bits; 0 = none, follow the host window's scale factor. One value
/// for the process (per-output scale is a later step).
static SCALE: AtomicU64 = AtomicU64::new(0);

/// Set (or clear) the nested output scale. Call before `wire`; a value that is
/// not finite and positive clears it.
pub fn set_scale(scale: Option<f64>) {
    let bits = scale.filter(|s| s.is_finite() && *s > 0.0).map_or(0, f64::to_bits);
    SCALE.store(bits, Ordering::Relaxed);
}

/// The scale the nested output runs at: compd's choice when set, else `host`
/// (the host window's scale factor).
pub fn output_scale(host: f64) -> Scale {
    match SCALE.load(Ordering::Relaxed) {
        0 => Scale::Fractional(host),
        bits => Scale::Fractional(f64::from_bits(bits)),
    }
}

pub struct WinitWindow {
    pub output: Output,
    pub mode: Mode,
    pub winit_backend: WinitGraphicsBackend<GlesRenderer>,
    pub winit_loop: WinitEventLoop,
}

pub fn create() -> Result<WinitWindow, String> {
    info!("Init winit backend");

    // With compd's `--scale S` the window asks for 1280x800 × S PHYSICAL px, so
    // the nested output is the same 1280x800 LOGICAL at any S and a gate's
    // coordinates mean what they mean at 1.0. The host
    // must have room for it (the gates run a 3840x2160 host at S != 1).
    // Without it, smithay's default: 1280x800 in the host's logical px.
    let (backend, winit) = match SCALE.load(Ordering::Relaxed) {
        0 => WinitBackend::init::<GlesRenderer>(),
        bits => {
            use smithay::reexports::winit::{dpi::PhysicalSize, window::WindowAttributes};
            let s = f64::from_bits(bits);
            WinitBackend::init_from_attributes::<GlesRenderer>(
                WindowAttributes::default()
                    .with_surface_size(PhysicalSize::new((1280.0 * s).round() as u32, (800.0 * s).round() as u32))
                    .with_title("Smithay")
                    .with_visible(true),
            )
        }
    }
    .map_err(|e| format!("winit init failed: {e:?}"))?;

    // Hide the host OS cursor: the compositor draws (and pane-clamps) its own cursor, so the
    // unconfined host cursor would otherwise cross viewport edges under nesting.
    backend.window().set_cursor_visible(false);

    let mode = Mode {
        size: backend.window_size(),
        refresh: 60_000,
    };
    info!(
        "winit backend OK: window {}x{} @ {}mHz",
        mode.size.w, mode.size.h, mode.refresh
    );

    let output = Output::new(
        "winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Smithay".into(),
            model: "Winit".into(),
            serial_number: "Unknown".into(),
        },
    );

    // The host scale is not known until the first Resized; until then the
    // output is at compd's scale, or 1.
    let scale = output_scale(1.0);
    // Transform NORMAL on the wire. The nested picture is drawn into a GL
    // default framebuffer (row 0 at the bottom), so it IS rendered through
    // Flipped180, but that is the renderer's business: the damage tracker
    // carries it (`compose::sync_tracker`). Advertised on the wl_output it told
    // every client the output was mirrored, and a client that honours the
    // transform (grim composes its picture through it) turned a correct
    // screencopy upside down (the ced parity gate's flipped capture).
    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        Some(scale),
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    info!("winit output 'winit' configured (scale {}; rendered through Flipped180, advertised Normal)", scale.fractional_scale());

    Ok(WinitWindow {
        output,
        mode,
        winit_backend: backend,
        winit_loop: winit,
    })
}
