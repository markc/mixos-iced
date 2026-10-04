//! EGL context construction with the High-priority policy + capability probe.
//! (Ex wire.rs GpuManager factory closure body.)

use smithay::backend::egl::context::{ContextPriority, GlAttributes, PixelFormatRequirements};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::renderer::gles::{GlesError, GlesRenderer};

/// The context priority policy for compositor render contexts.
pub fn context_priority() -> ContextPriority {
    ContextPriority::High
}

/// Build a GlesRenderer for an EGL display: high-priority context, then
/// capability-probed renderer construction.
pub fn create(display: &EGLDisplay) -> Result<GlesRenderer, GlesError> {
    // EGL context creation yields egl::Error, which does not convert into
    // GlesError; per the crash-first policy (§12.1) a failed assembly-time
    // context is not self-recovering, so panic with the cause.
    // Match winit's ES3 baseline. The configless priority constructor requests
    // ES2, whereas wgpu's external GL adapter and GLES offscreen readback need
    // ES3. Do not depend on a driver upgrading an ES2 request implicitly.
    let context = EGLContext::new_with_config_and_priority(
        display,
        GlAttributes {
            version: (3, 0),
            profile: None,
            debug: cfg!(debug_assertions),
            vsync: false,
        },
        PixelFormatRequirements::_8_bit(),
        context_priority(),
    )
    .unwrap_or_else(|e| abort!("EGL context creation failed: {e:?}"));
    let capabilities = unsafe { GlesRenderer::supported_capabilities(&context)? };
    Ok(unsafe { GlesRenderer::with_capabilities(context, capabilities)? })
}
