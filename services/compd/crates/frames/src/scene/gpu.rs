//! The GPU context compositor-owned UI and capture render through, bound to the
//! scene's GLES phase.
//!
//! wgpu runs on its GL backend over the renderer's OWN EGL context
//! (`runtime.surface` `wgpu_context`). That context
//! can only be wrapped while it is current, so the wgpu context — and the shared
//! iced renderer, the capture registry and every world's iced registry built from
//! it — is created here, on the compositor thread, the first time the scene has
//! the renderer in hand. There is no separate device and no background thread.
//!
//! Every wgpu call needs the context current (an external GL adapter makes
//! nothing current itself), so [`begin`] runs at the top of `scene::prepare` and
//! [`end`] at the bottom; all iced rasterization and capture readback happen
//! between them.

use std::sync::Arc;

use ui::{WgpuGlContext, create_wgpu_gl_context, make_current};
use world::state::Loop;
use smithay::backend::renderer::gles::GlesRenderer;

/// Make the renderer's context current for this frame's wgpu work, building the
/// GPU context and everything that hangs off it on the first call.
pub fn begin(state: &mut Loop, renderer: &mut GlesRenderer) {
    make_current(renderer);
    if state
        .inner
        .kernel
        .get(&world::surface::system::base::ICED_CONTEXT)
        .is_some()
    {
        // Worlds built after the first frame still get their registry (a no-op
        // when it exists, so this is one slot lookup per frame).
        world::surface::system::base::ensure_registry(
            state.inner.worlds.active_mut().storage_mut(),
            &state.inner.kernel,
        );
    } else {
        build(state, renderer);
        info!("wgpu-gl: context, shared iced renderer, capture registry and iced registries created");
    }
    // wgpu's vertex array, which `end` unbound last frame (graphics
    // `WgpuGlContext::acquire_gl_state`): without it every wgpu draw is refused.
    if let Some(ctx) = state.inner.kernel.get(&world::surface::system::base::ICED_CONTEXT).as_ref() {
        ctx.acquire_gl_state();
    }
}

/// Build the wgpu-gl context, the shared iced renderer and the registries
/// now, outside any frame (backend wiring calls it once the renderer and the
/// format roles exist), so the first frame does not carry ~50 ms of setup.
/// Idempotent: `begin` on a later frame finds the context and only ensures
/// late worlds' registries.
pub fn prewarm(state: &mut Loop, renderer: &mut GlesRenderer) {
    begin(state, renderer);
    end(state);
}

/// Hand the context back to smithay in the state its draws assume.
pub fn end(state: &Loop) {
    if let Some(ctx) = state
        .inner
        .kernel
        .get(&world::surface::system::base::ICED_CONTEXT)
        .as_ref()
    {
        ctx.release_gl_state();
    }
}

fn build(state: &mut Loop, renderer: &mut GlesRenderer) {
    let formats = state
        .inner
        .kernel
        .get(&render_gles::format::registrar::registrar::FORMATS)
        .clone();
    let ctx: Arc<WgpuGlContext> = match create_wgpu_gl_context(&formats, renderer) {
        Ok(ctx) => ctx.into_arc(),
        Err(e) => model::abort!(
            "wgpu-gl: no wgpu context over the renderer's EGL context: {e:?}"
        ),
    };
    *state
        .inner
        .kernel
        .get_mut(&world::surface::system::base::ICED_CONTEXT_MUT) = Some(ctx.clone());

    // BEFORE the registries: every world's registry takes a clone of this one
    // renderer, so it has to exist first.
    world::surface::system::base::ensure_engine(&mut state.inner.kernel);

    // Capture registry — kernel driver data shared by every backend.
    *state
        .inner
        .kernel
        .get_mut(&world::driver::capture::base::CAPTURE_REGISTRY_MUT) =
        Some(graphics::capture::registry::CaptureRegistry::new(ctx.clone()));

    // Every world's iced registry (each no-ops where its slot is absent — e.g. an
    // overlay world without SurfaceSystem). Covers the static worlds AND any
    // disk-restored ones.
    for id in state.inner.worlds.ids() {
        world::surface::system::base::ensure_registry(
            state.inner.worlds.get_mut(id).storage_mut(),
            &state.inner.kernel,
        );
    }

    // The main world hosts the iced surfaces; a miss here means the wiring above
    // is broken.
    let main = state
        .inner
        .worlds
        .get_mut(world::world::manager::manager::MAIN_WORLD)
        .storage_mut();
    if main
        .try_get_mut(&world::surface::system::base::SURFACE_MUT)
        .and_then(|s| s.registry.as_ref())
        .is_none()
    {
        model::abort!("main world iced registry missing after GPU init");
    }
}
