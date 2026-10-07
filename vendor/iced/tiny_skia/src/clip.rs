// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rectangular clipping reuses masks within a draw instead of clearing the
//! entire window for each text run. The rasterisation remains tiny-skia's.

use crate::core::Rectangle;

pub(crate) struct ClipMask<'a> {
    mask: &'a mut tiny_skia::Mask,
    bounds: Option<Rectangle>,
    #[cfg(test)]
    reference: bool,
}

impl<'a> ClipMask<'a> {
    pub(crate) fn new(mask: &'a mut tiny_skia::Mask) -> Self {
        Self {
            mask,
            bounds: None,
            #[cfg(test)]
            reference: false,
        }
    }

    pub(crate) fn mask(&self) -> &tiny_skia::Mask {
        self.mask
    }

    pub(crate) fn set(&mut self, bounds: Rectangle) {
        #[cfg(test)]
        if self.reference {
            crate::engine::adjust_clip_mask(self.mask, bounds);
            return;
        }
        if self.bounds == Some(bounds) {
            return;
        }
        if let Some(previous) = self.bounds {
            // Include an extra pixel around fractional edges. Clearing more
            // than the previous path touched is safe; leaving a pixel is not.
            let width = self.mask.width() as usize;
            let height = self.mask.height() as usize;
            let left = (previous.x.floor() - 1.0).clamp(0.0, width as f32) as usize;
            let right =
                ((previous.x + previous.width).ceil() + 1.0).clamp(0.0, width as f32) as usize;
            let top = (previous.y.floor() - 1.0).clamp(0.0, height as f32) as usize;
            let bottom =
                ((previous.y + previous.height).ceil() + 1.0).clamp(0.0, height as f32) as usize;
            for row in self
                .mask
                .data_mut()
                .chunks_exact_mut(width)
                .take(bottom)
                .skip(top)
            {
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
    fn mixed_text_and_primitives_preserve_clips_against_full_clear_oracle() {
        use crate::core::{Color, Font, Pixels, Point, Size, Transformation};
        use crate::graphics::Text;

        for scale in [1.0, 1.25, 1.5, 2.5] {
            let mut expected = tiny_skia::Pixmap::new(160, 120).unwrap();
            let mut actual = expected.clone();
            for (target, reference) in [(&mut expected, true), (&mut actual, false)] {
                let mut storage = tiny_skia::Mask::new(160, 120).unwrap();
                let mut mask = ClipMask::new(&mut storage);
                mask.reference = reference;
                let mut engine = crate::engine::Engine::new();
                let transform = Transformation::scale(scale);
                let clip = Rectangle {
                    x: 4.25,
                    y: 3.5,
                    width: 54.0,
                    height: 35.25,
                } * transform;
                for (index, width) in [8.5, 34.25, 8.5, 50.0].into_iter().enumerate() {
                    let text = Text::Cached {
                        content: " x  clipped text ".into(),
                        bounds: Rectangle::new(
                            Point::new(1.5, 4.25 + index as f32 * 3.0),
                            Size::new(70.0, 22.0),
                        ),
                        color: Color::WHITE,
                        size: Pixels(13.0),
                        line_height: Pixels(18.0),
                        font: Font::default(),
                        align_x: Default::default(),
                        align_y: crate::core::alignment::Vertical::Top,
                        shaping: Default::default(),
                        wrapping: Default::default(),
                        ellipsis: Default::default(),
                        clip_bounds: Rectangle {
                            x: 5.75,
                            y: 4.25,
                            width,
                            height: 29.0,
                        },
                    };
                    engine.draw_text(&text, transform, &mut target.as_mut(), &mut mask, clip);
                    #[cfg(feature = "image")]
                    {
                        // Raster clips really narrow the shared mask. The
                        // following primitive must restore its wider clip.
                        let image = crate::graphics::Image::Raster {
                            image: crate::core::Image::new(crate::core::image::Handle::from_rgba(
                                8,
                                8,
                                [200, 40, 90, 128].repeat(64),
                            )),
                            bounds: Rectangle {
                                x: 1.5,
                                y: 2.0,
                                width: 55.0,
                                height: 36.0,
                            },
                            clip_bounds: Rectangle {
                                x: 5.75,
                                y: 4.25,
                                width,
                                height: 15.0,
                            },
                        };
                        engine.draw_image(&image, transform, &mut target.as_mut(), &mut mask, clip);
                    }
                    let primitive = crate::Primitive::Fill {
                        path: tiny_skia::PathBuilder::from_rect(
                            tiny_skia::Rect::from_xywh(0.0, 0.0, 80.0, 60.0).unwrap(),
                        ),
                        paint: tiny_skia::Paint {
                            shader: tiny_skia::Shader::SolidColor(tiny_skia::Color::from_rgba8(
                                20, 80, 150, 70,
                            )),
                            ..Default::default()
                        },
                        rule: tiny_skia::FillRule::EvenOdd,
                    };
                    engine.draw_primitive(
                        &primitive,
                        transform,
                        &mut target.as_mut(),
                        &mut mask,
                        clip,
                    );
                }
            }
            assert_eq!(actual.data(), expected.data(), "scale={scale}");
            assert!(actual.data().iter().any(|&byte| byte != 0));
        }
    }

    #[test]
    fn reused_masks_match_fresh_paths_at_fractional_and_offscreen_edges() {
        let mut storage = tiny_skia::Mask::new(37, 29).unwrap();
        storage.data_mut().fill(71); // The first clip must erase unknown state.
        let mut cached = ClipMask::new(&mut storage);
        let mut fresh = tiny_skia::Mask::new(37, 29).unwrap();
        for x in [-40.0, -2.51, -0.49, 0.0, 0.49, 0.51, 6.25, 36.51, 80.0] {
            for y in [-35.0, -1.25, 0.0, 0.49, 8.75, 28.51, 70.0] {
                for (width, height) in [(1.0, 1.0), (6.25, 9.5), (80.0, 60.0)] {
                    let bounds = Rectangle {
                        x,
                        y,
                        width,
                        height,
                    };
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
        let bounds = Rectangle {
            x: 25.25,
            y: 500.5,
            width: 2150.0,
            height: 50.0,
        };
        let mut mask = tiny_skia::Mask::new(2250, 1250).unwrap();
        let start = Instant::now();
        for _ in 0..1000 {
            crate::engine::adjust_clip_mask(&mut mask, black_box(bounds));
            let _ = black_box(mask.data());
        }
        let fresh = start.elapsed();
        let mut cached = ClipMask::new(&mut mask);
        let start = Instant::now();
        for _ in 0..1000 {
            cached.set(black_box(bounds));
            let _ = black_box(cached.mask().data());
        }
        eprintln!(
            "1000 text clips: full-clear={fresh:?} reused={:?}",
            start.elapsed()
        );
    }
}
