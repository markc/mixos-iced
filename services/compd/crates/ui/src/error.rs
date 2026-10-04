//! Errors raised by the integration layer.

use thiserror;
use graphics::surface::SurfaceError;

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("surface allocation: {0}")]
    Surface(#[from] SurfaceError),
}

#[derive(Debug, thiserror::Error)]
pub enum ResizeError {
    #[error("surface resize: {0}")]
    Surface(#[from] SurfaceError),

    #[error("handle not registered: {0:?}")]
    UnknownHandle(super::handle::HandleId),
}

#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("handle not registered: {0:?}")]
    UnknownHandle(super::handle::HandleId),

    #[error("message dispatch: handle's UI type doesn't match the dispatched message type")]
    TypeMismatch,
}
