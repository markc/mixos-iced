use smithay::backend::renderer::{ImportAll, ImportMem, Renderer, Texture};
use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Physical, Point, Size};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;
use crate::state::Loop;
use crate::state::state::CoordinateTrait;
use ui::{
    HandleId, IcedRenderElement, IcedSpace, Transform as IcedTransform,
};
use crate::canvas::draw::element::element::Element;
use crate::window::draw::occlude::occlude::{Occluders, Visible};
use crate::window::interface::record::window::LoopWindow;
use protocols::window::find::find;

/// One drawable in the content band: a canvas element (window / cursor) or an
/// iced surface. Windows and world iced interleave here by the
/// renderer-agnostic DrawOrder ("everything interleaves").
pub enum ContentItem<R: Renderer> {
    Canvas { elem: Element<R> },
    Iced(IcedRenderElement),
}

/// The WORLD-space iced surface for `uuid`, if the registry still has one.
///
/// World only. A SCREEN surface reaching this band is drawn TWICE — here, and
/// again in its own band. The copies coincided exactly until a bundle's pointer
/// warp curved one of them and the desktop grew a second launcher. Fixed at the
/// source too (`native_press`), but this loop walks a PERSISTED order and must not
/// assume every id in it belongs to this band.
fn world_iced(
    state: &Loop,
    uuid: &Uuid,
    transform: &IcedTransform,
    size: Size<f64, Physical>,
) -> Option<IcedRenderElement> {
    let id = HandleId(uuid.as_u128() as u64);
    let registry = state.inner.surface().registry.as_ref()?;
    matches!(registry.space_of(id)?, IcedSpace::World)
        .then(|| registry.element_of(id, transform, size))?
}

fn placed(window: &Window) -> bool {
    window.user_data().get::<dispatcher::wayland::compositor::dispatch::wire::WindowPlacedMarker>().is_some()
}

pub fn scene<R>(state: &mut Loop, renderer: &mut R, size: Size<i32, Physical>) -> (Vec<ContentItem<R>>, Visible)
where
    R: Renderer + ImportAll + ImportMem + dispatcher::frame::frame::SceneDispatch,
    R::TextureId: Texture + Clone + Send + 'static,
{
    let canvas_context = context(state, renderer, size);
    let mut content: Vec<ContentItem<R>> = Vec::new();
    let mut visible_windows = Visible::default();
    // One accumulator per pane — an opaque rect only hides what is drawn into
    // the same viewport. Filled as the DrawOrder loop below walks front to back,
    // so each window is tested against everything already stacked over it.
    let mut occluders = Occluders::new();

    // Interleave windows + world iced by the DrawOrder authority (topmost-first).
    let order = state.inner.drawable_order();
    let by_uuid: HashMap<Uuid, Window> = state.inner.space_state().state
        .elements().filter_map(|w| w.uuid().map(|u| (u, w.clone()))).collect();
    let ordered: HashSet<Uuid> = order.iter().copied().collect();
    // iced camera transform (mirrors the surface scene): world items pan/zoom.
    let scale = state.viewport_context().scale;
    let cam = state.inner.camera().transform.clone();
    let iced_transform = IcedTransform { zoom: cam.zoom, position: Point::new(cam.position.x * scale, cam.position.y * scale) };
    let size_f64 = size.to_f64();

    let mut draw_window = |state: &mut Loop, renderer: &mut R, window: &Window, content: &mut Vec<ContentItem<R>>, visible: &mut Visible, occ: &mut Occluders| {
        if !placed(window) { return; }
        let (elems, drawn) = crate::window::draw::frame::scene::scene(state, renderer, size, window, &canvas_context, occ);
        visible.note(window, &drawn);
        occ.extend(&drawn.opaque);
        if elems.is_empty() { return; }
        for e in elems { content.push(ContentItem::Canvas { elem: Element::Window(e) }); }
    };

    for uuid in &order {
        if let Some(window) = by_uuid.get(uuid).cloned() {
            draw_window(state, renderer, &window, &mut content, &mut visible_windows, &mut occluders);
            continue;
        }
        let id = HandleId(uuid.as_u128() as u64);
        // SCREEN ids reach this order too (see `world_iced`), and an id the
        // registry no longer knows is stale. Skip both.
        if let Some(IcedSpace::World) = state.inner.surface().registry.as_ref().and_then(|r| r.space_of(id)) {
            if let Some(elem) = world_iced(state, uuid, &iced_transform, size_f64) {
                content.push(ContentItem::Iced(elem));
            }
        }
    }
    // Defensive: any placed window not in the order draws at the bottom.
    let leftovers: Vec<Window> = by_uuid.values().filter(|w| w.uuid().map(|u| !ordered.contains(&u)).unwrap_or(true)).cloned().collect();
    for window in leftovers {
        draw_window(state, renderer, &window, &mut content, &mut visible_windows, &mut occluders);
    }
    // The occlusion props, from what this pane's cull just decided:
    // covered = on the pane and not drawn. Never schedules a frame.
    crate::window::draw::occlude::record::record(
        &state.inner.current_output_key(),
        state.inner.render_target.as_ref().map(|target| target.slot),
        &visible_windows,
        by_uuid.keys().copied(),
    );

    // Canvas cursor: drawn only on the pane under the physical cursor (the
    // `pointer` slot) — it would otherwise appear once per pane. Outside the
    // per-region loop (no render target) it always draws. Only when the
    // `canvas_cursor` preference is on (default off):
    // off, there is no element, so nothing to damage and no frame asked for.
    let cursor_here = state.inner.preference.canvas_cursor
        && state
            .inner
            .render_target
            .map_or(true, |rt| rt.slot == state.inner.viewports().pointer);
    if cursor_here {
        for e in crate::canvas::cursor::element::scene::scene(state, renderer, size, &canvas_context) {
            content.push(ContentItem::Canvas { elem: Element::SolidBox(e) });
        }
    }

    (content, visible_windows)
}

pub use crate::canvas::draw::viewport::viewport::context;
