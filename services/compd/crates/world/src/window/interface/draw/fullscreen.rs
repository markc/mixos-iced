use smithay::desktop::Window;
use dispatcher::state::state::RedrawReason;
use dispatcher::wire::trait_::wire_trait::WireTrait;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::utils::{Logical, Point, Size};
use crate::camera::transform::translate::slot;
use crate::state::Loop;
use crate::window::interface::record::data::WindowFullscreen;
use crate::window::interface::record::window::LoopWindow;
use protocols::window::find::find;
use protocols::window::shell::shell;

/// Apply (or clear) fullscreen on a window.
///
/// This compositor has no physical "screen" to fill (the canvas is a
/// pannable/zoomable world), so "fullscreen" means: tell the client its
/// fullscreen size equals the region it conceptually owns.
///
///   * Ungrouped window  → its own current size (it is already "as large as
///     its screen"); we only flip the protocol state.
///   * Grouped window     → the group's padded bounding box, and we move the
///     window to fill that region.
///
/// The window is raised to the top of the stack so it stays above its peers
/// and captures input within its bounds. Pre-fullscreen geometry is stored so
/// it can be restored on un-fullscreen.
pub fn fullscreen_set(_loop: &mut Loop, window: Window, fullscreen: bool) {
    if crate::comp::live_window_space(
        &_loop.inner.comp, _loop.inner.all_world_spaces(), &window,
    ).is_none() || window.is_fullscreen() == fullscreen { return }
    let owner_world = _loop.inner.world_of_window(&window)
        .expect("a live window has an owning world");
    let activate = owner_world == _loop.inner.worlds.spawn_target();
    let target = fullscreen.then(|| crate::comp::fullscreen::target(_loop, &window)).flatten();
    if !fullscreen_set_in_space(
        &mut _loop.inner.space_of_mut(owner_world).state, &window, fullscreen, target, activate,
    ) {
        return;
    }
    if fullscreen && let Some(uuid) = window.uuid() {
        _loop.inner.raise_drawable_of(owner_world, uuid);
    }
    _loop.schedule_redraw(RedrawReason::WindowState);
}

/// Renderer-free fullscreen geometry and protocol state, on the resolved owner.
/// Shared with testkit so off-world regressions assert the real restore path.
/// A parked owner's geometry request must not take activation from the current world.
pub fn fullscreen_set_in_space(
    space: &mut smithay::desktop::Space<Window>,
    window: &Window,
    fullscreen: bool,
    target: Option<(Point<i32, Logical>, Size<i32, Logical>)>,
    activate: bool,
) -> bool {
    let Some(current_loc) = space.element_location(window) else { return false };
    if fullscreen {
        if window.is_fullscreen() {
            return false;
        }
        // The window's PRE-fullscreen slot (what it's rendered at + what the group bbox uses).
        let current_size = slot::expected_size(window)
            .filter(|s| s.w > 0 && s.h > 0)
            .unwrap_or_else(|| window.geometry().size);

        // Fullscreen covers an output's whole geometry, since world
        // coordinates are output-logical.
        let (target_loc, target_size) = target
            .unwrap_or((current_loc, current_size));

        window.set_fullscreen(Some(WindowFullscreen {
            restore_loc: current_loc,
            restore_size: current_size,
        }));

        // Move into place and raise above peers (exclusive within its bounds).
        space.map_element(window.clone(), target_loc, activate);
        space.raise_element(window, activate);

        // The compositor-decided slot IS the fullscreen size — without this the render keeps
        // fitting the stale (pre-fullscreen) slot and the window never grows. The group bbox uses
        // the restore rect (above), so it doesn't feed back off this new slot.
        slot::set_expected_size(window, target_size);

        shell::set_fullscreen(window, true);
        shell::stage(window, target_size, false);
        shell::send(window);
    } else {
        let Some(restore) = window.fullscreen() else {
            return false;
        };
        window.set_fullscreen(None);

        // Where to land: fullscreen covered the output, so the window goes back
        // to the rectangle it had before it.
        let (loc, size) = (restore.restore_loc, restore.restore_size);

        space.map_element(window.clone(), loc, false);

        slot::set_expected_size(window, size);

        shell::set_fullscreen(window, false);
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
        .inner.space_state()
        .state
        .elements()
        .find(|w| find::is_surface(w, &focus))
        .cloned()
}
