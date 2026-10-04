//! Presentation statistics: ledger's `StatsRegistry`, fed from three points.
//!
//! - **published**: a committed buffer becomes content `seq` of its surface
//!   (CompState's commit count; [`Presentation::published`]);
//! - **queued** ([`frame_queued`], the backends' call-out where a frame is
//!   actually handed to the display): which windows the frame shows, each
//!   surface with the content it samples, and which mapped windows it does
//!   not show;
//! - **presented** ([`presented`], where the backend learns the frame
//!   reached the screen: the page-flip event on kms, the submit on nested):
//!   the oldest queued frame of that output is folded in at its time.
//!
//! Content sources: compd's own iced surfaces
//! register through `ui::source`; [`frame_queued`] drains the
//! registrations into [`Presentation::sources`] and takes each source's report
//! for that output's frame, and [`presented`] resolves them with the frame.
//!
//! Stats follow content, not `wp_presentation` feedback, so every client is
//! measured. Times are CLOCK_MONOTONIC microseconds. Single output: a window
//! not in an output's frame is hidden by it.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use dispatcher::wire::trait_::surface_event::SurfaceHandle;
use ledger::presentation::{FrameSource, SourceLedger};
use ledger::presentation_stats::{StatsRegistry, SurfaceShown, WindowFrame};
use surfaces::SurfaceId;
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::{TraversalAction, with_surface_tree_downward};
use smithay::wayland::seat::WaylandFocus;

use crate::state::Loop;

/// Frames kept per output between queue and presentation.
const IN_FLIGHT: usize = 4;

/// One window as a queued frame shows it: `{id, generation}` and its
/// surfaces (`is_root`, content seq).
struct ShownWindow {
    id: u64,
    generation: u64,
    surfaces: Vec<(u64, bool, u64)>,
}

struct QueuedFrame {
    shown: Vec<ShownWindow>,
    /// Mapped managed windows the frame does not show: `{id, generation}`
    /// and the root's content seq.
    hidden: Vec<(u64, u64, u64)>,
    /// Each content source's report for this frame.
    sources: Vec<FrameSource>,
}

#[derive(Default)]
pub struct Presentation {
    pub stats: StatsRegistry,
    /// The content sources (`sources.*`, `comp.window.stats {source}`).
    pub sources: SourceLedger,
    queued: HashMap<String, VecDeque<QueuedFrame>>,
    /// Windows a presented frame showed since they last mapped
    /// (`comp.window.wait until=presented`).
    presented_since_map: HashSet<SurfaceId>,
}

impl Presentation {
    /// A buffer became content `seq` of `surface`, which belongs to
    /// `window` (its root `{id, generation}`, when that is a window).
    pub fn published(&mut self, surface: SurfaceId, window: Option<(u64, u64)>, seq: u64) {
        let now = super::injection::monotonic_us();
        if self.stats.epoch_us == 0 {
            self.stats.epoch_us = now;
        }
        self.stats.note_published(surface.0, window, seq, now);
    }

    /// The window mapped (again): nothing has presented it since.
    pub fn mapped(&mut self, id: SurfaceId) {
        self.presented_since_map.remove(&id);
    }

    /// The surface went (destroyed, dormant, a new role).
    pub fn forget(&mut self, id: SurfaceId) {
        self.stats.forget_surface(id.0);
        self.presented_since_map.remove(&id);
    }

    pub fn presented_since_map(&self, id: SurfaceId) -> bool {
        self.presented_since_map.contains(&id)
    }
}

/// The record ids of a window's surface tree, root first, with their
/// content seq.
fn surface_tree(lp: &Loop, root: &WlSurface, root_id: SurfaceId) -> Vec<(u64, bool, u64)> {
    let comp = &lp.inner.comp;
    let mut out = vec![(root_id.0, true, comp.commits(root_id))];
    // Collect first, resolve after: the traversal holds each surface's data
    // lock while visiting it, and `id_for_surface` walks parents through
    // `get_parent`, which takes that same lock. Resolving inside the visit
    // self-deadlocked the loop on the first client with a subsurface (foot).
    let mut children = Vec::new();
    with_surface_tree_downward(
        root,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        |surface, _, _| {
            if surface != root {
                children.push(surface.clone());
            }
        },
        |_, _, _| true,
    );
    for surface in &children {
        if let Some(id) = comp.id_for_surface(surface) {
            out.push((id.0, false, comp.commits(id)));
        }
    }
    out
}

/// The backends' call-out: a frame showing `visible` was handed to the
/// display of `output` (queued for a flip, or submitted nested).
pub fn frame_queued(lp: &mut Loop, output: &Output, visible: &[Window]) {
    // Registrations first, so a source registered this frame reports in it.
    let now = super::injection::monotonic_us();
    for event in ui::source::take_events() {
        match event {
            ui::source::SourceEvent::Registered { id, output } => {
                lp.inner.comp.presentation.sources.register(&id, output, now);
            }
            ui::source::SourceEvent::Unregistered { id, revision } => {
                let _ = lp.inner.comp.presentation.sources.unregister(&id, revision);
            }
        }
    }
    // Keyed as the scene host registers them: the engine's output key.
    let sources = ui::source::frame(&crate::state::state::output_key(output));
    let comp = &lp.inner.comp;
    let mut listed = HashSet::new();
    let mut shown = Vec::new();
    for window in visible {
        let Some(id) = SurfaceHandle::of_window(window).and_then(|handle| comp.registry.id_for_handle(&handle)) else {
            continue;
        };
        let Some(record) = comp.registry.get(id) else { continue };
        let Some(root) = window.wl_surface() else { continue };
        listed.insert(id);
        shown.push(ShownWindow {
            id: id.0,
            generation: record.generation(),
            surfaces: surface_tree(lp, &root, id),
        });
    }
    let hidden = comp
        .registry
        .surface_rows()
        .filter(|record| record.mapped() && record.role().managed_toplevel() && !listed.contains(&record.id()))
        .map(|record| (record.id().0, record.generation(), comp.commits(record.id())))
        .collect();
    let queue = lp.inner.comp.presentation.queued.entry(output.name()).or_default();
    if queue.len() == IN_FLIGHT {
        queue.pop_front();
    }
    queue.push_back(QueuedFrame { shown, hidden, sources });
}

/// The backends' call-out: `output`'s oldest queued frame reached the
/// screen at `time` (CLOCK_MONOTONIC), with this refresh and these
/// `wp_presentation` kind bits.
pub fn presented(lp: &mut Loop, output: &Output, time: Duration, refresh: Option<Duration>, flags: u32) {
    let name = output.name();
    let tv_us = u64::try_from(time.as_micros()).unwrap_or(u64::MAX);
    let refresh_us = refresh.and_then(|refresh| u64::try_from(refresh.as_micros()).ok());
    let presentation = &mut lp.inner.comp.presentation;
    let Some(frame) = presentation.queued.get_mut(&name).and_then(VecDeque::pop_front) else {
        return;
    };
    if presentation.stats.epoch_us == 0 {
        presentation.stats.epoch_us = tv_us;
    }
    let mut listed = HashSet::new();
    for window in &frame.shown {
        let mut fold = WindowFrame::default();
        for &(surface, is_root, seq) in &window.surfaces {
            presentation.stats.surface_frame(surface, is_root, seq, SurfaceShown::Shown, &mut fold);
        }
        presentation.stats.window_frame(window.id, window.generation, fold, tv_us, refresh_us);
        presentation.presented_since_map.insert(SurfaceId(window.id));
        listed.insert(window.id);
    }
    for &(id, generation, seq) in &frame.hidden {
        let mut fold = WindowFrame::default();
        presentation.stats.surface_frame(id, true, seq, SurfaceShown::Hidden, &mut fold);
        presentation.stats.window_frame(id, generation, fold, tv_us, refresh_us);
        listed.insert(id);
    }
    presentation.stats.hide_unlisted(|window| listed.contains(&window));
    presentation.stats.output_frame(&name, tv_us, flags, refresh_us);
    // The content sources this frame reported, against the same frame time.
    let Presentation { stats, sources, .. } = presentation;
    for source in &frame.sources {
        sources.resolve(source, tv_us, refresh_us, |input_seq, at_us| stats.input_mark(input_seq, at_us));
    }
}
