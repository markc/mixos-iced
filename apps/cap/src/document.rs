// SPDX-License-Identifier: MIT OR Apache-2.0
//! Immutable source pixels, bounded editable objects, and one painter for
//! preview and export. Undo snapshots contain geometry, never raster copies.
use image::RgbaImage;
use serde::{Deserialize, Serialize};
use std::{io::Write, path::Path, sync::Arc};
use tiny_skia::{Paint, PathBuilder, Pixmap, Stroke, Transform};

pub const MAX_PIXELS: u64 = 32 * 1024 * 1024;
pub const MAX_OBJECTS: usize = 256;
const MAX_POINTS: usize = 16_384;
const MAX_HISTORY: usize = 128;
const MAX_DOCUMENT_POINTS: usize = 131_072;
const MAX_HISTORY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}
impl Point {
    pub fn valid(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.x.abs() <= 65536.0
            && self.y.abs() <= 65536.0
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Line,
    Arrow,
    Rectangle,
    Ellipse,
    Pen,
    Highlighter,
    Redact,
    Text,
    Number,
}
impl Kind {
    pub const ALL: [Self; 9] = [
        Self::Arrow,
        Self::Line,
        Self::Rectangle,
        Self::Ellipse,
        Self::Pen,
        Self::Highlighter,
        Self::Redact,
        Self::Text,
        Self::Number,
    ];
    pub fn key(self) -> &'static str {
        match self {
            Self::Line => "line",
            Self::Arrow => "arrow",
            Self::Rectangle => "rectangle",
            Self::Ellipse => "ellipse",
            Self::Pen => "pen",
            Self::Highlighter => "highlighter",
            Self::Redact => "redact",
            Self::Text => "text-tool",
            Self::Number => "number-tool",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shape {
    pub kind: Kind,
    pub points: Vec<Point>,
    pub colour: [u8; 4],
    pub width: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<u32>,
}
impl Shape {
    pub fn validate(&self) -> Result<(), String> {
        if !self.width.is_finite() || !(0.5..=128.0).contains(&self.width) {
            return Err("stroke width must be 0.5..128 pixels".into());
        }
        let count = self.points.len();
        if count < 2
            || count > MAX_POINTS
            || self.points.iter().any(|p| !p.valid())
            || (!matches!(self.kind, Kind::Pen | Kind::Highlighter) && count != 2)
        {
            return Err("invalid annotation points".into());
        }
        if matches!(self.kind, Kind::Text | Kind::Number) {
            let (l, t, r, b) = self.bounds();
            if r - l < 1.0 || b - t < 1.0 {
                return Err("text needs a non-empty box".into());
            }
            if self
                .size
                .is_none_or(|s| !s.is_finite() || !(8.0..=256.0).contains(&s))
            {
                return Err("text size must be 8..256 pixels".into());
            }
            match self.kind {
                Kind::Text
                    if self.text.as_ref().is_none_or(|t| {
                        t.trim().is_empty() || t.len() > 4096 || t.lines().count() > 16
                    }) || self.number.is_some() =>
                {
                    return Err("text must contain 1..4096 bytes and at most 16 lines".into());
                }
                Kind::Number
                    if self.number.is_none_or(|n| !(1..=9999).contains(&n))
                        || self.text.is_some() =>
                {
                    return Err("marker number must be 1..9999".into());
                }
                _ => {}
            }
        } else if self.text.is_some() || self.size.is_some() || self.number.is_some() {
            return Err("text fields only apply to text and number tools".into());
        }
        Ok(())
    }
    pub fn translated(&self, dx: f32, dy: f32) -> Self {
        let mut shape = self.clone();
        for p in &mut shape.points {
            p.x += dx;
            p.y += dy;
        }
        shape
    }
    pub fn bounds(&self) -> (f32, f32, f32, f32) {
        self.points.iter().fold(
            (
                f32::INFINITY,
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::NEG_INFINITY,
            ),
            |(l, t, r, b), p| (l.min(p.x), t.min(p.y), r.max(p.x), b.max(p.y)),
        )
    }
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Object {
    pub id: u64,
    pub shape: Shape,
}
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
struct State {
    objects: Vec<Object>,
    crop: Option<Crop>,
}
impl State {
    fn bytes(&self) -> usize {
        self.objects
            .iter()
            .map(|o| {
                std::mem::size_of::<Object>()
                    + o.shape.points.len() * std::mem::size_of::<Point>()
                    + o.shape.text.as_ref().map_or(0, String::len)
            })
            .sum()
    }
}
#[derive(Debug, Clone)]
pub struct Document {
    original: Arc<RgbaImage>,
    state: State,
    undo: Vec<State>,
    redo: Vec<State>,
    saved: State,
    next_id: u64,
}
impl Document {
    pub fn new(image: RgbaImage) -> Result<Self, String> {
        check_size(image.width(), image.height())?;
        Ok(Self {
            original: Arc::new(image),
            state: State::default(),
            undo: vec![],
            redo: vec![],
            saved: State::default(),
            next_id: 1,
        })
    }
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut reader = image::ImageReader::open(path)
            .map_err(|e| e.to_string())?
            .with_guessed_format()
            .map_err(|e| e.to_string())?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(16384);
        limits.max_image_height = Some(16384);
        limits.max_alloc = Some(MAX_PIXELS * 8);
        reader.limits(limits);
        let (w, h) = image::ImageReader::open(path)
            .map_err(|e| e.to_string())?
            .with_guessed_format()
            .map_err(|e| e.to_string())?
            .into_dimensions()
            .map_err(|e| e.to_string())?;
        check_size(w, h)?;
        Self::new(reader.decode().map_err(|e| e.to_string())?.to_rgba8())
    }
    pub fn dimensions(&self) -> (u32, u32) {
        self.original.dimensions()
    }
    pub fn output_dimensions(&self) -> (u32, u32) {
        self.state
            .crop
            .map_or(self.dimensions(), |c| (c.width, c.height))
    }
    pub fn crop(&self) -> Option<Crop> {
        self.state.crop
    }
    pub fn objects(&self) -> &[Object] {
        &self.state.objects
    }
    pub fn dirty(&self) -> bool {
        self.state != self.saved
    }
    pub fn mark_saved(&mut self) {
        self.saved = self.state.clone();
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    /// Raster workers do not need undo history or the saved checkpoint.
    pub fn raster_snapshot(&self) -> Self {
        Self {
            original: self.original.clone(),
            state: self.state.clone(),
            undo: vec![],
            redo: vec![],
            saved: State::default(),
            next_id: self.next_id,
        }
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    fn checkpoint(&mut self) {
        self.undo.push(self.state.clone());
        while self.undo.len() > MAX_HISTORY
            || self.undo.iter().map(State::bytes).sum::<usize>() > MAX_HISTORY_BYTES
        {
            self.undo.remove(0);
        }
        self.redo.clear();
    }
    pub fn add(&mut self, shape: Shape) -> Result<u64, String> {
        shape.validate()?;
        if self.state.objects.len() >= MAX_OBJECTS {
            return Err("annotation limit reached".into());
        }
        if self
            .state
            .objects
            .iter()
            .map(|o| o.shape.points.len())
            .sum::<usize>()
            + shape.points.len()
            > MAX_DOCUMENT_POINTS
        {
            return Err("document point budget exceeded".into());
        }
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or("annotation ID exhausted")?;
        self.checkpoint();
        self.state.objects.push(Object { id, shape });
        Ok(id)
    }
    pub fn delete(&mut self, id: u64) -> Result<(), String> {
        let at = self
            .state
            .objects
            .iter()
            .position(|o| o.id == id)
            .ok_or("no such annotation")?;
        self.checkpoint();
        self.state.objects.remove(at);
        Ok(())
    }
    pub fn move_object(&mut self, id: u64, dx: f32, dy: f32) -> Result<(), String> {
        let at = self
            .state
            .objects
            .iter()
            .position(|o| o.id == id)
            .ok_or("no such annotation")?;
        let moved = self.state.objects[at].shape.translated(dx, dy);
        moved.validate()?;
        if dx != 0.0 || dy != 0.0 {
            self.checkpoint();
            self.state.objects[at].shape = moved;
        }
        Ok(())
    }
    pub fn set_crop(&mut self, crop: Option<Crop>) -> Result<(), String> {
        if let Some(c) = crop {
            let (w, h) = self.dimensions();
            if c.width == 0
                || c.height == 0
                || c.x.checked_add(c.width).is_none_or(|x| x > w)
                || c.y.checked_add(c.height).is_none_or(|y| y > h)
            {
                return Err("crop outside image".into());
            }
        }
        if self.state.crop != crop {
            self.checkpoint();
            self.state.crop = crop;
        }
        Ok(())
    }
    pub fn undo(&mut self) -> bool {
        if let Some(s) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.state, s));
            true
        } else {
            false
        }
    }
    pub fn redo(&mut self) -> bool {
        if let Some(s) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.state, s));
            true
        } else {
            false
        }
    }
    pub fn hit(&self, p: Point, tolerance: f32) -> Option<u64> {
        self.state
            .objects
            .iter()
            .rev()
            .find(|o| {
                let (l, t, r, b) = o.shape.bounds();
                let margin = tolerance.max(o.shape.width / 2.0);
                p.x >= l - margin && p.x <= r + margin && p.y >= t - margin && p.y <= b + margin
            })
            .map(|o| o.id)
    }
    pub fn info(&self) -> serde_json::Value {
        serde_json::json!({"width":self.output_dimensions().0,"height":self.output_dimensions().1,"original_width":self.dimensions().0,"original_height":self.dimensions().1,"dirty":self.dirty(),"objects":self.state.objects,"crop":self.state.crop,"undo":self.undo.len(),"redo":self.redo.len()})
    }
    pub fn render(&self) -> Result<RgbaImage, String> {
        let (w, h) = self.dimensions();
        let mut pixmap = Pixmap::new(w, h).ok_or("image allocation failed")?;
        for (to, from) in pixmap
            .data_mut()
            .chunks_exact_mut(4)
            .zip(self.original.as_raw().chunks_exact(4))
        {
            for c in 0..3 {
                to[c] = ((u16::from(from[c]) * u16::from(from[3]) + 127) / 255) as u8;
            }
            to[3] = from[3];
        }
        for object in &self.state.objects {
            paint(&mut pixmap, &object.shape)?;
        }
        let mut bytes = pixmap.data().to_vec();
        for px in bytes.chunks_exact_mut(4) {
            if px[3] != 0 {
                for c in 0..3 {
                    px[c] = ((u32::from(px[c]) * 255 + u32::from(px[3]) / 2) / u32::from(px[3]))
                        .min(255) as u8;
                }
            }
        }
        let image = RgbaImage::from_raw(w, h, bytes).ok_or("image layout mismatch")?;
        Ok(if let Some(c) = self.state.crop {
            image::imageops::crop_imm(&image, c.x, c.y, c.width, c.height).to_image()
        } else {
            image
        })
    }
    /// No-clobber publication: a new sibling is encoded, synced, then linked
    /// into place atomically. Existing destinations and symlinks are refused.
    pub fn export(&self, path: &Path) -> Result<(), String> {
        use std::os::unix::fs::OpenOptionsExt;
        if !path.is_absolute() {
            return Err("export path must be absolute".into());
        }
        let image = self.render()?;
        let parent = path.parent().ok_or("export needs a parent")?;
        let temporary = parent.join(format!(".cap-{}.partial", uuid::Uuid::now_v7()));
        let result = (|| -> Result<(), String> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
                .map_err(|e| e.to_string())?;
            image::codecs::png::PngEncoder::new(&mut file)
                .write_image(
                    image.as_raw(),
                    image.width(),
                    image.height(),
                    image::ExtendedColorType::Rgba8,
                )
                .map_err(|e| e.to_string())?;
            file.flush()
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            std::fs::hard_link(&temporary, path)
                .map_err(|e| format!("publish PNG without overwrite: {e}"))?;
            Ok(())
        })();
        let _ = std::fs::remove_file(temporary);
        result
    }
}
use image::ImageEncoder;
fn check_size(w: u32, h: u32) -> Result<(), String> {
    if w == 0 || h == 0 || w > 16384 || h > 16384 || u64::from(w) * u64::from(h) > MAX_PIXELS {
        Err("image exceeds Cap pixel budget".into())
    } else {
        Ok(())
    }
}
fn paint(pixmap: &mut Pixmap, shape: &Shape) -> Result<(), String> {
    shape.validate()?;
    if matches!(shape.kind, Kind::Text | Kind::Number) {
        return paint_text(pixmap, shape);
    }
    let mut path = PathBuilder::new();
    let a = shape.points[0];
    let b = shape.points[1];
    match shape.kind {
        Kind::Rectangle | Kind::Redact => {
            let (l, t, r, b) = shape.bounds();
            if let Some(rect) = tiny_skia::Rect::from_ltrb(l, t, r, b) {
                path.push_rect(rect)
            } else {
                return Ok(());
            }
        }
        Kind::Ellipse => {
            let (l, t, r, b) = shape.bounds();
            if let Some(rect) = tiny_skia::Rect::from_ltrb(l, t, r, b) {
                path.push_oval(rect)
            } else {
                return Ok(());
            }
        }
        _ => {
            path.move_to(a.x, a.y);
            for p in &shape.points[1..] {
                path.line_to(p.x, p.y)
            }
            if shape.kind == Kind::Arrow {
                let angle = (b.y - a.y).atan2(b.x - a.x);
                let length = (shape.width * 4.0).max(12.0);
                for offset in [-0.5_f32, 0.5] {
                    path.move_to(b.x, b.y);
                    path.line_to(
                        b.x - length * (angle + offset).cos(),
                        b.y - length * (angle + offset).sin(),
                    );
                }
            }
        }
    }
    let Some(path) = path.finish() else {
        return Ok(());
    };
    let mut colour = shape.colour;
    if shape.kind == Kind::Redact {
        colour[3] = 255;
    } else if shape.kind == Kind::Highlighter {
        colour[3] = colour[3].min(80);
    }
    let mut paint = Paint::default();
    paint.set_color_rgba8(colour[0], colour[1], colour[2], colour[3]);
    paint.anti_alias = shape.kind != Kind::Redact;
    if shape.kind == Kind::Redact {
        pixmap.fill_path(
            &path,
            &paint,
            tiny_skia::FillRule::Winding,
            Transform::identity(),
            None,
        )
    } else {
        pixmap.stroke_path(
            &path,
            &paint,
            &Stroke {
                width: shape.width,
                line_cap: tiny_skia::LineCap::Round,
                line_join: tiny_skia::LineJoin::Round,
                ..Stroke::default()
            },
            Transform::identity(),
            None,
        );
    }
    Ok(())
}

/// Shape and rasterise through iced's pinned cosmic-text version. The bundled
/// OFL Inter font makes headless export independent of host fonts or a bus.
fn paint_text(pixmap: &mut Pixmap, shape: &Shape) -> Result<(), String> {
    use iced::advanced::graphics::text::cosmic_text::{
        self, Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache,
    };
    let mut db = cosmic_text::fontdb::Database::new();
    db.load_font_data(
        include_bytes!("../../../vendor/font/Inter-VariableFont_opsz,wght.ttf").to_vec(),
    );
    let mut fonts = FontSystem::new_with_locale_and_db("en-US".into(), db);
    let mut cache = SwashCache::new();
    let size = shape.size.ok_or("missing text size")?;
    let (left, top, right, bottom) = shape.bounds();
    let content = if shape.kind == Kind::Number {
        shape.number.ok_or("missing marker number")?.to_string()
    } else {
        shape.text.clone().ok_or("missing text")?
    };
    let mut buffer = Buffer::new(&mut fonts, Metrics::new(size, size * 1.25));
    buffer.set_size(Some(right - left), Some(bottom - top));
    buffer.set_text(
        &content,
        &Attrs::new().family(Family::Name("Inter")),
        Shaping::Advanced,
        None,
    );
    let [r, g, b, a] = shape.colour;
    buffer.draw(
        &mut fonts,
        &mut cache,
        cosmic_text::Color::rgba(r, g, b, a),
        |x, y, w, h, colour| {
            let x = left.floor() as i32 + x;
            let y = top.floor() as i32 + y;
            // Clip to the object's box as well as the image. This keeps text hit
            // bounds, selection and export geometry identical.
            let clip_l = (left.floor() as i32).max(0);
            let clip_t = (top.floor() as i32).max(0);
            let clip_r = (right.ceil() as i32).min(pixmap.width() as i32);
            let clip_b = (bottom.ceil() as i32).min(pixmap.height() as i32);
            let [r, g, b, a] = colour.as_rgba();
            let width = pixmap.width() as usize;
            for yy in y.max(clip_t)..(y + h as i32).min(clip_b) {
                for xx in x.max(clip_l)..(x + w as i32).min(clip_r) {
                    let at = (yy as usize * width + xx as usize) * 4;
                    let px = &mut pixmap.data_mut()[at..at + 4];
                    for (to, from) in px.iter_mut().take(3).zip([r, g, b]) {
                        *to = ((u16::from(from) * u16::from(a)
                            + u16::from(*to) * (255 - u16::from(a))
                            + 127)
                            / 255) as u8;
                    }
                    px[3] = (u16::from(a) + (u16::from(px[3]) * (255 - u16::from(a)) + 127) / 255)
                        as u8;
                }
            }
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;
    fn doc() -> Document {
        Document::new(RgbaImage::from_pixel(32, 24, Rgba([255, 255, 255, 255]))).unwrap()
    }
    fn shape(kind: Kind) -> Shape {
        Shape {
            kind,
            points: vec![Point { x: 4.0, y: 4.0 }, Point { x: 20.0, y: 16.0 }],
            colour: [255, 0, 0, 255],
            width: 3.0,
            text: if kind == Kind::Text {
                Some("A".into())
            } else {
                None
            },
            size: if matches!(kind, Kind::Text | Kind::Number) {
                Some(8.0)
            } else {
                None
            },
            number: if kind == Kind::Number { Some(1) } else { None },
        }
    }
    #[test]
    fn gestures_undo_without_mutating_original() {
        let mut d = doc();
        let original = d.original.clone();
        let id = d.add(shape(Kind::Arrow)).unwrap();
        d.move_object(id, 2.0, 3.0).unwrap();
        assert_eq!(d.undo.len(), 2);
        assert!(Arc::ptr_eq(&original, &d.original));
        d.undo();
        assert_eq!(d.objects()[0].shape.points[0], Point { x: 4.0, y: 4.0 });
        d.undo();
        assert!(!d.dirty());
        d.redo();
        assert_eq!(d.objects()[0].id, id);
        d.mark_saved();
        assert!(!d.dirty());
        d.delete(id).unwrap();
        assert!(d.dirty());
        d.undo();
        assert!(!d.dirty());
    }
    #[test]
    fn crop_is_nondestructive_and_export_is_no_clobber() {
        let mut d = doc();
        d.add(shape(Kind::Redact)).unwrap();
        d.set_crop(Some(Crop {
            x: 4,
            y: 4,
            width: 12,
            height: 8,
        }))
        .unwrap();
        assert_eq!(d.render().unwrap().dimensions(), (12, 8));
        assert_eq!(d.dimensions(), (32, 24));
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("shot.png");
        d.export(&p).unwrap();
        let before = std::fs::read(&p).unwrap();
        assert!(d.export(&p).is_err());
        assert_eq!(before, std::fs::read(&p).unwrap());
        assert_eq!(image::open(p).unwrap().to_rgba8(), d.render().unwrap());
        d.undo();
        assert_eq!(d.output_dimensions(), (32, 24));
    }
    #[test]
    fn redaction_is_opaque_even_with_translucent_colour() {
        let mut d = doc();
        let mut s = shape(Kind::Redact);
        s.colour = [0, 0, 0, 1];
        d.add(s).unwrap();
        assert_eq!(*d.render().unwrap().get_pixel(10, 10), Rgba([0, 0, 0, 255]));
    }
    #[test]
    fn malformed_and_oversized_operations_do_not_change_history() {
        let mut d = doc();
        let mut s = shape(Kind::Pen);
        s.points[0].x = f32::NAN;
        assert!(d.add(s).is_err());
        assert!(
            d.set_crop(Some(Crop {
                x: u32::MAX,
                y: 0,
                width: 2,
                height: 2
            }))
            .is_err()
        );
        assert!(!d.can_undo());
        for _ in 0..MAX_OBJECTS {
            d.add(shape(Kind::Line)).unwrap();
        }
        assert!(d.add(shape(Kind::Line)).is_err());
        assert_eq!(d.undo.len(), MAX_HISTORY);
    }
    #[test]
    fn rendering_covers_all_initial_tools_and_new_edits_clear_redo() {
        for kind in Kind::ALL {
            let mut d = doc();
            d.add(shape(kind)).unwrap();
            assert_ne!(d.render().unwrap(), *d.original);
            d.undo();
            assert!(d.can_redo());
            d.add(shape(Kind::Rectangle)).unwrap();
            assert!(!d.can_redo());
        }
    }
}
