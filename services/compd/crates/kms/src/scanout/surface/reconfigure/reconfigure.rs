//! Mode/transform/scale changes on a RUNNING pipe. Real delegation: smithay
//! master exposes `use_mode` on the locked manager. This is the mechanism
//! `native.device/device.interface` routes mode changes through.

use crate::scanout::surface::output::output::{
    NativeDrmOutput, NativeDrmOutputManager,
};
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::drm::output::DrmOutputRenderElements;
use smithay::backend::renderer::element::RenderElement;
use smithay::backend::renderer::{Bind, Renderer, Texture};
use smithay::reexports::drm::control::{crtc, Mode as DrmMode};

pub fn set_mode<R, E>(
    manager: &mut NativeDrmOutputManager,
    pipe: crtc::Handle,
    mode: DrmMode,
    renderer: &mut R,
) -> Result<(), String>
where
    R: Renderer + Bind<Dmabuf>,
    R::TextureId: Texture + 'static,
    R::Error: Send + Sync + 'static,
    E: RenderElement<R>,
{
    manager
        .lock()
        .use_mode::<_, E>(&pipe, mode, renderer, &DrmOutputRenderElements::default())
        .map_err(|e| format!("use_mode failed: {e:?}"))
}

/// Per-output mode application when only the DrmOutput handle is available.
pub fn set_output_mode<R, E>(
    output: &mut NativeDrmOutput,
    mode: DrmMode,
    renderer: &mut R,
) -> Result<(), String>
where
    R: Renderer + Bind<Dmabuf>,
    R::TextureId: Texture + 'static,
    R::Error: Send + Sync + 'static,
    E: RenderElement<R>,
{
    output
        .use_mode::<_, E>(mode, renderer, &DrmOutputRenderElements::default())
        .map_err(|e| format!("use_mode failed: {e:?}"))
}
