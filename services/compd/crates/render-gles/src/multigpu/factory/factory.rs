//! GpuManager<GbmGlesBackend> construction + node addition. Owns the gles
//! multi-GPU type aliases (the only place the generic signatures are spelled
//! out on the gles side).
//! Failure policy: the selected renderer must construct — panic (original
//! unwrapped all three calls). The EGL factory closure keeps its local
//! Result: that is the smithay factory contract, local to implementation.

use crate::context::egl::egl;
use smithay::backend::allocator::gbm::GbmDevice;
use smithay::backend::drm::{DrmDeviceFd, DrmNode};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::multigpu::gbm::GbmGlesBackend;
use smithay::backend::renderer::multigpu::{GpuManager, MultiRenderer};

pub type NativeGpuBackend = GbmGlesBackend<GlesRenderer, DrmDeviceFd>;
pub type NativeGpuManager = GpuManager<NativeGpuBackend>;
pub type NativeMultiRenderer<'a> = MultiRenderer<'a, 'a, NativeGpuBackend, NativeGpuBackend>;

/// Create the manager with the High-priority EGL factory (ex wire.rs step 5).
pub fn create() -> NativeGpuManager {
    GpuManager::new(GbmGlesBackend::<GlesRenderer, DrmDeviceFd>::with_factory(
        |display| Ok(egl::create(display)?),
    ))
    .expect("GpuManager creation failed")
}

/// Register a GPU node with its GBM device.
pub fn add_node(gpus: &mut NativeGpuManager, node: DrmNode, gbm: GbmDevice<DrmDeviceFd>) {
    gpus.as_mut()
        .add_node(node, gbm)
        .expect("GPU node registration failed");
}

/// A renderer for a single node (the common case).
pub fn single_renderer<'a>(
    gpus: &'a mut NativeGpuManager,
    node: &DrmNode,
) -> NativeMultiRenderer<'a> {
    gpus.single_renderer(node).expect("single_renderer failed")
}
