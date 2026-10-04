//! Error types for the engine layer.

use thiserror;
use wgpu::Features;

#[derive(Debug, thiserror::Error)]
pub enum EngineInitError {
    #[error("failed to create iced_wgpu Engine: backend rejected the surface format {0:?}")]
    EngineCreation(wgpu::TextureFormat),

    #[error("required wgpu features missing: {missing:?}")]
    MissingFeatures { missing: Features },
}
