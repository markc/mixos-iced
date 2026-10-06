use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::{Physical, Point, Size, Transform};
use xcursor::CursorTheme;
use xcursor::parser::{Image, parse_xcursor};

pub struct Cursor {
    pub frames: Vec<CursorFrame>,
    pub hotspot: Point<i32, Physical>,
    pub size: Size<i32, Physical>,
    /// The xcursor nominal size the frames were taken at (the theme's closest
    /// to the size asked for).
    pub nominal: u32,
}

pub struct CursorFrame {
    pub buffer: MemoryRenderBuffer,
    pub delay: Duration,
}

pub struct CursorThemeCache {
    theme: CursorTheme,
    fallback_size: u32,
    cache: Mutex<HashMap<String, Option<Arc<Cursor>>>>,
    builtin: Mutex<HashMap<u32, Arc<Cursor>>>,
}

impl CursorThemeCache {
    pub fn new(theme_name: &str, size: u32) -> Self {
        let theme = CursorTheme::load(theme_name);

        let test_1 = theme.load_icon("default");
        let test_2 = theme.load_icon("text");
        let test_3 = theme.load_icon("pointer");
        info!(
            "theme_name={:?} load_icon('default')={:?} load_icon('text')={:?} load_icon('pointer')={:?}",
            theme_name, test_1, test_2, test_3
        );

        Self {
            theme,
            fallback_size: size.max(1),
            cache: Mutex::new(HashMap::new()),
            builtin: Mutex::new(HashMap::new()),
        }
    }

    /// Resolve a freedesktop cursor name to a loaded cursor.
    /// Walks the theme's inheritance chain, then falls back to "default"
    /// and "left_ptr", then a built-in arrow. A self-contained session must
    /// retain a visible named cursor even without installed theme files.
    pub fn get(&self, name: &str) -> Option<Arc<Cursor>> {
        self.get_sized(name, self.fallback_size)
    }

    /// The theme's base (scale 1) cursor size, in logical px.
    pub fn base_size(&self) -> u32 {
        self.fallback_size
    }

    /// `name` for an output at `scale`: the frames
    /// at the theme's nominal size closest to `base × scale`, so a 2.5 output
    /// draws a 60 px-class image rather than a 24 px one upscaled. Hotspots and
    /// the drawn size stay logical (`base_size`); the caller draws the image at
    /// `base / nominal` of its pixels.
    pub fn get_scaled(&self, name: &str, scale: f64) -> Option<Arc<Cursor>> {
        let target = (f64::from(self.fallback_size) * scale.max(0.1))
            .round()
            .max(1.0) as u32;
        self.get_sized(name, target)
    }

    fn get_sized(&self, name: &str, target: u32) -> Option<Arc<Cursor>> {
        // The base size keeps the bare name as its key, as before.
        let key = if target == self.fallback_size {
            name.to_string()
        } else {
            format!("{name}@{target}")
        };
        if let Some(entry) = self.cache.lock().unwrap().get(&key) {
            return entry.clone();
        }

        let loaded =
            self.load_shape(name, target)
                .or_else(|| self.load_shape("default", target))
                .or_else(|| self.load_shape("left_ptr", target))
                .or_else(|| {
                    // Bound fallback allocations even with an extreme configured
                    // size or output scale. Share one render buffer per size across
                    // all unavailable names, keeping its renderer identity stable.
                    let target = target.clamp(1, 512);
                    let mut builtin = self.builtin.lock().unwrap();
                    Some(builtin.entry(target).or_insert_with(|| {
                    warn!("No usable cursor theme image; using built-in arrow at {target}px");
                    Arc::new(builtin_arrow(target))
                }).clone())
                });

        self.cache.lock().unwrap().insert(key, loaded.clone());
        loaded
    }

    fn load_shape(&self, name: &str, target: u32) -> Option<Arc<Cursor>> {
        // let path = self.theme.load_icon(name)?;
        // let mut file = std::fs::File::open(path).ok()?;
        // let mut buf = Vec::new();
        // file.read_to_end(&mut buf).ok()?;
        // let images = parse_xcursor(&buf)?;

        let path = self.theme.load_icon(name)?;

        let mut file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                return None;
            }
        };
        let mut buf = Vec::new();
        if let Err(e) = file.read_to_end(&mut buf) {
            return None;
        }

        let images = match parse_xcursor(&buf) {
            Some(imgs) => imgs,
            None => {
                warn!("load_shape({}): parse_xcursor returned None", name);
                return None;
            }
        };

        // 1. Find the nominal size closest to what we want.
        let chosen_size = images
            .iter()
            .map(|img| img.size)
            .min_by_key(|s| (*s as i32 - target as i32).abs())?;

        trace!("chosen_size={:?}", chosen_size);
        // 2. Collect every frame at that size, in file order.
        //    For static cursors this is one frame; for animated cursors
        //    (wait, progress) it's the full sequence.
        let frames: Vec<CursorFrame> = images
            .iter()
            .filter(|img| img.size == chosen_size)
            .map(image_to_frame)
            .collect();

        if frames.is_empty() {
            return None;
        }

        // Hotspot and dimensions come from the first frame.
        // All frames in a well-formed xcursor share these values.
        let first = images.iter().find(|img| img.size == chosen_size)?;

        let hotspot = Point::from((first.xhot as i32, first.yhot as i32));
        let size = Size::from((first.width as i32, first.height as i32));

        Some(Arc::new(Cursor {
            frames,
            hotspot,
            size,
            nominal: chosen_size,
        }))
    }
}

/// Original, source-defined arrow: an opaque dark outline and light fill keep
/// it visible on both light and dark surfaces. No installed artwork is needed.
fn arrow_pixels(size: u32) -> Vec<u8> {
    const OUTLINE: [(f64, f64); 8] = [
        (0.0, 0.0),
        (0.0, 20.0),
        (5.0, 15.0),
        (9.0, 23.0),
        (12.0, 21.0),
        (8.0, 13.0),
        (16.0, 13.0),
        (0.0, 0.0),
    ];
    let mut pixels = vec![0; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let px = (f64::from(x) + 0.5) * 24.0 / f64::from(size);
            let py = (f64::from(y) + 0.5) * 24.0 / f64::from(size);
            let mut inside = false;
            let mut border = false;
            for edge in OUTLINE.windows(2) {
                let ((ax, ay), (bx, by)) = (edge[0], edge[1]);
                if (ay > py) != (by > py) && px < (bx - ax) * (py - ay) / (by - ay) + ax {
                    inside = !inside;
                }
                let (dx, dy) = (bx - ax, by - ay);
                let t = (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
                let distance = (px - ax - t * dx).powi(2) + (py - ay - t * dy).powi(2);
                border |= distance <= 0.9 * 0.9;
            }
            if inside {
                let shade = if border { 0 } else { 255 };
                let offset = ((y * size + x) * 4) as usize;
                // Opaque greys are identical in the renderer's ARGB byte order.
                pixels[offset..offset + 4].copy_from_slice(&[shade, shade, shade, 255]);
            }
        }
    }
    pixels
}

fn builtin_arrow(size: u32) -> Cursor {
    let dimensions = Size::from((size as i32, size as i32));
    let (format, _) = render_gles::format::answer::answer::constant(
        render_gles::format::answer::answer::Consumer::CursorImage,
    );
    Cursor {
        frames: vec![CursorFrame {
            buffer: MemoryRenderBuffer::from_slice(
                &arrow_pixels(size),
                format,
                dimensions,
                1,
                Transform::Normal,
                None,
            ),
            delay: Duration::from_millis(1),
        }],
        hotspot: (0, 0).into(),
        size: (size as i32, size as i32).into(),
        nominal: size,
    }
}

fn image_to_frame(img: &Image) -> CursorFrame {
    let width = img.width as i32;
    let height = img.height as i32;
    // let stride = width * 4;

    // XCursor pixels are ARGB by specification; the layer states it so this crate
    // does not have to name a format.
    let (cursor_fourcc, _) = render_gles::format::answer::answer::constant(
        render_gles::format::answer::answer::Consumer::CursorImage,
    );
    let buffer = MemoryRenderBuffer::from_slice(
        &img.pixels_rgba,
        cursor_fourcc,
        Size::from((width, height)),
        1, // scale: 1 for non-HiDPI cursors
        Transform::Normal,
        None,
    );
    // let buffer = MemoryRenderBuffer::from_slice(
    //     &img.pixels_rgba,
    //     Fourcc::Argb8888,
    //     Size::from((width, height)),
    //     stride,
    //     Transform::Normal,
    //     None,
    // );

    // xcursor delay is in milliseconds. A delay of 0 on a multi-frame
    // cursor would loop forever on one frame, so floor to 1ms.
    let delay = if img.delay == 0 {
        Duration::from_millis(1)
    } else {
        Duration::from_millis(img.delay as u64)
    };

    CursorFrame { buffer, delay }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ThemeFixture(std::path::PathBuf);

    impl ThemeFixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("cursor-test-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir_all(path.join("cursors")).unwrap();
            // An absolute theme path isolates lookup without process-wide env
            // changes. Prevent inheritance from the worker's default theme.
            std::fs::write(
                path.join("index.theme"),
                format!("Inherits=missing-{}\n", uuid::Uuid::now_v7()),
            )
            .unwrap();
            Self(path)
        }

        fn cache(&self, size: u32) -> CursorThemeCache {
            CursorThemeCache::new(self.0.to_str().unwrap(), size)
        }
    }

    impl Drop for ThemeFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_theme_keeps_named_cursors_visible_and_cached() {
        let fixture = ThemeFixture::new();
        let cache = fixture.cache(24);
        let arrow = cache.get("default").unwrap();
        assert_eq!(arrow.hotspot, (0, 0).into());
        assert_eq!(arrow.size, (24, 24).into());
        assert_eq!(arrow.frames.len(), 1);
        assert!(Arc::ptr_eq(&arrow, &cache.get("default").unwrap()));
        assert!(Arc::ptr_eq(&arrow, &cache.get("text").unwrap()));
        assert!(Arc::ptr_eq(&arrow, &cache.get("unavailable-name").unwrap()));
        let high_dpi = cache.get_scaled("pointer", 2.5).unwrap();
        assert_eq!(high_dpi.size, (60, 60).into());
        assert_eq!(high_dpi.nominal, 60);
        assert_eq!(cache.base_size(), 24);
    }

    #[test]
    fn malformed_theme_image_still_has_a_cursor() {
        let fixture = ThemeFixture::new();
        std::fs::write(fixture.0.join("cursors/default"), b"invalid cursor").unwrap();
        assert_eq!(
            fixture.cache(24).get("default").unwrap().size,
            (24, 24).into()
        );
    }

    #[test]
    fn installed_theme_image_takes_precedence() {
        let fixture = ThemeFixture::new();
        let mut bytes = b"Xcur".to_vec();
        // File header, TOC and one 2x2 image with a distinctive hotspot.
        for word in [
            16_u32, 1, 1, 0xfffd0002, 24, 28, 36, 0xfffd0002, 24, 1, 2, 2, 1, 1, 10,
        ] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes.extend_from_slice(&[255; 16]);
        std::fs::write(fixture.0.join("cursors/default"), bytes).unwrap();
        let cursor = fixture.cache(24).get("text").unwrap();
        assert_eq!(cursor.size, (2, 2).into());
        assert_eq!(cursor.hotspot, (1, 1).into());
        assert_eq!(cursor.frames[0].delay, Duration::from_millis(10));
    }

    #[test]
    fn arrow_has_contrast_transparency_and_bounded_fallback_sizes() {
        for size in [24, 60] {
            let pixels = arrow_pixels(size);
            assert!(pixels.chunks_exact(4).any(|p| p == [0, 0, 0, 255]));
            assert!(pixels.chunks_exact(4).any(|p| p == [255, 255, 255, 255]));
            assert!(pixels.chunks_exact(4).any(|p| p == [0, 0, 0, 0]));
            assert_eq!(&pixels[..4], &[0, 0, 0, 255]);
        }
        let fixture = ThemeFixture::new();
        assert_eq!(fixture.cache(0).get("default").unwrap().size, (1, 1).into());
        assert_eq!(
            fixture.cache(u32::MAX).get("default").unwrap().size,
            (512, 512).into()
        );
    }
}
