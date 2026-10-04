//! The chrome's pixels, rasterised on the CPU into premultiplied images: the
//! titlebar band (rounded top corners, fill, divider, border, the caption
//! buttons and their glyphs, the title) and the shadow's 9-slice tile.
//!
//! Every shape is a signed-distance field evaluated at the pixel centre and
//! turned into coverage over one pixel, so edges are anti-aliased at any scale.
//! This is the frame shader's maths run once per state change instead of once
//! per frame: chrome repaints only when what it shows changes.

use crate::layout::{ButtonShape, CaptionButton, ChromeLayout, DecoTheme, GlyphPolicy, Rect, Srgba};

use crate::state::ChromeState;
use crate::text::TextMask;

/// A premultiplied RGBA image, row-major from the top-left.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<[f32; 4]>,
}

impl Image {
    pub fn new(width: u32, height: u32) -> Image {
        Image {
            width,
            height,
            pixels: vec![[0.0; 4]; width as usize * height as usize],
        }
    }

    pub fn at(&self, x: u32, y: u32) -> [f32; 4] {
        self.pixels[(y * self.width + x) as usize]
    }

    /// `src` over pixel `(x, y)` (both premultiplied).
    fn over(&mut self, x: u32, y: u32, src: [f32; 4]) {
        let dst = &mut self.pixels[(y * self.width + x) as usize];
        let k = 1.0 - src[3];
        for c in 0..4 {
            dst[c] = src[c] + dst[c] * k;
        }
    }

    /// The pixels as `ARGB8888` (B, G, R, A in memory), premultiplied: what a
    /// Wayland shm buffer of that format holds.
    pub fn to_argb8888(&self) -> Vec<u8> {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        let mut out = Vec::with_capacity(self.pixels.len() * 4);
        for p in &self.pixels {
            out.extend_from_slice(&[q(p[2]), q(p[1]), q(p[0]), q(p[3])]);
        }
        out
    }
}

/// Straight-alpha sRGB to premultiplied, scaled by coverage `k`.
fn premul(c: Srgba, k: f32) -> [f32; 4] {
    let a = c.a * k;
    [c.r * a, c.g * a, c.b * a, a]
}

/// Coverage of a shape with signed distance `d` (pixels, negative inside).
fn coverage(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

/// Signed distance to a box at `origin`/`size` whose top corners are rounded
/// by `top` and bottom corners by `bottom`.
fn rounded_box(p: (f32, f32), origin: (f32, f32), size: (f32, f32), top: f32, bottom: f32) -> f32 {
    let (hw, hh) = (size.0 * 0.5, size.1 * 0.5);
    let (qx, qy) = (p.0 - origin.0 - hw, p.1 - origin.1 - hh);
    let r = if qy < 0.0 { top } else { bottom }.min(hw).min(hh).max(0.0);
    let (dx, dy) = (qx.abs() - hw + r, qy.abs() - hh + r);
    let outside = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt();
    outside + dx.max(dy).min(0.0) - r
}

fn segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (pax, pay) = (p.0 - a.0, p.1 - a.1);
    let (bax, bay) = (b.0 - a.0, b.1 - a.1);
    let h = ((pax * bax + pay * bay) / (bax * bax + bay * bay)).clamp(0.0, 1.0);
    ((pax - bax * h).powi(2) + (pay - bay * h).powi(2)).sqrt()
}

/// Signed distance to a caption button's shape (physical px).
fn button_shape(p: (f32, f32), rect: Rect, shape: ButtonShape, scale: f32) -> f32 {
    let c = (
        (rect.x + rect.w * 0.5) * scale,
        (rect.y + rect.h * 0.5) * scale,
    );
    match shape {
        ButtonShape::Circle { .. } => {
            let r = rect.w.min(rect.h) * 0.5 * scale;
            ((p.0 - c.0).powi(2) + (p.1 - c.1).powi(2)).sqrt() - r
        }
        ButtonShape::FullHeightRect { .. } => {
            let (hw, hh) = (rect.w * 0.5 * scale, rect.h * 0.5 * scale);
            let (dx, dy) = ((p.0 - c.0).abs() - hw, (p.1 - c.1).abs() - hh);
            (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() + dx.max(dy).min(0.0)
        }
    }
}

/// Distance to a button's glyph strokes: × close, □ maximise, – minimise.
fn glyph(p: (f32, f32), rect: Rect, button: CaptionButton, ratio: f32, scale: f32) -> f32 {
    let c = (
        (rect.x + rect.w * 0.5) * scale,
        (rect.y + rect.h * 0.5) * scale,
    );
    let e = rect.w.min(rect.h) * ratio * 0.5 * scale;
    let q = (p.0 - c.0, p.1 - c.1);
    match button {
        CaptionButton::Close => segment(q, (-e, -e), (e, e)).min(segment(q, (-e, e), (e, -e))),
        CaptionButton::Maximize => {
            // The outline of a square: |signed distance to the box|.
            let (dx, dy) = (q.0.abs() - e, q.1.abs() - e);
            let d = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() + dx.max(dy).min(0.0);
            d.abs()
        }
        CaptionButton::Minimize => segment(q, (-e, 0.0), (e, 0.0)),
    }
}

/// Where the title mask goes inside the titlebar (physical px, top-left).
pub fn title_origin(
    theme: &DecoTheme,
    layout: &ChromeLayout,
    mask: &TextMask,
    scale: f32,
) -> (i32, i32) {
    let slot = layout.title_slot;
    let x = match theme.metrics.title_align {
        crate::layout::TitleAlign::Leading => slot.x * scale,
        crate::layout::TitleAlign::Center => {
            // Centred on the WINDOW (mac), clamped into the free slot.
            let centred = (layout.window.w * scale - mask.width as f32) * 0.5;
            centred.clamp(
                slot.x * scale,
                ((slot.x + slot.w) * scale - mask.width as f32).max(slot.x * scale),
            )
        }
    };
    let y = (layout.titlebar.y + layout.titlebar.h * 0.5) * scale - mask.height as f32 * 0.5;
    (x.round() as i32, y.round() as i32)
}

/// The titlebar band: the window's full width, from its top edge to the
/// titlebar's bottom, at `scale` physical px per logical px.
pub fn titlebar(
    theme: &DecoTheme,
    layout: &ChromeLayout,
    state: &ChromeState,
    scale: f32,
    title: Option<&TextMask>,
) -> Image {
    let band_h = layout.titlebar.y + layout.titlebar.h;
    let (w, h) = (
        (layout.window.w * scale).round().max(1.0) as u32,
        (band_h * scale).round().max(1.0) as u32,
    );
    let mut img = Image::new(w, h);
    let focus = state.focus();
    let radius = if state.maximized {
        0.0
    } else {
        theme.metrics.corner_radius * scale
    };
    let border = theme.metrics.border_thickness * scale;
    let window_size = (layout.window.w * scale, layout.window.h * scale);
    let fill = theme.titlebar_fill(focus);
    let divider = theme.colors.titlebar_divider;
    let border_colour = theme.border(focus);
    let cluster = &theme.buttons;
    let show_glyphs = match cluster.glyphs {
        GlyphPolicy::Always => true,
        GlyphPolicy::ClusterHover => state.cluster_hovered,
    };
    let titlebar_bottom = band_h * scale;
    let stroke = (scale * 1.1).max(1.0);

    for y in 0..h {
        for x in 0..w {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            // The band never reaches the frame's bottom corners: the content clip
            // and the border's corner arcs round those (`clip`, `bottom_corners`).
            let sd = rounded_box(p, (0.0, 0.0), window_size, radius, 0.0);
            let shape = coverage(sd);
            if shape <= 0.0 {
                continue;
            }
            let mut px = premul(fill, 1.0);
            if divider.a > 0.0 && p.1 >= titlebar_bottom - scale.max(1.0) {
                let d = premul(divider, 1.0);
                px = over(px, d);
            }
            for &(button, rect) in &layout.buttons {
                let colours = cluster.colors(button);
                let bstate = state.button_state(button);
                let cov = coverage(button_shape(p, rect, cluster.shape, scale));
                if cov > 0.0 {
                    px = over(px, premul(colours.fill(bstate, focus), cov));
                }
                if show_glyphs {
                    let g = coverage(
                        glyph(p, rect, button, cluster.glyph_extent_ratio, scale) - stroke * 0.5,
                    );
                    if g > 0.0 {
                        let colour = match bstate {
                            crate::layout::ButtonState::Idle => colours.glyph,
                            _ => colours.glyph_hover,
                        };
                        px = over(px, premul(colour, g));
                    }
                }
            }
            if border > 0.0 && border_colour.a > 0.0 {
                // The ring inside the silhouette, `border` px deep.
                let ring = (1.0 - coverage(sd + border)).clamp(0.0, 1.0);
                if ring > 0.0 {
                    px = over(px, premul(border_colour, ring));
                }
            }
            img.pixels[(y * w + x) as usize] = px.map(|c| c * shape);
        }
    }

    if let Some(mask) = title {
        let (ox, oy) = title_origin(theme, layout, mask, scale);
        let colour = theme.title_text(focus);
        let slot_end = ((layout.title_slot.x + layout.title_slot.w) * scale).floor() as i32;
        for my in 0..mask.height {
            for mx in 0..mask.width {
                let a = mask.at(mx, my);
                let (x, y) = (ox + mx as i32, oy + my as i32);
                if a == 0 || x < 0 || y < 0 || x >= w as i32 || y >= h as i32 || x >= slot_end {
                    continue;
                }
                img.over(x as u32, y as u32, premul(colour, a as f32 / 255.0));
            }
        }
    }
    img
}

/// The border's two bottom corners, side by side in one tile: a ring
/// `thickness` px deep around the bottom of a box with corners of `radius` px.
/// The tile is `2r + 1` wide and `r` tall (`r` = `radius` rounded up): the
/// bottom-left corner is columns `0..r`, the bottom-right `r + 1..2r + 1`.
pub fn bottom_corners(colour: Srgba, radius: f32, thickness: f32) -> Image {
    let r = radius.ceil().max(1.0) as u32;
    let (w, h) = (2 * r + 1, r);
    let mut img = Image::new(w, h);
    // The box's bottom edge is the tile's; it reaches r px above the tile.
    let origin = (0.0, -(r as f32));
    let size = (w as f32, 2.0 * r as f32);
    for y in 0..h {
        for x in 0..w {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            let sd = rounded_box(p, origin, size, radius, radius);
            let ring = coverage(sd) * (1.0 - coverage(sd + thickness));
            img.pixels[(y * w + x) as usize] = premul(colour, ring.clamp(0.0, 1.0));
        }
    }
    img
}

fn over(dst: [f32; 4], src: [f32; 4]) -> [f32; 4] {
    let k = 1.0 - src[3];
    [
        src[0] + dst[0] * k,
        src[1] + dst[1] * k,
        src[2] + dst[2] * k,
        src[3] + dst[3] * k,
    ]
}

/// The shadow 9-slice: one tile holding a rounded box of radius `radius`
/// (physical px) with `softness` px of falloff on every side, so its four
/// corners are the shadow's corners and its middle row/column stretch into the
/// edges and centre. `corner` is the size of each corner cell.
#[derive(Clone, Debug, PartialEq)]
pub struct ShadowTile {
    pub image: Image,
    /// Edge length of each corner cell (physical px); the middle cell is 1 px.
    pub corner: u32,
}

/// The shadow tile for `colour` at `alpha`, `radius` and `softness` (both
/// physical px). Falloff: full inside the box, smoothstep to nothing over
/// `softness` outside it.
pub fn shadow_tile(colour: Srgba, alpha: f32, radius: f32, softness: f32) -> ShadowTile {
    let corner = (softness + radius).ceil().max(1.0) as u32;
    let size = corner * 2 + 1;
    let mut image = Image::new(size, size);
    // The box is the tile inset by `softness` on every side (its origin, not
    // `corner - softness`, which is `radius` and pushed the box off-centre).
    let inner = (softness, softness);
    let box_size = (size as f32 - 2.0 * softness, size as f32 - 2.0 * softness);
    for y in 0..size {
        for x in 0..size {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            let d = rounded_box(p, inner, box_size, radius, radius);
            let t = if softness <= 0.0 {
                coverage(d)
            } else {
                let s = (d / softness).clamp(0.0, 1.0);
                1.0 - s * s * (3.0 - 2.0 * s)
            };
            image.pixels[(y * size + x) as usize] = premul(colour, alpha * t);
        }
    }
    ShadowTile { image, corner }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{ChromeStyle, Mode, Scheme, presets, vec2};

    fn mac() -> DecoTheme {
        presets::resolve(ChromeStyle::Mac, Scheme::Ocean, Mode::Light)
    }

    fn close(a: [f32; 4], b: [f32; 4]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.02)
    }

    fn state() -> ChromeState {
        ChromeState {
            focused: true,
            content_size: (400, 300),
            ..Default::default()
        }
    }

    #[test]
    fn the_band_is_the_window_width_by_the_titlebar_height_at_scale() {
        let theme = mac();
        let s = state();
        let layout = s.layout(&theme);
        let img = titlebar(&theme, &layout, &s, 2.0, None);
        assert_eq!(img.width, (layout.window.w * 2.0).round() as u32);
        assert_eq!(
            img.height,
            ((layout.titlebar.y + layout.titlebar.h) * 2.0).round() as u32
        );
    }

    #[test]
    fn the_titlebar_is_its_fill_and_the_top_corners_are_cut() {
        let theme = mac();
        let s = state();
        let layout = s.layout(&theme);
        let img = titlebar(&theme, &layout, &s, 1.0, None);
        // A point mid-band, clear of buttons and title: the focused fill.
        let mid = img.at(img.width / 2, img.height / 2);
        assert!(
            close(mid, premul(theme.colors.titlebar_focused, 1.0)),
            "{mid:?}"
        );
        // The very corner pixel is outside the rounded silhouette.
        assert_eq!(img.at(0, 0)[3], 0.0);
        // Maximised: square corners.
        let mut max = s.clone();
        max.maximized = true;
        let square = titlebar(&theme, &layout, &max, 1.0, None);
        assert!(square.at(0, 0)[3] > 0.9);
    }

    #[test]
    fn focus_changes_the_fill() {
        let theme = mac();
        let mut s = state();
        let layout = s.layout(&theme);
        s.focused = false;
        let img = titlebar(&theme, &layout, &s, 1.0, None);
        let mid = img.at(img.width / 2, img.height / 2);
        assert!(
            close(mid, premul(theme.colors.titlebar_unfocused, 1.0)),
            "{mid:?}"
        );
    }

    #[test]
    fn buttons_take_their_fill_and_hover_their_hover_fill() {
        let theme = presets::resolve(ChromeStyle::Win11, Scheme::Ocean, Mode::Light);
        let mut s = state();
        let layout = s.layout(&theme);
        let (close_button, rect) = *layout
            .buttons
            .iter()
            .find(|(b, _)| *b == CaptionButton::Close)
            .unwrap();
        let c = rect.center();
        // Off-centre, away from the × strokes.
        let probe = (c.x as u32, (rect.y + 2.0) as u32);
        s.hovered = Some(close_button);
        let img = titlebar(&theme, &layout, &s, 1.0, None);
        let hover = theme.buttons.close.fill_hover;
        let expect = over(
            premul(theme.colors.titlebar_focused, 1.0),
            premul(hover, 1.0),
        );
        assert!(
            close(img.at(probe.0, probe.1), expect),
            "{:?}",
            img.at(probe.0, probe.1)
        );
    }

    #[test]
    fn the_title_lands_in_its_slot_in_the_title_colour() {
        let theme = presets::resolve(ChromeStyle::Win11, Scheme::Ocean, Mode::Light);
        let s = state();
        let layout = s.layout(&theme);
        let mask = TextMask {
            width: 4,
            height: 2,
            alpha: vec![255; 8],
        };
        let img = titlebar(&theme, &layout, &s, 1.0, Some(&mask));
        let (ox, oy) = title_origin(&theme, &layout, &mask, 1.0);
        assert!(ox as f32 >= layout.title_slot.x);
        let px = img.at(ox as u32, oy as u32);
        assert!(
            close(px, premul(theme.colors.title_text_focused, 1.0)),
            "{px:?}"
        );
    }

    #[test]
    fn the_shadow_tile_is_dense_inside_and_fades_to_nothing_outside() {
        let tile = shadow_tile(Srgba::new(0.0, 0.0, 0.0, 1.0), 0.5, 4.0, 10.0);
        let n = tile.image.width;
        assert_eq!(n, tile.corner * 2 + 1);
        assert!(
            (tile.image.at(n / 2, n / 2)[3] - 0.5).abs() < 1e-3,
            "full inside"
        );
        assert!(tile.image.at(0, 0)[3] < 1e-3, "nothing at the outer corner");
        // Monotonic falloff from the box edge outward along the middle row.
        let row: Vec<f32> = (0..tile.corner)
            .map(|x| tile.image.at(x, n / 2)[3])
            .collect();
        assert!(row.windows(2).all(|w| w[0] <= w[1] + 1e-6), "{row:?}");
    }

    #[test]
    fn the_bottom_corners_are_a_ring_on_the_arc_and_clear_outside_it() {
        let tile = bottom_corners(Srgba::new(1.0, 0.0, 0.0, 1.0), 8.0, 1.0);
        assert_eq!((tile.width, tile.height), (17, 8));
        // The very corner is outside the arc; the bottom row's inner end is ring.
        assert_eq!(tile.at(0, 7)[3], 0.0);
        assert!(tile.at(7, 7)[3] > 0.9, "{:?}", tile.at(7, 7));
        assert!(
            tile.at(0, 0)[3] > 0.9,
            "the left edge at the top of the tile"
        );
        // Inside the ring: clear.
        assert_eq!(tile.at(6, 2)[3], 0.0);
        // Mirrored on the right.
        assert_eq!(tile.at(16, 7)[3], 0.0);
        assert!(tile.at(16, 0)[3] > 0.9);
    }

    #[test]
    fn argb8888_is_bgra_in_memory_and_premultiplied() {
        let mut img = Image::new(1, 1);
        img.pixels[0] = premul(Srgba::new(1.0, 0.5, 0.0, 0.5), 1.0);
        assert_eq!(img.to_argb8888(), vec![0, 64, 128, 128]);
        let _ = vec2(0.0, 0.0);
    }
}
