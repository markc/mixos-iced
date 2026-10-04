//! The renderer contract. One renderer among peers (gles today, vulkan
//! arriving) implements this; hosts select through it. There is NO fallback
//! between renderers: a selected renderer that cannot run panics at assembly.
//!
//! Constraint (recorded in the architecture document): this crate must not
//! require the `renderer_gl` smithay feature. It speaks in renderer-agnostic
//! vocabulary only: format sets, dmabufs, display handles, fds.
//!
//! REVISION (Phase 4 Step 6), informed by what the vulkan work proved:
//! - import is modifier-aware: `supported_formats()` exposes the negotiated
//!   (fourcc x modifier) set so the linux-dmabuf global advertises exactly
//!   what the active renderer can take.
//! - explicit sync is a first-class capability: `export_render_fence` is the
//!   render-completion side; `sync_capable()` tells the syncobj protocol
//!   whether the renderer honors acquire points natively.

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::format::FormatSet;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::DisplayHandle;

/// Re-export of the pre-existing display-output backend trait, which
/// `lifecycle::initialize` consumes. The render contract extends it.
pub use scenegraph::display::output::backend::Backend as DisplayBackend;

/// Which renderer implementation is active. Mirrors
/// `compositor_kernel_graphic_preference_renderer_rank::rank::RendererKind` by name; kept
/// separate so the contract does not depend on the preference crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererId {
    Gles,
    Vulkan,
}

impl RendererId {
    /// The renderer's name as the `comp` Bus service reports it (`info.engine`).
    pub fn label(self) -> &'static str {
        match self {
            RendererId::Gles => "gles",
            RendererId::Vulkan => "vulkan",
        }
    }
}

/// The renderer contract — the surface the main project integrates against
/// (alongside `DisplayBackend`, which `lifecycle::initialize` consumes).
///
/// Capabilities, in the order they were added to the design:
/// 1. passes/elements      — exercised through the frame plan executor
///    (element types are renderer-internal; the contract does not name them).
/// 2. fence capability     — explicit-sync export of render completion
///    (`export_render_fence`, `sync_capable`).
/// 3. tap-blit capability  — observing a rendered pass into a tap target,
///    placed by the frame plan and keyed by `plan.tap` subscriptions.
/// 4. import capability    — REQUIRED. The linux-dmabuf global validates
///    client buffers through whichever renderer is active; the advertised
///    format/modifier set comes from `supported_formats`.
pub trait RenderContract {
    fn id(&self) -> RendererId;

    /// Legacy wl_drm client-acceleration bridge. GLES-only concept; Vulkan
    /// implementations return the dmabuf format set without binding.
    fn bind_display(&mut self, display_handle: &DisplayHandle) -> FormatSet;

    /// The (fourcc x modifier) set the renderer can import — what the
    /// linux-dmabuf global should advertise.
    fn supported_formats(&mut self) -> FormatSet;

    /// REQUIRED import capability: validate a client dmabuf by importing it
    /// on the primary render node. `false` = the protocol rejects the buffer.
    fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool;

    /// Early-import optimization: pre-import a surface's buffer ahead of
    /// render time (multi-GPU correctness + latency).
    fn early_import(&mut self, surface: &WlSurface);

    /// Whether the renderer honors explicit-sync acquire points natively
    /// (vulkan: yes via semaphore import; gles: not until EGL native fences).
    fn sync_capable(&self) -> bool;

    /// Fence capability: render-completion export (sync_file / opaque fd).
    /// `None` = implicit sync.
    fn export_render_fence(&mut self) -> Option<std::os::unix::io::OwnedFd>;
}
