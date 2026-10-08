// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::camera::transform::translate::slot;
use crate::state::Loop;
use crate::window::interface::record::data::WindowFullscreen;
use crate::window::interface::record::window::LoopWindow;
use dispatcher::state::state::RedrawReason;
use protocols::window::find::find;
use protocols::window::shell::shell;
use smithay::desktop::{Space, Window};
use smithay::utils::{Logical, Rectangle};

/// Apply (or clear) fullscreen on a window.
///
/// Fullscreen fills the selected output's whole logical geometry, including
/// panel reservations. Loop-owned band and drawable ordering remain here.
pub fn fullscreen_set(_loop: &mut Loop, window: Window, fullscreen: bool) {
    if fullscreen && window.is_fullscreen() {
        return;
    }
    let returning = if !fullscreen {
        let (comp, space) = _loop.inner.comp_space_mut();
        dispatcher::wire::trait_::surface_event::SurfaceHandle::of_window(&window)
            .and_then(|handle| comp.registry.id_for_handle(&handle))
            .and_then(|id| crate::comp::geometry::tile_return(comp, space, id))
    } else {
        None
    };
    let target = if fullscreen {
        crate::comp::fullscreen::target(_loop, &window)
            .map(|(location, size)| Rectangle::new(location, size))
    } else {
        returning.map(|target| target.area)
    };
    if !apply(
        &mut _loop.inner.space_state_mut().state,
        &window,
        fullscreen,
        target,
        returning.is_some_and(|target| target.tiled),
    ) {
        return;
    }
    if fullscreen && let Some(uuid) = window.uuid() {
        _loop.inner.raise_drawable(uuid);
    }
    let (comp, space) = _loop.inner.comp_space_mut();
    comp.mark_input_geometry_dirty();
    crate::comp::geometry::refresh_space(comp, space);
    _loop.schedule_redraw(RedrawReason::WindowState);
}

/// Production placement, restore and configure path without a renderer or Loop.
/// The caller resolves the owning Space and output; no synthetic window state
/// is needed by native protocol fixtures.
pub fn apply(
    space: &mut Space<Window>,
    window: &Window,
    fullscreen: bool,
    target: Option<Rectangle<i32, Logical>>,
    restore_tiled: bool,
) -> bool {
    if fullscreen {
        if window.is_fullscreen() {
            return false;
        }

        let current_loc = space.element_location(window).unwrap_or_default();
        // The window's PRE-fullscreen slot (what it's rendered at + what the group bbox uses).
        let current_size = slot::size_of(window)
            .filter(|s| s.w > 0 && s.h > 0)
            .unwrap_or_else(|| window.geometry().size);

        // Fullscreen covers an output's whole geometry, since world
        // coordinates are output-logical.
        let target = target.unwrap_or(Rectangle::new(current_loc, current_size));
        let (target_loc, target_size) = (target.loc, target.size);

        window.set_fullscreen(Some(WindowFullscreen {
            restore_loc: current_loc,
            restore_size: current_size,
        }));

        // Move into place and raise above peers (exclusive within its bounds).
        space.map_element(window.clone(), target_loc, true);
        space.raise_element(window, true);

        // The compositor-decided slot IS the fullscreen size — without this the render keeps
        // fitting the stale (pre-fullscreen) slot and the window never grows. The group bbox uses
        // the restore rect (above), so it doesn't feed back off this new slot.
        slot::set_expected_size(window, target_size);

        shell::set_fullscreen(window, true);
        shell::set_tiled(window, false);
        shell::stage(window, target_size, false);
        shell::send(window);
    } else {
        let Some(restore) = window.fullscreen() else {
            return false;
        };
        window.set_fullscreen(None);

        // Where to land: fullscreen covered the output, so the window goes back
        // to the rectangle it had before it.
        let normal = target.unwrap_or(Rectangle::new(restore.restore_loc, restore.restore_size));
        let (loc, size) = (normal.loc, normal.size);

        space.map_element(window.clone(), loc, false);

        slot::set_expected_size(window, size);

        shell::set_fullscreen(window, false);
        shell::set_tiled(window, restore_tiled);
        shell::stage(window, size, false);
        shell::send(window);
    }

    true
}

/// F11: clear fullscreen on the keyboard-focused window, but only if it is
/// currently fullscreen (set via the protocol). Never enters fullscreen.
/// Returns `true` when it actually un-fullscreened a window (so the key is
/// consumed), `false` otherwise (so the key falls through to the client).
pub fn fullscreen_unset_focused(_loop: &mut Loop) -> bool {
    let Some(window) = focused_window(_loop) else {
        return false;
    };
    if !window.is_fullscreen() {
        return false;
    }
    fullscreen_set(_loop, window, false);
    true
}

/// The window backing the current keyboard focus, if any.
fn focused_window(_loop: &Loop) -> Option<Window> {
    let focus = _loop
        .state
        .seat
        .seat
        .get_keyboard()
        .and_then(|kb| kb.current_focus())?;

    _loop
        .inner
        .space_state()
        .state
        .elements()
        .find(|w| find::is_surface(w, &focus))
        .cloned()
}
