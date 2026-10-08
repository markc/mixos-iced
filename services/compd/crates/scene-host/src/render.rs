//! Where each scene draws, and keeping its iced surface in step.
//!
//! Placement:
//! - an EDGE scene is a strip on its page seat's output, docked to its edge,
//!   drawn only while it is the active page of a shown edge in Quoin's panel
//!   model (`panels`: one page per edge, modes, motion). Its thickness is the
//!   model's, at least the envelope's `w` (left/right, default 360) or `h`
//!   (top/bottom, default 240); it spans the output the other way, and slides
//!   in and out by the edge's visible fraction;
//! - a DIALOG is centred on its seat's output at the envelope's `w`×`h`
//!   (title bar included, as Quoin's frozen layout measures it), and only
//!   while shown (`shell.dialog.show`, until `hide` or its close button).
//!
//! Each placed scene is one screen-space iced surface in the world surface
//! registry, so pointer and keyboard reach it the way they reach every
//! compositor-owned iced surface. A surface changes only when its scene's
//! revision, frame or rectangle does: a static scene costs no frames.

use std::collections::BTreeMap;
use std::sync::Arc;

use smithay::backend::renderer::gles::GlesRenderer;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Physical, Point, Rectangle, Size};
use ui::{HandleId, IcedHandle, IcedUi};
use world::scene::layer::base::Layer;
use world::state::Loop;
use world::surface::draw::handle::handle::{IcedSpace, load};

use crate::mount::{dialog_geometry, is_dialog, page_id, scene_edge};
use crate::seat::Edge;
use crate::store::{Mounted, SceneEntry, SceneStore};
use crate::view::{Content, Frame, SceneMessage, SceneUi, event_of};

/// A strip's default thickness when the envelope names none.
pub const EDGE_WIDTH: f32 = 360.0;
pub const EDGE_HEIGHT: f32 = 240.0;

/// How one scene is placed, before an output's size is known.
#[derive(Clone, Debug, PartialEq)]
pub enum Place {
    /// `offset` logical px in from `edge`, `extent` thick.
    Edge {
        edge: Edge,
        offset: f32,
        extent: f32,
        top: f32,
        bottom: f32,
    },
    Dialog {
        w: f32,
        h: f32,
        frame: Option<Frame>,
    },
}

/// One scene's target: the output it draws on and how.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub output: String,
    pub place: Place,
}

impl Target {
    fn frame(&self) -> Option<Frame> {
        match &self.place {
            Place::Dialog { frame, .. } => frame.clone(),
            Place::Edge { .. } => None,
        }
    }
}

/// A logical rectangle `(x, y, w, h)` on an output of `w`×`h` logical px.
pub fn rect(place: &Place, (out_w, out_h): (f32, f32)) -> (f32, f32, f32, f32) {
    match place {
        Place::Edge {
            edge,
            offset,
            extent,
            top,
            bottom,
        } => {
            let across = match edge {
                Edge::Left | Edge::Right => out_w,
                Edge::Top | Edge::Bottom => out_h,
            };
            let extent = extent.clamp(1.0, across.max(1.0));
            match edge {
                Edge::Left | Edge::Right => {
                    let top = top.clamp(0.0, out_h.max(0.0));
                    let height = (out_h - top - bottom.max(0.0)).max(0.0);
                    (
                        if *edge == Edge::Left {
                            *offset
                        } else {
                            out_w - offset - extent
                        },
                        top,
                        extent,
                        height,
                    )
                }
                Edge::Top => (0.0, *offset, out_w, extent),
                Edge::Bottom => (0.0, out_h - offset - extent, out_w, extent),
            }
        }
        Place::Dialog { w, h, .. } => dialog_rect(*w, *h, (0.0, 0.0, out_w, out_h)),
    }
}

/// The dialog's gap to the usable zone's sides, and its least size (Quoin's
/// dialog seat clamp, `max(240, zone - 2*24)`).
pub const DIALOG_MARGIN: f32 = 24.0;
pub const DIALOG_MIN: f32 = 240.0;

/// The dialog's rectangle: its envelope size fitted to the usable `zone`
/// (`x, y, w, h`, logical) less the margin, at least [`DIALOG_MIN`], and
/// centred in the zone (the area other layers' exclusive zones leave), as an
/// unanchored overlay layer surface is.
pub fn dialog_rect(w: f32, h: f32, (zx, zy, zw, zh): (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
    let w = w.min(zw - 2.0 * DIALOG_MARGIN).max(DIALOG_MIN);
    let h = h.min(zh - 2.0 * DIALOG_MARGIN).max(DIALOG_MIN);
    (zx + (zw - w) / 2.0, zy + (zh - h) / 2.0, w, h)
}

/// An output's usable zone (`x, y, w, h`, logical) after the docked edges'
/// reservations (`zones`, from [`crate::panels::Panels::zones`]).
pub fn usable_zone(zones: &[(Edge, f32)], (out_w, out_h): (f32, f32)) -> (f32, f32, f32, f32) {
    let (mut x, mut y, mut w, mut h) = (0.0, 0.0, out_w, out_h);
    for &(edge, px) in zones {
        match edge {
            Edge::Left => {
                x += px;
                w -= px;
            }
            Edge::Right => w -= px,
            Edge::Top => {
                y += px;
                h -= px;
            }
            Edge::Bottom => h -= px,
        }
    }
    (x, y, w.max(0.0), h.max(0.0))
}

/// Where every drawable scene goes. An edge scene draws on the output its
/// page seat names (no seat: not drawn), and only while it is its edge's
/// active page on a mapped edge (`panels`, Quoin's model: one page per edge),
/// at the edge's thickness, slid out by what of it is not yet visible. A
/// dialog draws only while it holds the dialog seat and is shown.
pub fn targets(store: &SceneStore, panels: &crate::panels::Panels) -> BTreeMap<String, Target> {
    let mut targets = BTreeMap::new();
    for (name, entry) in store.scenes() {
        let tree = entry.tree();
        if is_dialog(tree) {
            let Some(seat) = store.dialog_seat().filter(|seat| seat.scene == name) else {
                continue;
            };
            if !entry.visible() {
                continue;
            }
            let (w, h, title, chrome) = dialog_geometry(tree);
            let frame = chrome.then(|| Frame {
                title: title.unwrap_or_default(),
            });
            targets.insert(
                name.to_owned(),
                Target {
                    output: seat.output.clone(),
                    place: Place::Dialog { w, h, frame },
                },
            );
            continue;
        }
        let page = page_id(tree);
        let Some(seat) = store.pages().seat(&page) else {
            continue;
        };
        let edge = scene_edge(tree);
        let Some(drawn) = panels
            .drawn(&seat.output, edge)
            .filter(|drawn| drawn.page == page)
        else {
            continue;
        };
        let offset = -(drawn.thickness * (1.0 - drawn.fraction));
        let zones = panels.zones(&seat.output);
        let inset = |edge| {
            zones
                .iter()
                .find(|(e, _)| *e == edge)
                .map_or(0.0, |(_, px)| *px)
        };
        targets.insert(
            name.to_owned(),
            Target {
                output: seat.output.clone(),
                place: Place::Edge {
                    edge,
                    offset,
                    extent: drawn.thickness,
                    top: inset(Edge::Top),
                    bottom: inset(Edge::Bottom),
                },
            },
        );
    }
    targets
}

fn keep_surface(
    store: &SceneStore,
    targets: &BTreeMap<String, Target>,
    name: &str,
    output: &str,
) -> bool {
    targets
        .get(name)
        .is_some_and(|target| target.output == output)
        || store
            .scene(name)
            .filter(|entry| !is_dialog(entry.tree()))
            .and_then(|entry| store.pages().seat(&page_id(entry.tree())))
            .is_some_and(|seat| seat.output == output)
}

fn resident_targets(
    store: &SceneStore,
    panels: &crate::panels::Panels,
    mapped: &BTreeMap<String, Target>,
) -> BTreeMap<String, Target> {
    let mut resident = mapped.clone();
    for (name, entry) in store.scenes() {
        let tree = entry.tree();
        if is_dialog(tree) || resident.contains_key(name) {
            continue;
        }
        let page = page_id(tree);
        let Some(seat) = store.pages().seat(&page) else {
            continue;
        };
        let edge = scene_edge(tree);
        let extent = panels
            .page_thickness(&seat.output, edge, &page)
            .unwrap_or(EDGE_WIDTH);
        let zones = panels.zones(&seat.output);
        let inset = |edge| {
            zones
                .iter()
                .find(|(e, _)| *e == edge)
                .map_or(0.0, |(_, px)| *px)
        };
        resident.insert(
            name.into(),
            Target {
                output: seat.output.clone(),
                place: Place::Edge {
                    edge,
                    offset: -extent,
                    extent,
                    top: inset(Edge::Top),
                    bottom: inset(Edge::Bottom),
                },
            },
        );
    }
    resident
}

fn first_field(tree: &scene::ResolvedScene) -> Option<&str> {
    fn visit<'a>(tree: &'a scene::ResolvedScene, id: &'a str, depth: usize) -> Option<&'a str> {
        if depth > tree.nodes.len() {
            return None;
        }
        let node = tree.nodes.get(id)?;
        if node
            .ports
            .get("hidden")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            return None;
        }
        if node.family == "field" {
            return Some(id);
        }
        crate::templates::children(node).find_map(|child| visit(tree, child, depth + 1))
    }
    visit(tree, "root", 0)
}

fn viewport_ready(
    actual: iced_core::Size<u32>,
    scale: f32,
    wanted: Size<i32, Physical>,
    factor: f32,
) -> bool {
    actual.width == wanted.w.max(1) as u32
        && actual.height == wanted.h.max(1) as u32
        && scale == factor
}

/// A scene's live iced surface.
pub(crate) struct Surface {
    handle: HandleId,
    /// The world whose registry holds it (the registry is per world).
    world: u128,
    output: String,
    revision: u64,
    frame: Option<Frame>,
    rect: Rectangle<i32, Physical>,
    /// The iced scale factor it lays out at: the output scale, so the UI is
    /// laid out in logical px and rasterised at physical.
    factor: f32,
    autofocus_done: bool,
    panel_marks: Option<[bool; 3]>,
    appearance_generation: u64,
}

impl Surface {
    pub(crate) fn handle(&self) -> HandleId {
        self.handle
    }

    pub(crate) fn input_geometry(&self) -> (HandleId, Rectangle<i32, Physical>, f32) {
        (self.handle, self.rect, self.factor)
    }

    /// The output it is drawn on (empty: not bound to one).
    pub(crate) fn output(&self) -> &str {
        &self.output
    }
}

/// What a surface needs to reach the rest of the host: the event sink, and
/// the action channel for its frame's close button.
#[derive(Clone)]
pub(crate) struct Wiring {
    pub sink: crate::port::EventSink,
    pub actions: std::sync::mpsc::Sender<Action>,
    pub waker: crate::port::Waker,
}

/// What a surface asks of the host outside the Bus.
#[derive(Debug, PartialEq)]
pub enum Action {
    /// The dialog frame's close button: unmap, as `shell.dialog.hide`.
    HideDialog(String),
    EscapeEdge(String),
    EdgeFocus(String, bool),
    Menu(u64, crate::menu::Input),
}

fn panel_marks(
    store: &SceneStore,
    panels: &crate::panels::Panels,
    tree: &scene::ResolvedScene,
) -> Option<[bool; 3]> {
    (tree.name == "panel").then(|| {
        ["launcher", "calendar", "notes"].map(|name| {
            let Some(entry) = store.scene(name) else {
                return false;
            };
            let page = page_id(entry.tree());
            let Some(seat) = store.pages().seat(&page) else {
                return false;
            };
            let edge = scene_edge(entry.tree());
            !panels.hidden(&seat.output, edge)
                && panels.state(&seat.output, edge)["page"].as_str() == Some(page.as_str())
        })
    })
}

fn content(
    entry: &SceneEntry,
    frame: Option<Frame>,
    dialog: bool,
    marks: Option<[bool; 3]>,
) -> Arc<Content> {
    let mut tree = entry.tree().clone();
    // Button feedback follows the applied page/visibility, including hotspot
    // reveals. Click events still go to the scenes loader, which owns pin/restore.
    if let Some(marks) = marks {
        for (id, open) in ["launcher_btn", "clock", "st_notes"].into_iter().zip(marks) {
            if let Some(node) = tree.nodes.get_mut(id) {
                node.ports.insert(
                    "background".into(),
                    if open {
                        serde_json::json!("#3daee940")
                    } else {
                        serde_json::Value::Null
                    },
                );
            }
        }
    }
    Arc::new(Content {
        tree,
        lists: entry.prepared().clone(),
        revision: entry.revision(),
        frame,
        dialog,
    })
}

pub(crate) fn physical((x, y, w, h): (f32, f32, f32, f32), scale: f64) -> Rectangle<i32, Physical> {
    let s = |v: f32| (v as f64 * scale).round() as i32;
    Rectangle::new(
        Point::from((s(x), s(y))),
        Size::from((s(w).max(1), s(h).max(1))),
    )
}

fn destroy(state: &mut Loop, handle: HandleId) {
    if let Some(registry) = state.inner.surface_mut().registry.as_mut()
        && registry.contains(handle)
    {
        registry.destroy_by_id(handle);
    }
}

/// One output's pass, inside the frame's GLES prepare (where a surface can
/// be created). Called once per output per frame.
#[allow(clippy::too_many_arguments)]
pub(crate) fn reconcile(
    store: &mut SceneStore,
    panels: &mut crate::panels::Panels,
    surfaces: &mut BTreeMap<String, Surface>,
    placed: &mut BTreeMap<String, (f32, f32, f32, f32)>,
    palette: decor::Palette,
    prepared: Option<Arc<::appearance::settings::Prepared>>,
    appearance_generation: u64,
    frame_owners: &mut BTreeMap<
        String,
        (application::iced::window::Id, application::frames::Handle),
    >,
    frame_stamp: Option<application::frames::FrameStamp>,
    live_generation: Option<u64>,
    wiring: &Wiring,
    state: &mut Loop,
    renderer: &mut GlesRenderer,
    size: Size<i32, Physical>,
) {
    if state.inner.surface().registry.is_none() {
        return;
    }
    for mounted in store.take_removed() {
        destroy(state, HandleId(mounted.handle));
    }
    let targets = targets(store, panels);
    let resident = resident_targets(store, panels, &targets);
    // A deliberate conceal keeps the surface mapped during its outgoing
    // animation, but it must release the keyboard immediately.
    for (name, surface) in surfaces.iter_mut() {
        if let Some(entry) = store.scene(name)
            && !is_dialog(entry.tree())
            && (panels.hidden(&surface.output, scene_edge(entry.tree()))
                || !targets.contains_key(name))
        {
            surface.autofocus_done = false;
            if panels.hidden(&surface.output, scene_edge(entry.tree())) {
                release_edge_keyboard(state, name);
            }
            if panels.hidden(&surface.output, scene_edge(entry.tree()))
                && panels.focused(&surface.output) == Some(scene_edge(entry.tree()))
            {
                panels.focus(&surface.output, None);
            }
        }
    }
    // Carousel pages stay alive while seated, including while hidden. Only
    // unload/output migration destroys their renderer, layout and icon cache.
    let gone: Vec<String> = surfaces
        .iter()
        .filter(|(name, surface)| !keep_surface(store, &targets, name, &surface.output))
        .map(|(name, _)| name.clone())
        .collect();
    for name in gone {
        if let Some((_, frames)) = frame_owners.remove(&name) {
            frames.close();
        }
        if let Some(surface) = surfaces.remove(&name) {
            release_edge_keyboard(state, &name);
            destroy(state, surface.handle);
        }
        store.set_mounted(&name, None);
    }
    for (name, surface) in surfaces.iter() {
        if !targets.contains_key(name) {
            if let Some(registry) = state.inner.surface_mut().registry.as_mut() {
                registry.set_visible_by_id(surface.handle, false);
            }
            store.set_mounted(name, None);
        }
    }
    // The dialog that took the seat's keyboard is gone (hidden, unloaded,
    // moved): the window it took it from gets it back.
    if DIALOG_PRIOR.with_borrow(|prior| {
        prior
            .as_ref()
            .is_some_and(|(scene, _)| !surfaces.contains_key(scene))
    }) {
        release_seat_keyboard(state);
    }
    // Content sources (F6): a scene is `scene_<name>` from its first placement
    // until it leaves the store (a hidden dialog stays registered and reports
    // unshown), measured on the output it was first placed on.
    SOURCED.with_borrow_mut(|sourced| {
        sourced.retain(|name| {
            let keep = store.scene(name).is_some();
            if !keep {
                ui::source::unregister(&source_id(name));
            }
            keep
        });
        for (name, target) in &targets {
            if sourced.insert(name.clone()) {
                let output = state
                    .inner
                    .space_state()
                    .state
                    .outputs()
                    .find(|o| o.name() == target.output)
                    .map(world::state::state::output_key);
                ui::source::register(&source_id(name), output);
            }
            if let Some(entry) = store.scene(name) {
                ui::source::revise(&source_id(name), entry.revision());
            }
        }
    });
    // The last rect drawn is kept while a dialog is hidden (layout reports
    // it unmapped there), and dropped with the scene.
    placed.retain(|name, _| store.scene(name).is_some());
    let output_key = state.inner.current_output_key();
    let output = state.inner.current_output().name();
    let scale = state
        .inner
        .current_output()
        .current_scale()
        .fractional_scale()
        .max(0.1);
    let world = state.inner.worlds.spawn_target().as_u128();
    let logical = (size.w as f32 / scale as f32, size.h as f32 / scale as f32);
    // An edge page's surface was created this pass: it is on top of the
    // registry, above a dialog that was already up.
    let mut restack = false;
    for (name, target) in &resident {
        if !(target.output == output || target.output.is_empty()) {
            continue;
        }
        let Some(entry) = store.scene(name) else {
            continue;
        };
        // A dialog sits in what the docked edges leave, so it never lies under
        // (or over) a docked page.
        let logical_rect = match &target.place {
            Place::Dialog { w, h, .. } => {
                dialog_rect(*w, *h, usable_zone(&panels.zones(&target.output), logical))
            }
            Place::Edge { .. } => rect(&target.place, logical),
        };
        placed.insert(name.clone(), logical_rect);
        let rect = physical(logical_rect, scale);
        let frame = target.frame();
        let dialog = matches!(target.place, Place::Dialog { .. });
        let revision = entry.revision();
        let marks = panel_marks(store, panels, entry.tree());
        let live = surfaces.get(name).filter(|surface| {
            surface.world == world
                && state
                    .inner
                    .surface()
                    .registry
                    .as_ref()
                    .is_some_and(|registry| registry.contains(surface.handle))
        });
        // A registry/world replacement is a new surface incarnation even when
        // its scene name survives. Never let its callback adopt the old pixels.
        if live.is_none() {
            if let Some((_, frames)) = frame_owners.remove(name) {
                frames.close();
            }
        }
        let (frame_window, frames) = frame_owners.entry(name.clone()).or_insert_with(|| {
            (
                application::iced::window::Id::unique(),
                application::frames::Handle::new(),
            )
        });
        frames.set_live_generation(live_generation);
        let presentation = frame_stamp.map(|stamp| (*frame_window, frames.binding(stamp)));
        let factor = scale as f32;
        let mut autofocus_done = live.is_some_and(|surface| surface.autofocus_done);
        let handle = match live.map(|surface| {
            (
                surface.handle,
                surface.rect,
                surface.factor,
                surface.revision,
                surface.frame.clone(),
                surface.panel_marks,
                surface.appearance_generation,
            )
        }) {
            Some((
                handle,
                was,
                was_factor,
                applied,
                applied_frame,
                applied_marks,
                applied_appearance,
            )) => {
                if let Some(registry) = state.inner.surface_mut().registry.as_mut() {
                    if was.size != rect.size || was_factor != factor {
                        if panels.focused(&target.output) == Some(scene_edge(entry.tree())) {
                            autofocus_done = false;
                        }
                        registry.request_resize_scaled_by_id(handle, rect.size, factor);
                    }
                    if was.loc != rect.loc {
                        registry.set_location_by_id(handle, rect.loc);
                    }
                    if applied != revision || applied_frame != frame || applied_marks != marks {
                        let message =
                            SceneMessage::Replace(content(entry, frame.clone(), dialog, marks));
                        let _ = registry
                            .dispatch_message(IcedHandle::<SceneUi>::from_id(handle), message);
                        registry
                            .set_keyboard_transparent_by_id(handle, !takes_keyboard(entry.tree()));
                        // The whole surface redraws for new content; iced renders
                        // on the GPU, so nothing is uploaded.
                        ui::source::cost(&source_id(name), 0, area(rect));
                    }
                    let current = registry
                        .instance(IcedHandle::<SceneUi>::from_id(handle))
                        .and_then(|instance| instance.ui().frame_presentation());
                    let same = match (&current, &presentation) {
                        (Some((a, x)), Some((b, y))) => a == b && x.same_presentation(y),
                        (None, None) => true,
                        _ => false,
                    };
                    if applied_appearance != appearance_generation {
                        if let Some(prepared) = &prepared {
                            // Stamp and prepared appearance enter the same UI
                            // update, before either can be rendered to a slot.
                            let _ = registry.dispatch_message(
                                IcedHandle::<SceneUi>::from_id(handle),
                                SceneMessage::AppearanceFrame(
                                    Arc::clone(prepared),
                                    presentation.clone(),
                                ),
                            );
                        }
                    } else if !same {
                        let _ = registry.dispatch_message(
                            IcedHandle::<SceneUi>::from_id(handle),
                            SceneMessage::Presentation(presentation.clone()),
                        );
                    }
                }
                handle
            }
            None => {
                restack |= !dialog;
                let content = content(entry, frame.clone(), dialog, marks);
                let mut ui = prepared.as_ref().map_or_else(
                    || SceneUi::new(Arc::clone(&content), palette),
                    |prepared| SceneUi::from_prepared(Arc::clone(&content), Arc::clone(prepared)),
                );
                ui.update(SceneMessage::Presentation(presentation.clone()));
                let handle = load(
                    state,
                    renderer,
                    ui,
                    rect,
                    IcedSpace::Screen,
                    Layer::SCENE.bits(),
                );
                if let Some(registry) = state.inner.surface_mut().registry.as_mut() {
                    // Created at the registry's instance scale (1); laid out at the
                    // output's from its first frame.
                    registry.request_resize_scaled_by_id(handle.id, rect.size, factor);
                    registry
                        .set_keyboard_transparent_by_id(handle.id, !takes_keyboard(entry.tree()));
                    ui::source::cost(&source_id(name), 0, area(rect));
                    if !target.output.is_empty() {
                        registry.set_output_affinity_by_id(handle.id, Some(output_key.clone()));
                    }
                    let (sink, actions, waker, scene) = (
                        wiring.sink.clone(),
                        wiring.actions.clone(),
                        Arc::clone(&wiring.waker),
                        name.clone(),
                    );
                    registry.set_message_handler(handle, move |message: &SceneMessage| {
                        let action = match message {
                            SceneMessage::Close => Some(Action::HideDialog(scene.clone())),
                            SceneMessage::EscapeEdge => Some(Action::EscapeEdge(scene.clone())),
                            SceneMessage::EdgeFocus(focused) => {
                                Some(Action::EdgeFocus(scene.clone(), *focused))
                            }
                            _ => None,
                        };
                        if let Some(action) = action {
                            let _ = actions.send(action);
                            waker();
                        } else if let Some((citizen, verb, body)) = event_of(message) {
                            sink.emit(citizen, verb, body);
                        }
                    });
                    // A dialog takes the keyboard every time it maps, before
                    // any click (Quoin's grab-then-demote): keys go to it, not
                    // to the focused window, until it hides or something else
                    // takes the keyboard.
                    if dialog {
                        registry.set_keyboard_focus(Some(handle.id));
                    }
                }
                if dialog {
                    grab_seat_keyboard(state, name);
                }
                handle.id
            }
        };
        let edge = scene_edge(entry.tree());
        let mapped = targets.contains_key(name);
        if let Some(registry) = state.inner.surface_mut().registry.as_mut() {
            registry.retain_backing_by_id(handle, !dialog);
            registry.set_visible_by_id(handle, mapped);
        }
        let requested = panels.focus_requested(&target.output) == Some(edge);
        let field = crate::view::autofocus(entry.tree())
            .or_else(|| requested.then(|| first_field(entry.tree())).flatten());
        // A pending resize invalidates iced's widget cache (and its focus).
        // Wait until the instance has applied the final viewport, then focus.
        let ready = state
            .inner
            .surface_mut()
            .registry
            .as_mut()
            .and_then(|registry| {
                if registry
                    .get(handle)
                    .is_some_and(|item| item.pending_resize().is_some())
                {
                    return None;
                }
                registry.instance_mut(IcedHandle::<SceneUi>::from_id(handle))
            })
            .is_some_and(|instance| {
                viewport_ready(
                    instance.runtime().physical_size(),
                    instance.runtime().scale_factor(),
                    rect.size,
                    factor,
                )
            });
        if mapped
            && !dialog
            && (!autofocus_done || requested)
            && ready
            && !world::comp::session_lock::active(state)
            && panels.menu().is_none()
            && store
                .dialog_seat()
                .is_none_or(|seat| !store.scene(&seat.scene).is_some_and(SceneEntry::visible))
            && !panels.hidden(&target.output, scene_edge(entry.tree()))
            && panels
                .drawn(&target.output, scene_edge(entry.tree()))
                .is_some_and(|drawn| drawn.fraction >= 1.0)
            && (field.is_some() || requested)
        {
            grab_edge_keyboard(state, name, handle);
            if let Some(registry) = state.inner.surface_mut().registry.as_mut() {
                registry.set_keyboard_focus(Some(handle));
                if let Some(instance) =
                    registry.instance_mut(IcedHandle::<SceneUi>::from_id(handle))
                {
                    // Apply a Replace/Focused queued in this pass before
                    // visiting ids, so the operation targets the current tree.
                    instance.runtime_mut().tick();
                    autofocus_done = if let Some(id) = field {
                        let mut focus = crate::view::FocusField::new(id);
                        instance.runtime_mut().operate(&mut focus);
                        focus.found
                    } else {
                        true
                    };
                    instance.runtime_mut().request_redraw();
                }
            }
            panels.focus(&target.output, Some(scene_edge(entry.tree())));
            if autofocus_done {
                panels.focus_granted(&target.output);
            }
            state
                .state
                .schedule_redraw(dispatcher::state::state::RedrawReason::Publish);
        }
        if mapped && !dialog && !autofocus_done && !ready && (field.is_some() || requested) {
            state
                .state
                .schedule_redraw(dispatcher::state::state::RedrawReason::Publish);
        }
        surfaces.insert(
            name.clone(),
            Surface {
                handle,
                world,
                output: target.output.clone(),
                revision,
                frame,
                rect,
                factor,
                autofocus_done,
                panel_marks: marks,
                appearance_generation,
            },
        );
        store.set_mounted(
            name,
            mapped.then_some(Mounted {
                handle: handle.0,
                revision,
            }),
        );
        // Drawn on this output this frame.
        if mapped {
            ui::source::set_native_handle(&source_id(name), handle);
            ui::source::shown(&source_id(name), &output_key);
        }
    }
    // The dialog stacks above every edge page (Quoin: an Overlay layer over
    // the docked pages' Top), so a page created after it — an edge docked
    // while the dialog is up — neither covers it nor takes its clicks (the
    // registry hit-tests and draws last-on-top). Raised only when a page was
    // created, so it never climbs over compositor iced (capture overlays)
    // opened since.
    let dialogs: Vec<HandleId> = surfaces
        .iter()
        .filter(|(name, surface)| {
            restack
                && surface.output == output
                && targets
                    .get(*name)
                    .is_some_and(|target| matches!(target.place, Place::Dialog { .. }))
        })
        .map(|(_, surface)| surface.handle)
        .collect();
    if let Some(registry) = state.inner.surface_mut().registry.as_mut() {
        if restack {
            // Horizontal overlays sit above side content (§6.1), including
            // when a side page maps after an already pinned top/bottom page.
            for (name, surface) in surfaces
                .iter()
                .filter(|(_, surface)| surface.output == output)
            {
                if targets.get(name).is_some_and(|target| {
                    matches!(
                        target.place,
                        Place::Edge {
                            edge: Edge::Top | Edge::Bottom,
                            ..
                        }
                    )
                }) {
                    registry.raise(surface.handle);
                }
            }
        }
        for handle in dialogs {
            registry.raise(handle);
        }
    }
    // Seated pages not on screen here take their revision as it lands.
    store.apply_offscreen(&output, |name| {
        targets
            .get(name)
            .is_some_and(|target| target.output == output || target.output.is_empty())
    });
}

thread_local! {
    /// The dialog holding the seat's keyboard, and the surface it took the
    /// keyboard from (to give back when it goes).
    static DIALOG_PRIOR: std::cell::RefCell<Option<(String, Option<WlSurface>)>> =
        const { std::cell::RefCell::new(None) };
}

struct EdgePrior {
    scene: String,
    handle: HandleId,
    iced: Option<HandleId>,
    client: Option<WlSurface>,
}

thread_local! {
    static EDGE_PRIOR: std::cell::RefCell<Option<EdgePrior>> = const { std::cell::RefCell::new(None) };
}

fn grab_edge_keyboard(state: &mut Loop, scene: &str, handle: HandleId) {
    let iced = state
        .inner
        .surface()
        .registry
        .as_ref()
        .and_then(|registry| registry.keyboard_focus());
    let keyboard = state.state.seat.seat.get_keyboard();
    let client = keyboard
        .as_ref()
        .and_then(|keyboard| keyboard.current_focus());
    EDGE_PRIOR.with_borrow_mut(|prior| {
        // Moving between edge pages while held preserves the original client.
        if let Some(prior) = prior.as_mut().filter(|prior| iced == Some(prior.handle)) {
            prior.scene = scene.into();
            prior.handle = handle;
        } else {
            *prior = Some(EdgePrior {
                scene: scene.into(),
                handle,
                iced: iced.filter(|id| *id != handle),
                client,
            });
        }
    });
    if let Some(keyboard) = keyboard {
        keyboard.set_focus(
            &mut state.state,
            None,
            smithay::utils::SERIAL_COUNTER.next_serial(),
        );
    }
}

pub(crate) fn release_edge_keyboard(state: &mut Loop, scene: &str) {
    let prior = EDGE_PRIOR.with_borrow_mut(|prior| {
        if prior.as_ref().is_some_and(|prior| prior.scene == scene) {
            prior.take()
        } else {
            None
        }
    });
    let Some(prior) = prior else { return };
    let Some(registry) = state.inner.surface_mut().registry.as_mut() else {
        return;
    };
    // A click/activation elsewhere owns focus now; never steal it back.
    if registry
        .keyboard_focus()
        .is_some_and(|id| id != prior.handle)
    {
        return;
    }
    let iced = prior.iced.filter(|id| registry.contains(*id));
    registry.set_keyboard_focus(iced);
    if iced.is_none()
        && let Some(keyboard) = state.state.seat.seat.get_keyboard()
        && keyboard.current_focus().is_none()
        && let Some(client) = prior.client.filter(|client| client.is_alive())
    {
        keyboard.set_focus(
            &mut state.state,
            Some(client),
            smithay::utils::SERIAL_COUNTER.next_serial(),
        );
    }
}

/// The dialog takes the PRIMARY seat's keyboard off the focused window, as a
/// click on compositor iced does (native_press focuses `None`) and as a
/// layer-surface dialog does (the window gets `leave`). The iced registry
/// focus is set separately; with the seat's focus empty, every later focus
/// of a window (a click, comp.window.focus, an activation, focus-on-map) is
/// a real change, and that change clears the registry focus (world
/// `surface_event`), so a stale iced focus never keeps a window's keys.
fn grab_seat_keyboard(state: &mut Loop, scene: &str) {
    let Some(keyboard) = state.state.seat.seat.get_keyboard() else {
        return;
    };
    let prior = keyboard.current_focus();
    if prior.is_some() {
        keyboard.set_focus(
            &mut state.state,
            None,
            smithay::utils::SERIAL_COUNTER.next_serial(),
        );
    }
    // A re-map while still held keeps the window first taken from.
    DIALOG_PRIOR.with_borrow_mut(|held| match held {
        Some((held_scene, _)) if held_scene == scene => {}
        _ => *held = Some((scene.to_owned(), prior)),
    });
}

/// Give the keyboard back to the window the dialog took it from, if nothing
/// has taken it since (the seat's focus is still empty) and that window's
/// surface is still alive.
fn release_seat_keyboard(state: &mut Loop) {
    let Some((_, prior)) = DIALOG_PRIOR.with_borrow_mut(Option::take) else {
        return;
    };
    let Some(keyboard) = state.state.seat.seat.get_keyboard() else {
        return;
    };
    if keyboard.current_focus().is_none()
        && let Some(prior) = prior.filter(|surface| surface.is_alive())
    {
        keyboard.set_focus(
            &mut state.state,
            Some(prior),
            smithay::utils::SERIAL_COUNTER.next_serial(),
        );
    }
}

thread_local! {
    /// Scenes registered as content sources.
    static SOURCED: std::cell::RefCell<std::collections::BTreeSet<String>> =
        const { std::cell::RefCell::new(std::collections::BTreeSet::new()) };
}

/// Side content accepts keyboard focus even without a text field (calendar:
/// Escape and outside-click dismissal). Fieldless horizontal furniture is
/// keyboard-transparent: a click on the bottom panel
/// reaches it but neither moves keyboard focus nor deactivates the focused
/// window, so the taskbar sees the window the user is in and a task click
/// minimises it (Quoin's rule, quoin_scenes_gate Q2).
pub(crate) fn takes_keyboard(tree: &scene::ResolvedScene) -> bool {
    matches!(scene_edge(tree), Edge::Left | Edge::Right)
        || tree.nodes.values().any(|node| node.family == "field")
}

/// A surface's area in physical px (a full redraw's damage).
fn area(rect: Rectangle<i32, Physical>) -> u64 {
    u64::try_from(rect.size.w.max(0)).unwrap_or(0) * u64::try_from(rect.size.h.max(0)).unwrap_or(0)
}

/// A scene's content-source id: `scene_<name>` with `-` as `_`. The comp
/// service's source grammar allows `-`, but a prop path segment does not
/// (`[a-z0-9_]`, SPEC 07),
/// so `sources.scene-gate` could never be read by path. Injective: scene names
/// are `[a-z][a-z0-9-]{1,30}` and never contain `_`.
pub fn source_id(scene: &str) -> String {
    format!("scene_{}", scene.replace('-', "_"))
}

/// `shell.scene.layout` for a loaded scene (see `layout.rs` for the shape).
/// Measured from the live surface when it is mapped; otherwise
/// `visible:false` with the last rect drawn (or the authored size).
pub(crate) fn layout(
    store: &SceneStore,
    surfaces: &BTreeMap<String, Surface>,
    placed: &BTreeMap<String, (f32, f32, f32, f32)>,
    state: &mut Loop,
    scene: &str,
    node: Option<&str>,
) -> Result<serde_json::Value, serde_json::Value> {
    use serde_json::json;
    let Some(entry) = store.scene(scene) else {
        return Err(
            json!({"error_code":"NOT_FOUND", "message":format!("no scene named {scene} is loaded"), "scene":scene}),
        );
    };
    let tree = entry.tree();
    let dialog = is_dialog(tree);
    let output = if dialog {
        store
            .dialog_seat()
            .filter(|seat| seat.scene == scene)
            .map(|seat| seat.output.clone())
    } else {
        store
            .pages()
            .seat(&page_id(tree))
            .map(|seat| seat.output.clone())
    };
    // Seats already use the connector name, as Quoin does.
    let (x, y, w, h) = placed.get(scene).copied().unwrap_or_else(|| {
        let (w, h, _, _) = dialog_geometry(tree);
        if dialog {
            (0.0, 0.0, w, h)
        } else {
            (0.0, 0.0, 0.0, 0.0)
        }
    });
    let mut reply = json!({
        "scene": scene,
        "revision": entry.revision(),
        "applied_revision": entry.applied(),
        "visible": false,
        "surface": {
            "kind": if dialog { "dialog" } else { "edge" },
            "edge": (!dialog).then(|| scene_edge(tree).as_str()),
            "output": output,
            "x": x, "y": y, "w": w, "h": h,
        },
        // compd's extension: the opaque page colour painted under the scene
        // (`#rrggbbaa`, dark `secondary` for edges, `base` for dialogs), null
        // while not drawn.
        "page": null,
        "nodes": {},
        "instances": {},
        "chrome": {},
    });
    let Some(surface) = surfaces.get(scene) else {
        return Ok(reply);
    };
    if entry.mounted().is_none() {
        return Ok(reply);
    }
    // Readback belongs to this scene's output, which may differ from the
    // cursor output selected for this Bus request.
    let scale = surface.factor;
    let Some(registry) = state.inner.surface_mut().registry.as_mut() else {
        return Ok(reply);
    };
    let Some(instance) = registry.instance_mut(IcedHandle::<SceneUi>::from_id(surface.handle))
    else {
        return Ok(reply);
    };
    let applied = instance.ui().revision();
    let page = crate::view::hex_of(instance.ui().page());
    let factor = instance.runtime().scale_factor() / scale;
    let mut measure = crate::layout::Measure::default();
    instance.runtime_mut().operate(&mut measure);
    let (nodes, instances, chrome) = crate::layout::measured(tree, &measure.found, factor, node);
    reply["visible"] = json!(true);
    reply["applied_revision"] = json!(applied);
    reply["page"] = json!(page);
    reply["nodes"] = nodes;
    reply["instances"] = instances;
    reply["chrome"] = chrome;
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carousel_pages_remain_resident_when_hidden_and_only_unload_or_migration_drops_them() {
        let mut store = SceneStore::default();
        load(&mut store, &edge("calendar", "right", Some(360)), "DP-1");
        load(&mut store, &edge("notes", "right", Some(380)), "DP-1");
        let mut panels = panels_for(&store, "DP-1");
        let mapped = targets(&store, &panels);
        let resident = resident_targets(&store, &panels, &mapped);
        assert_eq!(
            resident.len(),
            2,
            "both pages prewarm before the first switch"
        );
        panels.page_set("DP-1", Edge::Right, "scene-notes").unwrap();
        let mapped = targets(&store, &panels);
        let switched = resident_targets(&store, &panels, &mapped);
        assert_eq!(
            rect(&resident["calendar"].place, (1920.0, 1080.0)).2,
            rect(&switched["calendar"].place, (1920.0, 1080.0)).2
        );
        assert_eq!(
            rect(&resident["notes"].place, (1920.0, 1080.0)).2,
            rect(&switched["notes"].place, (1920.0, 1080.0)).2
        );
        assert!(keep_surface(&store, &mapped, "calendar", "DP-1"));
        assert!(keep_surface(&store, &mapped, "notes", "DP-1"));
        assert!(!keep_surface(&store, &mapped, "calendar", "HDMI-1"));
        assert!(!keep_surface(&store, &mapped, "unloaded", "DP-1"));
    }

    #[test]
    fn autofocus_waits_for_physical_viewport_and_output_scale() {
        let wanted = Size::from((1100, 2030));
        assert!(!viewport_ready(
            iced_core::Size::new(440, 812),
            1.0,
            wanted,
            2.5
        ));
        assert!(!viewport_ready(
            iced_core::Size::new(1100, 2030),
            1.0,
            wanted,
            2.5
        ));
        assert!(viewport_ready(
            iced_core::Size::new(1100, 2030),
            2.5,
            wanted,
            2.5
        ));
    }
    use crate::store::SceneMount;
    use crate::verb::SceneVerb;
    use serde_json::json;

    fn edge(name: &str, edge: &str, w: Option<u32>) -> String {
        let w = w.map_or_else(String::new, |w| format!(",\"w\":{w}"));
        format!(
            "---\nscene: 1\nname: {name}\ncitizen: c\nwindow: {{\"kind\":\"edge\",\"edge\":\"{edge}\"{w}}}\n---\n```mix\nroot: {{widget: \"column\", children: []}}\n```\n"
        )
    }

    fn load(store: &mut SceneStore, source: &str, output: &str) {
        let mut mount = SceneMount {
            output,
            owner: "loader",
            accepted_at: 1,
        };
        let done = store.dispatch(
            SceneVerb::Load,
            source,
            &serde_json::Value::Null,
            &mut mount,
        );
        assert_eq!(done.rc, 0, "{}", done.body);
    }

    fn panels_for(store: &SceneStore, output: &str) -> crate::panels::Panels {
        let mut panels = crate::panels::Panels::default();
        panels.ensure(output, (1920.0, 1080.0));
        panels.sync(store);
        panels
    }

    /// Settle the panels' motion (Quoin's 200 ms travel).
    fn settle(panels: &mut crate::panels::Panels) {
        std::thread::sleep(std::time::Duration::from_millis(260));
        panels.tick();
    }

    #[test]
    fn only_the_active_page_of_a_shown_edge_is_placed_at_its_thickness() {
        let mut store = SceneStore::default();
        load(&mut store, &edge("bb", "right", Some(300)), "DP-1");
        load(&mut store, &edge("aa", "right", None), "DP-1");
        load(&mut store, &edge("cc", "left", Some(420)), "DP-1");
        let mut panels = panels_for(&store, "DP-1");
        panels.set_mode("DP-1", Edge::Right, "hidden").unwrap();
        settle(&mut panels);
        assert!(
            targets(&store, &panels).is_empty(),
            "left and explicitly hidden right draw nothing"
        );
        panels.set_mode("DP-1", Edge::Right, "pinned").unwrap();
        panels.set_mode("DP-1", Edge::Left, "pinned").unwrap();
        settle(&mut panels);
        let targets = targets(&store, &panels);
        // Same receipt here, so page-id order: the right edge shows scene-aa,
        // and only it.
        assert_eq!(targets.keys().collect::<Vec<_>>(), ["aa", "cc"]);
        let Place::Edge {
            edge,
            offset,
            extent,
            ..
        } = targets["cc"].place.clone()
        else {
            panic!()
        };
        assert!(
            edge == Edge::Left && offset.abs() < 0.5,
            "settled in: {offset}"
        );
        assert!(
            extent >= 420.0,
            "the page's authored width is the edge's minimum: {extent}"
        );
        let (x, y, w, h) = rect(&targets["cc"].place, (1920.0, 1080.0));
        assert!(x.abs() < 0.5 && (y, w, h) == (0.0, extent, 1080.0));
        panels.page_set("DP-1", Edge::Right, "scene-bb").unwrap();
        assert_eq!(
            super::targets(&store, &panels).keys().collect::<Vec<_>>(),
            ["bb", "cc"]
        );
    }

    #[test]
    fn a_dialog_is_fitted_and_centred_in_what_the_docked_edges_leave() {
        // Nothing docked: 880x620 centred on 1280x800.
        assert_eq!(
            dialog_rect(880.0, 620.0, usable_zone(&[], (1280.0, 800.0))),
            (200.0, 90.0, 880.0, 620.0)
        );
        // The launcher docked left (440) and the panel at the bottom (52):
        // the zone is 840x748 at (440, 0); 880 does not fit 840 - 48.
        let zone = usable_zone(
            &[(Edge::Left, 440.0), (Edge::Bottom, 52.0)],
            (1280.0, 800.0),
        );
        assert_eq!(zone, (440.0, 0.0, 840.0, 748.0));
        assert_eq!(dialog_rect(880.0, 620.0, zone), (464.0, 64.0, 792.0, 620.0));
        // Never below the least size.
        assert_eq!(
            dialog_rect(880.0, 620.0, (0.0, 0.0, 100.0, 100.0)).2,
            DIALOG_MIN
        );
    }

    #[test]
    fn side_panels_exclude_only_docked_horizontal_edges_on_their_own_output() {
        let mut store = SceneStore::default();
        for (name, edge_name) in [("side", "left"), ("top", "top"), ("bottom", "bottom")] {
            load(&mut store, &edge(name, edge_name, None), "DP-1");
        }
        load(&mut store, &edge("other", "right", None), "DP-2");
        let mut panels = panels_for(&store, "DP-1");
        panels.ensure("DP-2", (1920.0, 1080.0));
        panels.sync(&store);
        panels.set_mode("DP-1", Edge::Left, "pinned").unwrap();
        panels.set_mode("DP-2", Edge::Right, "pinned").unwrap();
        for mode in ["pinned", "docked", "hidden", "docked", "pinned"] {
            panels.set_mode("DP-1", Edge::Top, mode).unwrap();
            panels.set_mode("DP-1", Edge::Bottom, mode).unwrap();
            settle(&mut panels);
            let targets = targets(&store, &panels);
            let (_, y, _, h) = rect(&targets["side"].place, (1920.0, 1080.0));
            let zones = panels.zones("DP-1");
            let inset = |edge| {
                zones
                    .iter()
                    .find(|(e, _)| *e == edge)
                    .map_or(0.0, |(_, px)| *px)
            };
            assert_eq!(y, inset(Edge::Top));
            assert_eq!(h, 1080.0 - inset(Edge::Top) - inset(Edge::Bottom));
            assert_eq!(rect(&targets["other"].place, (1920.0, 1080.0)).3, 1080.0);
            if mode != "docked" {
                assert_eq!((y, h), (0.0, 1080.0));
            }
        }
    }

    #[test]
    fn a_dialog_is_placed_only_while_shown_and_centred_as_frozen() {
        let frozen: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/shell-verbs.json")).unwrap();
        let mut store = SceneStore::default();
        load(
            &mut store,
            frozen["shell.scene.load"]["request"]["source"]
                .as_str()
                .unwrap(),
            "DP-1",
        );
        let panels = crate::panels::Panels::default();
        assert!(
            targets(&store, &panels).is_empty(),
            "loaded dialogs wait unmapped"
        );
        assert!(store.set_dialog_visible("editor", true));
        let target = &targets(&store, &panels)["editor"];
        assert_eq!(target.output, "DP-1");
        assert_eq!(
            target.frame(),
            Some(Frame {
                title: "Scene Editor".into()
            })
        );
        // The frozen layout: an 880x620 dialog at 520,230 on 1920x1080.
        let surface = &frozen["shell.scene.layout"]["reply"]["surface"];
        let (x, y, w, h) = rect(&target.place, (1920.0, 1080.0));
        assert_eq!(
            json!([x, y, w, h]),
            json!([surface["x"], surface["y"], surface["w"], surface["h"]])
        );
        store.set_dialog_visible("editor", false);
        assert!(targets(&store, &panels).is_empty());
    }

    #[test]
    fn an_unseated_scene_is_not_drawn() {
        let mut store = SceneStore::default();
        store
            .request(
                SceneVerb::Load,
                &edge("aa", "left", None),
                &serde_json::Value::Null,
            )
            .unwrap();
        let mut panels = panels_for(&store, "DP-1");
        let _ = panels.set_mode("DP-1", Edge::Left, "pinned");
        assert!(targets(&store, &panels).is_empty());
    }

    #[test]
    fn side_content_takes_the_keyboard_and_horizontal_furniture_does_not() {
        let mut store = SceneStore::default();
        load(&mut store, &edge("bar", "bottom", None), "DP-1");
        let typed = "---\nscene: 1\nname: find\ncitizen: c\nwindow: {\"kind\":\"edge\",\"edge\":\"left\"}\n---\n```mix\nroot: {widget: \"column\", children: [\"q\"]}\nq: {widget: \"field\", value: \"\"}\n```\n";
        load(&mut store, typed, "DP-1");
        assert!(
            !takes_keyboard(store.scene("bar").unwrap().tree()),
            "a panel click leaves the keyboard where it is"
        );
        assert!(takes_keyboard(store.scene("find").unwrap().tree()));
        load(&mut store, &edge("calendar", "right", None), "DP-1");
        assert!(takes_keyboard(store.scene("calendar").unwrap().tree()));
    }

    #[test]
    fn source_ids_are_prop_path_segments() {
        assert_eq!(source_id("gate"), "scene_gate");
        assert_eq!(source_id("model-test"), "scene_model_test");
        assert!(ui::source::valid_id(&source_id("a-b-c-0")));
    }

    #[test]
    fn rects_convert_to_physical_pixels_at_the_output_scale() {
        let r = physical((10.0, 20.0, 300.5, 0.2), 2.0);
        assert_eq!((r.loc.x, r.loc.y, r.size.w, r.size.h), (20, 40, 601, 1));
    }

    #[test]
    fn right_edge_stays_on_the_right_at_fractional_kms_scale() {
        let logical = (3840.0 / 2.5, 2160.0 / 2.5);
        let place = |fraction: f32| Place::Edge {
            edge: Edge::Right,
            offset: -440.0 * (1.0 - fraction),
            extent: 440.0,
            top: 0.0,
            bottom: 0.0,
        };
        let settled = physical(rect(&place(1.0), logical), 2.5);
        assert_eq!(
            (settled.loc.x, settled.loc.y, settled.size.w, settled.size.h),
            (2740, 0, 1100, 2160)
        );
        assert_eq!(settled.loc.x + settled.size.w, 3840);
        for fraction in [0.0, 0.25, 0.5, 0.75, 1.0] {
            assert!(physical(rect(&place(fraction), logical), 2.5).loc.x >= settled.loc.x);
        }
    }
}
