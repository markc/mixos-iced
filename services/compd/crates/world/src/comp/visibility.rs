//! The workspace suppression on the engine's input path (the workspace
//! hit-test gap).
//!
//! `CompState::hidden` (carries a workspace, and minimised or off the
//! current workspace) already gates the draw through `DrawWindow::visible`,
//! but the central hit driver (`surface_under_filtered_cx`, used by touch,
//! tablet and the canvas press) reads only the window itself.
//! [`sync_hidden`] stamps the decision on each window (`ident::set_hidden`),
//! and the driver reads `ident::is_shown` (`is_drawn && !is_hidden`).
//! `is_drawn` is untouched: it feeds the wire-visible `windows.*.visible`
//! (whose projection already adds the hidden term), the truth lists and the
//! chrome. The foreign-toplevel mirror stays on `is_drawn` too: it exports
//! minimised and off-workspace windows, so a dock can restore them.

use std::cell::Cell;

use dispatcher::wire::trait_::surface_event::SurfaceHandle;
use dispatcher::wire::trait_::wire_trait::WireTrait;
use protocols::window::ident::ident;

use crate::state::Loop;

thread_local! {
    /// The CompState revision the last stamp ran at.
    static STAMPED: Cell<Option<u64>> = const { Cell::new(None) };
}

/// One loop pass: stamp every world window with the policy's hidden decision,
/// when CompState moved since the last stamp. Returns whether any stamp
/// changed.
pub fn sync_hidden(lp: &Loop) -> bool {
    let revision = lp.inner.comp.revision();
    if STAMPED.with(Cell::get) == Some(revision) {
        return false;
    }
    STAMPED.with(|stamped| stamped.set(Some(revision)));
    let comp = &lp.inner.comp;
    let mut changed = false;
    for space in lp.inner.all_world_spaces() {
        for window in space.state.elements() {
            let hidden = SurfaceHandle::of_window(window).is_some_and(|handle| comp.hidden(&handle));
            changed |= ident::set_hidden(window, hidden);
        }
    }
    changed
}
