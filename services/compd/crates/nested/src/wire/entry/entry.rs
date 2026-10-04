//! The winit backend entry. (Ex winit wire.rs `wire()` + `start()`.
//! WAYLAND_DISPLAY is now set by the loader after socket creation.)

use crate::scene::compose::compose::WinitRenderContext;
use outputs::render_contract::contract::{RenderContract, RendererId};
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::renderer::{ImportDma, ImportEgl};
use smithay::output::{Mode, Output};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::DisplayHandle;
use std::ffi::OsString;
use world::state::Loop;

/// The winit contract object handed to `lifecycle::initialize` (the
/// pre-existing DisplayBackend shape) — ex winit state.rs `Backend` impl.
pub struct WinitContract {
    pub output: Output,
    pub mode: Mode,
    pub winit: crate::window::factory::factory::WinitWindow,
}

impl outputs::render_contract::contract::DisplayBackend for WinitContract {
    fn load(&mut self) -> (&Output, &Mode) {
        (&self.output, &self.mode)
    }

    fn bind_display(&mut self, display_handle: &DisplayHandle) -> FormatSet {
        let renderer = self.winit.winit_backend.renderer();

        if renderer.bind_wl_display(display_handle).is_ok() {
            info!("EGL Hardware Acceleration bridge initialized for clients.");
        } else {
            warn!("Clients will not be able to use Hardware Acceleration.");
        }

        renderer.dmabuf_formats()
    }
}

impl RenderContract for WinitContract {
    fn id(&self) -> RendererId {
        RendererId::Gles
    }

    fn bind_display(&mut self, display_handle: &DisplayHandle) -> FormatSet {
        <Self as outputs::render_contract::contract::DisplayBackend>::bind_display(
            self,
            display_handle,
        )
    }

    fn supported_formats(&mut self) -> FormatSet {
        // The (fourcc x modifier) set the EGL context can sample from.
        self.winit
            .winit_backend
            .renderer()
            .egl_context()
            .dmabuf_texture_formats()
            .clone()
    }

    fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        let ok = self
            .winit
            .winit_backend
            .renderer()
            .import_dmabuf(dmabuf, None)
            .is_ok();
        trace!("winit: import_dmabuf -> {ok}");
        ok
    }

    fn early_import(&mut self, _surface: &WlSurface) {
        // Single renderer, single node: render-time import is already the
        // authoritative path; there is nothing to pre-stage.
    }

    fn sync_capable(&self) -> bool {
        // gles: not until EGL native fences are populated.
        false
    }

    fn export_render_fence(&mut self) -> Option<std::os::unix::io::OwnedFd> {
        // gles: implicit sync.
        None
    }
}

pub fn wire(_loop: &mut Loop, _wayland_socket_name: OsString, event_loop: &mut EventLoop<Loop>) {
    info!("Backend initialization - Winit");
    let winit = crate::window::factory::factory::create()
        .expect("winit backend initialization failed");

    info!("Backend initialization - wire backend to renderer and initialize renderer");

    let output = winit.output.clone();
    let mode = winit.mode;
    let display_handle = _loop.state.output.display_handle.clone();

    let mut contract = WinitContract {
        output: output.clone(),
        mode,
        winit,
    };
    let damage_tracker = scenegraph::state::lifecycle::lifecycle::initialize(
        _loop,
        &output.clone(),
        &display_handle.clone(),
        &mut contract,
    );
    info!("winit: lifecycle initialized, damage tracker ready");

    let winit = contract.winit;
    // GLES only: the `renderer` setting does not select a Vulkan path.
    info!("winit: renderer = gles");

    let mut context = WinitRenderContext {
        display_handle,
        output,
        winit_backend: winit.winit_backend,
        damage_tracker,
        tap_subscriptions: frames::draw::plan::tap::tap::TapSubscriptions::new(),
        owed_frames: None,
        this: std::rc::Weak::new(),
        swap_failures: 0,
        render_failures: 0,
    };

    // Producers keep their GLES-path resources (per-surface GlesTexture).
    model::stats::registry::base::set_compositor_prefers_dmabuf(false);
    // And WHAT it can import — the native backend's counterpart, which this path was
    // missing. The off-thread producers allocate their own dmabufs and hand them here to
    // be sampled, so they negotiate against this set and cannot ask the renderer directly
    // (it lives behind a `&mut` on this thread). Publishing nothing did not mean "no
    // constraint" to `worker_modifiers`, it meant an EMPTY intersection: the bridge saw a
    // renderer offering 0 formats against wgpu's 30, negotiated nothing, and every iced
    // and bevy surface failed to allocate. Published here, after the GLES fallback has
    // been resolved, so it names the renderer that will actually sample.
    {
        let formats = smithay::backend::renderer::ImportDma::dmabuf_formats(
            context.winit_backend.renderer(),
        );
        info!("winit: compositor-importable dmabuf pairs: {}", formats.iter().count());
        // Nested: no DRM node of our own, so this registers against
        // UNSPECIFIED. Still a real registration — what matters is that the
        // advertisement can see it.
        _loop.inner.kernel.get(&render_gles::format::registrar::registrar::FORMATS).register(
            render_gles::format::registrar::registrar::Device::UNSPECIFIED,
            render_gles::format::role::role::Role::Sample,
            formats,
            "winit renderer",
        );
    }

    // WHAT NESTED DOES NOT HAVE, said out loud.
    //
    // The format layer requires every role in `Role::ALL` to be REGISTERED before
    // it will answer anything, and it is deliberately not per-question: an answer
    // is not entitled to know which roles it happens to read. Nested composites
    // into a window on someone else's compositor — there is no scanout device and
    // no KMS plane — so these three have no answer here. Registering an empty set
    // states that; it reads identically to silence at every consumer (the term is
    // dropped either way) and differs only in that the layer can now tell "there
    // is none" from "it has not arrived yet".
    {
        use render_gles::format::role::role::Role;
        let formats = _loop.inner.kernel.get(&render_gles::format::registrar::registrar::FORMATS);
        formats.absent(Role::ScanoutEgl, "winit (nested: no scanout device)");
        formats.absent(Role::Render, "winit (nested: nothing renders into a scanout format)");
        formats.absent(Role::Plane, "winit (nested: no KMS plane)");
    }

    // The wgpu-gl context, the shared iced renderer and every registry, built
    // NOW rather than lazily inside the first frame, where they cost ~50 ms of
    // `comp_update` (the ced gate's 50 ms stall check counted it). After the
    // format roles above, which the context reads. `gpu::begin` on later
    // frames finds it built.
    frames::scene::gpu::prewarm(_loop, context.winit_backend.renderer());


    // Shared by the winit source, the redraw ping below and the empty-frame
    // callback timer (`compose::owe_frames`). calloop runs one callback at a
    // time, so the borrows never overlap.
    let context = std::rc::Rc::new(std::cell::RefCell::new(context));
    context.borrow_mut().this = std::rc::Rc::downgrade(&context);

    // A redraw REQUEST reaches winit only through the schedule's
    // ping, mapped here to `request_redraw` (which winit holds until the host's
    // frame callback). Nothing else asks winit for frames.
    let (redraw_ping, redraw_source) = smithay::reexports::calloop::ping::make_ping()
        .expect("winit: redraw ping");
    {
        let context = context.clone();
        event_loop
            .handle()
            .insert_source(redraw_source, move |_, _, state| {
                if let Ok(mut context) = context.try_borrow_mut() {
                    crate::scene::compose::compose::capture_offscreen(state, &mut context);
                    crate::frame::submit::submit::request_redraw(&mut context.winit_backend);
                } else {
                    screencopy::file::fail_all("no capture frame available");
                }
            })
            .expect("winit: redraw ping source");
    }
    _loop.state.redraw.set_ping(redraw_ping);

    // wlr-screencopy: advertised by the backend that services
    // the copies (`compose::draw` → `screencopy::service`).
    screencopy::create_global(&_loop.state.output.display_handle);

    // Server-side chrome (decor): the theme, from the shared design tokens
    // and the `chrome_style` preference, read once at startup.
    decor::window::install(decor::ChromeTheme::load(
        decor::ChromeStyle::from_name(&_loop.inner.preference.chrome_style).unwrap_or_default(),
    ));

    {
        let context = context.clone();
        event_loop
            .handle()
            .insert_source(winit.winit_loop, move |ref event, _, state| {
                crate::input::route::route::route(event, state, &mut context.borrow_mut());
            })
            .unwrap();
    }
    // The first frame: unknown pipes always need one.
    crate::frame::submit::submit::request_redraw(&mut context.borrow_mut().winit_backend);

    // No control-plane ping on winit: `control_ping` stays `None`.
    info!("winit backend wired: event source registered, entering loop");
}
