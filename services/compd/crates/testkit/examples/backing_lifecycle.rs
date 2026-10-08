// SPDX-License-Identifier: MIT OR Apache-2.0
//! Compd testkit's real shared GLES/wgpu Backing lifecycle acceptance. No presentation receipts
//! are synthesised: FrameBinding is inspected beside the actual published pixels.
use graphics::surface::{
    surface::IcedSurface,
    wgpu_context::{WgpuGlContext, create_wgpu_gl_context, make_current},
    wgpu_import::FOURCC,
};
use iced_core::window::{
    Id,
    presentation::{FrameBinding, FrameObserver, FrameStamp},
};
use render_gles::format::registrar::registrar::Registrar;
use smithay::{
    backend::{
        allocator::gbm::GbmDevice,
        egl::{EGLDevice, EGLDisplay},
        renderer::{ExportMem, Texture, gles::GlesRenderer},
    },
    utils::Rectangle,
};
use std::{
    error::Error,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn fresh(path: &Path, bytes: &[u8]) -> Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(bytes)?;
    Ok(())
}

struct Expected {
    window: Id,
    binding: FrameBinding,
    rgba: [u8; 4],
}

fn expected(window: Id, epoch: u64, rgba: [u8; 4]) -> Expected {
    Expected {
        window,
        binding: FrameBinding {
            stamp: FrameStamp {
                activation_epoch: epoch,
                local_revision: epoch + 100,
            },
            // This fixture checks ownership, never pretends a clear was a
            // Wayland/KMS presentation. Any callback is an acceptance failure.
            observer: FrameObserver::new(|_| panic!("fixture manufactured native presentation")),
        },
        rgba,
    }
}

fn draw(
    surface: &mut IcedSurface,
    ctx: &WgpuGlContext,
    gles: &mut GlesRenderer,
    wanted: &Expected,
    pipeline: bool,
) -> Result<()> {
    make_current(gles);
    ctx.acquire_gl_state();
    let view = surface
        .begin_render_view()
        .ok_or("released render target")?;
    if surface.slot_count() > 1 {
        assert_ne!(
            surface.target_slot(),
            surface.published_slot(),
            "ring would overwrite the currently published backing"
        );
    }
    surface.set_target_presentation(Some((wanted.window, wanted.binding.clone())));
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("backing lifecycle actual clear"),
        });
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("backing lifecycle distinct pixels"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: f64::from(wanted.rgba[0]) / 255.0,
                        g: f64::from(wanted.rgba[1]) / 255.0,
                        b: f64::from(wanted.rgba[2]) / 255.0,
                        a: f64::from(wanted.rgba[3]) / 255.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }
    ctx.queue.submit([encoder.finish()]);
    surface.submitted(pipeline);
    ctx.release_gl_state();
    Ok(())
}

/// The completion flag is set only by the real queue callback. A bounded device
/// wait drives it; no frame-count sleep or fake completion switch is involved.
fn complete(surface: &mut IcedSurface, ctx: &WgpuGlContext, gles: &mut GlesRenderer) -> Result<()> {
    make_current(gles);
    ctx.acquire_gl_state();
    let done = Arc::new(AtomicBool::new(false));
    let callback = Arc::clone(&done);
    ctx.queue
        .on_submitted_work_done(move || callback.store(true, Ordering::Release));
    ctx.device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: Some(Duration::from_secs(5)),
    })?;
    assert!(
        done.load(Ordering::Acquire),
        "real queue completion callback missing"
    );
    surface.poll();
    assert!(!surface.has_pending(), "completed ring remains pending");
    ctx.release_gl_state();
    Ok(())
}

fn inspect(
    surface: &IcedSurface,
    ctx: &WgpuGlContext,
    gles: &mut GlesRenderer,
    directory: &Path,
    label: &str,
    wanted: &Expected,
) -> Result<()> {
    make_current(gles);
    ctx.release_gl_state();
    let (window, binding) = surface
        .published_presentation()
        .ok_or("missing published binding")?;
    assert_eq!(
        window, wanted.window,
        "published window incarnation changed"
    );
    assert!(
        binding.same_presentation(&wanted.binding),
        "pixels/observer/stamp pair changed"
    );
    let texture = surface
        .gles_texture()
        .ok_or("missing published GLES texture")?;
    let mapping = gles.copy_texture(texture, Rectangle::from_size(texture.size()), FOURCC)?;
    let bytes = gles.map_texture(&mapping)?.to_vec();
    assert_eq!(bytes.len(), (surface.size.w * surface.size.h * 4) as usize);
    for pixel in bytes.chunks_exact(4) {
        for (actual, expected) in pixel.iter().zip(wanted.rgba) {
            assert!(
                actual.abs_diff(expected) <= 1,
                "{label}: actual GLES pixels {pixel:?} differ from {:?}",
                wanted.rgba
            );
        }
    }
    fresh(&directory.join(format!("{label}.rgba")), &bytes)?;
    let receipt = format!(
        "{{\"phase\":\"{label}\",\"width\":{},\"height\":{},\"slots\":{},\"published_slot\":{},\"target_slot\":{},\"texture\":{},\"generation\":{},\"activation_epoch\":{},\"local_revision\":{},\"rgba\":{:?},\"same_observer\":true,\"pending\":{}}}\n",
        surface.size.w,
        surface.size.h,
        surface.slot_count(),
        surface.published_slot().unwrap(),
        surface.target_slot().unwrap(),
        texture.tex_id(),
        surface.generation(),
        binding.stamp.activation_epoch,
        binding.stamp.local_revision,
        wanted.rgba,
        surface.has_pending()
    );
    fresh(&directory.join(format!("{label}.json")), receipt.as_bytes())?;
    Ok(())
}

fn run(node: &str, directory: &Path) -> Result<()> {
    fs::create_dir(directory)?;
    let path = fs::canonicalize(node)?;
    let device = EGLDevice::enumerate()?
        .find(|device| {
            !device.is_software()
                && (device.render_device_path().ok().as_ref() == Some(&path)
                    || device.drm_device_path().ok().as_ref() == Some(&path))
        })
        .ok_or("no hardware EGL device for requested node")?;
    // Match the native GbmGlesBackend: the EGL platform display owns a GBM
    // device over the selected render fd. EGLDevice platform displays do not
    // necessarily support the native renderer's window-capable config.
    assert!(!device.is_software());
    let fd = OpenOptions::new().read(true).write(true).open(&path)?;
    let gbm = GbmDevice::new(fd)?;
    // SAFETY: this fixture owns the render fd/GBM display for its entire
    // renderer lifetime; all GL/wgpu operations stay on this one thread.
    let display = unsafe { EGLDisplay::new(gbm)? };
    fresh(&directory.join("display.txt"), format!("platform=GBM\nnode={}\n", path.display()).as_bytes())?;
    let mut gles = render_gles::context::egl::egl::create(&display)?;
    let ctx = create_wgpu_gl_context(&Registrar::new(), &mut gles)?;
    let adapter = ctx.adapter.get_info();
    assert_eq!(adapter.backend, wgpu::Backend::Gl);
    assert!(
        !matches!(adapter.device_type, wgpu::DeviceType::Cpu),
        "software adapter refused"
    );
    fresh(
        &directory.join("adapter.txt"),
        format!("{:?}\n", adapter).as_bytes(),
    )?;
    make_current(&mut gles);
    let mut surface = IcedSurface::allocate(node, &ctx, &mut gles, (16, 12).into())?;
    assert!(surface.published_presentation().is_none());
    let window = Id::unique();
    let a = expected(window, 1, [17, 83, 149, 255]);
    let b = expected(window, 2, [191, 37, 71, 255]);
    let c = expected(window, 3, [43, 173, 97, 255]);
    let d = expected(window, 4, [211, 127, 23, 255]);
    let e = expected(window, 5, [61, 109, 229, 255]);
    let retired_resize = expected(window, 6, [89, 19, 139, 255]);
    let f = expected(window, 7, [157, 67, 199, 255]);
    let retired_release = expected(window, 8, [227, 53, 181, 255]);
    let g = expected(window, 9, [101, 233, 47, 255]);
    surface.sync_depth(node, &ctx, &mut gles, 2)?;
    assert_eq!(surface.slot_count(), 2);
    draw(&mut surface, &ctx, &mut gles, &a, true)?;
    assert!(
        !surface.has_pending(),
        "depth two must publish synchronously"
    );
    complete(&mut surface, &ctx, &mut gles)?;
    inspect(&surface, &ctx, &mut gles, directory, "depth-two-first", &a)?;
    let first_slot = surface.published_slot();
    draw(&mut surface, &ctx, &mut gles, &b, true)?;
    assert!(
        !surface.has_pending(),
        "depth two unexpectedly deferred publication"
    );
    complete(&mut surface, &ctx, &mut gles)?;
    assert_ne!(
        surface.published_slot(),
        first_slot,
        "actual depth-two backing did not rotate"
    );
    inspect(&surface, &ctx, &mut gles, directory, "depth-two-swap", &b)?;
    let kept_texture = surface.gles_texture().unwrap().tex_id();
    surface.sync_depth(node, &ctx, &mut gles, 3)?;
    assert_eq!(surface.slot_count(), 3);
    assert_eq!(surface.gles_texture().unwrap().tex_id(), kept_texture);
    inspect(
        &surface,
        &ctx,
        &mut gles,
        directory,
        "growth-three-preserves",
        &b,
    )?;
    draw(&mut surface, &ctx, &mut gles, &c, true)?;
    complete(&mut surface, &ctx, &mut gles)?;
    inspect(&surface, &ctx, &mut gles, directory, "depth-three-swap", &c)?;
    draw(&mut surface, &ctx, &mut gles, &d, true)?;
    fresh(
        &directory.join("before-shrink.json"),
        format!(
            "{{\"pending\":{},\"published_slot\":{},\"target_slot\":{},\"generation\":{}}}\n",
            surface.has_pending(),
            surface.published_slot().unwrap(),
            surface.target_slot().unwrap(),
            surface.generation()
        )
        .as_bytes(),
    )?;
    // Shrink goes through the production queue quiesce and moves the exact
    // completed Backing (both texture owners plus binding) to slot zero.
    make_current(&mut gles);
    ctx.acquire_gl_state();
    surface.sync_depth(node, &ctx, &mut gles, 2)?;
    ctx.release_gl_state();
    complete(&mut surface, &ctx, &mut gles)?;
    assert_eq!(surface.slot_count(), 2);
    assert_eq!(surface.published_slot(), Some(0));
    inspect(
        &surface,
        &ctx,
        &mut gles,
        directory,
        "shrink-two-preserves",
        &d,
    )?;
    let kept_texture = surface.gles_texture().unwrap().tex_id();
    surface.sync_depth(node, &ctx, &mut gles, 1)?;
    assert_eq!(surface.slot_count(), 1);
    assert_eq!(surface.gles_texture().unwrap().tex_id(), kept_texture);
    inspect(
        &surface,
        &ctx,
        &mut gles,
        directory,
        "shrink-one-preserves",
        &d,
    )?;
    draw(&mut surface, &ctx, &mut gles, &e, false)?;
    complete(&mut surface, &ctx, &mut gles)?;
    inspect(&surface, &ctx, &mut gles, directory, "depth-one-fresh", &e)?;
    surface.sync_depth(node, &ctx, &mut gles, 3)?;
    draw(&mut surface, &ctx, &mut gles, &retired_resize, true)?;
    fresh(&directory.join("before-resize.json"), format!(
        "{{\"pending\":{},\"published_slot\":{},\"target_slot\":{},\"generation\":{},\"submitted_epoch\":{}}}\n",
        surface.has_pending(), surface.published_slot().unwrap(),
        surface.target_slot().unwrap(), surface.generation(),
        retired_resize.binding.stamp.activation_epoch).as_bytes())?;
    let old_texture = surface.gles_texture().unwrap().tex_id();
    surface.resize(node, &ctx, &mut gles, (9, 7).into())?;
    assert_eq!(surface.generation(), 0);
    assert!(
        surface.published_presentation().is_none(),
        "resize inherited old binding"
    );
    assert_ne!(surface.gles_texture().unwrap().tex_id(), old_texture);
    draw(&mut surface, &ctx, &mut gles, &f, true)?;
    complete(&mut surface, &ctx, &mut gles)?;
    inspect(
        &surface,
        &ctx,
        &mut gles,
        directory,
        "resize-first-fresh",
        &f,
    )?;
    // Retire with a real submitted frame, then require ensure's fresh target to
    // start without any old stamp or observer before it is drawn again.
    draw(&mut surface, &ctx, &mut gles, &retired_release, true)?;
    make_current(&mut gles);
    surface.release();
    assert!(!surface.is_resident());
    assert!(surface.gles_texture().is_none());
    assert!(surface.published_presentation().is_none());
    assert!(!surface.has_pending());
    surface.ensure(node, &ctx, &mut gles)?;
    surface.sync_depth(node, &ctx, &mut gles, 3)?;
    assert_eq!(surface.generation(), 0);
    assert!(
        surface.published_presentation().is_none(),
        "ensure inherited retired binding"
    );
    // Drive all genuine outstanding queue callbacks before drawing the new
    // binding: retired work cannot repopulate metadata in the replacement ring.
    complete(&mut surface, &ctx, &mut gles)?;
    assert_eq!(surface.generation(), 0);
    assert!(
        surface.published_presentation().is_none(),
        "retired callback revived binding"
    );
    draw(&mut surface, &ctx, &mut gles, &g, true)?;
    complete(&mut surface, &ctx, &mut gles)?;
    inspect(
        &surface,
        &ctx,
        &mut gles,
        directory,
        "ensure-first-fresh",
        &g,
    )?;
    complete(&mut surface, &ctx, &mut gles)?;
    make_current(&mut gles);
    drop(surface);
    drop(ctx);
    fresh(&directory.join("passed.json"), b"{\"actual_shared_gles_wgpu_backings\":true,\"native_presentation_claimed\":false,\"phases\":9}\n")?;
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 {
        return Err("usage: backing_lifecycle RENDER_NODE FRESH_ARTIFACT_DIRECTORY".into());
    }
    run(&args[1], Path::new(&args[2]))
}
