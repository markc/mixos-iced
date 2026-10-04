//! Wrap a GL texture the compositor's renderer owns as a `wgpu::Texture`.
//!
//! With wgpu on the GL backend over the
//! SAME context as smithay's `GlesRenderer`, a texture smithay allocated (or
//! imported) is already a name wgpu can render into or copy from:
//! `hal::gles::Device::texture_from_raw` wraps it and `create_texture_from_hal`
//! lifts it into wgpu. smithay keeps ownership (the drop callback is a no-op), so
//! the texture is freed exactly once, by the `GlesTexture`.

use std::num::NonZeroU32;

use smithay::backend::renderer::gles::GlesTexture;
use smithay::backend::renderer::Texture;
use wgpu::hal::{MemoryFlags, TextureDescriptor as HalTextureDescriptor};
use wgpu::wgt::TextureUses;

use crate::surface::error::WgpuImportError;
use crate::surface::wgpu_context::WgpuGlContext;

/// The wgpu format every compositor-owned texture is seen through.
///
/// `Bgra8Unorm`, NOT `Bgra8UnormSrgb`. wgpu's GL backend stores
/// `Bgra8Unorm` as `GL_RGBA8` — exactly what smithay allocates for
/// [`FOURCC`] — and reads it back in BGRA byte order, which is what the capture
/// encoder consumes. Non-sRGB because smithay samples these textures raw: iced is
/// built with `web-colors` (no gamma linearization), so the bytes it writes are
/// already the sRGB values smithay should show.
pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

/// The DRM fourcc a compositor-owned GL texture is allocated with: `Abgr8888`
/// is smithay's `GL_RGBA8`, the storage wgpu expects for [`FORMAT`].
pub const FOURCC: smithay::backend::allocator::Fourcc = smithay::backend::allocator::Fourcc::Abgr8888;

/// The format iced renders through. Kept as a function for the callers that
/// took the registrar; the answer no longer depends on it.
pub fn texture_format(
    _formats: &render_gles::format::registrar::registrar::Registrar,
) -> wgpu::TextureFormat {
    FORMAT
}

/// Usage flags applied to wrapped textures. Render attachment for iced
/// drawing into, texture binding for sampling, copy source for capture readback,
/// copy destination for capture snapshots.
pub const TEXTURE_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::RENDER_ATTACHMENT
    .union(wgpu::TextureUsages::TEXTURE_BINDING)
    .union(wgpu::TextureUsages::COPY_SRC)
    .union(wgpu::TextureUsages::COPY_DST);

/// Wrap `texture` (a 2D GL texture on the renderer's context) as a
/// `wgpu::Texture` iced can render into and capture can copy from. The GL
/// context must be current (see `wgpu_context`).
pub fn wrap_gles_texture(
    ctx: &WgpuGlContext,
    texture: &GlesTexture,
) -> Result<wgpu::Texture, WgpuImportError> {
    let name = NonZeroU32::new(texture.tex_id()).ok_or(WgpuImportError::NullTexture)?;
    let size = wgpu::Extent3d {
        width: texture.width(),
        height: texture.height(),
        depth_or_array_layers: 1,
    };

    let hal_desc = HalTextureDescriptor {
        label: Some("compd_gl_wrapped"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: TextureUses::COLOR_TARGET
            | TextureUses::RESOURCE
            | TextureUses::COPY_SRC
            | TextureUses::COPY_DST,
        memory_flags: MemoryFlags::empty(),
        view_formats: vec![],
    };

    // SAFETY: `name` is a live TEXTURE_2D on the context wgpu wraps, created with
    // the storage `FORMAT` implies (`FOURCC` -> GL_RGBA8). The drop callback is a
    // no-op: the `GlesTexture` owns the name and outlives this wrap (the owners
    // keep both together and drop the wgpu side first).
    let hal_texture = unsafe {
        let hal_device = ctx
            .device
            .as_hal::<wgpu::hal::api::Gles>()
            .ok_or(WgpuImportError::NotGlBackend)?;
        hal_device.texture_from_raw(name, &hal_desc, Some(Box::new(|| {})))
    };

    let wgpu_desc = wgpu::TextureDescriptor {
        label: Some("compd_gl_wrapped"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: TEXTURE_USAGE,
        view_formats: &[],
    };

    // SAFETY: `hal_texture` was created on this device's backend just above.
    // Its contents are whatever the GL texture holds, so `UNINITIALIZED` lets
    // wgpu treat the first use as a fresh write.
    let wgpu_texture = unsafe {
        ctx.device.create_texture_from_hal::<wgpu::hal::api::Gles>(
            hal_texture,
            &wgpu_desc,
            TextureUses::UNINITIALIZED,
        )
    };
    trace!("wgpu-gl: wrapped GL texture {} ({}x{})", name, size.width, size.height);
    Ok(wgpu_texture)
}
