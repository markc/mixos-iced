//! A CPU-side captured frame.

/// RGBA-sized pixel buffer in **BGRA8** order (the imported dmabuf format is
/// `Bgra8UnormSrgb`). `bgra.len() == width * height * 4`.
#[derive(Clone)]
pub struct Frame {
    pub bgra: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl Frame {
    /// Flip the image vertically in place (row reversal). Used for backends
    /// whose framebuffer rows are bottom-up (the winit/nested GLES path), so the
    /// saved PNG / encoded video is upright.
    pub fn flip_vertical(&mut self) {
        let stride = (self.width * 4) as usize;
        if stride == 0 {
            return;
        }
        let h = self.height as usize;
        for row in 0..h / 2 {
            let top = row * stride;
            let bot = (h - 1 - row) * stride;
            // Split so we can swap two disjoint row slices.
            let (a, b) = self.bgra.split_at_mut(bot);
            a[top..top + stride].swap_with_slice(&mut b[..stride]);
        }
    }

    /// Resize to exactly `w` × `h` (BGRA preserved — resize is per-channel, so
    /// treating the buffer as RGBA for the resize math keeps byte order). Used
    /// to fix video frames to the encoder's constant dimensions when the
    /// captured region resizes mid-stream. Returns the raw BGRA bytes.
    pub fn fit(&self, w: u32, h: u32) -> Option<Vec<u8>> {
        if w == 0 || h == 0 {
            return None;
        }
        if self.width == w && self.height == h {
            return Some(self.bgra.clone());
        }
        let src = image::RgbaImage::from_raw(self.width, self.height, self.bgra.clone())?;
        let dst = image::imageops::resize(&src, w, h, image::imageops::FilterType::Triangle);
        Some(dst.into_raw())
    }
}
