use thiserror::Error;

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("dmabuf allocation: {0}")]
    Alloc(#[from] crate::surface::AllocError),

    #[error("gles import: {0}")]
    GlesImport(#[from] crate::surface::GlesImportError),

    #[error("wgpu import: {0}")]
    WgpuImport(#[from] crate::surface::WgpuImportError),

    #[error("registry has been dropped")]
    RegistryDropped,

    #[error("invalid output size: {w}x{h}")]
    InvalidSize { w: i32, h: i32 },

    #[error("gles error: {0}")]
    Gles(#[from] smithay::backend::renderer::gles::GlesError),
}
