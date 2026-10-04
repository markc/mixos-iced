//! Renderer-side assembly: allocator/exporter -> GpuManager -> render formats
//! (Law-7 modifier filter when gated) -> hosted DrmOutputManager -> pipe
//! bring-up over the mode fallback chain -> gpu binding + contract.
//! (Ex wire.rs `new()` steps 5 + 8, recomposed.)
//!
//! GLES only: there is no renderer selection and no Vulkan path.

use render_gles::element::wrap::wrap::GlesElementWrapper;
use crate::assemble::display::display::DisplayAssembly;
use kms::scanout::surface::output::output::{
    NativeDrmOutput, NativeDrmOutputManager,
};
use outputs::render_contract::contract::{RenderContract, RendererId};
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::renderer::ImportEgl;
use smithay::output::{Mode, Output};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::backend::renderer::gles::GlesRenderer;
use std::cell::RefCell;
use std::rc::Rc;
use scenegraph::scene::element::element::SceneElement;
use world::state::state::StateDRMBinding;

pub struct RendererAssembly {
    pub gpu_binding: Rc<RefCell<StateDRMBinding>>,
    pub drm_output_manager: Rc<RefCell<NativeDrmOutputManager>>,
    pub drm_output: NativeDrmOutput,
}

/// The assembly (GLES; the only renderer).
pub fn assemble(formats: &render_gles::format::registrar::registrar::Registrar, display: &mut DisplayAssembly) -> RendererAssembly {
    assemble_gles(formats, display)
}

/// The gles arm (the original path, with the mode fallback chain made real).
fn assemble_gles(formats: &render_gles::format::registrar::registrar::Registrar, display: &mut DisplayAssembly) -> RendererAssembly {
    // Native scanout machine validation (reinstated de-delegation crates):
    // kernel-checked against the real device, screen untouched. The hosted
    // manager remains the live path until the swap-over; a compiled-in
    // machine that fails its proof panics.
    #[cfg(feature = "native-scanout")]
    {
        let proof = native_scanout_self_test(display);
        info!("native scanout machine validated (TEST_ONLY): {proof}");
    }

    // GpuManager with the High-priority EGL factory; register the node.
    let mut gpus = render_gles::multigpu::factory::factory::create();
    render_gles::multigpu::factory::factory::add_node(
        &mut gpus,
        display.primary_gpu,
        display.gbm.clone(),
    );

    // Allocator + exporter (flag policy in drm.gbm/gbm.alloc).
    let allocator = kms::gbm::alloc::alloc::allocator(display.gbm.clone());
    let exporter =
        kms::gbm::alloc::alloc::exporter(display.gbm.clone(), display.primary_gpu);

    // Render formats from the primary's EGL context, narrowed by the Law-7
    // modifier filter when its double gate is satisfied.
    let mut renderer = render_gles::multigpu::factory::factory::single_renderer(
        &mut gpus,
        &display.primary_gpu,
    );
    // Register the scanout device's own answer, then ASK the format layer. This
    // crate no longer intersects anything: it hands over a capability and takes a
    // decision.
    formats.set_split_device(display.split_device);
    formats.register(
        render_gles::format::registrar::registrar::Device::of(&display.primary_gpu),
        render_gles::format::role::role::Role::ScanoutEgl,
        filter_formats(renderer.as_mut().egl_context().dmabuf_render_formats().clone()),
        "scanout egl (dmabuf_render_formats)",
    );
    // THE REST OF THE MACHINE, BEFORE THE FIRST ANSWER.
    //
    // `scanout_formats` below is an `answer::available` call, and the format layer
    // refuses to answer anything while `registrar::REQUIRED` is unsatisfied. It
    // reads only `ScanoutEgl` and `Render`, but the rule is deliberately not
    // per-question — an answer is not entitled to know which roles it happens to
    // read — so the two EGL-derived roles are registered HERE, where the renderer
    // that answers for them is already in hand, instead of after the assembly.
    //
    // Both are the same expression the later registration uses, so this is not a
    // placeholder that gets corrected: `bind::bind` returns exactly
    // `ImportDma::dmabuf_formats(renderer)` (the wl_drm bind beside it is a legacy
    // bridge and does not change the list), and `bind::texture_formats` is exactly
    // `egl_context().dmabuf_texture_formats()`. The re-registration that follows in
    // `wire.entry` therefore sees an identical set and is a silent no-op.
    formats.register(
        render_gles::format::registrar::registrar::Device::UNSPECIFIED,
        render_gles::format::role::role::Role::GlesSample,
        smithay::backend::renderer::ImportDma::dmabuf_formats(&renderer),
        "egl (dmabuf_formats)",
    );
    // `Sample` only if nobody has answered for it yet. A VULKAN composite registers
    // it before the assembly from its physical device — the identical value, since
    // `VulkanRenderer::dmabuf_formats()` IS `modifier::import_formats(&phd)` — and
    // that is the set that must stand. Registering unconditionally here would
    // overwrite it with the GLES renderer's, which is a different device's answer
    // to the same question. Checking rather than predicating on the `renderer`
    // setting also covers the case where Vulkan was asked for and its physical
    // device lookup failed: nothing registered, and this fills in.
    if formats.view().set(render_gles::format::role::role::Role::Sample).is_none() {
        formats.register(
            render_gles::format::registrar::registrar::Device::of(&display.primary_gpu),
            render_gles::format::role::role::Role::Sample,
            renderer.as_mut().egl_context().dmabuf_texture_formats().clone(),
            "gles composite (dmabuf_texture_formats)",
        );
    }
    let render_formats = scanout_formats(formats);

    let drm = display
        .drm
        .take()
        .expect("DrmDevice already taken by a previous renderer assembly");
    // Offer a 10-bit scanout in two independent cases (smithay falls back to
    // 8-bit if the plane can't): HDR (PQ needs the precision) and plain
    // deep-color SDR via depth == 10. Depth and HDR are decoupled — HDR
    // implies 10-bit, but depth == 10 gives 10-bit SDR without engaging
    // the PQ/HDR composite (the SDR transfer is byte-range identical, just finer
    // quantization). PQ is only signalled (stage C) when HDR is actually active.
    let env = model::environment::config::base::get();
    let hdr_scanout = display.hdr.hdr_capable() && env.hdr;
    let deep_color = env.depth == 10;
    let ten_bit = hdr_scanout || deep_color;
    info!("native scanout: hdr={hdr_scanout} deep_color={deep_color} → 10-bit={ten_bit}");
    let mut drm_output_manager = kms::scanout::surface::output::output::manager(
        drm,
        allocator,
        exporter,
        Some(display.gbm.clone()),
        render_formats,
        ten_bit,
    );

    // Pipe bring-up over the validating-modeset fallback chain (the original
    // wire.rs pseudocode made real). Chain exhaustion is the panic.
    let chain = display.mode_chain.clone();
    let mut slot: Option<NativeDrmOutput> = None;
    let chosen = kms::scanout::commit::test::test::try_chain(chain, |mode| {
        match kms::scanout::surface::output::output::initialize::<
            _,
            GlesElementWrapper<SceneElement<GlesRenderer>>,
        >(
            &mut drm_output_manager,
            display.pipe,
            mode,
            &[display.connector.handle()],
            &display.output,
            &mut renderer,
        ) {
            Ok(out) => {
                slot = Some(out);
                Ok(())
            }
            Err(e) => Err(e),
        }
    })
    .unwrap_or_else(|e| abort!("every candidate mode failed the validating modeset: {e}"));

    if chosen != display.drm_mode {
        // Keep the assembly's published mode honest with what actually drove
        // the pipe; the Output state propagates through the same path
        // `device.interface` uses.
        display.drm_mode = chosen;
        display.mode = Mode::from(chosen);
        display
            .output
            .change_current_state(Some(display.mode), None, None, None);
        warn!(
            "selected mode failed; pipe driven by fallback {}x{}@{}",
            chosen.size().0,
            chosen.size().1,
            chosen.vrefresh()
        );
    }
    let drm_output = slot.expect("try_chain returned Ok without an initialized output");

    // Record what the swapchain ACTUALLY settled on, now the modeset has succeeded and
    // smithay has chosen from the render formats we offered.
    //
    // `set_device_format` was previously called only for the bevy and monitor dmabuf
    // allocations, so the one buffer that matters most for scanout cost — the framebuffer
    // being flipped — never reached the Statistics tab. LINEAR versus a vendor tiled modifier
    // is the difference between "this hardware is slow" and "we are scanning out
    // uncompressed"; on a tiler (V3D) a linear render target is a structural cost, not a
    // rounding error. Pairs with the `IN_FORMATS` dump in `assemble.display`: that reports
    // what the plane WOULD accept, this reports what we took.
    drm_output.with_compositor(|comp| {
        let fourcc = comp.format();
        let offered = comp.modifiers().len();
        // The swapchain reports the modifier SET it may allocate from, not the single
        // modifier of the live buffer. With one entry those coincide; with more, the first is
        // what smithay ranks highest. The count is logged so an ambiguous case is visible
        // instead of being reported as fact.
        let modifier = comp
            .modifiers()
            .first()
            .copied()
            .unwrap_or(render_gles::format::rule::rule::UNKNOWN);
        // Print the WHOLE offered set, not just the head. Logging only the first turned out
        // to be useless in the one case that matters: `modifier=Invalid (2 offered)` says we
        // are on the implicit/driver-negotiated path but hides what the alternative was, so it
        // cannot distinguish "the driver had a tiled option and we let it choose" from "linear
        // was the only other candidate".
        let all: Vec<String> = comp.modifiers().iter().map(|m| format!("{m:?}")).collect();
        info!(
            "scanout swapchain: fourcc={fourcc:?} modifier={modifier:?} \
             ({offered} offered: {})",
            all.join(", ")
        );
        use render_gles::format::rule::rule;
        // The achieved depth, for producers that should match it rather than
        // render 8-bit into a 10-bit pipeline (the background worker does).
        formats.set_scanout_fourcc(u32::from(display.pipe) as u64, fourcc);
        model::stats::registry::base::set_device_format(
            "scanout",
            &format!("{fourcc:?}"),
            modifier.into(),
            rule::label(rule::classify(modifier)),
            1,
        );
    });

    // M4: enable VRR / adaptive-sync on capable outputs (controlled by `vrr`).
    // smithay sets VRR_ENABLED on the CRTC; a no-op on fixed-refresh panels. With
    // VRR active and our damage-driven scheduling, the refresh rate tracks content.
    // Same call the runtime builder (`context.display/display.build`) makes for
    // every pipe it constructs — assembly is just the first one. Kept as one
    // implementation so a second monitor and a failed-over primary get exactly
    // what the assembly pipe gets.
    let conn = display.connector.handle();
    let vrr = kms::scanout::surface::output::output::apply_vrr(
        &drm_output,
        conn,
        env.vrr,
    );
    info!(
        "native: VRR requested={} supported={} enabled={}",
        env.vrr, vrr.supported, vrr.enabled
    );
    model::stats::registry::base::set_vrr(vrr.supported, vrr.enabled);

    // Output + mode for the Statistics tab.
    {
        let m = display.mode;
        let mode_str = format!(
            "{}x{}@{:.2}",
            m.size.w,
            m.size.h,
            m.refresh as f32 / 1000.0
        );
        model::stats::registry::base::set_output(
            &display.output.name(),
            &mode_str,
        );
    }

    drop(renderer);

    let gpu_binding = Rc::new(RefCell::new(StateDRMBinding {
        gpus,
        primary: display.primary_gpu,
    }));

    info!("Init native backend OK (assemble.renderer, gles)");
    RendererAssembly {
        gpu_binding,
        drm_output_manager: Rc::new(RefCell::new(drm_output_manager)),
        drm_output,
    }
}

#[cfg(feature = "modifier-fallback")]
fn filter_formats(formats: FormatSet) -> FormatSet {
    if render_gles::preference::enable::safety::safety::get().modifier_fallback {
        kms::scanout::framebuffer::modifier::modifier::filter_legacy(formats)
    } else {
        formats
    }
}

#[cfg(not(feature = "modifier-fallback"))]
fn filter_formats(formats: FormatSet) -> FormatSet {
    formats
}

/// The scanout swapchain's candidate set, from the format layer.
///
/// The narrowing itself (scanout EGL ∩ what the composite can colour-attach, with
/// `INVALID` dropped) lives in `format.resolve`. What stays here is the RESPONSE
/// to a refusal, because only this layer can abort startup.
///
/// A refusal means there is no legal arrangement in which this composite renders
/// directly into this scanout device's buffers. The only correct answers are the
/// cross-device blit path (composite renders into its OWN tiled buffer, a copy
/// bridges to the scanout — see `~/PRIME_VULKAN.md`), which is not implemented, or
/// a composite on the scanout device itself. Failing at startup beats a session
/// that renders undefined pixels and freezes later — which is what the old
/// widen-back-to-EGL fallback did.
fn scanout_formats(formats: &render_gles::format::registrar::registrar::Registrar) -> FormatSet {
    use render_gles::format::answer::answer;
    use render_gles::format::rule::rule::Outcome;
    let answer::Answer::Set { set, outcome } =
        answer::available(formats, answer::Consumer::ScanoutSwapchain)
    else {
        abort!("scanout: the format layer answered the swapchain with a non-set shape");
    };
    if let Outcome::Refused(why) = outcome {
        abort!(
            "scanout: the composite can render into NONE of the pair(s) this scanout device \
             offers — {why}. Rendering into an unsanctioned modifier is what this refuses to \
             do. Until the cross-device blit path lands, run the composite on the scanout \
             device instead: set `renderer = \"gles\"`, or point `render_node` at the \
             scanout device."
        );
    }
    info!(
        "scanout render formats: {} pair(s) after the composite's renderable set",
        set.iter().count()
    );
    set
}

/// Exercise the reinstated de-delegation crates end-to-end against the real
/// device: property discovery -> primary plane -> swapchain -> framebuffer
/// import (cached) -> full-modeset request (+ OUT_FENCE_PTR arm and
/// IN_FENCE_FD attach where the device supports them) -> TEST_ONLY commit ->
/// page-flip request -> TEST_ONLY commit -> slot submission/aging.
#[cfg(feature = "native-scanout")]
fn native_scanout_self_test(display: &DisplayAssembly) -> String {
    use kms::scanout::commit::build::build;
    use kms::scanout::commit::submit::submit;
    use kms::scanout::swapchain::acquire::acquire;
    use kms::scanout::swapchain::slot::slot;
    use smithay::backend::allocator::{Buffer, Modifier};

    let drm_fd = &display.drm_fd;
    let res = kms::connector::scan::scan::resources(
        display.drm.as_ref().expect("self-test must run before the manager takes the device"),
    );

    // Pipeline property tables + the primary plane for the claimed pipe.
    let plane = build::primary_plane(drm_fd, &res, display.pipe);
    let props = build::pipeline_props(drm_fd, display.connector.handle(), display.pipe, plane);

    // Swapchain over the GL-path allocator; one slot; cached framebuffer.
    let allocator = kms::gbm::alloc::alloc::allocator(display.gbm.clone());
    let exporter =
        kms::gbm::alloc::alloc::exporter(display.gbm.clone(), display.primary_gpu);
    let (w, h) = (
        display.drm_mode.size().0 as u32,
        display.drm_mode.size().1 as u32,
    );
    // A self-test swapchain, deliberately implicit: it proves the KMS plumbing,
    // not the negotiation, so it asks the layer for the fourcc and seeds the
    // driver-negotiated modifier by name rather than spelling either here.
    let mut swapchain = slot::create(
        allocator,
        (w, h),
        render_gles::format::catalog::catalog::FLOOR,
        vec![render_gles::format::rule::rule::UNKNOWN],
    );
    let buffer_slot = acquire::acquire(&mut swapchain);
    let fb = kms::scanout::framebuffer::export::export::framebuffer_for(
        &exporter, drm_fd, &buffer_slot,
    );
    let _ = buffer_slot.size(); // the slot derefs to the allocator buffer
    let fb_handle = kms::scanout::framebuffer::export::export::handle(&fb);

    let frame = build::PlaneFrame {
        fb: fb_handle,
        src: (w, h),
        dst: (0, 0, w, h),
    };

    // Full modeset request, fences armed where supported, kernel-validated.
    let mut req = build::build_modeset(
        drm_fd,
        display.connector.handle(),
        display.pipe,
        plane,
        &props,
        &display.drm_mode,
        frame,
    );
    let mut out_slot = kms::scanout::fence::out::out::OutFenceSlot::new();
    let out_supported = kms::scanout::fence::out::out::OutFenceSlot::supported(&props);
    if out_supported {
        out_slot.arm(&mut req, display.pipe, &props);
    }
    let in_supported = kms::scanout::fence::in_::r#in::has_in_fence(&props);
    let _held_fence; // must outlive the commit ioctl
    if in_supported {
        let syncobj = kms::syncobj::device::device::create(drm_fd, true)
            .expect("self-test syncobj creation failed");
        let fence = kms::scanout::fence::in_::r#in::from_syncobj(drm_fd, syncobj);
        kms::scanout::fence::in_::r#in::attach(&mut req, plane, &props, &fence);
        _held_fence = Some(fence);
        kms::syncobj::device::device::destroy(drm_fd, syncobj)
            .expect("self-test syncobj destroy failed");
    } else {
        _held_fence = None;
    }
    submit::test(drm_fd, req, true)
        .unwrap_or_else(|e| abort!("native modeset request failed kernel validation: {e}"));

    // Page-flip shape, kernel-validated; then slot pacing.
    let flip_req = build::build_flip(display.pipe, plane, &props, frame);
    submit::test(drm_fd, flip_req, false)
        .unwrap_or_else(|e| abort!("native flip request failed kernel validation: {e}"));
    acquire::submitted(&mut swapchain, &buffer_slot);
    let age = slot::age(&buffer_slot);

    format!(
        "primary plane {plane:?}, modeset+flip TEST_ONLY OK, slot age {age},          IN_FENCE_FD {}, OUT_FENCE_PTR {}",
        if in_supported { "attached" } else { "unsupported (nvidia-class)" },
        if out_supported { "armed" } else { "unsupported" },
    )
}

/// The contract object handed to `lifecycle::initialize` (the pre-existing
/// DisplayBackend shape) and kept as the import-capability surface for the
/// dmabuf/syncobj globals — part of the handles `wire.entry` returns to the
/// main project.
pub struct NativeContract {
    pub output: Output,
    pub mode: Mode,
    pub gpu_binding: Rc<RefCell<StateDRMBinding>>,
}

impl outputs::render_contract::contract::DisplayBackend for NativeContract {
    fn load(&mut self) -> (&Output, &Mode) {
        (&self.output, &self.mode)
    }

    fn bind_display(&mut self, display_handle: &DisplayHandle) -> FormatSet {
        let mut binding = self.gpu_binding.borrow_mut();
        let StateDRMBinding { gpus, primary } = &mut *binding;
        let primary = *primary;
        render_gles::multigpu::bind::bind::bind(gpus, &primary, display_handle)
    }
}

impl RenderContract for NativeContract {
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
        let mut binding = self.gpu_binding.borrow_mut();
        let StateDRMBinding { gpus, primary } = &mut *binding;
        let primary = *primary;
        render_gles::multigpu::bind::bind::texture_formats(gpus, &primary)
    }

    fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        let mut binding = self.gpu_binding.borrow_mut();
        let StateDRMBinding { gpus, primary } = &mut *binding;
        let primary = *primary;
        render_gles::multigpu::bind::bind::import_dmabuf(gpus, &primary, dmabuf)
    }

    fn early_import(&mut self, surface: &WlSurface) {
        let mut binding = self.gpu_binding.borrow_mut();
        let StateDRMBinding { gpus, primary } = &mut *binding;
        let primary = *primary;
        render_gles::multigpu::bind::bind::early_import(gpus, &primary, surface);
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
