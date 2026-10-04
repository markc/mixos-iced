//! `IcedSurface`: one GL texture the compositor's renderer owns, wrapped as a
//! `wgpu::Texture` iced renders into.
//!
//! With wgpu on the GL backend
//! over the renderer's own context, the slot is a plain GL texture smithay
//! allocates (`Offscreen::create_buffer`) and wgpu wraps
//! (`wgpu_import::wrap_gles_texture`) — no dmabuf at all.
//!
//! This is the unit the engine renders into and the compositor samples from.
//! Each Iced instance owns one.

use crate::bridge::publish::ring::ring::Ring;
use model::environment::interface::base as interface;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::Offscreen;
use smithay::utils::{Buffer as BufferCoord, Physical, Size};

use crate::surface::error::SurfaceError;
use crate::surface::wgpu_context::WgpuGlContext;
use crate::surface::wgpu_import::{wrap_gles_texture, FOURCC};

/// One render target, addressable from both wgpu and GLES.
///
/// ## Drop ordering
/// Within a slot the wgpu wrap drops before the `GlesTexture` that owns the GL
/// name (see [`Backing`]).
pub struct IcedSurface {
    /// GPU backing: a ring of GL textures, each with its wgpu wrap. `None` when the
    /// surface has been **released** to reclaim memory while it isn't visible —
    /// its `IcedRuntime` keeps running; `ensure` re-allocates on demand before
    /// the next render. Each slot keeps its wrap and texture together so they drop
    /// in the required order (wgpu → gles) on release and resize alike.
    ///
    /// Ring depth is the live `Surfaces` setting. One slot is the disabled path
    /// and behaves exactly as the single backing did: iced draws into the very
    /// buffer the compositor samples this frame.
    backing: Option<Ring<Backing>>,
    /// The logical size of the surface, retained across release so a released
    /// surface can be re-allocated at the same size without the caller re-stating it.
    pub size: Size<i32, Physical>,
}

/// One slot: the wgpu wrap of a GL texture and the texture itself. Field order is
/// drop order — the wrap (which never frees the name) goes first, then the
/// `GlesTexture` that does.
pub struct Backing {
    wgpu_texture: wgpu::Texture,
    gles_texture: GlesTexture,
}

impl std::fmt::Debug for IcedSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IcedSurface")
            .field("size", &self.size)
            .finish()
    }
}

impl IcedSurface {
    /// Allocate the ring at the depth the live setting asks for, each slot a
    /// GL texture wrapped for wgpu. Starts resident.
    pub fn allocate(
        render_node: &str,
        wgpu_ctx: &WgpuGlContext,
        gles: &mut GlesRenderer,
        size: Size<i32, Physical>,
    ) -> Result<Self, SurfaceError> {
        Ok(Self {
            backing: Some(Self::ring(render_node, wgpu_ctx, gles, size)?),
            size,
        })
    }

    fn ring(
        render_node: &str,
        wgpu_ctx: &WgpuGlContext,
        gles: &mut GlesRenderer,
        size: Size<i32, Physical>,
    ) -> Result<Ring<Backing>, SurfaceError> {
        let depth = interface::get().depth();
        let mut slots = Vec::with_capacity(depth);
        for _ in 0..depth {
            slots.push(Backing::allocate(render_node, wgpu_ctx, gles, size)?);
        }
        Ok(Ring::new(slots, wgpu_ctx.device.clone(), wgpu_ctx.queue.clone()))
    }

    /// Match the ring to the live setting, allocating or dropping slots. Called
    /// per frame, so the knob applies without a restart; a no-op while released
    /// or once the depth already matches.
    pub fn sync_depth(
        &mut self,
        render_node: &str,
        wgpu_ctx: &WgpuGlContext,
        gles: &mut GlesRenderer,
        want: usize,
    ) -> Result<(), SurfaceError> {
        let size = self.size;
        let Some(ring) = self.backing.as_mut() else { return Ok(()) };
        if want == ring.len() {
            return Ok(());
        }
        trace!("IcedSurface ring {} -> {want} slots", ring.len());
        ring.set_depth(want, || Backing::allocate(render_node, wgpu_ctx, gles, size))
    }

    /// Retire finished frames. Returns whether a new buffer became visible, which
    /// is what the instance's commit counter — and so its damage — follows.
    pub fn poll(&mut self) -> bool {
        self.backing.as_mut().is_some_and(|r| r.poll())
    }

    /// Whether a submitted frame has yet to be published. The host must keep
    /// scheduling while this holds: iced renders only when dirty, so a deferred
    /// publish would otherwise land on a frame that never comes and the surface
    /// would sit on stale pixels.
    pub fn has_pending(&self) -> bool {
        self.backing.as_ref().is_some_and(|r| r.has_pending())
    }

    /// Record that iced submitted its frame for the claimed slot.
    ///
    /// `pipeline` is passed in rather than re-read: the caller already holds the
    /// settings for this frame, and re-reading here made it one `RwLock`
    /// acquisition per surface per frame for a value that cannot have changed.
    pub fn submitted(&mut self, pipeline: bool) {
        if let Some(ring) = self.backing.as_mut() {
            ring.submitted(pipeline);
        }
    }

    /// Bumped on every publish; the instance's damage follows it.
    pub fn generation(&self) -> u64 {
        self.backing.as_ref().map_or(0, |r| r.generation())
    }

    /// Whether the GPU backing is currently allocated. `false` after `release`
    /// and before the next `ensure`.
    pub fn is_resident(&self) -> bool {
        self.backing.is_some()
    }

    /// Free the GPU backing (GL texture + its wrap) while keeping `size`. The
    /// slot drops in the required order (wgpu → gles). No-op if
    /// already released. Re-`ensure` before rendering or sampling again.
    pub fn release(&mut self) {
        if self.backing.is_some() {
            trace!("IcedSurface::release {}x{}", self.size.w, self.size.h);
        }
        self.backing = None;
    }

    /// Re-allocate the backing at the current `size` if it was released. No-op
    /// if already resident.
    pub fn ensure(
        &mut self,
        render_node: &str,
        wgpu_ctx: &WgpuGlContext,
        gles: &mut GlesRenderer,
    ) -> Result<(), SurfaceError> {
        if self.backing.is_some() {
            return Ok(());
        }
        self.backing = Some(Self::ring(render_node, wgpu_ctx, gles, self.size)?);
        Ok(())
    }

    /// Sampleable GLES view of the PUBLISHED slot, or `None` while released —
    /// never the slot iced is drawing into.
    pub fn gles_texture(&self) -> Option<&GlesTexture> {
        self.backing.as_ref().map(|r| &r.published().gles_texture)
    }

    /// Resize. Destroy-and-recreate in drop-safe order when resident; when
    /// released, only the retained `size` changes (the backing is re-allocated
    /// at the new size on the next `ensure`).
    ///
    /// On a resident resize, a replacement is allocated first so a failure
    /// leaves `*self` unchanged and the caller sees a clean error.
    pub fn resize(
        &mut self,
        render_node: &str,
        wgpu_ctx: &WgpuGlContext,
        gles: &mut GlesRenderer,
        new_size: Size<i32, Physical>,
    ) -> Result<(), SurfaceError> {
        if new_size == self.size {
            return Ok(());
        }

        trace!(
            "IcedSurface::resize {}x{} -> {}x{}",
            self.size.w, self.size.h, new_size.w, new_size.h
        );

        if let Some(ring) = self.backing.as_mut() {
            // Allocate every replacement first (clean error on failure), then let
            // the old slots drop (wgpu → gles) as they are replaced.
            let mut slots = Vec::with_capacity(ring.len());
            for _ in 0..ring.len() {
                slots.push(Backing::allocate(render_node, wgpu_ctx, gles, new_size)?);
            }
            ring.replace(slots);
        }
        self.size = new_size;
        Ok(())
    }

    /// Claim the slot to render into and produce a `wgpu::TextureView` for it,
    /// or `None` while released. May block once the GPU is a whole ring behind.
    pub fn begin_render_view(&mut self) -> Option<wgpu::TextureView> {
        self.backing.as_mut().map(|r| {
            r.begin().wgpu_texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("compd_iced_render_view"),
                ..Default::default()
            })
        })
    }
}

impl Backing {
    /// One slot: a GL texture on the renderer's context, wrapped for wgpu.
    /// `_render_node` is unused since the slot is no longer a gbm buffer; it stays
    /// in the signature chain for the ui move (compd: drop it there).
    fn allocate(
        _render_node: &str,
        wgpu_ctx: &WgpuGlContext,
        gles: &mut GlesRenderer,
        size: Size<i32, Physical>,
    ) -> Result<Self, SurfaceError> {
        trace!("IcedSurface backing allocate {}x{}", size.w, size.h);
        let buffer_size: Size<i32, BufferCoord> = Size::from((size.w, size.h));
        let gles_texture = gles
            .create_buffer(FOURCC, buffer_size)
            .map_err(SurfaceError::GlesTexture)?;
        let wgpu_texture = wrap_gles_texture(wgpu_ctx, &gles_texture)?;
        Ok(Self { wgpu_texture, gles_texture })
    }
}
