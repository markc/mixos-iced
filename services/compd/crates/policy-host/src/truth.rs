//! `compd.truth`: compd's own view, for the Bus-truth
//! comparator. Read straight from `CompState` and the engine, deliberately
//! NOT through [`crate::project`], so a projection bug cannot agree with
//! itself.
//!
//! ```json
//! {
//!   "revision": 7,            // CompState::content_revision, folded with the
//!                             // engine geometry the windows rows carry (below)
//!   "content_revision": 7,    // CompState::content_revision alone
//!   "surface_count": 1,       // non-dormant registry records
//!   "windows": [{"id", "generation", "app_id", "title", "workspace", "visible",
//!                "minimized", "maximized", "x", "y", "width", "height"}],
//!                             // x/y/width/height: the window geometry in the Space
//!                             // mapped xdg toplevels, by id; `visible` is the draw's
//!                             // own predicate (`DrawWindow::visible` + is_drawn)
//!   "workspaces": {"count", "current"},   // CompState's model
//!   "commits": {"<id>": n},   // buffer commits per window (frame-callback oracle)
//!   "configured": {"<id>": [w, h]},   // the size the client last acked (xdg), else
//!                             // its geometry: a maximise is told the output size
//!                             // even when the client keeps drawing its own
//!   "focus": {"keyboard": 1 | null, "window": {"id", "generation"} | null},
//!   "corners": {             // CompState's corner state
//!     "enabled", "deadzone_px", "dwell_ms", "velocity_max_px_s",
//!     "affordance", "discovery",   // the configuration (in `revision`)
//!     "holders": true,              // HOLDER_PLANE_AVAILABLE
//!     "enforced": {top, bottom, left, right},  // concealed layers per edge
//!     "held": {top, bottom, left, right},      // explicit holds per edge
//!     "output": "o_<slug>" | null,  // the output the detector samples
//!     "contact": "tl" | null,       // the hotspot the pointer is in
//!     "engaged": "tl" | null,       // dwelled or pushed into
//!     "owned_buttons": [272]        // presses taken on a corner
//!   },
//!   "input": {"agent_pointer": [x, y] | null,  // injection, host Space
//!             "bindings": {"fired": n, "last": "WorkspaceJump" | null}},  // chords
//!                             // the binding filter took; no props leaf
//!   "panels": [{output, edge, surface, id, mode, verdict, held, pointer,
//!               focused, owned, stalled, probing, enforced, commits}],
//!   "concealed": [id],         // layers enforcement hides (the conceal marker's set)
//!   "outputs": {"o_<slug>": {"usable": {x, y, width, height}}},  // CompState's
//!                             // usable areas, as `outputs.*.usable` reads
//!   "output_generations": {"o_<slug>": n},  // CompState's; no props leaf,
//!                             // so the comparator does not compare it
//!   "engine": {
//!     "windows": [1],         // drawn xdg toplevels in the engine's Spaces, as registry ids
//!     "keyboard": 1 | null    // the seat's live keyboard focus, as a registry id
//!   },
//!   "chrome": {              // server-side chrome (decor), as drawn
//!     "installed": true,     // a theme is installed (startup)
//!     "ssd_enabled": true,   // the decorations_ssd preference, as the handlers read it
//!     "style": "mac", "tokens": "file" | "embedded",
//!     "titlebar_focused": [r, g, b, a], "titlebar_unfocused": [r, g, b, a],
//!     "extents": {top, left, right, bottom},   // logical px
//!     "corner_radius": 12,   // logical px (0 drawn when maximised)
//!     "decorated": [1],        // drawn toplevels compd draws chrome for, as registry ids
//!     "windows": {"1": {"frame": [x, y, w, h],   // what the content is cut to
//!                 "titlebar": [x, y, w, h], "buttons": {"close": [x, y, w, h],
//!                 "maximize": [..], "minimize": [..]}}}   // host Space, logical px
//!   }                          // (no props leaf: gates read it, the comparator does not)
//!   "session_lock": {"phase": "none" | "locking" | "locked" | "orphaned",
//!                    "generation": n, "surfaces": [id]}   // dispatcher's lifecycle
//! }
//! ```
//!
//! `focus` is what the registry's focus events left; `engine` is what the
//! engine itself holds. A comparator that brackets its Bus reads between two truth
//! calls with the same `revision` knows every read saw this state. That
//! includes window geometry: a client commit that changes its window size
//! (an unfullscreen's restore configure acked late) moves no registry
//! content, so `revision` folds a digest of every row's engine half (x, y,
//! width, height, visible, maximised) into the content revision; a commit
//! between the two truth calls then reads as a change and the comparator
//! retries instead of diffing two moments.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use comp_model::snapshot::output_key;
use surfaces::SurfaceRole;
use protocols::window::ident::ident;
use dispatcher::wire::trait_::surface_event::SurfaceHandle;
use dispatcher::wire::trait_::wire_trait::WireTrait;
use world::state::Loop;

pub fn truth(lp: &Loop) -> Value {
    let comp = &lp.inner.comp;
    let registry = &comp.registry;
    // Each engine window's registry id: whether the draw would show it, whether the
    // client committed maximised, and its window geometry in the Space.
    let drawn: BTreeMap<u64, (bool, bool, [f32; 4])> = lp
        .inner
        .all_world_spaces()
        .iter()
        .flat_map(|space| {
            space
                .state
                .elements()
                .map(move |window| (window, space.state.element_location(window)))
        })
        .filter_map(|(window, location)| {
            let id = SurfaceHandle::of_window(window).and_then(|handle| registry.id_for_handle(&handle))?;
            let location = location.unwrap_or_default();
            let size = window.geometry().size;
            Some((
                id.0,
                (
                    crate::control::drawn(lp, window),
                    crate::control::committed_maximized(window),
                    [location.x as f32, location.y as f32, size.w as f32, size.h as f32],
                ),
            ))
        })
        .collect();
    let mut commits = BTreeMap::new();
    let configured: BTreeMap<String, [i32; 2]> = lp
        .inner
        .all_world_spaces()
        .iter()
        .flat_map(|space| space.state.elements())
        .filter_map(|window| {
            let id = SurfaceHandle::of_window(window).and_then(|handle| registry.id_for_handle(&handle))?;
            let size = window
                .toplevel()
                .and_then(|toplevel| toplevel.with_committed_state(|state| state.and_then(|state| state.size)))
                .unwrap_or_else(|| window.geometry().size);
            Some((id.0.to_string(), [size.w, size.h]))
        })
        .collect();
    let windows: Vec<Value> = registry
        .surface_rows()
        .filter(|record| record.role() == SurfaceRole::Toplevel && record.mapped())
        .map(|record| {
            commits.insert(record.id().0.to_string(), comp.commits(record.id()));
            let engine = drawn.get(&record.id().0).copied().unwrap_or((false, false, [0.0; 4]));
            json!({
                "id": record.id().0,
                "generation": record.generation(),
                "app_id": record.app_id().map(|value| value.to_string()),
                "title": record.title().map(|value| value.to_string()),
                "workspace": record.workspace().unwrap_or(0),
                "visible": engine.0,
                "minimized": record.minimized(),
                "maximized": engine.1,
                "x": engine.2[0],
                "y": engine.2[1],
                "width": engine.2[2],
                "height": engine.2[3],
            })
        })
        .collect();
    let focus_window = comp
        .focused()
        .and_then(|id| registry.get(id))
        .filter(|record| record.mapped() && record.role().managed_toplevel())
        .map(|record| json!({"id": record.id().0, "generation": record.generation()}));
    let mut engine_windows: Vec<u64> = lp
        .inner
        .all_world_spaces()
        .iter()
        .flat_map(|space| space.state.elements())
        .filter(|window| window.toplevel().is_some() && ident::is_drawn(window))
        .filter_map(SurfaceHandle::of_window)
        .filter_map(|handle| registry.id_for_handle(&handle))
        .map(|id| id.0)
        .collect();
    engine_windows.sort_unstable();
    engine_windows.dedup();
    // Each decorated window with where its chrome parts are in its Space, so
    // a gate can aim injected input at a titlebar or a button.
    let mut decorated: BTreeMap<u64, Value> = BTreeMap::new();
    for space in lp.inner.all_world_spaces().iter() {
        for window in space.state.elements() {
            if !ident::is_drawn(window) || !decor::window::decorated(window) {
                continue;
            }
            let Some(id) = SurfaceHandle::of_window(window).and_then(|handle| registry.id_for_handle(&handle))
            else {
                continue;
            };
            let rect = |r: smithay::utils::Rectangle<f64, smithay::utils::Logical>| {
                json!([r.loc.x, r.loc.y, r.size.w, r.size.h])
            };
            let parts = space.state.element_location(window).and_then(|origin| {
                let size = world::camera::transform::translate::slot::expected_size(window)?;
                decor::window::parts(window, origin, size)
            });
            let entry = match parts {
                Some(parts) => {
                    let mut buttons = serde_json::Map::new();
                    for (button, r) in parts.buttons {
                        let name = match button {
                            decor::CaptionButton::Close => "close",
                            decor::CaptionButton::Maximize => "maximize",
                            decor::CaptionButton::Minimize => "minimize",
                        };
                        buttons.insert(name.to_string(), rect(r));
                    }
                    json!({"frame": rect(parts.frame), "titlebar": rect(parts.titlebar), "buttons": buttons})
                }
                None => json!({}),
            };
            decorated.insert(id.0, entry);
        }
    }
    let engine_keyboard = lp
        .state
        .seat
        .seat
        .get_keyboard()
        .and_then(|keyboard| keyboard.current_focus())
        .and_then(|surface| comp.id_for_surface(&surface))
        .map(|id| id.0);
    json!({
        // Masked to 53 bits: JSON readers that decode numbers as doubles
        // must still compare it exactly.
        "revision": (comp.content_revision() ^ geometry_digest(&windows).rotate_left(17))
            & ((1u64 << 53) - 1),
        "content_revision": comp.content_revision(),
        "surface_count": registry.surface_rows().count(),
        "windows": windows,
        "workspaces": {
            "count": comp.workspaces.count,
            "current": comp.current_workspace(),
        },
        "commits": commits,
        "configured": configured,
        "focus": {
            "keyboard": comp.focused().map(|id| id.0),
            "window": focus_window,
        },
        "corners": corners(lp),
        "input": {
            "agent_pointer": comp.injection.agent_pointer.map(|(x, y)| [x, y]),
            "bindings": {
                "fired": comp.bindings.fired,
                "last": comp.bindings.last,
            },
        },
        "panels": crate::panel::truth(lp),
        "concealed": comp.panels.enforced.iter().map(|id| id.0).collect::<Vec<_>>(),
        "outputs": comp
            .usable
            .iter()
            .map(|(name, area)| {
                (
                    output_key(name),
                    json!({"usable": {
                        "x": area.loc.x as f32,
                        "y": area.loc.y as f32,
                        "width": area.size.w as f32,
                        "height": area.size.h as f32,
                    }}),
                )
            })
            .collect::<serde_json::Map<String, Value>>(),
        "output_generations": comp
            .output_generations
            .iter()
            .filter(|(_, entry)| entry.signature.is_some())
            .map(|(name, entry)| (output_key(name), json!(entry.generation)))
            .collect::<serde_json::Map<String, Value>>(),
        "engine": {
            "windows": engine_windows,
            "keyboard": engine_keyboard,
        },
        "chrome": chrome(decorated),
        "session_lock": {
            "phase": lp.state.session_lock.phase().name(),
            "generation": lp.state.session_lock.generation(),
            "surfaces": lp
                .state
                .session_lock
                .surfaces()
                .filter_map(|(_, lock)| comp.id_for_surface(lock.wl_surface()).map(|id| id.0))
                .collect::<Vec<_>>(),
        },
    })
}

/// The chrome theme compd installed and draws with, so a pixel gate reads its
/// expected colours from compd itself.
fn chrome(windows: BTreeMap<u64, Value>) -> Value {
    let decorated: Vec<u64> = windows.keys().copied().collect();
    let windows: serde_json::Map<String, Value> =
        windows.into_iter().map(|(id, entry)| (id.to_string(), entry)).collect();
    let ssd_enabled = decor::window::ssd_enabled();
    let Some(theme) = decor::window::installed() else {
        return json!({"installed": false, "ssd_enabled": ssd_enabled, "decorated": decorated});
    };
    let deco = &theme.deco;
    let e = decor::DecoExtents::of(deco);
    json!({
        "installed": true,
        "ssd_enabled": ssd_enabled,
        "style": deco.style.name(),
        "tokens": match theme.tokens {
            decor::TokenSource::File => "file",
            decor::TokenSource::Embedded => "embedded",
        },
        "titlebar_focused": decor::theme::rgba8(deco.colors.titlebar_focused),
        "titlebar_unfocused": decor::theme::rgba8(deco.colors.titlebar_unfocused),
        "extents": {"top": e.top, "left": e.left, "right": e.right, "bottom": e.bottom},
        "corner_radius": deco.metrics.corner_radius,
        "decorated": decorated,
        "windows": windows,
    })
}

/// CompState's corner configuration and detector state.
fn corners(lp: &Loop) -> Value {
    let corners = &lp.inner.comp.corners;
    let config = corners.config();
    let (enforced, held) = crate::panel::edge_counts(lp);
    json!({
        "enabled": config.enabled,
        "deadzone_px": config.deadzone_px,
        "dwell_ms": config.dwell_ms,
        "velocity_max_px_s": config.velocity_max_px_s,
        "affordance": config.affordance,
        "discovery": config.discovery,
        "holders": comp_model::observation::HOLDER_PLANE_AVAILABLE,
        "enforced": enforced,
        "held": held,
        "output": corners.output().map(output_key),
        "contact": corners.contact().map(|corner| corner.name()),
        "engaged": corners.engaged().map(|corner| corner.name()),
        "owned_buttons": corners.owned_buttons(),
    })
}

/// FNV-1a over the engine half of every windows row, in row order: what the
/// Bus-truth comparator compares beyond registry content.
fn geometry_digest(windows: &[Value]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for row in windows {
        for key in ["id", "x", "y", "width", "height", "visible", "maximized"] {
            feed(row.get(key).map_or_else(String::new, Value::to_string).as_bytes());
            feed(b"|");
        }
    }
    hash
}
