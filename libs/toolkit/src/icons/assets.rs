// SPDX-License-Identifier: MIT OR Apache-2.0
//! Eager SVG/raster fallbacks after the installed icon font. Resolve on a
//! caller-owned worker, then put the ready pixels in the first rendered frame.
//! Cache variants include physical size and symbolic tint; file changes are
//! checked on each resolve. No timer, global cache or filesystem watcher.

use super::freedesktop::Lookup;
use crate::Icon;
use iced_core::{
    Color, Element, Font,
    image::Handle,
    text,
    widget::text::{Catalog, StyleFn},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Mutex,
    time::SystemTime,
};

type Key = (PathBuf, u32, Option<[u8; 4]>);
type Entry = (Option<SystemTime>, u64, Handle);
const CAP: usize = 512;
const MAX_SIDE: f32 = 2048.0;
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// A glyph, decoded image or visible icon-name fallback. Owns everything it
/// needs to render, so a worker can return it without keeping the cache alive.
#[derive(Debug, Clone)]
pub enum Ready {
    Text(Icon),
    Image { handle: Handle, logical_size: f32 },
}

impl Ready {
    pub fn view<'a, Message, Theme, Renderer>(self) -> Element<'a, Message, Theme, Renderer>
    where
        Theme: Catalog + 'a,
        Theme::Class<'a>: From<StyleFn<'a, Theme>>,
        Renderer: text::Renderer + iced_core::image::Renderer<Handle = Handle> + 'a,
        Renderer::Font: From<Font>,
    {
        match self {
            Self::Text(icon) => icon.into(),
            Self::Image {
                handle,
                logical_size,
            } => iced_widget::image::Image::new(handle)
                .width(logical_size)
                .height(logical_size)
                .into(),
        }
    }
}

/// Caller-owned fallback catalogue plus a bounded cache. Explicit assets are
/// tried in registration order, followed by the optional freedesktop theme.
#[derive(Default)]
pub struct Assets {
    files: BTreeMap<String, Vec<(PathBuf, bool)>>,
    theme: Option<Lookup>,
    cache: Mutex<BTreeMap<Key, Entry>>,
}

impl Assets {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn themed(mut self, theme: Lookup) -> Self {
        self.theme = Some(theme);
        self
    }
    /// `symbolic` replaces the asset's RGB with the resolve tint, preserving
    /// coverage. Ordinary coloured assets keep their own colours.
    pub fn fallback(
        mut self,
        name: impl Into<String>,
        path: impl Into<PathBuf>,
        symbolic: bool,
    ) -> Self {
        self.files
            .entry(name.into())
            .or_default()
            .push((path.into(), symbolic));
        self
    }
    /// Glyph → registered SVG/raster → theme asset → visible name. Invalid
    /// size/scale and unreadable/blank assets also end in the visible name.
    /// Re-resolve symbolic assets when theme colour or display scale changes.
    pub fn resolve(&self, icon: Icon, logical_size: f32, scale: f32, tint: Color) -> Ready {
        let fallback = || {
            Ready::Text(
                icon.clone()
                    .size(if logical_size.is_finite() && logical_size > 0.0 {
                        logical_size
                    } else {
                        16.0
                    }),
            )
        };
        if icon.glyph().is_some() {
            return fallback();
        }
        let side = (logical_size * scale).ceil();
        if !logical_size.is_finite()
            || logical_size <= 0.0
            || !scale.is_finite()
            || scale <= 0.0
            || !(1.0..=MAX_SIDE).contains(&side)
        {
            return fallback();
        }
        let decode = |path: &Path, symbolic: bool| {
            self.image(path, side as u32, symbolic.then(|| tint.into_rgba8()))
        };
        if let Some(files) = self.files.get(icon.name()) {
            for (path, symbolic) in files {
                if let Some(handle) = decode(path, *symbolic) {
                    return Ready::Image {
                        handle,
                        logical_size,
                    };
                }
            }
        }
        if let Some(path) = self
            .theme
            .as_ref()
            .and_then(|theme| theme.find(icon.name(), side as u32))
        {
            let symbolic = path
                .file_stem()
                .is_some_and(|name| name.to_string_lossy().ends_with("-symbolic"));
            if let Some(handle) = decode(&path, symbolic) {
                return Ready::Image {
                    handle,
                    logical_size,
                };
            }
        }
        fallback()
    }
    fn image(&self, path: &Path, side: u32, tint: Option<[u8; 4]>) -> Option<Handle> {
        let metadata = path.metadata().ok()?;
        if !metadata.is_file() || metadata.len() > MAX_BYTES {
            return None;
        }
        let stamp = (metadata.modified().ok(), metadata.len());
        let key = (path.to_owned(), side, tint);
        let mut cache = self.cache.lock().ok()?;
        if let Some((_, _, handle)) = cache
            .get(&key)
            .filter(|(time, len, _)| (*time, *len) == stamp)
        {
            return Some(handle.clone());
        }
        let (width, height, mut pixels) = if super::freedesktop::is_svg(path) {
            let options = resvg::usvg::Options {
                // Icons are self-contained vectors. Do not let SVG hrefs read
                // neighbouring files or decode embedded raster payloads.
                image_href_resolver: resvg::usvg::ImageHrefResolver {
                    resolve_data: Box::new(|_, _, _| None),
                    resolve_string: Box::new(|_, _| None),
                },
                ..Default::default()
            };
            let bytes = std::fs::read(path).ok()?;
            let tree = resvg::usvg::Tree::from_data(&bytes, &options).ok()?;
            let size = tree.size();
            let factor = (side as f32 / size.width()).min(side as f32 / size.height());
            let transform = resvg::tiny_skia::Transform::from_row(
                factor,
                0.0,
                0.0,
                factor,
                (side as f32 - size.width() * factor) / 2.0,
                (side as f32 - size.height() * factor) / 2.0,
            );
            let mut bitmap = resvg::tiny_skia::Pixmap::new(side, side)?;
            resvg::render(&tree, transform, &mut bitmap.as_mut());
            let mut pixels = bitmap.take();
            // tiny-skia is premultiplied; iced's eager RGBA handles are straight.
            for pixel in pixels.chunks_exact_mut(4) {
                for channel in 0..3 {
                    pixel[channel] = if pixel[3] == 0 {
                        0
                    } else {
                        ((u32::from(pixel[channel]) * 255 + u32::from(pixel[3]) / 2)
                            / u32::from(pixel[3]))
                        .min(255) as u8
                    };
                }
            }
            (side, side, pixels)
        } else {
            let bitmap = iced_graphics::image::load(&Handle::from_path(path)).ok()?;
            (bitmap.width(), bitmap.height(), bitmap.into_raw().to_vec())
        };
        if !pixels.chunks_exact(4).any(|pixel| pixel[3] != 0) {
            return None;
        }
        if let Some(tint) = tint {
            for pixel in pixels.chunks_exact_mut(4) {
                pixel[..3].copy_from_slice(&tint[..3]);
                pixel[3] = ((u16::from(pixel[3]) * u16::from(tint[3]) + 127) / 255) as u8;
            }
        }
        let handle = Handle::from_rgba(width, height, pixels);
        if cache.len() >= CAP {
            cache.pop_first();
        }
        cache.insert(key, (stamp.0, stamp.1, handle.clone()));
        Some(handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture_svg() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fallback.svg");
        std::fs::write(&path, r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="#ff0000" fill-opacity="0.5"/></svg>"##).unwrap();
        (directory, path)
    }
    fn image(ready: Ready) -> Handle {
        match ready {
            Ready::Image { handle, .. } => handle,
            Ready::Text(_) => panic!("expected decoded icon"),
        }
    }
    #[test]
    fn svg_fallback_is_eager_correctly_unpremultiplied_and_cached_by_scale_and_tint() {
        let (_directory, path) = fixture_svg();
        let assets = Assets::new()
            .fallback("asset-fixture", &path, false)
            .fallback("symbolic-fixture", &path, true);
        let resolve = |name, scale, tint| image(assets.resolve(Icon::new(name), 10.0, scale, tint));
        let first = resolve("asset-fixture", 1.0, Color::BLACK);
        assert_eq!(first.id(), resolve("asset-fixture", 1.0, Color::WHITE).id());
        assert_ne!(first.id(), resolve("asset-fixture", 2.0, Color::BLACK).id());
        let Handle::Rgba {
            width,
            height,
            pixels,
            ..
        } = first
        else {
            panic!()
        };
        assert_eq!((width, height), (10, 10));
        assert_eq!(&pixels[..3], &[255, 0, 0]);
        assert!((126..=129).contains(&pixels[3]));
        let black = resolve("symbolic-fixture", 1.0, Color::BLACK);
        let white = resolve("symbolic-fixture", 1.0, Color::WHITE);
        assert_ne!(black.id(), white.id());
        assert_eq!(
            white.id(),
            resolve("symbolic-fixture", 1.0, Color::WHITE).id()
        );
    }
    #[test]
    fn deleted_corrupt_blank_and_invalid_size_assets_have_visible_names() {
        let (_directory, path) = fixture_svg();
        let assets = Assets::new().fallback("fixture", &path, false);
        let first = image(assets.resolve(Icon::new("fixture"), 16.0, 1.0, Color::WHITE));
        std::fs::write(&path, "invalid SVG").unwrap();
        assert!(matches!(
            assets.resolve(Icon::new("fixture"), 16.0, 1.0, Color::WHITE),
            Ready::Text(_)
        ));
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(
            assets.resolve(Icon::new("fixture"), 16.0, 1.0, Color::WHITE),
            Ready::Text(_)
        ));
        for (size, scale) in [
            (f32::NAN, 1.0),
            (16.0, f32::INFINITY),
            (-1.0, 1.0),
            (16.0, 0.0),
        ] {
            assert!(matches!(
                assets.resolve(Icon::new("fixture"), size, scale, Color::WHITE),
                Ready::Text(_)
            ));
        }
        assert!(matches!(first, Handle::Rgba { .. }));
    }
    #[test]
    fn corrupt_primary_asset_can_fall_back_to_a_ready_raster() {
        let assets = Assets::new()
            .fallback("fixture", "/missing/icon.svg", false)
            .fallback(
                "fixture",
                Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/icon.png"),
                true,
            );
        let Handle::Rgba { pixels, .. } =
            image(assets.resolve(Icon::new("fixture"), 24.0, 2.0, Color::WHITE))
        else {
            panic!()
        };
        assert_eq!(&pixels[..3], &[255, 255, 255]);
    }
}
