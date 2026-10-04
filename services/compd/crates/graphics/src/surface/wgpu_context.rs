//! The wgpu context compositor-owned UI renders through: wgpu's **GL backend**
//! over the compositor's OWN EGL context.
//!
//! There is one GPU API. The adapter IS the
//! compositor's EGL context: `wgpu::hal::gles::Adapter::new_external` wraps the
//! GL context smithay's `GlesRenderer` already owns, and iced draws straight into
//! GL textures that renderer allocated (`wgpu_import::wrap_gles_texture`). No
//! dmabuf, no second device, no adapter pinning — the context is the one the
//! compositor composites with, by construction.
//!
//! # The one rule: the context must be CURRENT
//!
//! An external GL adapter makes nothing current itself (`AdapterContext` has no
//! EGL handle), so every wgpu call that reaches GL — device creation, submit,
//! poll, and the deferred destruction those run — must happen on the compositor
//! thread while the renderer's context is current. [`make_current`] does that;
//! the scene's GLES `prepare()` phase calls it, then
//! [`WgpuGlContext::acquire_gl_state`], before any iced or capture work and
//! [`WgpuGlContext::release_gl_state`] after, so smithay's own draws start from the
//! state they assume.
//!
//! # wgpu needs a vertex array of its own
//!
//! wgpu-hal's GL backend binds its vertex array ONCE, when the device opens
//! (`Adapter::open`), and never again: it owns its context in every other
//! embedding. Here `release_gl_state` unbinds it after every pass, so the next
//! pass ran on the default vertex array, where GLES 3.1 and core GL refuse
//! `glVertexAttribFormat`/`glVertexAttribBinding` ("No array object bound") and
//! every iced draw was dropped: the surfaces kept their size and hit-tests but
//! drew nothing (quoin_scenes_gate Q5, a pure-black panel band). wgpu sets every
//! attribute it uses per pass and clears them at the pass end, so any vertex
//! array works; this context creates one and `acquire_gl_state` binds it.
//!
//! # Where an off-thread worker would hook back in
//!
//! iced is rasterized on the compositor thread. Moving it to a worker thread
//! with GL needs a SECOND context SHARED with the renderer's
//! (`EGLContext::new_shared`), made current on the worker thread with its own
//! `new_external` adapter; the texture names are then visible to both, and each
//! published frame needs an `EGLSync` fence the compositor waits on before
//! sampling. Do that only if measurement shows iced rasterization on the
//! compositor thread costs frames.

use std::sync::Arc;

use smithay::backend::renderer::gles::GlesRenderer;
use wgpu::{Adapter, Device, DeviceDescriptor, ExperimentalFeatures, Instance, Queue};

use crate::surface::error::WgpuContextError;

/// The wgpu instance/adapter/device/queue over the compositor's GL context.
///
/// Wrapped in `Arc` so it can be shared across the iced engine, every world's
/// registry and the capture registry. Keep it alive for the whole program, and
/// drop it only with the GL context current.
pub struct WgpuGlContext {
    pub instance: Instance,
    pub adapter: Adapter,
    pub device: Device,
    pub queue: Queue,
    /// The kernel's format registrar, carried for the producers (capture) that
    /// allocate through this context.
    pub formats: render_gles::format::registrar::registrar::Registrar,
    /// The vertex array wgpu's passes run on (see the module docs); `None` on
    /// a context without vertex arrays, where the default one is all there is.
    vertex_array: Option<glow::VertexArray>,
}

impl WgpuGlContext {
    /// Wrap in `Arc` for sharing.
    pub fn into_arc(self) -> Arc<Self> {
        Arc::new(self)
    }

    /// Bind the vertex array wgpu's passes need (module docs) before any wgpu
    /// work this frame. Call with the context current; pairs with
    /// [`Self::release_gl_state`].
    pub fn acquire_gl_state(&self) {
        use glow::HasContext;
        let Some(vertex_array) = self.vertex_array else { return };
        // SAFETY: called on the compositor thread with the renderer's context
        // current (module docs); only the vertex-array binding changes.
        unsafe {
            let Some(hal) = self.device.as_hal::<wgpu::hal::api::Gles>() else { return };
            let gl = hal.context().lock();
            gl.bind_vertex_array(Some(vertex_array));
        }
    }

    /// Put the GL state smithay's renderer assumes back after wgpu used the
    /// context: no framebuffer, program or vertex array bound, and the per-pass
    /// enables wgpu may have left on switched off. smithay binds its own target,
    /// program, buffers and texture unit per draw; these are the bindings it does
    /// NOT restate. Call with the context current.
    pub fn release_gl_state(&self) {
        use glow::HasContext;
        // SAFETY: called on the compositor thread with the renderer's context
        // current (see the module docs); only state bindings are reset.
        unsafe {
            let Some(hal) = self.device.as_hal::<wgpu::hal::api::Gles>() else { return };
            let gl = hal.context().lock();
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.bind_vertex_array(None);
            gl.use_program(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, None);
            // wgpu's transfer/barrier commands can leave these bound. A GLES
            // texture allocation/upload otherwise interprets its CPU pointer
            // (including null for allocation) as an offset into that PBO.
            gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
            // Sampler objects: wgpu binds one per texture unit it samples
            // through, and a bound sampler OVERRIDES the texture's own filter
            // and LOD state. smithay never binds samplers, so one wgpu left on a
            // unit decides how smithay samples there; a mipmapped filter over
            // a single-level texture makes it incomplete, which GL samples as
            // opaque black (0,0,0,1).
            for unit in 0..16 {
                gl.bind_sampler(unit, None);
            }
            gl.active_texture(glow::TEXTURE0);
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::STENCIL_TEST);
            gl.disable(glow::CULL_FACE);
            gl.blend_equation_separate(glow::FUNC_ADD, glow::FUNC_ADD);
            gl.color_mask(true, true, true, true);
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
            gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
            gl.pixel_store_i32(glow::UNPACK_IMAGE_HEIGHT, 0);
            gl.pixel_store_i32(glow::UNPACK_SKIP_PIXELS, 0);
            gl.pixel_store_i32(glow::UNPACK_SKIP_ROWS, 0);
            gl.pixel_store_i32(glow::UNPACK_SKIP_IMAGES, 0);
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);
            gl.pixel_store_i32(glow::PACK_ROW_LENGTH, 0);
            gl.pixel_store_i32(glow::PACK_SKIP_PIXELS, 0);
            gl.pixel_store_i32(glow::PACK_SKIP_ROWS, 0);
        }
    }
}

/// Make the renderer's GL context current on this thread. Every wgpu call on a
/// [`WgpuGlContext`] must be preceded by this (or by any smithay renderer call,
/// which makes the same context current) — see the module docs.
pub fn make_current(gles: &mut GlesRenderer) {
    // wgpu's EGL instance initialization may select desktop OpenGL. EGL's
    // current-context queries, fence operations and unbinding depend on this
    // thread-local API even when eglMakeCurrent was given an explicit ES context.
    unsafe {
        use smithay::backend::egl::ffi::egl;
        assert_ne!(
            egl::BindAPI(egl::OPENGL_ES_API),
            egl::FALSE,
            "wgpu-gl: could not select the renderer's GLES API"
        );
    }
    if let Err(e) = gles.with_context(|_| ()) {
        panic!("wgpu-gl: could not make the renderer's EGL context current: {e:?}");
    }
}

/// Build the wgpu context on `gles`'s EGL context (made current here).
///
/// Synchronous and on the compositor thread: the adapter has to be created with
/// the context current, so this runs the first time the scene's GLES phase has
/// the renderer in hand, not on a worker.
pub fn create_wgpu_gl_context(
    formats: &render_gles::format::registrar::registrar::Registrar,
    gles: &mut GlesRenderer,
) -> Result<WgpuGlContext, WgpuContextError> {
    make_current(gles);

    let instance = Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::GL,
        flags: wgpu::InstanceFlags::empty(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        backend_options: wgpu::BackendOptions::default(),
        display: None,
    });
    // Instance initialization owns a separate EGL backend and may change the
    // bound client API. Re-establish our context before wrapping it externally.
    make_current(gles);

    // SAFETY: the renderer's context is current (above) and stays the context
    // every later wgpu call runs under (module docs).
    let exposed = unsafe {
        wgpu::hal::gles::Adapter::new_external(
            |name| smithay::backend::egl::get_proc_address(name),
            wgpu::GlBackendOptions::default(),
        )
    }
    .ok_or(WgpuContextError::NoAdapter)?;
    // SAFETY: as above.
    let adapter = unsafe { instance.create_adapter_from_hal(exposed) };

    let info = adapter.get_info();
    info!(
        "wgpu-gl: adapter over the compositor's EGL context: {} ({:?}, backend={:?})",
        info.name, info.device_type, info.backend
    );
    render_gles::format::audit::audit::node(
        "iced wgpu (compositor GL context)",
        &format!("{} ({:?})", info.name, info.device_type),
    );

    // The GL backend's own limits, not the WebGPU defaults: GLES 3.x advertises
    // fewer of several (colour attachments among them), and `request_device`
    // fails outright on any default the adapter does not meet.
    let limits = wgpu::Limits::default().or_worse_values_from(&adapter.limits());

    let (device, queue) = pollster::block_on(adapter.request_device(&DeviceDescriptor {
        experimental_features: ExperimentalFeatures::disabled(),
        label: Some("compd_iced_gl_device"),
        required_features: wgpu::Features::empty(),
        required_limits: limits,
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: wgpu::Trace::Off,
    }))
    .map_err(WgpuContextError::DeviceCreation)?;
    info!("wgpu-gl: device + queue created");

    // wgpu's own vertex array, rebound before each pass (module docs).
    // SAFETY: the renderer's context is current (above).
    let vertex_array = unsafe {
        use glow::HasContext;
        device.as_hal::<wgpu::hal::api::Gles>().and_then(|hal| hal.context().lock().create_vertex_array().ok())
    };
    if vertex_array.is_none() {
        warn!("wgpu-gl: no vertex array for wgpu's passes; iced draws need a GL with vertex array objects");
    }

    Ok(WgpuGlContext {
        instance,
        adapter,
        device,
        queue,
        formats: formats.clone(),
        vertex_array,
    })
}
