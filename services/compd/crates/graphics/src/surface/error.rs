//! Error types for the runtime layer.
//!
//! Every fallible operation in this crate funnels through one of these.

use thiserror;

#[derive(Debug, thiserror::Error)]
pub enum AllocError {
    #[error("failed to open DRM render node: {0}")]
    OpenDrm(std::io::Error),

    #[error("failed to initialize gbm device: {0}")]
    GbmInit(std::io::Error),

    #[error("failed to create gbm buffer object: {0}")]
    CreateBo(std::io::Error),

    #[error("failed to export fd for plane: {0}")]
    ExportFd(gbm::InvalidFdError),

    #[error("failed to build Dmabuf from gbm buffer")]
    BuildDmabuf,

    #[error("invalid dimensions: width and height must be > 0 (got {width}x{height})")]
    InvalidDimensions { width: u32, height: u32 },
}

#[derive(Debug, thiserror::Error)]
pub enum WgpuContextError {
    /// compd: wgpu's GL backend could not wrap the compositor's EGL context.
    #[error("wgpu GL backend could not create an adapter over the compositor's EGL context")]
    NoAdapter,

    #[error("failed to create the wgpu GL device: {0}")]
    DeviceCreation(wgpu::RequestDeviceError),
}

#[derive(Debug, thiserror::Error)]
pub enum WgpuImportError {
    /// compd: the context is GL-only now; anything else is a wiring mistake.
    #[error("wgpu device isn't on the GL backend")]
    NotGlBackend,

    #[error("GL texture name is 0")]
    NullTexture,
}

#[derive(Debug, thiserror::Error)]
pub enum GlesImportError {
    #[error("GlesRenderer failed to import dmabuf: {0}")]
    ImportFailed(smithay::backend::renderer::gles::GlesError),
}

/// Aggregate error for `IcedSurface` operations (GL texture + its wgpu wrap).
#[derive(Debug, thiserror::Error)]
pub enum SurfaceError {
    /// compd: the GL texture iced renders into could not be allocated.
    #[error("gles texture allocation: {0}")]
    GlesTexture(smithay::backend::renderer::gles::GlesError),

    #[error("dmabuf allocation: {0}")]
    Alloc(#[from] AllocError),

    #[error("wgpu import: {0}")]
    WgpuImport(#[from] WgpuImportError),

    #[error("gles import: {0}")]
    GlesImport(#[from] GlesImportError),
}
