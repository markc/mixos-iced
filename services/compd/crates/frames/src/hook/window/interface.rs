use smithay::backend::renderer::gles::GlesRenderer;
use dispatcher::state::state::RedrawReason;
use smithay::desktop::Window;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle, Size};
use smithay::wayland::compositor::with_states;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::xdg::{
    SurfaceCachedState, ToplevelCachedState, XdgToplevelSurfaceData,
};
use uuid::Uuid;
use world::camera::transform::translate::slot;
use world::state::state::CoordinateTrait;
use world::state::{Loop, Transform};
use world::window::interface::record::window::LoopWindow;
use world::window::lifecycle::event::event::WindowLifecycleEvent;
use protocols::window::find::find;
use protocols::window::ident::ident;
use protocols::window::shell::shell;
/// Activate `window` on behalf of an external request (e.g. a dock via wlr foreign-toplevel
/// `activate`): raise + activate + give it keyboard focus.
///
/// The camera is pinned to identity, so activation does not move the view.
fn activate_window(_loop: &mut Loop, window: Window) {
    // An override-redirect X11 window is never activated or focused (a dock or
    // xdg-activation cannot reach one through the registry; this is the
    // engine-side backstop).
    if window.x11_surface().is_some_and(|x11| x11.is_override_redirect()) {
        return;
    }
    // Cross-world activation: if the window lives on another world, switch to it FIRST.
    let hosted = _loop.inner.worlds.spawn_target();
    let cross_world = matches!(_loop.inner.world_of_window(&window), Some(w) if w != hosted);
    if cross_world {
        if let Some(w) = _loop.inner.world_of_window(&window) {
            _loop.inner.switch_to_world(w);
        }
    }

    _loop.inner.space_state_mut().state.raise_element(&window, true);
    // Activate the target and DEACTIVATE every other window across all worlds (not just
    // the hosted one). A cross-world activate otherwise leaves the previously-focused
    // window still `activated` in another world; the foreign mirror advertises all worlds
    // (when `all_worlds`), so that stale flag makes the target's re-activation a no-op
    // diff and sfwbar never sees it become focused.
    _loop.inner.set_activated_exclusive(Some(&window));
    // The `Option` is passed STRAIGHT THROUGH, and `None` clearing the keyboard focus
    // is the point rather than an oversight — do not "fix" this into an `if let`.
    //
    // `set_activated_exclusive` above has just deactivated every other window across
    // every world. Skipping the focus call when there is no surface would leave the
    // PREVIOUS window holding the keyboard while the UI paints this one as active:
    // keystrokes would go to a window the user has just been shown as inactive, which
    // is the one outcome worth ruling out. Clearing keeps the two in step — nobody is
    // shown active without the keyboard, and nobody receives it while shown inactive.
    //
    // Reachable now, where it was not before: every Space element used to be an xdg
    // toplevel, whose surface is always present. An X11 window has none until Xwayland
    // associates one, which can still be pending when a dock activates a window that
    // has only just mapped (and, by timing, at map itself). Such a window is focused
    // when Xwayland associates its surface instead: the latch below is taken in
    // dispatcher `surface_associated` (compd, nested_smoke 14q).
    let surface = ident::surface(&window);
    // No surface yet (an X11 window Xwayland has not associated): the focus
    // lands at `surface_associated` instead (dispatcher), if this window is
    // still the activated one then.
    if surface.is_none()
        && let Some(x11) = window.x11_surface()
    {
        protocols::window::shell::shell::defer_focus_until_associated(x11);
    }
    if let Some(keyboard) = _loop.state.seat.seat.get_keyboard() {
        let serial = smithay::utils::SERIAL_COUNTER.next_serial();
        keyboard.set_focus(&mut _loop.state, surface, serial);
    }
}

/// Generally all hooks are temporary - they indicate something immediate is being deferred(due to complex ownership.)
/// This hook is temporary because it wires the WireTrait impl and WireObject state.
pub fn hook(_loop: &mut Loop, renderer: &mut GlesRenderer) {
    _apply_toplevel_drag_moves(_loop);

    let process = std::mem::take(
        &mut _loop.inner.window_lifecycle_mut()
            .incoming,
    );
    // generally no-op. _state.inner.window.incoming becomes Vec::default().
    // _loop.inner.window.incoming.clear();

    if process.len() == 0 {
        return;
    }

    for item in process {
        match item {
            world::window::lifecycle::event::event::WindowLifecycleEvent::InitialMap(
                window,
            ) => {
                _initial_mapped(_loop, window);
            }
            WindowLifecycleEvent::Withdrawn(uuid, discard_placeholder) => {
                _withdraw(_loop, uuid, renderer, discard_placeholder);
            }
            WindowLifecycleEvent::Fullscreen(window, fullscreen) => {
                world::window::interface::draw::fullscreen::fullscreen_set(
                    _loop, window, fullscreen,
                );
            }
            WindowLifecycleEvent::Activate(window, _origin) => {
                activate_window(_loop, window);
            }
            // A settled toplevel drag needs nothing: the window is already where
            // the drag moves put it.
            WindowLifecycleEvent::DragSettled(_surface) => {}
            WindowLifecycleEvent::Destroyed(uuid, activation, discard_placeholder) => {
                _destroy(_loop, uuid, renderer, discard_placeholder);

                // CHECK: Token is cleared on surface deletion. if a splash screen uses this token, it will be removed and no longer valid.
                //
                // Every token the surface was named by, not just one: a window that
                // presented several (a launch token, then a later self-activation) would
                // otherwise leave the rest behind, and no placeholder holds them so the
                // reachability sweep in `placeholder.interface::handler` cannot either.
                for activation in &activation {
                    _loop
                        .state
                        .xdg_activation
                        .xdg_activation
                        .remove_token(&activation.token);
                }
            }
        }
    }

    // Map/unmap/destroy/fullscreen may have changed the captured window set.
    recorder::interface::interface::on_window_geometry_changed(_loop);

    _loop.schedule_redraw(RedrawReason::WindowState);
}

/// Apply the positions an `xdg_toplevel_drag_v1` grab queued for the window it
/// is carrying.
///
/// Once per frame, not once per motion. The grab cannot place the window itself
/// (`Dispatch` owns no `Space`) so it queues a world position on every motion,
/// but that position is only ever OBSERVED at render — applying it more often
/// just overwrites values nothing has read, so a higher input rate than the
/// frame rate buys no smoothness. Running here also gives it one defined place
/// in the frame: ahead of the lifecycle queue below, so a drop settling this
/// frame reads the position this frame put down.
///
/// The value is already a world coordinate — `map_element` stores camera-independent
/// coordinates and the camera is applied at render time — so it goes in
/// unprojected.
fn _apply_toplevel_drag_moves(state: &mut Loop) {
    for (surface, location) in std::mem::take(&mut state.state.toplevel_drag.moves) {
        // Resolved across ALL worlds, and mapped into the one that actually holds
        // it. A drag can outlive the world it started in, and every `space_state`
        // accessor is bound to `spawn_target` — so after a switch the carried
        // window is simply not found, and the move is dropped without a trace:
        // the window freezes where it was and the drop lands at a stale position.
        let Some((world, window)) = state.inner.window_of_surface(&surface) else {
            continue;
        };
        state
            .inner.space_of_mut(world)
            .state
            .map_element(window, location.to_i32_round(), false);
    }
}

fn _initial_mapped(state: &mut Loop, window: Window) {
    // The window may already be GONE, and mapping it now would RESURRECT it.
    //
    // `InitialMap` is queued when the window is placed and applied later, so a destroy
    // can land in between — routine for X11, where a client maps at one size, unmaps and
    // re-maps at another within milliseconds. The drain removes the element from the
    // Space immediately; this event's tail would put it straight back, and nothing would
    // remove it again, since the matching `Destroyed` only tears down the placeholder.
    //
    // Tested by presence in the Space: the drain maps the window before queueing this, so
    // "still an element" is exactly "not torn down since".
    if state.inner.space_state().state.element_location(&window).is_none() {
        warn!("initial map for a window no longer in the Space; dropping it");
        return;
    }
    // Clients ask for tearing through wp_tearing_control_v1; nothing is
    // inferred from the window's process.
    // A toplevel that maps while an `xdg_toplevel_drag_v1` is ALREADY carrying it
    // is a torn-off tab, not a new window: the user is holding it, so it belongs
    // under the cursor at its attach offset. Centring it and letting the next
    // pointer motion correct it is exactly the visible flick to mid-screen and
    // back that a tab tear shows.
    //
    // Same arithmetic as the grab (`state.grab/grab.drag.state`): the pointer's
    // location is already a world coordinate, so the surface-local offset subtracts
    // straight off it with no projection.
    //
    // Resolved BEFORE the restore below, because it decides whether the restore's
    // verdict may be honoured at all.
    let carried = state
        .state
        .toplevel_drag
        .carried_with_offset()
        .filter(|(surface, _)| find::is_surface(&window, surface))
        .and_then(|(_, offset)| {
            let pointer = state.state.seat.seat.get_pointer()?;
            let at = pointer.current_location() - offset.to_f64();
            Some((at.x, at.y))
        });

    // The slot we lock the window to. For X11 this is its CONFIGURE size, not its
    // `geometry`: smithay subtracts `_GTK_FRAME_EXTENTS` from the latter, so locking
    // that would hand a CSD client a configure one shadow smaller than it asked for.
    let mut geometry = window.geometry();
    geometry.size = shell::configured_size(&window);

    // An X11 CHILD is placed against its parent, not against the camera.
    //
    // A menu, tooltip or dialog is positioned by its client in X space — a space the
    // compositor knows nothing about and must not read as a canvas location. What carries across is the
    // DIFFERENCE from its parent (`child::parent_offset`), so the child lands beside the
    // window it belongs to wherever that window is on the canvas. Both sides are storage
    // coordinates, so nothing here converts between spaces.
    //
    // `carried` wins: a window being dragged tracks the cursor, which is a stronger
    // statement about where it goes than its parentage.
    //
    // The fallback parent — the last hovered or focused X11 window — is offered ONLY to
    // an ephemeral window. An X11 top-level that declares no `WM_TRANSIENT_FOR` is not a
    // child of anything, and anchoring one to whatever the pointer last touched opens a
    // freshly launched app beside it instead of on the camera. A menu that
    // `is_popup_x11` declines still needs the anchor, and X11 does not require it to name
    // a parent either.
    let fallback = ident::is_ephemeral_x11(&window)
        .then(|| state.state.xwayland.map_position_parent_hover.or(state.state.xwayland.map_position_parent_focus))
        .flatten();
    let parented = carried.is_none().then(|| {
        let space = &state.inner.space_state().state;
        let (parent, offset) = protocols::window::child::child::parent_offset(
            space.elements(),
            &window,
            fallback,
        )?;
        space.element_location(&parent).map(|at| at + offset)
    }).flatten();

    // ONE position, resolved before the `Transform` is built: the map location and the
    // placeholder record below must not be derived separately, or a parented window opens
    // in one place and its placeholder stands in another. Storage coordinates are what
    // `Transform::pos` holds, so a parented point converts exactly.
    //
    // The unparented case centres on the ACTIVE monitor's camera (the output under the
    // cursor), NOT `camera_mut()`. This hook drains the InitialMap queue from inside the
    // per-output render loop, so `render_output` is pinned to whichever output is being
    // drawn — `camera_mut()`/`current_output_key()` would resolve THAT output's camera and
    // spawn the window on the wrong monitor.
    let (x, y) = carried
        .or_else(|| parented.map(|p| (p.x as f64, p.y as f64)))
        .unwrap_or_else(|| {
            let cam = state.inner.active_camera().transform.position();
            (
                cam.x - geometry.size.w as f64 / 2.0,
                cam.y - geometry.size.h as f64 / 2.0,
            )
        });

    // A managed, unparented X11 toplevel takes the X11 placement (stated origin
    // inside the usable area, else cascade; chrome pushes it in; size clamped
    // by WM_NORMAL_HINTS and the room left). Everything else keeps the clamp
    // into the usable area, clear of the panels' exclusive zones.
    let x11_placed = (carried.is_none() && parented.is_none())
        .then(|| world::comp::x11_place::initial(state, &window))
        .flatten();
    if let Some((_, size)) = x11_placed {
        geometry.size = size;
    }
    let (x, y) = if let Some((origin, _)) = x11_placed {
        (origin.x as f64, origin.y as f64)
    } else if carried.is_none() && parented.is_none() {
        world::comp::usable::inside_usable(state, (x, y), geometry.size)
    } else {
        (x, y)
    };
    let t: Transform = ((x, y), state.size_ctx_all()).into();
    let at = t.into_storage_point();

    state.inner.space_state_mut().state.map_element(window.clone(), at, false);
    // The comp registry's map edge for a window: placed, here, at frame time
    // (the lifecycle stage is not moved).
    if let Some(handle) = dispatcher::wire::trait_::surface_event::SurfaceHandle::of_window(&window) {
        dispatcher::wire::trait_::wire_trait::WireTrait::surface_event(
            &mut state.inner,
            dispatcher::wire::trait_::surface_event::SurfaceEvent::Placed(handle),
        );
    }

    // Dialog/child windows (a set xdg `parent` / X11 `WM_TRANSIENT_FOR`, e.g.
    // nautilus's merge-conflict window) size themselves — leave them `Auto`; lock+grace
    // the rest to the mapped size.
    if shell::has_parent(&window) {
        slot::set_expected_auto(&window);
    } else {
        slot::set_expected_size(&window, geometry.size);
        shell::stage(&window, geometry.size, false);
        shell::send(&window);
        dispatcher::wayland::compositor::place::arm_size_propagation(&window, geometry.size);
    }

    // Register the window in the spatial world's draw-order authority
    // (non-destructive; spawn = top of stack).
    if let Some(uuid) = window.uuid() {
        state.inner.register_drawable(uuid, world::order::track::base::DrawLayer::CONTENT);
    }

    // Focus-on-map: a managed window takes the keyboard (and the top of the
    // stack) when it first maps, without waiting for a click. An override-redirect X11
    // window is not a focus candidate.
    if !window.x11_surface().is_some_and(|x11| x11.is_override_redirect()) {
        activate_window(state, window.clone());
    }

    // A window may map ALREADY fullscreen, and nothing else notices.
    //
    // `fullscreen_request` is a REQUEST — an xdg `set_fullscreen`, or an X11
    // `_NET_WM_STATE` client message — sent about a window that already exists. A client
    // that wants to START fullscreen sets the state BEFORE mapping, which smithay reads
    // into `net_state` while handling the MapRequest, so no request ever arrives.
    //
    // Last, after placement and the slot: `fullscreen_set` reads `element_location` and
    // `slot::expected_size` to record what to restore to. Idempotent for the request path.
    if ident::states(&window).fullscreen {
        world::window::interface::draw::fullscreen::fullscreen_set(state, window, true);
    }
}

/// A window withdrew: it is out of the Space, so it stops being drawn and selectable —
/// but it is NOT gone.
///
/// Deliberately less than [`_destroy`]. The group membership and the introspection
/// registration are kept, because the process is still running and the window may map
/// again; only the two things that are meaningless for something not on the canvas are
/// dropped. The draw-order slot comes back on the remap (`readmit_x11` re-registers it),
/// which is also how the placeholder hands it over in the meantime.
// The only bookkeeping for a withdrawn or destroyed window is the draw-order
// slot.
fn _withdraw(state: &mut Loop, uuid: Uuid, _renderer: &mut GlesRenderer, _discard_placeholder: bool) {
    state.inner.remove_drawable(uuid);
}

fn _destroy(state: &mut Loop, uuid: Uuid, _renderer: &mut GlesRenderer, _discard_placeholder: bool) {
    // DrawOrder GC: drop the window from the draw-order authority.
    state.inner.remove_drawable(uuid);
}

pub struct TransformUpdate {
    pub position: Option<Point<i32, Logical>>,
    pub size: Option<Size<i32, Logical>>,
}

// There are a few places where size is set or modified:
// Initial placement:
//  this is the part of wayland configuration
//  where the client requests a specific size ( or any size )
//  the wayland server decides the size ( at dispatcher's code currently )
//  the wayland server sets the size locally and submits it to the client(which must behave with the decided size)
//
//
// Canvas events
// Grab events - not handled for now. they should be part of WireTrait
// Similarly for movements. location is simplified- the client has no idea. (not sure whether it can request it at all)
// and it is mapped in the place_window call. which should probably call refresh_geometry.
// better yet - to have the initial mapping use place_window and avoid the "WindowPlaced" marker. it is more likely the Window size marker.(eg. post configure size)
//
// This function is used to request a new size/position for a window
pub fn reform(state: &mut Loop, window: Window, transform_update: TransformUpdate) {
    _reform(state, window, transform_update, false);
}

pub fn reform_force(state: &mut Loop, window: Window, transform_update: TransformUpdate) {
    _reform(state, window, transform_update, true);
}

// `finish_resize` moved to `world::canvas::system` (the release input
// system that uses it) — it is Loop-free (smithay + `slot`), so it lives with its
// only caller rather than in this Loop-coupled crate (which a system can't depend
// on without a cycle via the orchestration focus accessors).

fn _reform(state: &mut Loop, window: Window, transform_update: TransformUpdate, force: bool) {
    if let Some(position) = transform_update.position {
        state
            .inner.space_state_mut()
            .state
            .map_element(window.clone(), position, false);
    }

    if let Some(size) = transform_update.size {
        // The compositor's new decided size — the window is enforced at this until the next
        // reform. This is the authority the render/input fit uses.
        slot::set_expected_size(&window, size);

        if force {
            // Interactive resize drag (`reform_force`, from canvas motion): throttle the configure
            // — one client commit per pointer motion is what stutters — and arm the stretch so the
            // window follows the cursor between commits. The final size + settle happen on release
            // (`finish_resize`). `note_resize` returns whether a configure is due now.
            //
            //
            // `stage_drag` keeps the two shells apart: the xdg pending state is written on
            // EVERY motion (a focus change mid-drag emits it, and must not emit a stale
            // size), while for X11 staging IS the emit — the per-frame flush sends whatever
            // is staged — so its stage is gated with the send.
            let due = slot::note_resize(&window, size);
            shell::stage_drag(&window, size, due);
            if due {
                shell::send(&window);
            }
        } else {
            shell::stage(&window, size, true);
            // One-off resize (navigator maximize, tiling, etc.): send immediately, no throttle and
            // no stretch — there's no drag/release to settle it, so arming the stretch would leave
            // the window stuck stretching and re-sending configures forever.
            shell::send(&window);
        }
    }

    if force {
        // let opt = (transform_update.position, transform_update.size);

        // force_window_geometry(&window, opt);
    }

    // A window moved/resized — refresh the capture region's tracked bbox +
    // force-render set (event-driven, mirrors the group bbox invalidation).
    recorder::interface::interface::on_window_geometry_changed(state);

}

// fn force_window_geometry(window: &Window, new_geom: Rectangle<i32, Logical>) {
//     let surface = window.wl_surface().unwrap();
//     if let Some(surface) = window.wl_surface() {
//         // It's the set geometry clamped to the bounding box with the full bounding box as the fallback.
//         let details = with_states(&surface, |states| {
//             states
//                 .cached_state
//                 .get::<SurfaceCachedState>()
//                 .current()
//                 .geometry
//                 .and_then(|geo| geo.intersection(bbox))
//         }).unwrap();
//     }
// }

fn force_window_geometry(
    window: &Window,
    new_geom: (Option<Point<i32, Logical>>, Option<Size<i32, Logical>>),
) {
    let Some(surface) = window.wl_surface() else {
        return;
    };

    with_states(&surface, |states| {
        let mut cached = states.cached_state.get::<SurfaceCachedState>();
        let current = cached.current();
        let Some(mut geom) = current.geometry.clone() else {
            return;
        };

        if let Some(position) = new_geom.0 {
            geom.loc = position;
        }

        if let Some(size) = new_geom.1 {
            geom.size = size;
        }

        current.geometry = Some(geom);
    });
}
