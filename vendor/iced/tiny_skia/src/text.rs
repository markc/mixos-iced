use crate::core::alignment;
use crate::core::text::{Alignment, Ellipsis, Shaping, Wrapping};
use crate::core::{Color, Font, Pixels, Point, Rectangle, Transformation};
use crate::graphics::text::cache::{self, Cache};
use crate::graphics::text::editor;
use crate::graphics::text::font_system;
use crate::graphics::text::paragraph;

use rustc_hash::{FxHashMap, FxHashSet};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::hash_map;

#[derive(Debug)]
pub struct Pipeline {
    glyph_cache: GlyphCache,
    cache: RefCell<Cache>,
}

impl Pipeline {
    pub fn new() -> Self {
        Pipeline {
            glyph_cache: GlyphCache::new(),
            cache: RefCell::new(Cache::new()),
        }
    }

    // TODO: Shared engine
    #[allow(dead_code)]
    pub fn load_font(&mut self, bytes: Cow<'static, [u8]>) {
        font_system()
            .write()
            .expect("Write font system")
            .load_font(bytes);

        self.cache = RefCell::new(Cache::new());
    }

    pub fn draw_paragraph(
        &mut self,
        paragraph: &paragraph::Weak,
        position: Point,
        color: Color,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        clip_mask: Option<&tiny_skia::Mask>,
        clip_bounds: Rectangle,
        transformation: Transformation,
    ) {
        let Some(paragraph) = paragraph.upgrade() else {
            return;
        };

        let mut font_system = font_system().write().expect("Write font system");

        draw(
            font_system.raw(),
            &mut self.glyph_cache,
            paragraph.buffer(),
            position,
            color,
            pixels,
            clip_mask,
            clip_bounds,
            transformation,
        );
    }

    pub fn draw_editor(
        &mut self,
        editor: &editor::Weak,
        position: Point,
        color: Color,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        clip_mask: Option<&tiny_skia::Mask>,
        clip_bounds: Rectangle,
        transformation: Transformation,
    ) {
        let Some(editor) = editor.upgrade() else {
            return;
        };

        let mut font_system = font_system().write().expect("Write font system");

        draw(
            font_system.raw(),
            &mut self.glyph_cache,
            editor.buffer(),
            position,
            color,
            pixels,
            clip_mask,
            clip_bounds,
            transformation,
        );
    }

    pub fn draw_cached(
        &mut self,
        content: &str,
        bounds: Rectangle,
        color: Color,
        size: Pixels,
        line_height: Pixels,
        font: Font,
        align_x: Alignment,
        align_y: alignment::Vertical,
        shaping: Shaping,
        wrapping: Wrapping,
        ellipsis: Ellipsis,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        clip_mask: Option<&tiny_skia::Mask>,
        clip_bounds: Rectangle,
        transformation: Transformation,
    ) {
        let line_height = f32::from(line_height);

        let mut font_system = font_system().write().expect("Write font system");
        let font_system = font_system.raw();

        let key = cache::Key {
            bounds: bounds.size(),
            content,
            font,
            size: size.into(),
            line_height,
            shaping,
            wrapping,
            ellipsis,
            align_x,
        };

        let (_, entry) = self.cache.get_mut().allocate(font_system, key);

        let width = entry.min_bounds.width;
        let height = entry.min_bounds.height;

        let x = match align_x {
            Alignment::Default | Alignment::Left | Alignment::Justified => bounds.x,
            Alignment::Center => bounds.x - width / 2.0,
            Alignment::Right => bounds.x - width,
        };

        let y = match align_y {
            alignment::Vertical::Top => bounds.y,
            alignment::Vertical::Center => bounds.y - height / 2.0,
            alignment::Vertical::Bottom => bounds.y - height,
        };

        draw(
            font_system,
            &mut self.glyph_cache,
            &entry.buffer,
            Point::new(x, y),
            color,
            pixels,
            clip_mask,
            clip_bounds,
            transformation,
        );
    }

    pub fn draw_raw(
        &mut self,
        buffer: &cosmic_text::Buffer,
        position: Point,
        color: Color,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        clip_mask: Option<&tiny_skia::Mask>,
        clip_bounds: Rectangle,
        transformation: Transformation,
    ) {
        let mut font_system = font_system().write().expect("Write font system");

        draw(
            font_system.raw(),
            &mut self.glyph_cache,
            buffer,
            position,
            color,
            pixels,
            clip_mask,
            clip_bounds,
            transformation,
        );
    }

    pub fn trim_cache(&mut self) {
        self.cache.get_mut().trim();
        self.glyph_cache.trim();
    }
}

fn draw(
    font_system: &mut cosmic_text::FontSystem,
    glyph_cache: &mut GlyphCache,
    buffer: &cosmic_text::Buffer,
    position: Point,
    color: Color,
    pixels: &mut tiny_skia::PixmapMut<'_>,
    clip_mask: Option<&tiny_skia::Mask>,
    clip_bounds: Rectangle,
    transformation: Transformation,
) {
    let position = position * transformation;
    #[cfg(test)]
    let cull = clip_mask.is_some() && !glyph_cache.disable_culling;
    #[cfg(not(test))]
    let cull = clip_mask.is_some();
    #[cfg(test)]
    let mut drawn_glyphs = 0;

    let mut swash = cosmic_text::SwashCache::new();
    let scroll = buffer.scroll();

    for run in buffer.layout_runs() {
        for glyph in run.glyphs {
            let physical_glyph = glyph.physical(
                (position.x - scroll.horizontal, position.y),
                transformation.scale_factor(),
            );

            if let Some((buffer, placement)) = glyph_cache.allocate(
                physical_glyph.cache_key,
                glyph.color_opt.map(from_color).unwrap_or(color),
                font_system,
                &mut swash,
            ) {
                // Test actual rasterised ink, including bearings and baseline
                // rounding. A layout/run box can omit glyph overhang. tiny-skia
                // clips to the target but still builds a pipeline and scans
                // pixels for glyphs wholly outside this rectangular mask.
                let x = physical_glyph.x + placement.left;
                let y = physical_glyph.y - placement.top
                    + (run.line_y * transformation.scale_factor()).round() as i32;
                let ink = Rectangle {
                    x: x as f32,
                    y: y as f32,
                    width: placement.width as f32,
                    height: placement.height as f32,
                };
                if cull && !ink.intersects(&clip_bounds) {
                    continue;
                }
                #[cfg(test)]
                {
                    drawn_glyphs += 1;
                }
                let pixmap =
                    tiny_skia::PixmapRef::from_bytes(buffer, placement.width, placement.height)
                        .expect("Create glyph pixel map");

                let opacity =
                    color.a * glyph.color_opt.map(|c| c.a() as f32 / 255.0).unwrap_or(1.0);

                pixels.draw_pixmap(
                    x,
                    y,
                    pixmap,
                    &tiny_skia::PixmapPaint {
                        opacity,
                        ..tiny_skia::PixmapPaint::default()
                    },
                    tiny_skia::Transform::identity(),
                    clip_mask,
                );
            }
        }
    }
    #[cfg(test)]
    {
        glyph_cache.drawn_glyphs += drawn_glyphs;
    }
}

fn from_color(color: cosmic_text::Color) -> Color {
    let [r, g, b, a] = color.as_rgba();

    Color::from_rgba8(r, g, b, a as f32 / 255.0)
}

#[derive(Debug, Clone, Default)]
struct GlyphCache {
    entries:
        FxHashMap<(cosmic_text::CacheKey, [u8; 3]), Option<(Vec<u32>, cosmic_text::Placement)>>,
    recently_used: FxHashSet<(cosmic_text::CacheKey, [u8; 3])>,
    trim_count: usize,
    #[cfg(test)]
    disable_culling: bool,
    #[cfg(test)]
    drawn_glyphs: usize,
}

impl GlyphCache {
    const TRIM_INTERVAL: usize = 300;
    const CAPACITY_LIMIT: usize = 16 * 1024;

    fn new() -> Self {
        GlyphCache::default()
    }

    fn allocate(
        &mut self,
        cache_key: cosmic_text::CacheKey,
        color: Color,
        font_system: &mut cosmic_text::FontSystem,
        swash: &mut cosmic_text::SwashCache,
    ) -> Option<(&[u8], cosmic_text::Placement)> {
        let [r, g, b, _a] = color.into_rgba8();
        let key = (cache_key, [r, g, b]);

        if let hash_map::Entry::Vacant(entry) = self.entries.entry(key) {
            // TODO: Outline support
            let Some(image) = swash.get_image_uncached(font_system, cache_key) else {
                let _ = entry.insert(None);
                let _ = self.recently_used.insert(key);
                return None;
            };

            let glyph_size = image.placement.width as usize * image.placement.height as usize;

            if glyph_size == 0 {
                // Spaces and missing glyphs are cache hits too. Otherwise a
                // warm line still invokes the rasteriser for every space.
                let _ = entry.insert(None);
                let _ = self.recently_used.insert(key);
                return None;
            }

            let mut buffer = vec![0u32; glyph_size];

            match image.content {
                cosmic_text::SwashContent::Mask => {
                    let mut i = 0;

                    // TODO: Blend alpha

                    for _y in 0..image.placement.height {
                        for _x in 0..image.placement.width {
                            buffer[i] = bytemuck::cast(
                                tiny_skia::ColorU8::from_rgba(b, g, r, image.data[i]).premultiply(),
                            );

                            i += 1;
                        }
                    }
                }
                cosmic_text::SwashContent::Color => {
                    let mut i = 0;

                    for _y in 0..image.placement.height {
                        for _x in 0..image.placement.width {
                            // TODO: Blend alpha
                            buffer[i >> 2] = bytemuck::cast(
                                tiny_skia::ColorU8::from_rgba(
                                    image.data[i + 2],
                                    image.data[i + 1],
                                    image.data[i],
                                    image.data[i + 3],
                                )
                                .premultiply(),
                            );

                            i += 4;
                        }
                    }
                }
                cosmic_text::SwashContent::SubpixelMask => {
                    // TODO
                }
            }

            let _ = entry.insert(Some((buffer, image.placement)));
        }

        let _ = self.recently_used.insert(key);

        self.entries
            .get(&key)
            .and_then(Option::as_ref)
            .map(|(buffer, placement)| (bytemuck::cast_slice(buffer.as_slice()), *placement))
    }

    pub fn trim(&mut self) {
        if self.trim_count > Self::TRIM_INTERVAL || self.recently_used.len() >= Self::CAPACITY_LIMIT
        {
            self.entries
                .retain(|key, _| self.recently_used.contains(key));

            self.recently_used.clear();

            self.entries.shrink_to(Self::CAPACITY_LIMIT);
            self.recently_used.shrink_to(Self::CAPACITY_LIMIT);

            self.trim_count = 0;
        } else {
            self.trim_count += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shaped_buffer(content: &str, width: f32, height: f32) -> cosmic_text::Buffer {
        let mut fonts = font_system().write().unwrap();
        let fonts = fonts.raw();
        let mut buffer = cosmic_text::Buffer::new(fonts, cosmic_text::Metrics::new(13.0, 20.0));
        buffer.set_size(Some(width), Some(height));
        buffer.set_text(
            content,
            &cosmic_text::Attrs::new(),
            cosmic_text::Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(fonts, false);
        buffer
    }

    #[test]
    fn glyph_ink_culling_matches_partial_masks_and_full_text_pixels() {
        let buffer = shaped_buffer(&"Áj fi e\u{301} 中 clipped text\n".repeat(8), 200.0, 180.0);
        let mut skipped = 0;
        for scale in [1.0, 1.25, 1.5, 2.5] {
            for x in [-18.25, 0.0, 19.5] {
                let position = Point::new(x, -5.25);
                let transform = Transformation::scale(scale);
                let mut full = tiny_skia::Pixmap::new(240, 120).unwrap();
                full.fill(tiny_skia::Color::from_rgba8(13, 21, 37, 255));
                let mut full_pipeline = Pipeline::new();
                full_pipeline.draw_raw(
                    &buffer,
                    position,
                    Color::WHITE,
                    &mut full.as_mut(),
                    None,
                    Rectangle {
                        x: 0.0,
                        y: 0.0,
                        width: 240.0,
                        height: 120.0,
                    },
                    transform,
                );
                assert!(full_pipeline.glyph_cache.drawn_glyphs > 0);
                for clip in [
                    Rectangle {
                        x: 0.25,
                        y: 6.25,
                        width: 9.5,
                        height: 65.25,
                    },
                    Rectangle {
                        x: 10.25,
                        y: 32.5,
                        width: 180.0,
                        height: 2.5,
                    },
                    Rectangle {
                        x: -7.5,
                        y: -3.25,
                        width: 66.75,
                        height: 47.5,
                    },
                ] {
                    let mut mask = tiny_skia::Mask::new(240, 120).unwrap();
                    crate::engine::adjust_clip_mask(&mut mask, clip);
                    let mut reference = tiny_skia::Pixmap::new(240, 120).unwrap();
                    reference.fill(tiny_skia::Color::from_rgba8(13, 21, 37, 255));
                    let mut actual = reference.clone();
                    let mut baseline = Pipeline::new();
                    baseline.glyph_cache.disable_culling = true;
                    let mut optimised = Pipeline::new();
                    for (pipeline, target) in [
                        (&mut baseline, &mut reference),
                        (&mut optimised, &mut actual),
                    ] {
                        pipeline.draw_raw(
                            &buffer,
                            position,
                            Color::WHITE,
                            &mut target.as_mut(),
                            Some(&mask),
                            clip,
                            transform,
                        );
                    }
                    assert_eq!(
                        actual.data(),
                        reference.data(),
                        "scale={scale} x={x} clip={clip:?}"
                    );
                    for (index, &coverage) in mask.data().iter().enumerate() {
                        if coverage == 255 {
                            assert_eq!(
                                &actual.data()[index * 4..index * 4 + 4],
                                &full.data()[index * 4..index * 4 + 4],
                                "partial text differs from full redraw at pixel {index}",
                            );
                        }
                    }
                    assert!(
                        optimised.glyph_cache.drawn_glyphs <= baseline.glyph_cache.drawn_glyphs
                    );
                    skipped +=
                        baseline.glyph_cache.drawn_glyphs - optimised.glyph_cache.drawn_glyphs;
                }
            }
        }
        assert!(
            skipped > 100,
            "the oracle must exercise skipped real glyphs"
        );
    }

    #[test]
    #[ignore = "release-only narrow text damage performance measurement"]
    fn narrow_text_damage_bench() {
        use std::hint::black_box;
        use std::time::Instant;
        let buffer = shaped_buffer(&format!("{}\n", "x".repeat(120)).repeat(40), 1000.0, 800.0);
        let clip = Rectangle {
            x: 1000.25,
            y: 600.5,
            width: 8.5,
            height: 50.0,
        };
        let mut mask = tiny_skia::Mask::new(2250, 1250).unwrap();
        crate::engine::adjust_clip_mask(&mut mask, clip);
        for cull in [false, true] {
            let mut pipeline = Pipeline::new();
            pipeline.glyph_cache.disable_culling = !cull;
            let mut pixels = tiny_skia::Pixmap::new(2250, 1250).unwrap();
            // Warm shaping and glyph cache before measuring repeated caret damage.
            pipeline.draw_raw(
                &buffer,
                Point::ORIGIN,
                Color::WHITE,
                &mut pixels.as_mut(),
                Some(&mask),
                clip,
                Transformation::scale(2.5),
            );
            pipeline.glyph_cache.drawn_glyphs = 0;
            let started = Instant::now();
            for _ in 0..100 {
                pipeline.draw_raw(
                    &buffer,
                    Point::ORIGIN,
                    Color::WHITE,
                    &mut pixels.as_mut(),
                    Some(&mask),
                    clip,
                    Transformation::scale(2.5),
                );
                let _ = black_box(pixels.data());
            }
            eprintln!(
                "100 narrow text frames cull={cull}: {:?}, draw_pixmap calls={}",
                started.elapsed(),
                pipeline.glyph_cache.drawn_glyphs
            );
        }
    }

    #[test]
    fn empty_glyphs_are_retained_and_trimmed_with_visible_glyphs() {
        let mut fonts = font_system().write().unwrap();
        let fonts = fonts.raw();
        let mut buffer = cosmic_text::Buffer::new(fonts, cosmic_text::Metrics::new(16.0, 20.0));
        buffer.set_size(Some(100.0), Some(20.0));
        buffer.set_text(
            " x",
            &cosmic_text::Attrs::new(),
            cosmic_text::Shaping::Basic,
            None,
        );
        buffer.shape_until_scroll(fonts, false);
        let keys: Vec<_> = buffer
            .layout_runs()
            .flat_map(|run| {
                run.glyphs
                    .iter()
                    .map(|glyph| glyph.physical((0.0, 0.0), 1.0).cache_key)
            })
            .collect();
        assert_eq!(keys.len(), 2, "a real font must shape a space and x");
        let mut cache = GlyphCache::new();
        let mut swash = cosmic_text::SwashCache::new();
        assert!(
            cache
                .allocate(keys[0], Color::WHITE, fonts, &mut swash)
                .is_none()
        );
        assert!(
            cache
                .allocate(keys[1], Color::WHITE, fonts, &mut swash)
                .is_some()
        );
        for _ in 0..50 {
            assert!(
                cache
                    .allocate(keys[0], Color::WHITE, fonts, &mut swash)
                    .is_none()
            );
        }
        assert_eq!(cache.entries.len(), 2);
        assert!(cache.entries[&(keys[0], [255; 3])].is_none());
        cache.trim_count = GlyphCache::TRIM_INTERVAL + 1;
        cache.trim();
        assert_eq!(
            cache.entries.len(),
            2,
            "active empty glyphs survive trimming"
        );
        cache.trim_count = GlyphCache::TRIM_INTERVAL + 1;
        cache.trim();
        assert!(
            cache.entries.is_empty(),
            "unused negative entries are bounded too"
        );
    }
}
