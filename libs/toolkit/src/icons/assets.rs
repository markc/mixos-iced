// SPDX-License-Identifier: MIT OR Apache-2.0
//! Eager SVG/raster fallbacks after the installed icon font. Resolve on a
//! caller-owned worker, then put the ready pixels in the first rendered frame.
//! Cache variants include physical size and symbolic tint; file changes are
//! checked on each resolve. No timer, global cache or filesystem watcher.
//!
//! [`decode_owned`] is the one decoder shared by both paths: explicit
//! prepared resources and the legacy [`Assets`] catalogue. It takes the exact
//! verified bytes plus a declared format, owns no path, reopens nothing and
//! never touches a freedesktop theme.

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
    fmt,
    io::Cursor,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::SystemTime,
};

type Key = (PathBuf, u32, Option<[u8; 4]>);
type Entry = (Option<SystemTime>, u64, Handle);
const CAP: usize = 512;

/// Decoder process limits (image policy, toolkit scope).
/// The largest physical side a decoded variant may request or hold.
pub const MAX_PHYSICAL_SIDE: u32 = 2048;
/// The largest encoded asset the decoder accepts.
pub const MAX_ENCODED_BYTES: u64 = 8 * 1024 * 1024;
/// The raster codec allocation limit and checked reported source charge.
/// Some codecs treat internal scratch as advisory. This is not a whole-SVG
/// heap ceiling; SVG conversion has separate structural complexity limits.
pub const MAX_DECODER_ALLOC: u64 = 64 * 1024 * 1024;
/// The binding retained pixel charge: width × height × 4. Both paths cap
/// each side at [`MAX_PHYSICAL_SIDE`], so 2048² × 4 = 16 MiB is the real
/// ceiling; the value is derived, not a second knob.
pub const MAX_DECODED_BYTES: u64 = MAX_PHYSICAL_SIDE as u64 * MAX_PHYSICAL_SIDE as u64 * 4;
/// The largest number of XML nodes the SVG pre-scan lets usvg materialise.
pub const MAX_SVG_NODES: usize = 65_536;
/// The largest total of `d`-attribute bytes the SVG pre-scan accepts.
/// usvg converts path data into segments with an input-proportional
/// amplification, so path-dense payloads are capped well below the
/// encoded-byte cap.
pub const MAX_SVG_PATH_BYTES: usize = 512 * 1024;

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
            } => {
                // Keep the decoded intrinsic aspect ratio inside the logical
                // square instead of forcing a square layout that distorts
                // non-square rasters.
                let (width, height) = image_layout(&handle, logical_size);
                iced_widget::image::Image::new(handle)
                    .width(width)
                    .height(height)
                    .into()
            }
        }
    }
}

/// The logical width and height that keep a decoded image's intrinsic
/// aspect ratio inside the `logical_size` square.
fn image_layout(handle: &Handle, logical_size: f32) -> (f32, f32) {
    // Only decoded (Rgba) handles reach `Ready::Image`; the other variants
    // have no intrinsic size yet and keep the square.
    let Handle::Rgba { width, height, .. } = handle else {
        return (logical_size, logical_size);
    };
    let scale = logical_size / u32::max(*width, *height).max(1) as f32;
    (*width as f32 * scale, *height as f32 * scale)
}

/// How the owned bytes are decoded. The caller derives it from the verified
/// declared asset type or locked extension — never by reopening the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Svg,
    Raster,
}

/// Why [`decode_owned`] refused the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconDecodeError {
    /// The encoded payload is empty.
    Empty,
    /// The encoded payload exceeds [`MAX_ENCODED_BYTES`].
    EncodedTooLarge { bytes: u64 },
    /// The requested physical side is outside 1..=[`MAX_PHYSICAL_SIDE`].
    SideOutOfRange { side: u32 },
    /// The SVG does not parse.
    SvgParse,
    /// The SVG references an external or data image href, which is refused.
    SvgImageDependency,
    /// The SVG contains text, which needs a font provider this decoder does
    /// not have; a required asset must not render without its text.
    SvgTextDependency,
    /// The SVG uses a filter (`filter` element or attribute), which is
    /// outside the documented supported subset: resvg would render it into
    /// an intermediate surface the decode budget cannot clamp.
    SvgFilter,
    /// CSS styles are outside the bounded SVG subset.
    SvgStyleDependency,
    /// Expansion, isolation or indirect painting outside the bounded subset.
    SvgUnsupported,
    /// The SVG `use` href is not a same-document fragment (`#id`).
    SvgUseDependency,
    /// The SVG exceeds the pre-scan complexity budget.
    SvgComplexity { nodes: usize, path_bytes: usize },
    /// The SVG could not be rendered at the requested side.
    SvgRender,
    /// The raster bytes do not decode within the configured limits.
    RasterDecode,
    /// The decoded pixels have no visible coverage.
    Blank,
    /// The requested tint has zero alpha, so the tinted icon would render
    /// invisibly.
    TintAlphaZero,
    /// The decoded pixel charge (width × height × 4) exceeds
    /// [`MAX_DECODED_BYTES`].
    DecodedTooLarge { bytes: u64 },
}

impl fmt::Display for IconDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "encoded icon bytes are empty"),
            Self::EncodedTooLarge { bytes } => write!(
                f,
                "encoded icon is {bytes} bytes; the limit is {MAX_ENCODED_BYTES}"
            ),
            Self::SideOutOfRange { side } => {
                write!(f, "physical side {side} is outside 1..={MAX_PHYSICAL_SIDE}")
            }
            Self::SvgParse => write!(f, "SVG does not parse"),
            Self::SvgImageDependency => {
                write!(f, "SVG references an external or data image href")
            }
            Self::SvgTextDependency => write!(f, "SVG contains text without a font provider"),
            Self::SvgFilter => write!(f, "SVG uses a filter, which is outside the supported subset"),
            Self::SvgStyleDependency => write!(f, "SVG uses CSS outside the supported subset"),
            Self::SvgUnsupported => write!(f, "SVG uses expansion or indirect painting outside the supported subset"),
            Self::SvgUseDependency => {
                write!(f, "SVG `use` href is not a same-document fragment")
            }
            Self::SvgComplexity { nodes, path_bytes } => write!(
                f,
                "SVG exceeds the complexity budget: {nodes} XML nodes and {path_bytes} \
                 path attribute bytes; the limits are {MAX_SVG_NODES} and {MAX_SVG_PATH_BYTES}"
            ),
            Self::SvgRender => write!(f, "SVG could not be rendered at the requested side"),
            Self::RasterDecode => write!(f, "raster bytes do not decode within the limits"),
            Self::Blank => write!(f, "decoded icon has no visible pixel"),
            Self::TintAlphaZero => write!(f, "tint alpha is zero; the tinted icon would be invisible"),
            Self::DecodedTooLarge { bytes } => write!(
                f,
                "decoded pixels are {bytes} bytes; the limit is {MAX_DECODED_BYTES}"
            ),
        }
    }
}

impl std::error::Error for IconDecodeError {}

/// Straight (unpremultiplied) RGBA pixels decoded from owned bytes, with the
/// dimensions and the checked decoded-byte charge. Converting to an iced
/// [`Handle`] performs no further decode and no file read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedIcon {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl DecodedIcon {
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The straight-RGBA pixels.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// The checked retained charge: width × height × 4.
    pub fn byte_charge(&self) -> u64 {
        self.pixels.len() as u64
    }

    /// A renderer-ready handle over a copy of these pixels.
    pub fn handle(&self) -> Handle {
        Handle::from_rgba(self.width, self.height, self.pixels.clone())
    }

    /// A renderer-ready handle owning these pixels.
    pub fn into_handle(self) -> Handle {
        Handle::from_rgba(self.width, self.height, self.pixels)
    }
}

/// Decode owned bytes into straight-RGBA pixels. The bytes are the exact
/// verified source; nothing is reopened or discovered from a path.
///
/// SVG is scaled into a `physical_side` × `physical_side` canvas and
/// tiny-skia's premultiplied output is unpremultiplied for iced's eager RGBA
/// handles. The supported subset is filled paths/basic shapes and bounded
/// gradients. DTDs, CSS, text/images, nested SVG, strokes, local/external uses,
/// filters, masks, clips, patterns, markers and isolation are refused before
/// conversion. XML nodes/depth, path/points bytes and gradient expansion are
/// budgeted; converted groups must also require no isolated surfaces. These
/// structural limits are separate from retained final-pixel accounting.
///
/// Raster is decoded over a [`Cursor`] on these bytes with bounded
/// dimensions and allocation — the decoder's reported total bytes are
/// reserved before the pixel buffer is materialised — and EXIF orientation
/// applied, at its intrinsic size; `physical_side` is not a raster resize.
///
/// A fully transparent result is [`IconDecodeError::Blank`], and the checked
/// pixel charge (width × height × 4) must fit [`MAX_DECODED_BYTES`]. `tint`
/// replaces the RGB and scales the alpha of symbolic assets; ordinary
/// coloured assets pass `None`. A tint with zero alpha is refused
/// ([`IconDecodeError::TintAlphaZero`]) instead of decoding an icon that
/// renders invisibly.
pub fn decode_owned(
    bytes: Arc<[u8]>,
    format: ImageFormat,
    physical_side: u32,
    tint: Option<[u8; 4]>,
) -> Result<DecodedIcon, IconDecodeError> {
    if bytes.is_empty() {
        return Err(IconDecodeError::Empty);
    }
    if bytes.len() as u64 > MAX_ENCODED_BYTES {
        return Err(IconDecodeError::EncodedTooLarge {
            bytes: bytes.len() as u64,
        });
    }
    if !(1..=MAX_PHYSICAL_SIDE).contains(&physical_side) {
        return Err(IconDecodeError::SideOutOfRange {
            side: physical_side,
        });
    }
    if tint.is_some_and(|tint| tint[3] == 0) {
        return Err(IconDecodeError::TintAlphaZero);
    }
    let (width, height, mut pixels) = match format {
        ImageFormat::Svg => decode_svg(&bytes, physical_side)?,
        ImageFormat::Raster => decode_raster(&bytes)?,
    };
    let charge = u64::from(width) * u64::from(height) * 4;
    if charge > MAX_DECODED_BYTES {
        return Err(IconDecodeError::DecodedTooLarge { bytes: charge });
    }
    if !pixels.chunks_exact(4).any(|pixel| pixel[3] != 0) {
        return Err(IconDecodeError::Blank);
    }
    if let Some(tint) = tint {
        for pixel in pixels.chunks_exact_mut(4) {
            pixel[..3].copy_from_slice(&tint[..3]);
            pixel[3] = ((u16::from(pixel[3]) * u16::from(tint[3]) + 127) / 255) as u8;
        }
    }
    Ok(DecodedIcon {
        width,
        height,
        pixels,
    })
}

fn decode_svg(bytes: &[u8], side: u32) -> Result<(u32, u32, Vec<u8>), IconDecodeError> {
    // Scan with the same XML parser family usvg uses, before any usvg tree
    // exists. A usvg build without its `text` feature silently drops text
    // elements before the tree exists, so the tree cannot witness them:
    // the scan refuses them at the document level along with every other
    // construct outside the documented subset, and it budgets node and
    // path-attribute complexity ahead of usvg's segment conversion.
    //
    // DOCTYPEs are deliberately refused (roxmltree's default): usvg accepts
    // them and expands entities, and entity expansion is kept out of the
    // decode path entirely. Do not enable `allow_dtd` merely to match usvg.
    let document = roxmltree::Document::parse_with_options(
        std::str::from_utf8(bytes).map_err(|_| IconDecodeError::SvgParse)?,
        roxmltree::ParsingOptions { allow_dtd: false, nodes_limit: MAX_SVG_NODES as u32, ..Default::default() },
    )
    .map_err(|error| if matches!(error, roxmltree::Error::NodesLimitReached) {
        IconDecodeError::SvgComplexity { nodes: MAX_SVG_NODES + 1, path_bytes: 0 }
    } else { IconDecodeError::SvgParse })?;
    scan_svg(&document)?;
    // Image and feImage nodes are refused by the scan, so usvg never sees an
    // image href. The refusing resolvers stay as a backstop: should any
    // reference slip past the scan, it is detected and refused instead of
    // rendering partially.
    let referenced = Arc::new(AtomicBool::new(false));
    let data = referenced.clone();
    let external = referenced.clone();
    let options = resvg::usvg::Options {
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_data: Box::new(move |_, _, _| {
                data.store(true, Ordering::Relaxed);
                None
            }),
            resolve_string: Box::new(move |_, _| {
                external.store(true, Ordering::Relaxed);
                None
            }),
        },
        ..Default::default()
    };
    let tree =
        resvg::usvg::Tree::from_xmltree(&document, &options).map_err(|_| IconDecodeError::SvgParse)?;
    if referenced.load(Ordering::Relaxed) {
        return Err(IconDecodeError::SvgImageDependency);
    }
    check_render_subset(tree.root())?;
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
    let mut bitmap = resvg::tiny_skia::Pixmap::new(side, side).ok_or(IconDecodeError::SvgRender)?;
    resvg::render(&tree, transform, &mut bitmap.as_mut());
    let mut pixels = bitmap.take();
    // tiny-skia is premultiplied; iced's eager RGBA handles are straight.
    for pixel in pixels.chunks_exact_mut(4) {
        for channel in 0..3 {
            pixel[channel] = if pixel[3] == 0 {
                0
            } else {
                ((u32::from(pixel[channel]) * 255 + u32::from(pixel[3]) / 2) / u32::from(pixel[3]))
                    .min(255) as u8
            };
        }
    }
    Ok((side, side, pixels))
}

fn scan_svg(document: &roxmltree::Document<'_>) -> Result<(), IconDecodeError> {
    let mut nodes = 0usize;
    let mut path_bytes = 0usize;
    let mut stops = 0usize;
    let mut painted = 0usize;
    for node in document.descendants() {
        nodes += 1;
        if nodes > MAX_SVG_NODES {
            return Err(IconDecodeError::SvgComplexity { nodes, path_bytes });
        }
        if !node.is_element() {
            continue;
        }
        if node.ancestors().count() > 64 || (node.tag_name().name() == "svg" && node != document.root_element()) {
            return Err(IconDecodeError::SvgUnsupported);
        }
        if node.tag_name().name() == "stop" { stops += 1; }
        if matches!(node.tag_name().name(), "path" | "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon") { painted += 1; }
        match node.tag_name().name() {
            "image" | "feImage" => return Err(IconDecodeError::SvgImageDependency),
            "text" | "tspan" | "textPath" => return Err(IconDecodeError::SvgTextDependency),
            "filter" => return Err(IconDecodeError::SvgFilter),
            "style" => return Err(IconDecodeError::SvgStyleDependency),
            "use" | "pattern" | "marker" | "mask" | "clipPath" => return Err(IconDecodeError::SvgUnsupported),
            "svg" | "g" | "defs" | "path" | "rect" | "circle" | "ellipse"
            | "line" | "polyline" | "polygon" | "linearGradient" | "radialGradient"
            | "stop" | "title" | "desc" => {},
            _ => return Err(IconDecodeError::SvgUnsupported),
        }
        for attribute in node.attributes() {
            if attribute.name() == "style" {
                // Do not interpret CSS escapes or cascades here. Explicit
                // SVG presentation attributes remain supported.
                return Err(IconDecodeError::SvgStyleDependency);
            }
            if attribute.name() == "filter" {
                return Err(IconDecodeError::SvgFilter);
            }
            if matches!(attribute.name(), "opacity" | "isolation" | "mix-blend-mode"
                | "mask" | "clip-path" | "marker-start" | "marker-mid" | "marker-end"
                | "stroke-dasharray" | "stroke-dashoffset") {
                return Err(IconDecodeError::SvgUnsupported);
            }
            if (attribute.name() == "stroke" && attribute.value().trim() != "none")
                || attribute.name() == "href" {
                return Err(IconDecodeError::SvgUnsupported);
            }
            if matches!(attribute.name(), "d" | "points") {
                path_bytes += attribute.value().len();
                if path_bytes > MAX_SVG_PATH_BYTES {
                    return Err(IconDecodeError::SvgComplexity { nodes, path_bytes });
                }
            }
        }
    }
    if stops > 256 || stops.saturating_mul(painted) > 65_536 {
        return Err(IconDecodeError::SvgComplexity { nodes, path_bytes });
    }
    Ok(())
}

fn check_render_subset(group: &resvg::usvg::Group) -> Result<(), IconDecodeError> {
    if group.should_isolate() { return Err(IconDecodeError::SvgUnsupported); }
    for node in group.children() {
        match node {
            resvg::usvg::Node::Group(group) => check_render_subset(group)?,
            resvg::usvg::Node::Path(path) => {
                if path.stroke().is_some() || path.fill().is_some_and(|fill| matches!(fill.paint(), resvg::usvg::Paint::Pattern(_))) {
                    return Err(IconDecodeError::SvgUnsupported);
                }
            }
            _ => return Err(IconDecodeError::SvgUnsupported),
        }
    }
    Ok(())
}

fn decode_raster(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), IconDecodeError> {
    // Decode over a cursor on these exact bytes: no path, no reopen, no
    // metadata or mtime consultation. Limits bound dimensions and allocation;
    // the pixel charge is checked again by the caller.
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| IconDecodeError::RasterDecode)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_PHYSICAL_SIDE);
    limits.max_image_height = Some(MAX_PHYSICAL_SIDE);
    limits.max_alloc = Some(MAX_DECODER_ALLOC);
    reader.limits(limits.clone());
    use image::ImageDecoder;
    let mut decoder = reader
        .into_decoder()
        .map_err(|_| IconDecodeError::RasterDecode)?;
    let orientation = decoder
        .orientation()
        .map_err(|_| IconDecodeError::RasterDecode)?;
    // Some codecs implement only the strict dimension limits. Reserve the
    // reported source allocation ourselves, as ImageReader::decode does,
    // while retaining access to the decoder's EXIF orientation.
    limits
        .reserve(decoder.total_bytes())
        .map_err(|_| IconDecodeError::RasterDecode)?;
    let mut bitmap =
        image::DynamicImage::from_decoder(decoder).map_err(|_| IconDecodeError::RasterDecode)?;
    bitmap.apply_orientation(orientation);
    let bitmap = bitmap.into_rgba8();
    Ok((bitmap.width(), bitmap.height(), bitmap.into_raw()))
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
            || !(1.0..=MAX_PHYSICAL_SIDE as f32).contains(&side)
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
        if !metadata.is_file() || metadata.len() > MAX_ENCODED_BYTES {
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
        let bytes: Arc<[u8]> = std::fs::read(path).ok()?.into();
        let format = if super::freedesktop::is_svg(path) {
            ImageFormat::Svg
        } else {
            ImageFormat::Raster
        };
        let handle = decode_owned(bytes, format, side, tint).ok()?.handle();
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
        let [red, green, blue, _] = crate::Tokens::dark().palette.primary.into_rgba8();
        std::fs::write(&path, format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="rgb({red},{green},{blue})" fill-opacity="0.5"/></svg>"#)).unwrap();
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
        let first = resolve("asset-fixture", 1.0, crate::Tokens::light().palette.text);
        assert_eq!(
            first.id(),
            resolve("asset-fixture", 1.0, crate::Tokens::dark().palette.text).id()
        );
        assert_ne!(
            first.id(),
            resolve("asset-fixture", 2.0, crate::Tokens::light().palette.text).id()
        );
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
        for (actual, expected) in pixels[..3]
            .iter()
            .zip(crate::Tokens::dark().palette.primary.into_rgba8())
        {
            assert!(actual.abs_diff(expected) <= 1);
        }
        assert!((126..=129).contains(&pixels[3]));
        let black = resolve("symbolic-fixture", 1.0, crate::Tokens::light().palette.text);
        let white = resolve("symbolic-fixture", 1.0, crate::Tokens::dark().palette.text);
        assert_ne!(black.id(), white.id());
        assert_eq!(
            white.id(),
            resolve("symbolic-fixture", 1.0, crate::Tokens::dark().palette.text).id()
        );
    }
    #[test]
    fn deleted_corrupt_blank_and_invalid_size_assets_have_visible_names() {
        let (_directory, path) = fixture_svg();
        let assets = Assets::new().fallback("fixture", &path, false);
        let first = image(assets.resolve(
            Icon::new("fixture"),
            16.0,
            1.0,
            crate::Tokens::dark().palette.text,
        ));
        std::fs::write(
            &path,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"/>"#,
        )
        .unwrap();
        assert!(matches!(
            assets.resolve(
                Icon::new("fixture"),
                16.0,
                1.0,
                crate::Tokens::dark().palette.text
            ),
            Ready::Text(_)
        ));
        let external = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/icon.png");
        std::fs::write(&path, format!(r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10"><image width="10" height="10" xlink:href="{}"/></svg>"#, external.display())).unwrap();
        assert!(
            matches!(
                assets.resolve(
                    Icon::new("fixture"),
                    16.0,
                    1.0,
                    crate::Tokens::dark().palette.text
                ),
                Ready::Text(_)
            ),
            "external files must not be rendered"
        );
        std::fs::write(&path, "invalid SVG").unwrap();
        assert!(matches!(
            assets.resolve(
                Icon::new("fixture"),
                16.0,
                1.0,
                crate::Tokens::dark().palette.text
            ),
            Ready::Text(_)
        ));
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(
            assets.resolve(
                Icon::new("fixture"),
                16.0,
                1.0,
                crate::Tokens::dark().palette.text
            ),
            Ready::Text(_)
        ));
        for (size, scale) in [
            (f32::NAN, 1.0),
            (16.0, f32::INFINITY),
            (-1.0, 1.0),
            (16.0, 0.0),
        ] {
            assert!(matches!(
                assets.resolve(
                    Icon::new("fixture"),
                    size,
                    scale,
                    crate::Tokens::dark().palette.text
                ),
                Ready::Text(_)
            ));
        }
        assert!(matches!(first, Handle::Rgba { .. }));
    }
    #[test]
    fn same_length_asset_change_invalidates_cached_pixels_by_mtime() {
        let (_directory, path) = fixture_svg();
        let assets = Assets::new().fallback("fixture", &path, false);
        let tint = crate::Tokens::dark().palette.text;
        let first = image(assets.resolve(Icon::new("fixture"), 16.0, 1.0, tint));
        let before = path.metadata().unwrap();
        let changed = std::fs::read_to_string(&path)
            .unwrap()
            .replace("0.5", "0.4");
        std::fs::write(&path, changed).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(before.modified().unwrap() + std::time::Duration::from_secs(2)),
            )
            .unwrap();
        assert_eq!(before.len(), path.metadata().unwrap().len());
        let second = image(assets.resolve(Icon::new("fixture"), 16.0, 1.0, tint));
        assert_ne!(first.id(), second.id());
        let Handle::Rgba { pixels, .. } = second else {
            panic!()
        };
        assert!((100..=104).contains(&pixels[3]));
    }
    #[test]
    fn limited_raster_decoder_preserves_exif_orientation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("oriented.png");
        let mut info = png::Info::with_size(2, 1);
        info.color_type = png::ColorType::Rgba;
        info.bit_depth = png::BitDepth::Eight;
        // TIFF IFD: Orientation = 6 (rotate clockwise).
        info.exif_metadata = Some(std::borrow::Cow::Owned(vec![
            73, 73, 42, 0, 8, 0, 0, 0, 1, 0, 18, 1, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
        ]));
        let tokens = crate::Tokens::dark();
        let pixels = [
            tokens.palette.primary.into_rgba8(),
            tokens.palette.text.into_rgba8(),
        ]
        .concat();
        let mut encoded = Vec::new();
        {
            let encoder = png::Encoder::with_info(&mut encoded, info).unwrap();
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&pixels).unwrap();
        }
        std::fs::write(&path, encoded).unwrap();
        let assets = Assets::new().fallback("oriented", path, false);
        let Handle::Rgba {
            width,
            height,
            pixels: decoded,
            ..
        } = image(assets.resolve(Icon::new("oriented"), 16.0, 1.0, tokens.palette.text))
        else {
            panic!()
        };
        assert_eq!((width, height), (1, 2));
        assert_eq!(decoded.as_ref(), pixels.as_slice());
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
        let Handle::Rgba { pixels, .. } = image(assets.resolve(
            Icon::new("fixture"),
            24.0,
            2.0,
            crate::Tokens::dark().palette.text,
        )) else {
            panic!()
        };
        assert_eq!(
            &pixels[..3],
            &crate::Tokens::dark().palette.text.into_rgba8()[..3]
        );
    }

    #[test]
    fn explicitly_prepared_glyphs_resolve_without_the_installed_table() {
        let icon = Icon::with_glyph("fixture-glyph", 'A', Font::DEFAULT);
        match Assets::new().resolve(icon.clone(), 16.0, 1.0, crate::Tokens::dark().palette.text) {
            Ready::Text(ready) => assert_eq!(ready.glyph(), Some(('A', Font::DEFAULT))),
            Ready::Image { .. } => panic!("a prepared glyph resolves to text"),
        }
    }

    #[test]
    fn decode_owned_scales_tints_and_charges_the_genuine_packaged_svg() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/terminal.svg");
        let bytes: Arc<[u8]> = std::fs::read(&path).unwrap().into();
        let tint = crate::Tokens::dark().palette.text.into_rgba8();
        let plain = decode_owned(Arc::clone(&bytes), ImageFormat::Svg, 48, None).unwrap();
        let tinted = decode_owned(Arc::clone(&bytes), ImageFormat::Svg, 48, Some(tint)).unwrap();
        assert_eq!(plain.dimensions(), (48, 48));
        assert_eq!(tinted.dimensions(), (48, 48));
        assert_eq!(tinted.byte_charge(), 48 * 48 * 4);
        assert!(plain.pixels().chunks_exact(4).any(|pixel| pixel[3] != 0));
        for (before, after) in plain
            .pixels()
            .chunks_exact(4)
            .zip(tinted.pixels().chunks_exact(4))
        {
            assert_eq!(&after[..3], &tint[..3]);
            assert_eq!(
                after[3],
                ((u16::from(before[3]) * u16::from(tint[3]) + 127) / 255) as u8
            );
        }
        // Deterministic for one source, side and tint.
        assert_eq!(
            tinted,
            decode_owned(Arc::clone(&bytes), ImageFormat::Svg, 48, Some(tint)).unwrap()
        );
        // Converting to a handle performs no decode; the pixels carry over.
        let Handle::Rgba {
            width,
            height,
            pixels,
            ..
        } = tinted.clone().into_handle()
        else {
            panic!("expected an RGBA handle");
        };
        assert_eq!((width, height), (48, 48));
        assert_eq!(pixels.as_ref(), tinted.pixels());
    }

    #[test]
    fn decode_owned_svg_refuses_external_data_text_and_blank_dependencies() {
        let external = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10"><image width="10" height="10" xlink:href="{}"/></svg>"#,
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/icon.png")
                .display()
        );
        assert_eq!(
            decode_owned(external.into_bytes().into(), ImageFormat::Svg, 10, None),
            Err(IconDecodeError::SvgImageDependency)
        );
        let data = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><image width="10" height="10" href="data:image/png;base64,iVBORw0KGgo="/></svg>"#;
        assert_eq!(
            decode_owned(data.as_bytes().to_vec().into(), ImageFormat::Svg, 10, None),
            Err(IconDecodeError::SvgImageDependency)
        );
        let text = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="red"/><text x="1" y="8">label</text></svg>"#;
        assert_eq!(
            decode_owned(text.as_bytes().to_vec().into(), ImageFormat::Svg, 10, None),
            Err(IconDecodeError::SvgTextDependency)
        );
        // Text hidden inside a clip path is a text dependency all the same.
        let clipped = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><defs><clipPath id="c"><text>label</text></clipPath></defs><rect width="10" height="10" fill="red" clip-path="url(#c)"/></svg>"#;
        assert_eq!(
            decode_owned(
                clipped.as_bytes().to_vec().into(),
                ImageFormat::Svg,
                10,
                None
            ),
            Err(IconDecodeError::SvgTextDependency)
        );
        assert_eq!(
            decode_owned(
                "invalid SVG".as_bytes().to_vec().into(),
                ImageFormat::Svg,
                10,
                None
            ),
            Err(IconDecodeError::SvgParse)
        );
        let blank = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="none"/></svg>"#;
        assert_eq!(
            decode_owned(blank.as_bytes().to_vec().into(), ImageFormat::Svg, 10, None),
            Err(IconDecodeError::Blank)
        );
    }

    #[test]
    fn decode_owned_raster_uses_the_packaged_png_bytes_and_enforces_caps() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/icon.png");
        let bytes: Arc<[u8]> = std::fs::read(&path).unwrap().into();
        let first = decode_owned(Arc::clone(&bytes), ImageFormat::Raster, 48, None).unwrap();
        let (width, height) = first.dimensions();
        assert!(width > 0 && height > 0);
        assert_eq!(
            first.byte_charge(),
            u64::from(width) * u64::from(height) * 4
        );
        assert!(first.pixels().chunks_exact(4).any(|pixel| pixel[3] != 0));
        // The physical side is not a raster resize: the intrinsic dimensions
        // and pixels do not change with the requested side.
        assert_eq!(
            first,
            decode_owned(Arc::clone(&bytes), ImageFormat::Raster, 24, None).unwrap()
        );
        let Handle::Rgba {
            width: w,
            height: h,
            pixels,
            ..
        } = first.clone().into_handle()
        else {
            panic!("expected an RGBA handle");
        };
        assert_eq!((w, h), (width, height));
        assert_eq!(pixels.as_ref(), first.pixels());
        // Corrupt and truncated payloads, empty input and out-of-range sizes.
        let truncated: Arc<[u8]> = bytes[..bytes.len() / 2].to_vec().into();
        assert_eq!(
            decode_owned(truncated, ImageFormat::Raster, 48, None),
            Err(IconDecodeError::RasterDecode)
        );
        assert_eq!(
            decode_owned(
                "not a raster".as_bytes().to_vec().into(),
                ImageFormat::Raster,
                48,
                None
            ),
            Err(IconDecodeError::RasterDecode)
        );
        assert_eq!(
            decode_owned(Vec::new().into(), ImageFormat::Raster, 48, None),
            Err(IconDecodeError::Empty)
        );
        assert_eq!(
            decode_owned(
                vec![0; 8 * 1024 * 1024 + 1].into(),
                ImageFormat::Raster,
                48,
                None
            ),
            Err(IconDecodeError::EncodedTooLarge {
                bytes: 8 * 1024 * 1024 + 1
            })
        );
        for side in [0u32, 2049] {
            assert_eq!(
                decode_owned(Arc::clone(&bytes), ImageFormat::Raster, side, None),
                Err(IconDecodeError::SideOutOfRange { side })
            );
        }
    }

    #[test]
    fn svg_subset_refuses_unbounded_surfaces_entities_css_and_external_use() {
        for (body, expected) in [
            (r#"<defs><filter id="f" filterUnits="userSpaceOnUse" width="1000000" height="1000000"><feGaussianBlur stdDeviation="1000"/></filter></defs><rect width="1" height="1" filter="url(#f)"/>"#, IconDecodeError::SvgFilter),
            (r#"<rect width="1" height="1" filter="url(#external)"/>"#, IconDecodeError::SvgFilter),
            (r#"<style>rect { f\69 lter: url(#f); }</style><rect width="1" height="1"/>"#, IconDecodeError::SvgStyleDependency),
            (r#"<rect width="1" height="1" style="fill:red"/>"#, IconDecodeError::SvgStyleDependency),
            (r#"<use href="file:///missing.svg#icon"/>"#, IconDecodeError::SvgUnsupported),
            (r#"<use href="data:image/svg+xml,ignored"/>"#, IconDecodeError::SvgUnsupported),
            (r#"<use href="#"/>"#, IconDecodeError::SvgUnsupported),
            (r#"<pattern id="p" width="1000000" height="1000000" patternUnits="userSpaceOnUse"><rect width="1" height="1"/></pattern>"#, IconDecodeError::SvgUnsupported),
            (r#"<g opacity="0.5"><g opacity="0.5"><rect width="1" height="1"/></g></g>"#, IconDecodeError::SvgUnsupported),
            (r#"<path d="M0 0 L1000000000 0" stroke="red" stroke-dasharray="0.01 0.01"/>"#, IconDecodeError::SvgUnsupported),
            (r#"<feImage href="data:malformed"/>"#, IconDecodeError::SvgImageDependency),
        ] {
            let svg = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="0.01" height="0.01">{body}</svg>"#);
            assert_eq!(decode_owned(svg.into_bytes().into(), ImageFormat::Svg, 2048, None), Err(expected));
        }
        let dtd = br#"<!DOCTYPE svg [<!ENTITY colour "red">]><svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><rect width="1" height="1" fill="&colour;"/></svg>"#;
        assert_eq!(decode_owned(dtd.to_vec().into(), ImageFormat::Svg, 16, None), Err(IconDecodeError::SvgParse));
        let local = br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><defs><rect id="icon" width="10" height="10" fill="red"/></defs><use href="#icon"/></svg>"##;
        assert_eq!(decode_owned(local.to_vec().into(), ImageFormat::Svg, 16, None), Err(IconDecodeError::SvgUnsupported));
        assert_eq!(decode_owned(local.to_vec().into(), ImageFormat::Svg, 16, Some([255,0,0,0])), Err(IconDecodeError::TintAlphaZero));
    }

    #[test]
    fn svg_complexity_is_refused_before_path_materialisation() {
        let svg = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><path d="{}"/></svg>"#, "M0 0 ".repeat(MAX_SVG_PATH_BYTES / 5 + 1));
        assert!(matches!(decode_owned(svg.into_bytes().into(), ImageFormat::Svg, 16, None), Err(IconDecodeError::SvgComplexity { path_bytes, .. }) if path_bytes > MAX_SVG_PATH_BYTES));
        let svg = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">{}</svg>"#, "<g/>".repeat(MAX_SVG_NODES));
        assert!(matches!(decode_owned(svg.into_bytes().into(), ImageFormat::Svg, 16, None), Err(IconDecodeError::SvgComplexity { nodes, .. }) if nodes > MAX_SVG_NODES));
    }

    #[test]
    fn every_advertised_raster_codec_decodes_and_retains_intrinsic_layout() {
        let bitmap = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(32, 16, image::Rgb([211, 79, 37])));
        for format in [image::ImageFormat::Png, image::ImageFormat::Jpeg, image::ImageFormat::Gif, image::ImageFormat::WebP, image::ImageFormat::Bmp, image::ImageFormat::Ico] {
            let mut encoded = Cursor::new(Vec::new());
            bitmap.write_to(&mut encoded, format).unwrap();
            let decoded = decode_owned(encoded.into_inner().into(), ImageFormat::Raster, 24, None).unwrap();
            assert_eq!(decoded.dimensions(), (32, 16), "{format:?}");
            assert_eq!(decoded.byte_charge(), 32 * 16 * 4);
            assert!(decoded.pixels().chunks_exact(4).any(|pixel| pixel[0] > 100 && pixel[3] != 0), "{format:?}");
            assert_eq!(image_layout(&decoded.into_handle(), 24.0), (24.0, 12.0));
        }
        assert_eq!(image_layout(&Handle::from_rgba(8, 32, vec![255;8 * 32 * 4]), 24.0), (6.0, 24.0));
    }

    #[test]
    fn decode_owned_raster_rejects_blank_pixels() {
        let mut encoded = Vec::new();
        {
            let mut info = png::Info::with_size(1, 1);
            info.color_type = png::ColorType::Rgba;
            info.bit_depth = png::BitDepth::Eight;
            let encoder = png::Encoder::with_info(&mut encoded, info).unwrap();
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[0, 0, 0, 0]).unwrap();
        }
        assert_eq!(
            decode_owned(encoded.into(), ImageFormat::Raster, 1, None),
            Err(IconDecodeError::Blank)
        );
    }
}
