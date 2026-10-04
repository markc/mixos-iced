//! GPU→CPU readback of a captured wgpu texture (Bgra8Unorm) into a
//! [`Frame`].
//!
//! Two flavors:
//! - [`readback`] — synchronous (`copy_texture_to_buffer` + blocking poll +
//!   map). Used for the one-shot screenshot at Stop.
//! - [`AsyncReadback`] — pipelined: submit a copy + `map_async`, then consume
//!   the result a frame or two later via a non-blocking poll. Used for video
//!   so the render thread never blocks on the GPU each frame.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::surface::WgpuGlContext;

use crate::capture::encode::frame::Frame;

/// Read `tex` back to CPU as BGRA8. `tex` must have been imported with
/// `COPY_SRC` usage (the capture registry's textures are). Returns `None` on
/// any failure.
pub fn readback(ctx: &WgpuGlContext, tex: &wgpu::Texture) -> Option<Frame> {
    let width = tex.width();
    let height = tex.height();
    if width == 0 || height == 0 {
        return None;
    }
    let bpp = 4u32;
    let unpadded = width * bpp;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;

    let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("compd-capture-readback"),
        size: (padded as u64) * (height as u64),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("compd-capture-readback"),
        });
    encoder.copy_texture_to_buffer(
        tex.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    ctx.queue.submit(Some(encoder.finish()));

    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res.is_ok());
    });
    if ctx.device.poll(wgpu::PollType::wait_indefinitely()).is_err() {
        return None;
    }
    match rx.recv() {
        Ok(true) => {}
        _ => {
            warn!("capture readback: buffer map failed");
            return None;
        }
    }

    let mapped = slice.get_mapped_range().ok()?;
    let mut bgra = Vec::with_capacity((unpadded * height) as usize);
    for row in 0..height {
        let start = (row * padded) as usize;
        bgra.extend_from_slice(&mapped[start..start + unpadded as usize]);
    }
    drop(mapped);
    buffer.unmap();

    Some(Frame {
        bgra,
        width,
        height,
    })
}

const ST_IDLE: u8 = 0;
const ST_PENDING: u8 = 1;
const ST_READY: u8 = 2;
const ST_FAILED: u8 = 3;

/// A single-slot pipelined readback. `submit` starts a copy+map; `poll`
/// (non-blocking) returns the [`Frame`] once the map callback has fired. While
/// a readback is in flight, further `submit`s are ignored.
pub struct AsyncReadback {
    buffer: Option<wgpu::Buffer>,
    width: u32,
    height: u32,
    padded: u32,
    state: Arc<AtomicU8>,
    inflight: bool,
}

impl Default for AsyncReadback {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncReadback {
    pub fn new() -> Self {
        Self {
            buffer: None,
            width: 0,
            height: 0,
            padded: 0,
            state: Arc::new(AtomicU8::new(ST_IDLE)),
            inflight: false,
        }
    }

    pub fn inflight(&self) -> bool {
        self.inflight
    }

    fn ensure_buffer(&mut self, ctx: &WgpuGlContext, w: u32, h: u32) {
        let padded = (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        if self.buffer.is_none() || self.width != w || self.height != h {
            self.buffer = Some(ctx.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("compd-capture-async-readback"),
                size: (padded as u64) * (h as u64),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }));
            self.width = w;
            self.height = h;
            self.padded = padded;
        }
    }

    /// Issue a copy of `tex` into the staging buffer and begin mapping. No-op
    /// if a readback is already in flight.
    pub fn submit(&mut self, ctx: &WgpuGlContext, tex: &wgpu::Texture) {
        if self.inflight {
            return;
        }
        let w = tex.width();
        let h = tex.height();
        if w == 0 || h == 0 {
            return;
        }
        self.ensure_buffer(ctx, w, h);
        let buffer = self.buffer.as_ref().unwrap();

        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("compd-capture-async-readback"),
            });
        encoder.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        ctx.queue.submit(Some(encoder.finish()));

        self.state.store(ST_PENDING, Ordering::Release);
        let st = self.state.clone();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |res| {
            st.store(
                if res.is_ok() { ST_READY } else { ST_FAILED },
                Ordering::Release,
            );
        });
        self.inflight = true;
    }

    /// Non-blocking: advance map callbacks and, if the in-flight readback is
    /// ready, return the frame (BGRA). Otherwise `None`.
    pub fn poll(&mut self, ctx: &WgpuGlContext) -> Option<Frame> {
        if !self.inflight {
            return None;
        }
        let _ = ctx.device.poll(wgpu::PollType::Poll);
        match self.state.load(Ordering::Acquire) {
            ST_READY => {
                let buffer = self.buffer.as_ref()?;
                let unpadded = (self.width * 4) as usize;
                let frame = {
                    let mapped = buffer.slice(..).get_mapped_range().ok()?;
                    let mut bgra = Vec::with_capacity(unpadded * self.height as usize);
                    for row in 0..self.height {
                        let start = (row * self.padded) as usize;
                        bgra.extend_from_slice(&mapped[start..start + unpadded]);
                    }
                    Frame {
                        bgra,
                        width: self.width,
                        height: self.height,
                    }
                };
                buffer.unmap();
                self.state.store(ST_IDLE, Ordering::Release);
                self.inflight = false;
                Some(frame)
            }
            ST_FAILED => {
                if let Some(b) = &self.buffer {
                    b.unmap();
                }
                self.state.store(ST_IDLE, Ordering::Release);
                self.inflight = false;
                None
            }
            _ => None,
        }
    }
}
