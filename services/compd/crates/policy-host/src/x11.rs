//! X11's side of the workspace and minimise policy.
//!
//! - The root `_NET_NUMBER_OF_DESKTOPS` / `_NET_CURRENT_DESKTOP` are the
//!   workspace count and the current workspace − 1 ([`publish_desktops`]).
//! - Every mapped managed X11 window carries `_NET_WM_DESKTOP` = its workspace
//!   − 1, and `_NET_WM_STATE_HIDDEN` exactly when it is minimised or off the
//!   current workspace ([`sync_window`]). HIDDEN only: `WM_STATE` stays
//!   Normal (smithay `set_hidden_hint`, not `set_hidden`); a
//!   workspace switch is not an ICCCM iconify, and games stop rendering on
//!   Iconic. Override-redirect windows are never written.
//!
//! The policy effects (`PublishDesktops`, `ResyncAllX11`, `Relabelled`,
//! `Withdraw`, `Present`) call these, and the host pass runs [`sync`] after
//! every state change, so a map stamp or a minimise with no effect of its
//! own is published too. Writes are idempotent: smithay rewrites
//! `_NET_WM_STATE` only on a change, `_NET_WM_DESKTOP` is compared with the
//! surface's mirror first, and the root pair with the last one published.
//! With no window manager (Xwayland off or dead) every call is a no-op.

use std::cell::Cell;

use dispatcher::wire::trait_::wire_trait::WireTrait;
use surfaces::{SurfaceId, SurfaceRole};
use world::state::Loop;

thread_local! {
    /// The root `(count, current)` last published, so a pass that changed
    /// neither writes nothing.
    static PUBLISHED: Cell<Option<(u32, u32)>> = const { Cell::new(None) };
}

/// `_NET_NUMBER_OF_DESKTOPS` / `_NET_CURRENT_DESKTOP` on the root.
pub fn publish_desktops(lp: &Loop) {
    let Some(xwm) = lp.state.xwayland.xwm.as_ref() else {
        PUBLISHED.with(|published| published.set(None));
        return;
    };
    let comp = &lp.inner.comp;
    let pair = (comp.workspaces.count, comp.current_workspace().saturating_sub(1));
    if PUBLISHED.with(Cell::get) == Some(pair) {
        return;
    }
    let count = xwm.set_number_of_desktops(pair.0);
    let current = xwm.set_current_desktop(pair.1);
    if count.is_ok() && current.is_ok() {
        PUBLISHED.with(|published| published.set(Some(pair)));
    }
}

/// One window's `_NET_WM_DESKTOP` and HIDDEN, when it is a mapped managed
/// X11 window that carries a workspace.
pub fn sync_window(lp: &Loop, id: SurfaceId) {
    sync_windows(lp, std::iter::once(id));
}

/// Resolve a batch in one pass through the hosted windows. A workspace switch
/// used to scan every window once per leaving/arriving X11 record (O(n²)).
pub fn sync_windows(lp: &Loop, ids: impl IntoIterator<Item = SurfaceId>) {
    if lp.state.xwayland.xwm.is_none() {
        return;
    }
    let comp = &lp.inner.comp;
    use dispatcher::wire::trait_::surface_event::SurfaceHandle;
    let mut wanted: std::collections::HashMap<u32, (u32, bool)> = ids.into_iter().filter_map(|id| {
        let record = comp.registry.get(id)?;
        if record.role() != (SurfaceRole::X11 { override_redirect: false }) || !record.mapped() { return None; }
        let SurfaceHandle::X11(window_id) = record.handle() else { return None };
        Some((*window_id, (record.workspace()?.saturating_sub(1),
            policy::workspaces::x11_suspended(record, comp.current_workspace()))))
    }).collect();
    if wanted.is_empty() { return; }
    for space in lp.inner.all_world_spaces() {
        for window in space.state.elements() {
            let Some(x11) = window.x11_surface() else { continue };
            let Some((desktop, suspended)) = wanted.remove(&x11.window_id()) else { continue };
            if x11.desktop() != Some(desktop) { let _ = x11.set_desktop(desktop); }
            let _ = x11.set_hidden_hint(suspended);
            if wanted.is_empty() { return; }
        }
    }
}

/// Every mapped managed X11 window (desktop and HIDDEN), then the root pair.
pub fn sync(lp: &Loop) {
    if lp.state.xwayland.xwm.is_none() {
        PUBLISHED.with(|published| published.set(None));
        return;
    }
    let ids: Vec<SurfaceId> = lp
        .inner
        .comp
        .registry
        .surface_rows()
        .filter(|record| record.role() == SurfaceRole::X11 { override_redirect: false } && record.mapped())
        .map(|record| record.id())
        .collect();
    sync_windows(lp, ids);
    publish_desktops(lp);
}

thread_local! {
    /// The CompState revision the last [`sync_if_changed`] ran at.
    static SYNCED: Cell<Option<u64>> = const { Cell::new(None) };
}

/// The host pass's call: [`sync`] when CompState moved since the last one
/// (a map stamp, a minimise, a switch, a count change) or the window
/// manager came up. Idle passes write nothing.
pub fn sync_if_changed(lp: &mut Loop) {
    let revision = lp.state.xwayland.xwm.as_ref().map(|_| lp.inner.comp.revision());
    if SYNCED.with(Cell::get) == revision {
        return;
    }
    SYNCED.with(|synced| synced.set(revision));
    sync(lp);
    // A map, unmap, focus (an activation raises) or band change moved the
    // revision; the X stack follows the draw order.
    sync_stacking(lp);
}

/// A new X server generation (the one retry): everything
/// mirrored into the dead one is unknown to it, so the next pass writes the
/// root pair, every window and the stack afresh. The pump-side resets on
/// `xwm == None` cover a pass that ran in between; this covers one that did
/// not.
pub fn forget() {
    PUBLISHED.with(|published| published.set(None));
    SYNCED.with(|synced| synced.set(None));
    STACKED.with(|stacked| stacked.borrow_mut().clear());
}

thread_local! {
    /// The X11 stack (bottom to top, window ids) last mirrored.
    static STACKED: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Mirror the
/// compositor's draw order into the X server's stack, so the X stack (and
/// `_NET_CLIENT_LIST_STACKING`) agrees with what is drawn and an ungrabbed
/// pointer event goes where it looks like it goes. Mapped managed X11
/// windows only: an override-redirect window is never restacked (its
/// configure is refused) and is not in the client list. Written only when the
/// order changed.
pub fn sync_stacking(lp: &mut Loop) {
    if lp.state.xwayland.xwm.is_none() {
        STACKED.with(|stacked| stacked.borrow_mut().clear());
        return;
    }
    let windows: Vec<smithay::xwayland::X11Surface> = {
        let comp = &lp.inner.comp;
        // Workspace settlement also restacks. Index both sides once rather
        // than scanning the registry and every window for each drawable.
        let records: std::collections::HashMap<_, _> = comp.registry.surface_rows().filter_map(|record| {
            if record.role() != (SurfaceRole::X11 { override_redirect: false }) || !record.mapped() { return None; }
            let dispatcher::wire::trait_::surface_event::SurfaceHandle::X11(id) = record.handle() else { return None };
            Some((record.uuid()?, *id))
        }).collect();
        let elements: std::collections::HashMap<_, _> = lp
            .inner
            .all_world_spaces()
            .iter()
            .flat_map(|space| space.state.elements())
            .filter_map(|window| window.x11_surface().map(|x11| (x11.window_id(), x11.clone())))
            .collect();
        // `drawable_order` is topmost first; the XWM wants bottom to top.
        lp.inner
            .drawable_order()
            .iter()
            .rev()
            .filter_map(|uuid| elements.get(records.get(uuid)?).cloned())
            .collect()
    };
    let order: Vec<u32> = windows.iter().map(|window| window.window_id()).collect();
    if order.is_empty() || STACKED.with(|stacked| *stacked.borrow() == order) {
        return;
    }
    let Some(xwm) = lp.state.xwayland.xwm.as_mut() else { return };
    if xwm.update_stacking_order_upwards(windows.iter()).is_ok() {
        STACKED.with(|stacked| *stacked.borrow_mut() = order);
    }
}
