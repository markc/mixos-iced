// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rectangular clipping reuses masks within a draw instead of clearing the
//! entire window for each text run. The rasterisation remains tiny-skia's.

use crate::core::Rectangle;

pub(crate) struct ClipMask<'a> {
    mask: &'a mut tiny_skia::Mask,
    bounds: Option<Rectangle>,
}

impl<'a> ClipMask<'a> {
    pub(crate) fn new(mask: &'a mut tiny_skia::Mask) -> Self {
        Self { mask, bounds: None }
    }

    pub(crate) fn mask(&self) -> &tiny_skia::Mask {
        self.mask
    }

    pub(crate) fn set(&mut self, bounds: Rectangle) {
        if self.bounds == Some(bounds) {
            return;
        }
        if let Some(previous) = self.bounds {
            // Include an extra pixel around fractional edges. Clearing more
            // than the previous path touched is safe; leaving a pixel is not.
            let width = self.mask.width() as usize;
            let height = self.mask.height() as usize;
            let left = (previous.x.floor() - 1.0).clamp(0.0, width as f32) as usize;
            let right = ((previous.x + previous.width).ceil() + 1.0).clamp(0.0, width as f32)
                as usize;
            let top = (previous.y.floor() - 1.0).clamp(0.0, height as f32) as usize;
            let bottom = ((previous.y + previous.height).ceil() + 1.0).clamp(0.0, height as f32)
                as usize;
            for row in self.mask.data_mut().chunks_exact_mut(width).take(bottom).skip(top) {
                row[left..right].fill(0);
            }
        } else {
            // The caller may have used this mask outside the current draw.
            self.mask.clear();
        }
        let path = tiny_skia::PathBuilder::from_rect(
            tiny_skia::Rect::from_xywh(bounds.x, bounds.y, bounds.width, bounds.height)
                .expect("Create clip rectangle"),
        );
        self.mask.fill_path(
            &path,
            tiny_skia::FillRule::EvenOdd,
            false,
            tiny_skia::Transform::identity(),
        );
        self.bounds = Some(bounds);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reused_masks_match_fresh_paths_at_fractional_and_offscreen_edges() {
        let mut storage = tiny_skia::Mask::new(37, 29).unwrap();
        storage.data_mut().fill(71); // The first clip must erase unknown state.
        let mut cached = ClipMask::new(&mut storage);
        let mut fresh = tiny_skia::Mask::new(37, 29).unwrap();
        for x in [-40.0, -2.51, -0.49, 0.0, 0.49, 0.51, 6.25, 36.51, 80.0] {
            for y in [-35.0, -1.25, 0.0, 0.49, 8.75, 28.51, 70.0] {
                for (width, height) in [(1.0, 1.0), (6.25, 9.5), (80.0, 60.0)] {
                    let bounds = Rectangle { x, y, width, height };
                    crate::engine::adjust_clip_mask(&mut fresh, bounds);
                    cached.set(bounds);
                    assert_eq!(cached.mask().data(), fresh.data(), "{bounds:?}");
                    cached.set(bounds);
                    assert_eq!(cached.mask().data(), fresh.data(), "repeat {bounds:?}");
                }
            }
        }
    }

    #[test]
    #[ignore = "release-only clip-mask performance measurement"]
    fn repeated_text_clip_bench() {
        use std::hint::black_box;
        use std::time::Instant;
        let bounds = Rectangle { x: 25.25, y: 500.5, width: 2150.0, height: 50.0 };
        let mut mask = tiny_skia::Mask::new(2250, 1250).unwrap();
        let start = Instant::now();
        for _ in 0..1000 {
            crate::engine::adjust_clip_mask(&mut mask, black_box(bounds));
            black_box(mask.data());
        }
        let fresh = start.elapsed();
        let mut cached = ClipMask::new(&mut mask);
        let start = Instant::now();
        for _ in 0..1000 {
            cached.set(black_box(bounds));
            black_box(cached.mask().data());
        }
        eprintln!("1000 text clips: full-clear={fresh:?} reused={:?}", start.elapsed());
    }
}
