//! A window's chrome as scene elements: the rasterised titlebar band, the
//! border strips below it, and the 9-slice shadow behind everything.
//!
//! Each window keeps its chrome in its user data ([`elements`] builds it on
//! first use): the state, the last images, and stable element ids. The images
//! are re-rasterised only when the state, the theme generation or the scale
//! changes; otherwise the same textures and commits come back, so the damage
//! tracker sees nothing new. That is the whole idle story: no timer, no tick.

use std::cell::RefCell;

use crate::layout::{CaptionButton, ChromeLayout, DecoExtents};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::backend::renderer::{ImportMem, Renderer};
use smithay::desktop::Window;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Physical, Point, Rectangle, Scale, Size, Transform};

use crate::raster;
use crate::state::{Chrome, ChromeState};
use crate::theme::ChromeTheme;

/// A rasterised chrome texture (or a slice of one) placed at a fixed physical
/// rectangle. The geometry is the chrome's own decision, whatever scale the
/// damage tracker asks with; the commit moves when the image is redrawn.
pub struct Piece<R: Renderer> {
    id: Id,
    commit: CommitCounter,
    inner: MemoryRenderBufferRenderElement<R>,
    src: Rectangle<f64, Buffer>,
    dst: Rectangle<i32, Physical>,
}

impl<R: Renderer> Element for Piece<R> {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.src
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.dst
    }

    fn damage_since(
        &self,
        _scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        if commit == Some(self.commit) {
            DamageSet::default()
        } else {
            DamageSet::from_slice(&[Rectangle::from_size(self.dst.size)])
        }
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl<R> RenderElement<R> for Piece<R>
where
    R: Renderer + ImportMem,
    R::TextureId: 'static,
{
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), R::Error> {
        self.inner
            .draw(frame, src, dst, damage, opaque_regions, cache)
    }

    fn underlying_storage(&self, _renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

smithay::render_elements! {
    /// One element of a window's chrome.
    pub ChromeElement<R> where R: ImportMem;
    Piece = Piece<R>,
    Solid = SolidColorRenderElement,
}

/// A window's chrome elements: `front` sits above the window's content (the
/// titlebar and the border strips, which never overlap it), `back` below
/// everything of the window (the shadow).
pub struct Elements<R: ImportMem> {
    pub front: Vec<ChromeElement<R>>,
    pub back: Vec<ChromeElement<R>>,
    /// The rounded rectangle the window's content is cut to
    /// ([`crate::clip::Clipped`]); `None` when nothing is cut (maximised).
    pub clip: Option<crate::clip::Clip>,
}

impl<R: ImportMem> Default for Elements<R> {
    fn default() -> Self {
        Elements {
            front: Vec::new(),
            back: Vec::new(),
            clip: None,
        }
    }
}

/// What the last raster was made from; a different key re-rasterises.
#[derive(Clone, PartialEq)]
struct RasterKey {
    commit: u64,
    scale_bits: u32,
}

/// The per-window chrome, kept in the window's user data.
#[derive(Default)]
struct WindowChrome {
    chrome: Chrome,
    titlebar: Option<(RasterKey, MemoryRenderBuffer, Size<i32, Buffer>)>,
    shadow: Option<(ShadowKey, MemoryRenderBuffer, u32)>,
    commit: CommitCounter,
    shadow_commit: CommitCounter,
    titlebar_id: Option<Id>,
    shadow_ids: Option<[Id; 9]>,
    border_ids: Option<[Id; 5]>,
    /// The clip last logged.
    last_clip: Option<crate::clip::Clip>,
    /// The border's two bottom corners (an arc each), when the frame is rounded.
    corners: Option<(CornerKey, MemoryRenderBuffer, i32)>,
}

#[derive(Clone, PartialEq)]
struct CornerKey {
    radius_bits: u32,
    thickness: i32,
    colour_bits: [u32; 4],
}

#[derive(Clone, PartialEq)]
struct ShadowKey {
    radius_bits: u32,
    softness_bits: u32,
    alpha_bits: u32,
}

/// The title an xdg toplevel set, or an X11 window's `_NET_WM_NAME`/`WM_NAME`.
fn title_of(window: &Window) -> String {
    if let Some(x11) = window.x11_surface() {
        return x11.title();
    }
    window
        .toplevel()
        .and_then(|toplevel| {
            smithay::wayland::compositor::with_states(toplevel.wl_surface(), |states| {
                states
                    .data_map
                    .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                    .and_then(|data| data.lock().ok().and_then(|role| role.title.clone()))
            })
        })
        .unwrap_or_default()
}

fn toplevel_has(window: &Window, state: xdg_toplevel::State) -> bool {
    window.toplevel().is_some_and(|toplevel| {
        toplevel.with_committed_state(|s| s.is_some_and(|s| s.states.contains(state)))
    })
}

/// Update `window`'s chrome state from what the window itself says (focus =
/// the `activated` state the client was configured with, the title,
/// maximised) and the slot's logical size; keeps hover and press. Returns the
/// state now in force.
fn refresh(
    chrome: &mut Chrome,
    window: &Window,
    content_size: Size<i32, smithay::utils::Logical>,
) -> ChromeState {
    let mut next = chrome.state().clone();
    if let Some(x11) = window.x11_surface() {
        // The X11 counterparts: `_NET_WM_STATE_FOCUSED` / `_MAXIMIZED_*` as the
        // WM last set them.
        next.focused = x11.is_activated();
        next.maximized = x11.is_maximized();
    } else {
        next.focused = toplevel_has(window, xdg_toplevel::State::Activated);
        next.maximized = toplevel_has(window, xdg_toplevel::State::Maximized)
            || toplevel_has(window, xdg_toplevel::State::TiledLeft)
                && toplevel_has(window, xdg_toplevel::State::TiledRight);
    }
    next.title = title_of(window);
    next.content_size = (content_size.w, content_size.h);
    chrome.update(next, crate::window::generation());
    chrome.state().clone()
}

/// Set which caption button the pointer is over and which is held, for
/// `window` (the input seam calls this). Returns whether the chrome changed,
/// i.e. whether a frame is owed.
pub fn set_pointer(
    window: &Window,
    hovered: Option<CaptionButton>,
    cluster_hovered: bool,
    pressed: Option<CaptionButton>,
) -> bool {
    let cell = window
        .user_data()
        .get_or_insert(|| RefCell::new(WindowChrome::default()));
    let mut wc = cell.borrow_mut();
    let mut next = wc.chrome.state().clone();
    next.hovered = hovered;
    next.cluster_hovered = cluster_hovered;
    next.pressed = pressed;
    wc.chrome.update(next, crate::window::generation())
}

/// The chrome's layout for `window` at its current state, in its frame space
/// (logical px, the frame's top-left at 0,0), for hit-testing.
pub fn layout_of(window: &Window) -> Option<ChromeLayout> {
    let theme = crate::window::installed()?;
    if !crate::window::decorated(window) {
        return None;
    }
    let cell = window
        .user_data()
        .get_or_insert(|| RefCell::new(WindowChrome::default()));
    let wc = cell.borrow();
    Some(wc.chrome.state().layout(&theme.deco))
}

fn ensure_ids<const N: usize>(slot: &mut Option<[Id; N]>) -> [Id; N] {
    slot.get_or_insert_with(|| std::array::from_fn(|_| Id::new()))
        .clone()
}

fn piece<R>(
    renderer: &mut R,
    id: Id,
    commit: CommitCounter,
    buffer: &MemoryRenderBuffer,
    src: Rectangle<f64, Buffer>,
    dst: Rectangle<i32, Physical>,
    clip: Rectangle<i32, Physical>,
) -> Option<ChromeElement<R>>
where
    R: Renderer + ImportMem,
    R::TextureId: Send + Clone + 'static,
{
    if dst.size.w <= 0 || dst.size.h <= 0 {
        return None;
    }
    // Clip to the pane: crop the destination and take the matching part of
    // the source, so nothing bleeds into a neighbouring pane.
    let clipped = dst.intersection(clip)?;
    let (sx, sy) = (
        src.size.w / dst.size.w as f64,
        src.size.h / dst.size.h as f64,
    );
    let src = Rectangle::new(
        (
            src.loc.x + (clipped.loc.x - dst.loc.x) as f64 * sx,
            src.loc.y + (clipped.loc.y - dst.loc.y) as f64 * sy,
        )
            .into(),
        (clipped.size.w as f64 * sx, clipped.size.h as f64 * sy).into(),
    );
    let inner = MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        clipped.loc.to_f64(),
        buffer,
        None,
        None,
        None,
        Kind::Unspecified,
    )
    .ok()?;
    Some(ChromeElement::Piece(Piece {
        id,
        commit,
        inner,
        src,
        dst: clipped,
    }))
}

/// `window`'s chrome around its content at `content` (physical px: the slot as
/// drawn), at `scale` physical px per logical px (output scale × zoom),
/// clipped to `clip` (the pane being drawn). Empty when the window gets no
/// chrome ([`crate::window::decorated`]).
pub fn elements<R>(
    renderer: &mut R,
    window: &Window,
    content: Rectangle<i32, Physical>,
    content_size: Size<i32, smithay::utils::Logical>,
    scale: f64,
    clip: Rectangle<i32, Physical>,
) -> Elements<R>
where
    R: Renderer + ImportMem,
    R::TextureId: Send + Clone + 'static,
{
    let mut out = Elements::default();
    let Some(theme) = crate::window::installed() else {
        return out;
    };
    if !crate::window::decorated(window) || scale <= 0.0 {
        return out;
    }
    let ChromeTheme { deco, .. } = &*theme;
    let cell = window
        .user_data()
        .get_or_insert(|| RefCell::new(WindowChrome::default()));
    let mut wc = cell.borrow_mut();
    let state = refresh(&mut wc.chrome, window, content_size);
    let layout = state.layout(deco);
    let s = scale as f32;
    let extents = DecoExtents::of(deco);

    // The frame's physical origin: the content's corner less the extents.
    let origin = Point::<i32, Physical>::from((
        content.loc.x - (extents.left * s).round() as i32,
        content.loc.y - (extents.top * s).round() as i32,
    ));

    // Titlebar band.
    let key = RasterKey {
        commit: wc.chrome.commit(),
        scale_bits: s.to_bits(),
    };
    if wc.titlebar.as_ref().is_none_or(|(k, ..)| *k != key) {
        let slot_w = layout.title_slot.w * s;
        let mask = crate::text::title_mask(
            &state.title,
            &deco.metrics.title_font_family,
            deco.metrics.title_size_px,
            deco.metrics.title_font_weight.0,
            slot_w,
            s,
        );
        let image = raster::titlebar(deco, &layout, &state, s, mask.as_ref());
        let size = Size::<i32, Buffer>::from((image.width as i32, image.height as i32));
        let buffer = MemoryRenderBuffer::from_slice(
            &image.to_argb8888(),
            Fourcc::Argb8888,
            size,
            1,
            Transform::Normal,
            None,
        );
        wc.titlebar = Some((key, buffer, size));
        wc.commit.increment();
    }
    let titlebar_id = wc.titlebar_id.get_or_insert_with(Id::new).clone();
    if let Some((_, buffer, size)) = &wc.titlebar {
        let dst = Rectangle::new(origin, (size.w, size.h).into());
        if let Some(e) = piece(
            renderer,
            titlebar_id,
            wc.commit,
            buffer,
            Rectangle::from_size(size.to_f64()),
            dst,
            clip,
        ) {
            out.front.push(e);
        }
    }

    // The frame, rounded all the way round unless maximised: the content is cut
    // to it (phase 2, `clip::Clipped`), the titlebar band rounds its top.
    let radius = if state.maximized {
        0.0
    } else {
        deco.metrics.corner_radius * s
    };
    let frame_w = (layout.window.w * s).round() as i32;
    let frame_h = (layout.window.h * s).round() as i32;
    let frame_rect = Rectangle::<i32, Physical>::new(origin, (frame_w, frame_h).into());
    if radius > 0.0 {
        out.clip = Some(crate::clip::Clip {
            rect: frame_rect,
            radius,
        });
    }
    // Logged when it changes (map, move, resize, maximise), not per frame:
    // what the content is cut to, for diagnosis.
    if wc.last_clip != out.clip {
        info!(
            "chrome clip {:?}: {:?} (content {:?}, scale {s})",
            title_of(window),
            out.clip,
            content
        );
        wc.last_clip = out.clip;
    }

    // Border below the band (left, right, bottom) in the border colour: straight
    // strips, and with a rounded frame the two bottom corners as arcs (one
    // rasterised tile) where the strips stop short.
    let border = deco.border(state.focus());
    if deco.metrics.border_thickness > 0.0 && border.a > 0.0 {
        let ids = ensure_ids(&mut wc.border_ids);
        let b = (deco.metrics.border_thickness * s).round().max(1.0) as i32;
        let band_bottom = origin.y + ((layout.titlebar.y + layout.titlebar.h) * s).round() as i32;
        let frame_bottom = origin.y + frame_h;
        let rr = radius.ceil() as i32;
        let side_h = (frame_bottom - rr - band_bottom).max(0);
        let colour = [
            border.r * border.a,
            border.g * border.a,
            border.b * border.a,
            border.a,
        ];
        let strips = [
            Rectangle::new((origin.x, band_bottom).into(), (b, side_h).into()),
            Rectangle::new(
                (origin.x + frame_w - b, band_bottom).into(),
                (b, side_h).into(),
            ),
            Rectangle::new(
                (origin.x + rr, frame_bottom - b).into(),
                ((frame_w - 2 * rr).max(0), b).into(),
            ),
        ];
        for (id, rect) in ids[..3].iter().cloned().zip(strips) {
            if let Some(rect) = rect.intersection(clip) {
                out.front
                    .push(ChromeElement::Solid(SolidColorRenderElement::new(
                        id,
                        rect,
                        wc.commit,
                        colour,
                        Kind::Unspecified,
                    )));
            }
        }
        if rr > 0 {
            let key = CornerKey {
                radius_bits: radius.to_bits(),
                thickness: b,
                colour_bits: colour.map(f32::to_bits),
            };
            if wc.corners.as_ref().is_none_or(|(k, ..)| *k != key) {
                let tile = raster::bottom_corners(border, radius, b as f32);
                let size = (tile.width as i32, tile.height as i32);
                let buffer = MemoryRenderBuffer::from_slice(
                    &tile.to_argb8888(),
                    Fourcc::Argb8888,
                    size,
                    1,
                    Transform::Normal,
                    None,
                );
                wc.corners = Some((key, buffer, rr));
            }
            if let Some((_, buffer, rr)) = &wc.corners {
                let rr = *rr;
                let pieces = [
                    (
                        ids[3].clone(),
                        (0.0, rr as f64 + 1.0),
                        (origin.x, frame_bottom - rr),
                    ),
                    (
                        ids[4].clone(),
                        (rr as f64 + 1.0, rr as f64 + 1.0),
                        (origin.x + frame_w - rr, frame_bottom - rr),
                    ),
                ];
                for (id, (sx, _), (dx, dy)) in pieces {
                    let src = Rectangle::new((sx, 0.0).into(), (rr as f64, rr as f64).into());
                    let dst = Rectangle::new((dx, dy).into(), (rr, rr).into());
                    if let Some(e) = piece(renderer, id, wc.commit, buffer, src, dst, clip) {
                        out.front.push(e);
                    }
                }
            }
        }
    }

    // Shadow: a 9-slice of one cached tile around the frame, dropped by the
    // offset, behind everything of the window.
    let shadow = deco.metrics.shadow;
    let alpha = deco.shadow_alpha(state.focus());
    if alpha > 0.0 && shadow.softness > 0.0 {
        let radius = if state.maximized {
            0.0
        } else {
            deco.metrics.corner_radius * s
        };
        let softness = shadow.softness * s;
        let skey = ShadowKey {
            radius_bits: radius.to_bits(),
            softness_bits: softness.to_bits(),
            alpha_bits: alpha.to_bits(),
        };
        if wc.shadow.as_ref().is_none_or(|(k, ..)| *k != skey) {
            let tile = raster::shadow_tile(shadow.color, alpha, radius, softness);
            let size = (tile.image.width as i32, tile.image.height as i32);
            let buffer = MemoryRenderBuffer::from_slice(
                &tile.image.to_argb8888(),
                Fourcc::Argb8888,
                size,
                1,
                Transform::Normal,
                None,
            );
            wc.shadow = Some((skey, buffer, tile.corner));
            wc.shadow_commit.increment();
        }
        let ids = ensure_ids(&mut wc.shadow_ids);
        if let Some((_, buffer, corner)) = &wc.shadow {
            let c = *corner as i32;
            let soft = softness.round() as i32;
            let frame = Rectangle::<i32, Physical>::new(
                (origin.x, origin.y + (shadow.offset_y * s).round() as i32).into(),
                (
                    (layout.window.w * s).round() as i32,
                    (layout.window.h * s).round() as i32,
                )
                    .into(),
            );
            // The shadow box is the frame grown by the softness.
            let (x0, y0) = (frame.loc.x - soft, frame.loc.y - soft);
            let (x3, y3) = (
                frame.loc.x + frame.size.w + soft,
                frame.loc.y + frame.size.h + soft,
            );
            let (x1, y1, x2, y2) = (x0 + c, y0 + c, (x3 - c).max(x0 + c), (y3 - c).max(y0 + c));
            let cols = [
                (x0, x1, 0.0, c as f64),
                (x1, x2, c as f64, 1.0),
                (x2, x3, c as f64 + 1.0, c as f64),
            ];
            let rows = [
                (y0, y1, 0.0, c as f64),
                (y1, y2, c as f64, 1.0),
                (y2, y3, c as f64 + 1.0, c as f64),
            ];
            let mut i = 0;
            for &(ya, yb, sy, sh) in &rows {
                for &(xa, xb, sx, sw) in &cols {
                    let dst = Rectangle::new((xa, ya).into(), (xb - xa, yb - ya).into());
                    let src = Rectangle::new((sx, sy).into(), (sw, sh).into());
                    if let Some(e) = piece(
                        renderer,
                        ids[i].clone(),
                        wc.shadow_commit,
                        buffer,
                        src,
                        dst,
                        clip,
                    ) {
                        out.back.push(e);
                    }
                    i += 1;
                }
            }
        }
    }
    out
}
