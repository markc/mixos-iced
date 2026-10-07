// SPDX-License-Identifier: MIT OR Apache-2.0
//! Owned font policy to validated renderer candidate: the managed resource
//! route into [`Raster`].
//!
//! The settings resource worker assembles an [`OwnedFontPolicy`] from the
//! toolkit registry's immutable owned selection and prepares it here. The
//! types are neutral to term-core on purpose: exact source bytes, face
//! indices, ordered declared fallback groups and the effective weight, with
//! no toolkit dependency and no discovery. Everything fallible — parsing,
//! metric validation, primary selection, cell geometry — happens in
//! [`PreparedRaster::prepare`]; [`PreparedRaster::activate`] only constructs
//! infallible empty render caches around the validated candidate on the
//! render thread. [`Raster::from_owned`] is the direct form for callers that
//! live on the same thread.
use std::sync::Arc;

use swash::{FontRef, NormalizedCoord, tag_from_bytes};

use super::unicode::{Face, Fonts};
use crate::config::Cursor;

/// Exact source bytes and face index for one declared candidate face.
///
/// Clones share the source allocation; a candidate never re-reads a file.
#[derive(Clone, Debug)]
pub struct OwnedFace {
    pub bytes: Arc<[u8]>,
    pub index: u32,
}

/// Declared, ordered candidate groups plus the exact effective weight.
///
/// `groups[0]` holds the primary family's candidates; later groups are
/// ordered Unicode fallback families. Within a group, faces are in
/// preference order. The first face of the first group must be a usable
/// terminal primary; every other valid face stays a declared fallback
/// candidate in declared order. Later groups never replace the primary
/// chosen by the resource registry. Nothing is discovered or fetched on this
/// route: not `TERM_SPIKE_FONT`, not an installed role, not a system face.
#[derive(Clone, Debug)]
pub struct OwnedFontPolicy {
    pub groups: Vec<Vec<OwnedFace>>,
    pub weight: u16,
}

/// Bounds the managed route re-checks even though the registry already
/// budgeted its candidates.
pub(super) const MAX_GROUPS: usize = 16;
pub(super) const MAX_FACES: usize = 64;
pub(super) const MAX_FACE_BYTES: usize = 32 * 1024 * 1024;
pub(super) const MAX_TOTAL_BYTES: usize = 128 * 1024 * 1024;

/// Validate the whole policy and build one `Face` per declared candidate, in
/// declared order, each pinned to its own normalised coordinates for the
/// exact policy weight. Every malformed, out-of-range or unreadable input is
/// reported here, at the public boundary.
fn validate(policy: &OwnedFontPolicy) -> Result<Vec<Face>, String> {
    if !(1..=1000).contains(&policy.weight) {
        return Err(format!("invalid owned font policy weight {}", policy.weight));
    }
    if policy.groups.is_empty() {
        return Err("owned font policy has no groups".into());
    }
    if policy.groups.len() > MAX_GROUPS {
        return Err(format!(
            "owned font policy has {} groups (limit {MAX_GROUPS})",
            policy.groups.len()
        ));
    }
    let count: usize = policy.groups.iter().map(Vec::len).sum();
    if count == 0 {
        return Err("owned font policy has no faces".into());
    }
    if count > MAX_FACES {
        return Err(format!(
            "owned font policy has {count} faces (limit {MAX_FACES})"
        ));
    }
    let mut total = 0usize;
    let mut sources: Vec<&Arc<[u8]>> = Vec::new();
    let mut faces = Vec::with_capacity(count);
    for (group_index, group) in policy.groups.iter().enumerate() {
        if group.is_empty() {
            return Err(format!("owned font policy group {group_index} is empty"));
        }
        for (face_index, face) in group.iter().enumerate() {
            let at = format!("group {group_index} face {face_index}");
            if face.bytes.is_empty() {
                return Err(format!("{at}: empty source bytes"));
            }
            if face.bytes.len() > MAX_FACE_BYTES {
                return Err(format!(
                    "{at}: source is {} bytes (limit {MAX_FACE_BYTES})",
                    face.bytes.len()
                ));
            }
            if !sources.iter().any(|source| Arc::ptr_eq(source, &face.bytes)) {
                total = total.saturating_add(face.bytes.len());
                sources.push(&face.bytes);
            }
            if total > MAX_TOTAL_BYTES {
                return Err(format!(
                    "owned font policy exceeds {MAX_TOTAL_BYTES} total source bytes"
                ));
            }
            let font = FontRef::from_index(&face.bytes, face.index as usize)
                .ok_or_else(|| format!("{at}: unreadable font at index {}", face.index))?;
            let supported = match font.variations().find_by_tag(tag_from_bytes(b"wght")) {
                Some(axis) => {
                    let (min, max) = (axis.min_value(), axis.max_value());
                    min.is_finite() && max.is_finite() && min <= max
                        && (min..=max).contains(&f32::from(policy.weight))
                }
                None => font.attributes().weight().0 == policy.weight,
            };
            if !supported {
                return Err(format!("{at}: face cannot honour effective weight {}", policy.weight));
            }
            let candidate =
                Face::with_weight(face.bytes.clone(), face.index, policy.weight)
                    .filter(|candidate| super::primary_font::metrics_readable(candidate.font()))
                    .ok_or_else(|| {
                        format!("{at}: unreadable or malformed font at index {}", face.index)
                    })?;
            faces.push(candidate);
        }
    }
    Ok(faces)
}

/// All printable ASCII must be present with finite, equal advances: the
/// terminal's ASCII fast path cannot ask Unicode fallback for missing glyphs.
fn monospaced(font: FontRef<'_>, coords: &[NormalizedCoord]) -> bool {
    let metrics = font.glyph_metrics(coords);
    let charmap = font.charmap();
    let glyph = charmap.map('M');
    if glyph == 0 { return false; }
    let first = metrics.advance_width(glyph);
    first.is_finite() && first > 0.0 && (' '..='~').all(|c| {
        let glyph = charmap.map(c);
        let advance = metrics.advance_width(glyph);
        glyph != 0 && advance.is_finite() && (advance - first).abs() < 0.5
    })
}

/// Validated primary plus ordered declared coverage, sealed against the
/// global fallback OnceLock.
fn build_fonts(policy: &OwnedFontPolicy) -> Result<Arc<Fonts>, String> {
    let mut faces = validate(policy)?;
    if !monospaced(faces[0].font(), &faces[0].variations) {
        return Err("unsupported terminal font: selected primary lacks monospaced ASCII metrics".into());
    }
    let primary = faces.remove(0);
    Ok(Fonts::owned(primary, faces))
}

/// Cell geometry from the primary face at its exact normalised coordinates.
/// Shared by the legacy constructor, the owned route and every resize, so
/// the arithmetic exists once.
pub(super) fn geometry(
    font: FontRef<'_>,
    coords: &[NormalizedCoord],
    scale: f32,
    logical_px: f32,
) -> Result<(u32, u32, i32, f32, f32), String> {
    if !scale.is_finite() || !(0.5..=8.0).contains(&scale)
        || !logical_px.is_finite() || logical_px <= 0.0 {
        return Err("invalid font scale or logical size".into());
    }
    // Startup resolves logical size once; scale makes it physical for HiDPI.
    let px = logical_px * scale;
    if !px.is_finite() { return Err("invalid physical font size".into()); }
    let metrics = font.metrics(coords).scale(px);
    let glyph = font.charmap().map('M');
    if glyph == 0 { return Err("primary font lacks the cell measurement glyph".into()); }
    let advance = font
        .glyph_metrics(coords)
        .scale(px)
        .advance_width(glyph);
    let extent = metrics.ascent + metrics.descent.abs() + metrics.leading;
    if !advance.is_finite() || advance <= 0.0
        || !metrics.ascent.is_finite() || !metrics.descent.is_finite()
        || !metrics.leading.is_finite() || !extent.is_finite() || extent <= 0.0 {
        return Err("invalid font cell metrics".into());
    }
    let width = advance.ceil();
    let height = extent.ceil();
    let baseline = metrics.ascent.ceil();
    // Physical-pixel cells scale with the display; allow generous HiDPI room.
    if width > 512.0 || height > 1024.0 || !(0.0..=height).contains(&baseline) {
        return Err("font metrics exceed cell limits".into());
    }
    Ok((width as u32, height as u32, baseline as i32, px, scale))
}

/// Immutable, `Send` candidate built on the settings side of the worker
/// boundary. All parsing, validation, face selection and metrics computation
/// happen in [`PreparedRaster::prepare`], so activation cannot fail and no
/// fallible parser or discovery call runs in the UI activation closure.
#[derive(Clone)]
pub struct PreparedRaster {
    fonts: Arc<Fonts>,
    weight: u16,
    scale: f32,
    px: f32,
    logical_px: f32,
    cursor: Cursor,
    width: u32,
    height: u32,
    baseline: i32,
}

impl PreparedRaster {
    /// Validate the policy, select the primary and compute the cell metrics.
    /// Every failure mode of the managed route is reported here.
    pub fn prepare(
        policy: OwnedFontPolicy,
        scale: f32,
        logical_px: f32,
        cursor: Cursor,
    ) -> Result<Self, String> {
        let weight = policy.weight;
        let fonts = build_fonts(&policy)?;
        let (width, height, baseline, px, scale) = geometry(
            fonts.primary.font(),
            &fonts.primary.variations,
            scale,
            logical_px,
        )?;
        Ok(Self {
            fonts,
            weight,
            scale,
            px,
            logical_px,
            cursor,
            width,
            height,
            baseline,
        })
    }

    /// Construct the render-thread state. Cannot fail: the candidate was
    /// fully validated and measured in [`Self::prepare`].
    pub fn activate(self) -> super::Raster {
        let data = self.fonts.primary.data.clone();
        super::Raster {
            identity: Arc::new(()),
            cursor: self.cursor,
            data,
            context: swash::scale::ScaleContext::new(),
            cache: Default::default(),
            unicode: super::UnicodeRaster::new(self.fonts),
            width: self.width,
            height: self.height,
            scale: self.scale,
            px: self.px,
            baseline: self.baseline,
            weight: self.weight,
        }
    }

    /// The same validated policy and coordinates at another size: the source
    /// bytes and declared candidates are shared, never re-read or re-selected.
    pub fn resized(&self, scale: f32, logical_px: f32) -> Result<Self, String> {
        let (width, height, baseline, px, scale) = geometry(
            self.fonts.primary.font(),
            &self.fonts.primary.variations,
            scale,
            logical_px,
        )?;
        Ok(Self {
            fonts: self.fonts.clone(),
            weight: self.weight,
            scale,
            px,
            logical_px,
            cursor: self.cursor,
            width,
            height,
            baseline,
        })
    }

    /// The exact effective weight the candidate was prepared for.
    pub fn weight(&self) -> u16 {
        self.weight
    }
    pub fn scale(&self) -> f32 {
        self.scale
    }
    pub fn logical_px(&self) -> f32 {
        self.logical_px
    }
    pub fn cursor(&self) -> Cursor {
        self.cursor
    }
    /// Cell dimensions in physical device pixels, as validated at prepare.
    pub fn cell(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    pub fn baseline(&self) -> i32 {
        self.baseline
    }
}
