//! The native backend entry: assembles display + renderer, hooks syncobj,
//! initializes the compositor lifecycle, and registers all loop sources.
//! (Ex wire.rs `wire()` + `start()`, recomposed. WAYLAND_DISPLAY is now set
//! by the loader after socket creation — backends no longer touch process
//! environment.)
//!
//! Returns `NativeHandles` — THE integration surface for the main project:
//! `device.interface::apply` consumes the context handle for runtime
//! settings (mode changes, Law-7 enables); everything else is wired
//! internally. Assembly failures panic inside the assemble crates (crash
//! over fallback).

use crate::context::render::render::NativeRenderContext;
use smithay::reexports::calloop::EventLoop;
use std::cell::RefCell;
use std::ffi::OsString;
use std::rc::Rc;
use world::state::Loop;

/// The handles the main project integrates against (see
/// `native.device/device.interface`).
pub struct NativeHandles {
    pub ctx: Rc<RefCell<NativeRenderContext>>,
}

/// `Role::Render` has no answer on this machine — state it.
///
/// Every role must be REGISTERED before the format layer will answer anything,
/// and "the composite is GLES" or "vulkan enumerated nothing" are answers, not
/// gaps. An empty set reads identically to a missing one at every consumer
/// (`set_or_empty` → empty → the term is dropped), so this changes no decision;
/// what it changes is that the layer can tell "there is none" from "not yet".
fn absent(_loop: &mut Loop, why: &'static str) {
    _loop.inner
        .kernel
        .get(&render_gles::format::registrar::registrar::FORMATS)
        .absent(render_gles::format::role::role::Role::Render, why);
}

pub fn wire(
    _loop: &mut Loop,
    _wayland_socket_name: OsString,
    event_loop: &mut EventLoop<'static, Loop>,
) -> NativeHandles {
    info!("Backend initialization - Native");

    // ---- Display + renderer assembly (ex new()); panics internally on
    //      failure — a compositor without a display/renderer cannot run.
    trace!("native: assembling display (DRM/GBM) then renderer");
    let mut display = crate::assemble::display::display::assemble(
        _loop.inner.kernel.get(&render_gles::format::registrar::registrar::FORMATS),
    );
    // The composite is GLES only. GLES consults no Vulkan device, so the `Render`
    // role has no renderable set to publish — SAY so rather than leaving it silent:
    // `format.registrar` treats an unregistered role as a race and refuses to
    // answer across it, and the assembly below is the first thing that asks.
    absent(_loop, "gles composite (no vulkan device is consulted)");
    let renderer = crate::assemble::renderer::renderer::assemble(
        _loop.inner.kernel.get(&render_gles::format::registrar::registrar::FORMATS),
        &mut display,
    );

    // Bind the RC cell into loop so it can register a DMABuf global.
    *_loop.inner.kernel.get_mut(&world::state::state::GPU_BINDING_MUT) = Some(renderer.gpu_binding.clone());

    // logind client for lid-close suspend (None if logind is unavailable).
    *_loop
        .inner
        .kernel
        .get_mut(&drivers::logind::base::LOGIND_MUT) =
        match drivers::logind::base::LogindHandle::new() {
            Ok(h) => Some(h),
            Err(e) => {
                warn!("logind unavailable; lid-close suspend disabled: {e}");
                None
            }
        };

    // Hook syncobj impls; the support probe (`drm.syncobj/syncobj.device`)
    // records what the device can do for the explicit-sync path.
    let syncobj_eventfd =
        kms::syncobj::device::device::supports_eventfd(&display.drm_fd);
    info!("syncobj eventfd support: {syncobj_eventfd}");
    dispatcher::wayland::dmabuf::dispatch::dispatch::hook_syncobj::<dispatcher::state::state::Dispatch>(
        &mut _loop.state,
        display.drm_fd.clone(),
    );

    // ---- The COMPOSITING renderer, chosen and PUBLISHED here (`Role::Sample`).
    //      The dmabuf feedback clients negotiate against narrows the EGL set to
    //      this; without it there is nothing to narrow to and the compositor
    //      advertises formats it cannot import (an fp16 client then gets a blank
    //      window, every frame, forever).
    //
    //      The ordering that used to make this fragile is gone: the feedback is
    //      no longer built by `lifecycle::initialize` below, but by the loader,
    //      after `Registrar::expect` has been armed and every role has landed. So
    //      this no longer has to run before that call to be correct — it has to
    //      run before the loader advertises, which the manifest now enforces.
    // compd: GLES composes and scans out (Vulkan is not carried). Producers
    // keep their GLES-path resources (per-surface GlesTexture).
    let env = model::environment::config::base::get();
    model::stats::registry::base::set_compositor_prefers_dmabuf(false);
    // And WHAT it can import. The off-thread producers allocate their own
    // dmabufs and hand them here to be sampled, so they have to negotiate a
    // modifier this renderer accepts — and they cannot ask it directly, since it
    // lives behind a `&mut` on this thread and staying off it is the point.
    // Published once, here, where the fallback has already been resolved.
    {
        let formats = {
            let mut binding = renderer.gpu_binding.borrow_mut();
            let world::state::state::StateDRMBinding {
                gpus,
                primary,
            } = &mut *binding;
            let primary = *primary;
            render_gles::multigpu::bind::bind::texture_formats(gpus, &primary)
        };
        _loop.inner.kernel.get(&render_gles::format::registrar::registrar::FORMATS).register(
            render_gles::format::registrar::registrar::Device::of(&render_gles::format::registrar::registrar::composite_node(display.primary_gpu)),
            render_gles::format::role::role::Role::Sample,
            formats,
            "gles (texture_formats)",
        );
    }
    // And whether the composite can be TOLD what a surface's numbers mean. Only
    // the HDR composite takes a per-surface transfer, so this decides whether the
    // extended-range (fp16) formats are offered at all: in SDR they would be
    // sampled as if sRGB-encoded and come out far too dark, so declining them and
    // leaving the client on 8-bit sRGB is the honest answer. Computed here rather
    // than with the HDR block below because the dmabuf feedback is built in
    // `lifecycle::initialize`, which is next.
    // compd: only the Vulkan composite took a per-surface transfer (HDR); GLES is SDR.
    let hdr_active = env.hdr && display.hdr.hdr_capable() && false;
    _loop.inner.kernel.get(&render_gles::format::registrar::registrar::FORMATS).set_color_managed(hdr_active);
    // A split configuration is legal but only half-supported, and it is invisible
    // from the client side: what gets advertised is the SCANOUT device, while the
    // off-thread producers allocate on `render_node`. Their modifier sets then
    // have to intersect for the bridge to negotiate anything, and across vendors
    // they do not. Say it once, here, next to the topology line.
    {
        let cfg = model::environment::config::base::get();
        if let Some(rn) = kms::device::node::node::render_node(
            std::path::Path::new(&cfg.render_node),
        ) {
            if rn.dev_id() != display.primary_gpu.dev_id() {
                warn!(
                    "render_node {:?} is a DIFFERENT device from the scanout node {:?}. dmabuf \
                     feedback advertises the scanout device (correct — it is what composites), \
                     but the off-thread producers allocate on render_node, so every buffer they \
                     hand over crosses devices and their modifier sets may not intersect at all. \
                     Full multi-GPU is not yet supported; expect linear or implicit allocation.",
                    rn.dev_path(),
                    display.primary_gpu.dev_path(),
                );
            }
        }
    }

    info!("Backend initialization - wire backend to renderer and initialize renderer");

    // ---- Compositor lifecycle init through the contract (DisplayBackend shape).
    let mut contract =
        crate::assemble::renderer::renderer::NativeContract {
            output: display.output.clone(),
            mode: display.mode,
            gpu_binding: renderer.gpu_binding.clone(),
        };
    // The returned `OutputDamageTracker` is DISCARDED: on the native path every
    // pipe's damage tracking lives inside its own smithay `DrmCompositor`, so a
    // second tracker here would never be read. `initialize` is still called for
    // its side effects (output registration + EGL bind/registration); winit does
    // use the return value.
    let _ = scenegraph::state::lifecycle::lifecycle::initialize(
        _loop,
        &display.output.clone(),
        &_loop.inner.loader.display_handle.clone(),
        &mut contract,
    );


    // ---- Input stack (panics internally; a compositor without input cannot run).
    trace!("native: creating libinput stack for seat '{}'", display.seat_name);
    let libinput_context = kms::input::libinput::factory::factory::create(
        display.session.clone(),
        &display.seat_name,
    );
    let libinput_source =
        kms::input::loop_::libinput::libinput::source(libinput_context.clone());

    info!("Backend initialization - Native.start()");

    // ---- The shared render context (ex start()).
    _loop.state.seat.libseat = Some(display.session.clone());

    // HDR (M5): opt-in via COMPOSITOR_HDR, Vulkan-only, and only on a
    // PQ-capable display. Until the full pipeline lands the path is incomplete;
    // this records capability + state for the developer tool (Statistics tab)
    // and gates later stages. SDR is the default and is untouched.
    let hdr_caps = display.hdr;
    let hdr_requested = env.hdr;
    // `hdr_active` was computed with the renderer above (the dmabuf feedback needed
    // it); this is the same value, not a second decision.
    let hdr_transfer = if hdr_active {
        if hdr_caps.hdr.eotf_pq { "PQ" } else { "HLG" }
    } else {
        "SDR"
    };
    // Deep-color SDR (depth == 10) is independent of HDR: 10-bit scanout
    // with the normal sRGB transfer. Report it so the Statistics tab reflects the
    // real scanout depth.
    let deep_color = env.depth == 10;
    let color_format = if hdr_active {
        "10-bit PQ (BT.2020)"
    } else if deep_color {
        "10-bit sRGB"
    } else {
        "8-bit sRGB"
    };
    info!(
        "native HDR: requested={hdr_requested} capable={} active={hdr_active} deep_color={deep_color}",
        hdr_caps.hdr_capable()
    );
    model::stats::registry::base::set_hdr_info(
        hdr_active,
        hdr_caps.hdr_capable(),
        hdr_transfer,
        hdr_caps.hdr.max_luminance.unwrap_or(0.0),
        hdr_caps.colorimetry.bt2020_rgb,
        color_format,
    );

    let ctx_rc = Rc::new(RefCell::new(NativeRenderContext {
        display_handle: _loop.inner.loader.display_handle.clone(),
        outputs: vec![crate::context::render::render::OutputPipe {
            crtc: display.pipe,
            mode: display.mode,
            output: display.output.clone(),
            drm_output: Some(renderer.drm_output),
            hdr_caps,
            hdr_active,
            props_applied: false,
            render_failures: 0,
            connector: display.connector.handle(),
            current_drm_mode: display.drm_mode,
            modes: display.connector.modes().to_vec(),
            mode_revert: None,
            global: None,
            last_vblank: None,
            render_start: None,
            flip_trace: None,
            last_tear: false,
            cap_interval: None,
            cap_wake: None,
            settlement: Default::default(),
            owed_frames: None,
        }],
        drm_output_manager: renderer.drm_output_manager,
        gpu_binding: renderer.gpu_binding.clone(),
        libinput_context,
        tap_subscriptions: frames::draw::plan::tap::tap::TapSubscriptions::new(),
        safety: render_gles::preference::enable::safety::safety::get(),
        drm_fd: display.drm_fd.clone(),
        watchdog: None,
    }));

    // wlr-screencopy: advertised by the backend that services
    // the copies (`render::execute` → `screencopy::service`, after each
    // pipe's render_frame).
    screencopy::create_global(&_loop.inner.loader.display_handle);

    // Server-side chrome (decor): the theme, from the shared design tokens
    // and the `chrome_style` preference, read once at startup.
    decor::window::install(decor::ChromeTheme::load(
        decor::ChromeStyle::from_name(&_loop.inner.preference.chrome_style).unwrap_or_default(),
    ));

    // ---- Advertised-mode snapshot for the settings Display panel (kernel → rim).
    //      The UI reads OUTPUT_MODES_SNAPSHOT directly. mHz = vrefresh*1000.
    {
        use drivers::output::base::{ModeInfo, OutputModesSnapshot};
        let to_info = |m: &smithay::reexports::drm::control::Mode| ModeInfo {
            width: m.size().0,
            height: m.size().1,
            refresh_mhz: m.vrefresh() * 1000,
        };
        *_loop
            .inner
            .kernel
            .get_mut(&drivers::output::base::OUTPUT_MODES_SNAPSHOT_MUT) =
            OutputModesSnapshot {
                // EDID identity "make model serial" — the per-monitor key the picker
                // selects with and the settings-editor persists.
                edid_key: display.identity.key(),
                current: Some(to_info(&display.drm_mode)),
                available: display.connector.modes().iter().map(to_info).collect(),
            };
    }

    // ---- Topology bookkeeping for the device authority.
    let registry = Rc::new(RefCell::new(
        kms::gpu::registry::node::node::NodeRegistry::new(),
    ));
    {
        let mut reg = registry.borrow_mut();
        reg.add(display.primary_gpu.dev_id(), display.primary_gpu);
        reg.set_primary(display.primary_gpu);
    }
    let topology = Rc::new(RefCell::new(
        crate::context::topology::topology::Topology::new(),
    ));
    {
        let mut topo = topology.borrow_mut();
        let dev_id = display.primary_gpu.dev_id();
        topo.register_device(dev_id, display.primary_gpu);
        topo.register_connector(
            dev_id,
            crate::context::topology::topology::ConnectorEntry {
                handle: display.connector.handle(),
                kind: kms::connector::kind::kind::classify(&display.connector),
                pipe: Some(display.pipe),
                output: Some(display.output.clone()),
            },
        );
        // The hotplug diff baseline: the full connector state at assembly.
        topo.set_snapshot(dev_id, display.initial_snapshot.clone());
    }

    // ---- Initial display snapshot for the lid policy (external present? is the
    //      active output the internal panel?). Refreshed on hotplug by wire.plugin.
    {
        let active = display.connector.handle();
        let ctx = ctx_rc.borrow();
        let manager = ctx.drm_output_manager.borrow();
        let snap = crate::context::display::base::compute(
            manager.device(),
            active,
        );
        drop(manager);
        drop(ctx);
        *_loop
            .inner
            .kernel
            .get_mut(&drivers::lid::base::DISPLAY_SNAPSHOT_MUT) = snap;
    }

    // ---- Full connected-monitor list for the settings preferred-monitor picker
    //      (kernel → rim). Lists the active connector plus connected-but-inactive
    //      monitors; refreshed on a live switch by `display.reconcile`.
    {
        use drivers::output::base::ModeInfo;
        let active_mode = ModeInfo {
            width: display.drm_mode.size().0,
            height: display.drm_mode.size().1,
            refresh_mhz: display.drm_mode.vrefresh() * 1000,
        };
        let ctx = ctx_rc.borrow();
        let manager = ctx.drm_output_manager.borrow();
        // Only the primary pipe exists at boot; secondaries are added by the hotplug
        // reconcile, which rewrites this snapshot with every lit pipe's current mode.
        let lit = [(display.connector.handle(), active_mode)];
        let snap = crate::context::display::enumerate::enumerate::enumerate(
            manager.device(),
            display.connector.handle(),
            &lit,
        );
        drop(manager);
        drop(ctx);
        *_loop
            .inner
            .kernel
            .get_mut(&drivers::output::base::OUTPUTS_SNAPSHOT_MUT) = snap;
    }

    // ---- Loop sources.
    crate::wire::session::session::register(
        event_loop,
        display.session_notifier,
        ctx_rc.clone(),
        |ctx, state| {
            // The watchdog's kick is the frame executor; the loop handle it
            // captures is the state's own. Outcome handling belongs to the
            // pacing layer — the watchdog only needs the kick.
            let handle = state.loop_handle.clone();
            // A resume render has no commit behind it: move the epoch, or the
            // executor's epoch-current skip finds every pipe up to date.
            state.state.redraw.request_silent_for(protocols::redraw::schedule::schedule::RedrawReason::Resume);
            let _ = crate::render::execute::execute::execute(
                ctx, handle, state,
                crate::render::execute::execute::RenderScope::All,
            );
        },
    );

    crate::wire::frame::frame::register(
        event_loop,
        _loop,
        display.drm_notifier,
        ctx_rc.clone(),
    );

    crate::wire::input::input::register(
        event_loop,
        libinput_source,
        ctx_rc.clone(),
    );

    // Control-plane ping: drains the input-independent control-plane OFF the
    // libinput source, on its own loop iteration when pinged (via
    // `state.inner.ping_control()`) — the display request queues (output mode /
    // preferred-monitor switch / lid apply) and the one-shot lock engage. So a
    // settings-window mode change, a lid action, or a lock keybinding all apply
    // without waiting for the next input event, and none of them poll per frame.
    {
        let (ping, source) = smithay::reexports::calloop::ping::make_ping()
            .expect("control-plane ping creation failed");
        let ctx = ctx_rc.clone();
        event_loop
            .handle()
            .insert_source(source, move |_, _, state| {
                crate::context::display::apply::apply::drain(state, &ctx);
                crate::context::display::mode::mode::drain(state, &ctx);
                // Multi-output branch: the single-output active-switch transaction was
                // replaced by the set-reconciler (activate/deactivate/hotplug).
                crate::context::display::reconcile::reconcile::drain_reconcile(state, &ctx);
            })
            .expect("control-plane ping source registration failed");
        _loop.inner.control_ping = Some(ping);
    }

    // Retained udev watch (panics internally if udev vanished post-snapshot).
    let watch = kms::udev::loop_::watch::watch::watch(&display.seat_name);
    crate::wire::plugin::plugin::register(
        event_loop,
        watch,
        registry,
        topology,
        ctx_rc.clone(),
    );

    // Light up every OTHER connected monitor as an additional output (the primary
    // is already up from assembly). The set-reconciler adds one pipe per connected
    // connector not yet driven — the same path a runtime hotplug takes.
    crate::context::display::reconcile::reconcile::reconcile(_loop, &ctx_rc);

    // The wgpu-gl context, the shared iced renderer and every registry, built
    // before the first frame rather than inside it (~50 ms of `comp_update`;
    // the nested twin is in nested `wire`). `gpu::begin` finds it built.
    {
        let gpu_binding = ctx_rc.borrow().gpu_binding.clone();
        let mut binding = gpu_binding.borrow_mut();
        let world::state::state::StateDRMBinding { gpus, primary } = &mut *binding;
        match gpus.single_renderer(primary) {
            Ok(mut renderer) => {
                let gles: &mut smithay::backend::renderer::gles::GlesRenderer = renderer.as_mut();
                frames::scene::gpu::prewarm(_loop, gles);
            }
            Err(err) => warn!("native: no renderer to prewarm the wgpu-gl context ({err:?}); the first frame builds it"),
        }
    }

    NativeHandles { ctx: ctx_rc }
}
