//! Server-side chrome on the pointer path (decor step 4): hover on
//! motion, press and release on the primary button, and the intents those
//! produce, turned into exactly the SurfaceEvents a client's own requests make:
//! `xdg_toplevel.move` / `.resize` (the interactive grab), `.set_maximized`,
//! `.set_minimized`, and a close (`WindowRequest::Close`, the polite
//! close). Chrome adds no path of its own into window management.

use smithay::desktop::Window;
use smithay::utils::{Logical, Point};

use decor::Intent;
use decor::window::ChromeHit;
use dispatcher::state::state::RedrawReason;
use dispatcher::wire::trait_::surface_event::{SurfaceEvent, SurfaceHandle, WindowRequest};
use world::state::Loop;
use world::surface::interface::hit::{SurfaceHit, surface_under_filtered};
use world::window::interface::draw::visible::DrawWindow;

/// `evdev` BTN_LEFT: chrome answers the primary button only.
pub const BTN_LEFT: u32 = 0x110;

/// The chrome under a hit, if it is chrome with a part.
pub fn target(hit: Option<&SurfaceHit>) -> Option<(Window, ChromeHit)> {
    match hit {
        Some(SurfaceHit::WindowChrome {
            window,
            chrome: Some(chrome),
        }) => Some((window.clone(), *chrome)),
        _ => None,
    }
}

/// Where `position` (host Space) is relative to `window`'s slot.
fn relative(lp: &Loop, window: &Window, position: Point<f64, Logical>) -> (f64, f64) {
    let origin = lp
        .inner
        .space_state()
        .state
        .element_location(window)
        .unwrap_or_default();
    (position.x - origin.x as f64, position.y - origin.y as f64)
}

/// After a pointer motion: hover onto (or off) the chrome under the pointer,
/// a frame if that changed what the chrome shows, and the resize cursor over
/// an edge (taken back off when the pointer leaves it, if still ours).
pub fn motion(lp: &mut Loop, target: Option<(Window, ChromeHit)>) {
    let hover = decor::seat::hover(target.as_ref().map(|(w, h)| (w, *h)));
    if hover.redraw {
        lp.state.redraw.request_for(RedrawReason::Cursor);
    }
    if let Some(icon) = hover.set_cursor {
        lp.state.seat.force_cursor = Some(icon);
        lp.state.redraw.request_for(RedrawReason::Cursor);
    } else if let Some(icon) = hover.clear_cursor
        && lp.state.seat.force_cursor == Some(icon)
    {
        lp.state.seat.force_cursor = None;
        lp.state.redraw.request_for(RedrawReason::Cursor);
    }
}

/// A primary-button press that hit `hit` at `position` (host Space), at event
/// `time`. Does nothing for a hit that is not a chrome part.
pub fn press(lp: &mut Loop, hit: &SurfaceHit, position: Point<f64, Logical>, time: u32) {
    let Some((window, chrome)) = target(Some(hit)) else {
        return;
    };
    let relative = relative(lp, &window, position);
    if let Some(intent) = decor::seat::press(&window, chrome, relative, time) {
        apply(lp, &window, intent);
    }
    // The pressed look (a caption button) or the grab's start.
    lp.state.redraw.request_for(RedrawReason::Cursor);
}

/// A primary-button release at the pointer's current position: fires a caption
/// button armed by the press if the release lands on the same button, and
/// clears its pressed look either way.
pub fn release(lp: &mut Loop) {
    let pointer = lp.state.seat.seat.get_pointer().unwrap();
    let position = pointer.current_location();
    let hit = surface_under_filtered(lp, position, &|hit| match hit.window() {
        Some(window) => window.visible(lp),
        None => true,
    });
    let under = target(hit.as_ref());
    if let Some((window, intent)) = decor::seat::release(under.as_ref().map(|(w, h)| (w, *h))) {
        apply(lp, &window, intent);
        lp.state.redraw.request_for(RedrawReason::Cursor);
    }
}

/// An intent from the chrome, as the request the client itself would make.
fn apply(lp: &mut Loop, window: &Window, intent: Intent) {
    let Some(handle) = SurfaceHandle::of_window(window) else {
        return;
    };
    let tile_owned = lp
        .inner
        .comp
        .registry
        .id_for_handle(&handle)
        .is_some_and(|id| {
            lp.inner
                .comp
                .tiles
                .members()
                .iter()
                .any(|member| member.target.id == id)
        });
    let tile_flags = protocols::window::shell::shell::requested_tiled(window)
        || protocols::window::shell::shell::committed_tiled(window);
    if (tile_owned || tile_flags) && matches!(intent, Intent::Move | Intent::Resize { .. }) {
        return;
    }
    match intent {
        Intent::Move => {
            dispatcher::wayland::grab::interactive::start_chrome(&mut lp.state, handle, 0);
        }
        Intent::Resize { edges } => {
            dispatcher::wayland::grab::interactive::start_chrome(&mut lp.state, handle, edges);
        }
        Intent::ToggleMaximize => {
            let maximized = window.toplevel().is_some_and(|toplevel| {
                toplevel.with_committed_state(|state| {
                    state.is_some_and(|state| {
                        state.states.contains(
                            smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Maximized,
                        )
                    })
                })
            });
            lp.state.push_surface_event(SurfaceEvent::Request {
                handle,
                request: WindowRequest::Maximize(!maximized),
            });
        }
        Intent::Minimize => lp.state.push_surface_event(SurfaceEvent::Request {
            handle,
            request: WindowRequest::Minimize,
        }),
        Intent::Close => lp.state.push_surface_event(SurfaceEvent::Request {
            handle,
            request: WindowRequest::Close,
        }),
    }
}
