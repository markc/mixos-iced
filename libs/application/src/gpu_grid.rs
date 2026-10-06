// SPDX-License-Identifier: MIT OR Apache-2.0
//! The grid on the GPU: one persistent texture, damage-band uploads.
//!
//! This is the whole reason the frontend is on `iced::widget::shader` rather
//! than `iced::widget::image`. An iced image handle is immutable and cached by
//! id, so putting a terminal grid through one means a new handle — and a new
//! texture — per damaged frame, which is precisely the Bevy terminal's
//! `Image::new`-per-frame cost that the 2026-09-20 memory anatomy found as
//! 320 MB in three GEM objects.
//!
//! Here the texture is created once per grid geometry and lives until the
//! geometry changes. A damaged frame costs `queue.write_texture` over the rows
//! that actually changed; an idle frame costs a three-vertex draw and no
//! transfer at all.

use iced::wgpu;
use iced::widget::shader;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

/// The `Shader` program: it owns nothing but the handle to the shared frame.
#[derive(Clone)]
pub struct GridProgram {
    frame: Arc<Mutex<dyn FrameSource>>,
    id: GridId,
}

/// Lifetime identity, retained by renderer caches. Equality compares allocation
/// identity, and retaining the key prevents a reused address from inheriting a
/// previous frame's texture. A fresh frame must receive a fresh token.
#[derive(Clone, Debug)]
pub struct GridId(Arc<()>);

impl Default for GridId {
    fn default() -> Self {
        Self(Arc::new(()))
    }
}
impl PartialEq for GridId {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for GridId {}
impl Hash for GridId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}

/// A physical pixel rectangle in the source buffer.
#[derive(Debug, Clone, Copy)]
pub struct DamageBand {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Frame access is scoped to one mutex guard during prepare. The source owns
/// pixels and accumulated damage; constructing a view never consumes damage.
pub trait FrameSource: Send + 'static {
    fn identity(&self) -> GridId;
    fn dimensions(&self) -> (u32, u32);
    fn stride(&self) -> usize;
    fn pixels(&self) -> &[u8];
    fn take_damage(&mut self) -> Vec<DamageBand>;
    fn clear_damage(&mut self);
}

/// Layout for a previously validated source rectangle. `stride` must fit u32
/// and contain the complete source row; offsets must fit the source buffer.
pub fn upload_region(
    stride: usize,
    band: DamageBand,
) -> (wgpu::Origin3d, wgpu::TexelCopyBufferLayout, wgpu::Extent3d) {
    (
        wgpu::Origin3d {
            x: band.x,
            y: band.y,
            z: 0,
        },
        wgpu::TexelCopyBufferLayout {
            offset: u64::from(band.y)
                .checked_mul(stride as u64)
                .and_then(|offset| offset.checked_add(u64::from(band.x) * 4))
                .expect("validated source offset"),
            bytes_per_row: Some(u32::try_from(stride).expect("validated source stride")),
            rows_per_image: Some(band.height),
        },
        wgpu::Extent3d {
            width: band.width,
            height: band.height,
            depth_or_array_layers: 1,
        },
    )
}

fn valid_surface(width: u32, height: u32, stride: usize, bytes: usize) -> bool {
    if width == 0 || height == 0 || u32::try_from(stride).is_err() {
        return false;
    }
    let Some(row_bytes) = (width as usize).checked_mul(4) else {
        return false;
    };
    stride >= row_bytes
        && stride
            .checked_mul(height as usize - 1)
            .and_then(|offset| offset.checked_add(row_bytes))
            .is_some_and(|required| required <= bytes)
}

impl GridProgram {
    pub fn new<T: FrameSource>(frame: Arc<Mutex<T>>) -> Self {
        let id = frame.lock().expect("frame lock").identity();
        Self { frame, id }
    }
}

impl<Message> shader::Program<Message> for GridProgram {
    type State = ();
    type Primitive = GridPrimitive;

    fn draw(
        &self,
        _state: &Self::State,
        _cursor: iced::mouse::Cursor,
        _bounds: iced::Rectangle,
    ) -> Self::Primitive {
        GridPrimitive {
            frame: self.frame.clone(),
            id: self.id.clone(),
        }
    }
}

pub struct GridPrimitive {
    frame: Arc<Mutex<dyn FrameSource>>,
    id: GridId,
}

// `Primitive: Debug`, and a Mutex<Frame> has nothing legible to print — the
// derive would only add a `Frame: Debug` bound for a line nobody reads.
impl std::fmt::Debug for GridPrimitive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GridPrimitive")
    }
}

impl shader::Primitive for GridPrimitive {
    type Pipeline = GridPipeline;

    fn prepare(
        &self,
        pipeline: &mut Self::Pipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _bounds: &iced::Rectangle,
        _viewport: &shader::Viewport,
    ) {
        let id = self.id.clone();
        pipeline.live.insert(id.clone());
        let mut frame = self.frame.lock().expect("frame lock");
        let (width, height) = frame.dimensions();
        if width == 0 || height == 0 {
            // The grid has no paintable rows. Drop the texture rather than
            // keep presenting pixels whose source is gone.
            pipeline.textures.remove(&id);
            frame.clear_damage();
            return;
        }
        let stride = frame.stride();
        if width > device.limits().max_texture_dimension_2d
            || height > device.limits().max_texture_dimension_2d
            || !valid_surface(width, height, stride, frame.pixels().len())
        {
            pipeline.textures.remove(&id);
            frame.clear_damage();
            return;
        }
        // A texture that was just created holds nothing, so pending damage is
        // not merely stale, it is wrong: upload the lot and drop it.
        if pipeline.ensure_texture(device, id.clone(), width, height) {
            pipeline.upload(
                queue,
                id,
                frame.pixels(),
                stride,
                DamageBand {
                    x: 0,
                    y: 0,
                    width,
                    height,
                },
            );
            frame.clear_damage();
            return;
        }
        for band in frame.take_damage() {
            // Clamp rather than trust: the surface is re-measured above, and a
            // band recorded against a larger one would be a GPU-side panic.
            let y = band.y.min(height);
            let rows = band.height.min(height - y);
            let x = band.x.min(width);
            let columns = band.width.min(width - x);
            if rows > 0 && columns > 0 {
                pipeline.upload(
                    queue,
                    id.clone(),
                    frame.pixels(),
                    stride,
                    DamageBand {
                        x,
                        y,
                        width: columns,
                        height: rows,
                    },
                );
            }
        }
    }

    fn draw(&self, pipeline: &Self::Pipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        let Some(texture) = pipeline.textures.get(&self.id) else {
            // True regardless: the fallback `render` path would begin a whole
            // extra render pass to draw the nothing we have.
            return true;
        };
        render_pass.set_pipeline(&pipeline.pipeline);
        render_pass.set_bind_group(0, &texture.bind_group, &[]);
        render_pass.draw(0..3, 0..1);
        true
    }
}


struct GridTexture {
    bind_group: wgpu::BindGroup,
    texture: wgpu::Texture,
    width: u32,
    height: u32,
}

pub struct GridPipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// The texture's own format, chosen to make the sample -> write path an
    /// identity transform against whatever surface format iced gave us.
    format: wgpu::TextureFormat,
    /// One entry per live grid; see [`GridId`].
    textures: HashMap<GridId, GridTexture>,
    /// Grids that prepared this frame. `trim` drops everything else, so a
    /// closed pane's texture is freed on the next frame rather than living
    /// until the process exits.
    ///
    /// Every key retains its lifetime token; retired allocation addresses
    /// cannot alias a still-live texture.
    live: HashSet<GridId>,
}

impl GridPipeline {
    /// Creates or resizes a grid's texture. Returns true when the caller now
    /// owes a full upload.
    fn ensure_texture(
        &mut self,
        device: &wgpu::Device,
        id: GridId,
        width: u32,
        height: u32,
    ) -> bool {
        if self
            .textures
            .get(&id)
            .is_some_and(|texture| texture.width == width && texture.height == height)
        {
            return false;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("native grid"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("native grid bind group"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.textures.insert(
            id,
            GridTexture {
                bind_group,
                texture,
                width,
                height,
            },
        );
        true
    }

    /// Write a cell range directly from the full-stride RGBA surface, without
    /// repacking. Queue::write_texture does not require 256-byte row alignment.
    fn upload(
        &self,
        queue: &wgpu::Queue,
        id: GridId,
        rgba: &[u8],
        stride: usize,
        band: DamageBand,
    ) {
        let Some(target) = self.textures.get(&id) else {
            return;
        };
        let (origin, layout, extent) = upload_region(stride, band);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &target.texture,
                mip_level: 0,
                origin,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            layout,
            extent,
        );
    }
}

impl shader::Pipeline for GridPipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        // The VT hands us sRGB-encoded bytes. Matching the target's encoding
        // makes sample-then-write an identity: against an sRGB target the
        // sampler linearises and the blend-free write re-encodes; against a
        // plain Unorm target neither happens. Getting this backwards is the
        // classic washed-out-terminal bug, and it is invisible in a unit test.
        let texture_format = if format.is_srgb() {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("native grid shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("grid.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("native grid bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("native grid pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("native grid pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // The grid is opaque by construction (the raster writes
                    // alpha 255 into every cell), so there is nothing to blend
                    // and REPLACE saves the read.
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("native grid sampler"),
            // Nearest, and the quad is laid out at exactly the texture's
            // physical size: a terminal that resamples its own glyphs is a
            // blurry terminal, which is the HiDPI bug the raster's physical
            // -pixel cells exist to avoid.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        Self {
            pipeline,
            layout,
            sampler,
            format: texture_format,
            textures: HashMap::new(),
            live: HashSet::new(),
        }
    }

    /// Called by iced at the end of each frame.
    fn trim(&mut self) {
        self.textures.retain(|id, _| self.live.contains(id));
        self.live.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shader::{Pipeline, Primitive};

    struct TestFrame {
        id: GridId,
        pixels: Vec<u8>,
        damage: Vec<DamageBand>,
    }
    impl TestFrame {
        fn new(colour: [u8; 4]) -> Self {
            let mut pixels = vec![0; 272 * 3];
            for row in 0..3 {
                for x in 0..64 {
                    pixels[row * 272 + x * 4..row * 272 + x * 4 + 4].copy_from_slice(&colour);
                }
            }
            Self {
                id: GridId::default(),
                pixels,
                damage: Vec::new(),
            }
        }
    }
    impl FrameSource for TestFrame {
        fn identity(&self) -> GridId {
            self.id.clone()
        }
        fn dimensions(&self) -> (u32, u32) {
            (64, 3)
        }
        fn stride(&self) -> usize {
            272
        }
        fn pixels(&self) -> &[u8] {
            &self.pixels
        }
        fn take_damage(&mut self) -> Vec<DamageBand> {
            std::mem::take(&mut self.damage)
        }
        fn clear_damage(&mut self) {
            self.damage.clear();
        }
    }

    fn render(
        primitive: &GridPrimitive,
        pipeline: &GridPipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Vec<u8> {
        let size = wgpu::Extent3d {
            width: 64,
            height: 3,
            depth_or_array_layers: 1,
        };
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("grid acceptance target"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("grid acceptance readback"),
            size: 256 * 3,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let view = target.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("grid acceptance draw"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            assert!(primitive.draw(pipeline, &mut pass));
        }
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(3),
                },
            },
            size,
        );
        queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        let (send, receive) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            send.send(result).unwrap()
        });
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        receive.recv().unwrap().unwrap();
        let pixels = slice.get_mapped_range().unwrap().to_vec();
        buffer.unmap();
        pixels
    }

    #[test]
    fn gpu_renders_sparse_damage_without_replacing_other_panes_and_retires_closed_panes() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = iced::futures::executor::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
        )
        .expect("GPU acceptance requires a real adapter, including Mesa software adapters");
        let (device, queue) = iced::futures::executor::block_on(
            adapter.request_device(&wgpu::DeviceDescriptor::default()),
        )
        .unwrap();
        let mut pipeline = GridPipeline::new(&device, &queue, wgpu::TextureFormat::Rgba8Unorm);
        let first = Arc::new(Mutex::new(TestFrame::new([20, 40, 60, 255])));
        let second = Arc::new(Mutex::new(TestFrame::new([80, 100, 120, 255])));
        let primitives = [first.clone(), second.clone()].map(|frame| GridPrimitive {
            id: frame.lock().unwrap().identity(),
            frame: frame.clone(),
        });
        let bounds = iced::Rectangle::with_size(iced::Size::new(64.0, 3.0));
        let viewport = shader::Viewport::with_physical_size(
            iced::Size::new(64, 3), iced::advanced::renderer::Scale::default(),
        );
        for primitive in &primitives {
            primitive.prepare(&mut pipeline, &device, &queue, &bounds, &viewport);
        }
        assert_eq!(pipeline.textures.len(), 2);
        let original = render(&primitives[0], &pipeline, &device, &queue);
        let other = render(&primitives[1], &pipeline, &device, &queue);
        assert_eq!(original, [20, 40, 60, 255].repeat(64 * 3));
        assert_eq!(other, [80, 100, 120, 255].repeat(64 * 3));
        let texture = pipeline.textures[&primitives[0].id].texture.clone();
        pipeline.trim();
        {
            let mut frame = first.lock().unwrap();
            frame.pixels[272 + 12 * 4..272 + 12 * 4 + 4].copy_from_slice(&[180, 160, 140, 255]);
            // Changing an undamaged pixel must not leak into this upload.
            frame.pixels[..4].copy_from_slice(&[255, 0, 0, 255]);
            frame.damage.push(DamageBand {
                x: 12,
                y: 1,
                width: 1,
                height: 1,
            });
        }
        primitives[0].prepare(&mut pipeline, &device, &queue, &bounds, &viewport);
        assert_eq!(texture, pipeline.textures[&primitives[0].id].texture);
        let changed = render(&primitives[0], &pipeline, &device, &queue);
        let mut expected = original;
        expected[256 + 12 * 4..256 + 12 * 4 + 4].copy_from_slice(&[180, 160, 140, 255]);
        assert_eq!(changed, expected);
        assert_eq!(render(&primitives[1], &pipeline, &device, &queue), other);
        assert!(first.lock().unwrap().damage.is_empty());
        pipeline.trim();
        assert_eq!(pipeline.textures.len(), 1);
        assert!(!pipeline.textures.contains_key(&primitives[1].id));
        pipeline.trim();
        assert!(pipeline.textures.is_empty());
    }

    #[test]
    fn retained_identities_never_alias_new_frames() {
        let first = GridId::default();
        let retained = first.clone();
        assert_eq!(first, retained);
        let mut cache = HashSet::new();
        cache.insert(first);
        for _ in 0..10_000 {
            let fresh = GridId::default();
            assert!(!cache.contains(&fresh));
        }
        assert!(cache.contains(&retained));
    }

    #[test]
    fn source_validation_rejects_short_rows_buffers_and_overflow() {
        assert!(valid_surface(8, 3, 36, 104));
        assert!(!valid_surface(8, 3, 31, 104));
        assert!(!valid_surface(8, 3, 36, 103));
        assert!(!valid_surface(8, 3, usize::MAX, usize::MAX));
        assert!(!valid_surface(0, 3, 0, 0));
        assert!(!valid_surface(8, 0, 32, 0));
    }
}
