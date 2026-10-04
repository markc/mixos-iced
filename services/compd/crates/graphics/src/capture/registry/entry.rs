//! Internal capture entry — owns the dmabuf and texture resources.

use std::sync::Arc;

use smithay::backend::renderer::gles::GlesTexture;
use smithay::utils::{Physical, Size};
use crate::surface::AllocatedDmabuf;

use crate::capture::registry::source::CaptureSource;

/// Opaque, `Copy + Eq + Hash` identifier for a capture entry. Owners
/// observe entry changes by comparing this across frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntryId(pub(crate) u64);

/// One continuous capture entry — owns the dmabuf, gles texture, wgpu
/// texture. Lives inside the registry; `CaptureHandle`s reference it by id.
pub(crate) struct CaptureEntry {
    pub id: EntryId,
    pub source: CaptureSource,
    pub size: Size<i32, Physical>,

    pub dmabuf: AllocatedDmabuf,
    pub gles_tex: GlesTexture,
    pub wgpu_tex: Arc<wgpu::Texture>,

    /// Number of `CaptureHandle`s pointing at this entry. When zero, the
    /// registry frees it.
    pub refcount: usize
}

/// The data block transferred zero-copy from a `CaptureEntry` to a
/// `SnapshotHandle` when `take` is the sole owner. Same fields minus
/// registry bookkeeping.
pub(crate) struct SnapshotData {
    pub size: Size<i32, Physical>,
    pub dmabuf: AllocatedDmabuf,
    pub gles_tex: GlesTexture,
    pub wgpu_tex: Arc<wgpu::Texture>,
}

impl From<CaptureEntry> for SnapshotData {
    fn from(e: CaptureEntry) -> Self {
        Self {
            size: e.size,
            dmabuf: e.dmabuf,
            gles_tex: e.gles_tex,
            wgpu_tex: e.wgpu_tex,
        }
    }
}
