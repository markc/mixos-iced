//! `SnapshotHandle`: a detached frozen dmabuf.
//!
//! Returned by `CaptureHandle::take` and `CaptureHandle::snapshot`. Owns
//! its dmabuf and texture imports. Drops resources when the last clone is
//! released.

use std::sync::Arc;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::utils::{Physical, Size};

use crate::capture::registry::entry::SnapshotData;

/// Shared ownership of a frozen capture dmabuf.
///
/// Cloneable (Arc). When the last clone drops, the dmabuf and texture
/// imports release.
#[derive(Clone)]
pub struct SnapshotHandle {
    inner: Arc<SnapshotData>,
}

impl SnapshotHandle {
    pub(crate) fn from_entry(data: SnapshotData) -> Self {
        Self { inner: Arc::new(data) }
    }

    pub fn size(&self) -> Size<i32, Physical> {
        self.inner.size
    }

    /// Returns the underlying dmabuf. Clone via `dmabuf().clone()` if you
    /// need an owned `Dmabuf` to import elsewhere.
    pub fn dmabuf(&self) -> &Dmabuf {
        &self.inner.dmabuf.dmabuf
    }

    /// Returns the GLES texture import. The compositor side can sample
    /// from this if needed (typically the wgpu side is what consumers want).
    pub fn gles_texture(&self) -> &GlesTexture {
        &self.inner.gles_tex
    }

    /// Returns the wgpu texture import for downstream wiring (Bevy, etc).
    pub fn wgpu_texture(&self) -> Arc<wgpu::Texture> {
        self.inner.wgpu_tex.clone()
    }
}

impl std::fmt::Debug for SnapshotHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnapshotHandle")
            .field("size", &self.inner.size)
            .finish()
    }
}
