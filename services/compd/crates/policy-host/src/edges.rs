//! The observation edge pass: surface edges, the focus edge and property
//! diffs.
//!
//! Runs after each dispatch with the Bus service, and only when the comp
//! registry moved (`CompState::revision`): every surface lifecycle, buffer,
//! name and focus event bumps it, so a frame with no registry traffic costs a
//! comparison. Nothing polls.
//!
//! - `surface.mapped` / `surface.unmapped`: per record, the mapped edge since
//!   the last pass; a remap under a new generation inside one pass is still two
//!   edges (the window observers knew is gone). A destroyed record
//!   reads as unmapped. Every role is tracked.
//! - `focus.changed`: the primary seat's keyboard focus record changed (a
//!   `focus_changed(None)` reads as no focus). `exclusive_latch` stays null
//!   until compd has a latch to report.
//! - `props.changed`: while `comp.props.watch` holds a baseline, the leaves that
//!   changed between it and a fresh projection (comp-model `diff`), one
//!   record per leaf, cause [`CAUSE`].
//! - `output.changed`: an output's geometry or usable area moved,
//!   looked at every pass (outputs change without registry traffic).
//! - `corner.entered` / `corner.left` / `corner.clicked` / `corner.clicked.v2`:
//!   the topics the pointer path earned, drained by
//!   [`corner_topics`] in the order they happened.
//!
//! Records come out numbered 0; the caller numbers them as it offers them to
//! the outbox (`ObservationRecord::with_event_seq`), so `event_seq` is strictly
//! increasing across topics.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use comp_model::diff::collect_snapshot_diff;
use comp_model::observation::{ObservationRecord, PendingPropChanges, SurfaceEdgeWindow};
use comp_model::snapshot::{CompSnapshot, OutputSnapshot, ReadScopes, output_key};
use policy::corner::CornerTopic;
use dispatcher::wire::trait_::surface_event::SurfaceHandle;
use surfaces::{SurfaceRecord, SurfaceRole};
use world::state::Loop;

use crate::project::{Identity, project, project_focus, wl_surface};

/// The `cause` of every `props.changed` record compd publishes: the diff runs
/// over the whole projection after the engine's own state moved, so there is
/// one dirtying source.
pub const CAUSE: &str = "compd.state";

/// What observers last heard about one surface.
#[derive(Clone, Debug)]
struct Published {
    mapped: bool,
    role: &'static str,
    foreign_id: Option<String>,
    window: SurfaceEdgeWindow,
}

/// The corner topics the pointer path earned since the last call, in order,
/// each output named by its `o_<slug>` key. Numbered by the caller as it
/// offers them (`CornerTopic::into_record`).
pub fn corner_topics(lp: &mut Loop) -> Vec<CornerTopic> {
    let mut topics = lp.inner.comp.corners.take_topics();
    for topic in &mut topics {
        let (CornerTopic::Entered { output, .. }
        | CornerTopic::Left { output, .. }
        | CornerTopic::Clicked { output, .. }
        | CornerTopic::ClickedV2 { output, .. }) = topic;
        *output = output_key(output);
    }
    topics
}

#[derive(Default)]
pub struct Edges {
    revision: Option<u64>,
    surfaces: BTreeMap<u64, Published>,
    keyboard: Option<u64>,
    /// The exclusive latch observers last heard.
    latch: Option<u64>,
    /// The `props.changed` baseline, while a watch holds one.
    baseline: Option<CompSnapshot>,
    /// The output rows observers last heard (`output.changed`); `None` until
    /// the first pass seeds them.
    outputs: Option<BTreeMap<String, OutputSnapshot>>,
}

impl Edges {
    pub fn new() -> Self {
        Self::default()
    }

    /// `comp.props.watch` / the broker's `topic.active` (`true`) seeds the
    /// diff baseline, or keeps the one there; `topic.idle` (`false`) drops it.
    pub fn watch(&mut self, lp: &Loop, identity: Identity, active: bool) -> bool {
        if !active {
            self.baseline = None;
        } else if self.baseline.is_none() {
            self.baseline = Some(project(lp, identity, &ReadScopes::All));
        }
        true
    }

    /// One pass: the records owed since the last one, numbered 0.
    pub fn pass(&mut self, lp: &Loop, identity: impl FnOnce() -> Identity) -> Vec<ObservationRecord> {
        let mut records = Vec::new();
        // Outputs change without registry traffic (a resize, a scale): looked
        // at every pass, a few rows.
        self.output_edges(lp, &mut records);
        let revision = lp.inner.comp.revision();
        if self.revision == Some(revision) {
            return records;
        }
        self.revision = Some(revision);
        self.surface_edges(lp, &mut records);
        self.focus_edge(lp, &mut records);
        if let Some(baseline) = self.baseline.as_mut() {
            let next = project(lp, identity(), &ReadScopes::All);
            let mut changes = PendingPropChanges::new();
            collect_snapshot_diff(baseline, &next, CAUSE, &mut changes);
            let unix_ms = unix_millis();
            // Each path carries the cause noted for the most specific subtree
            // that changed it, else compd's generic one.
            let causes = &lp.inner.comp.causes;
            records.extend(changes.into_iter().map(|(path, (old, new, cause))| {
                let cause = causes.resolve(&path).unwrap_or(cause);
                ObservationRecord::PropsChanged {
                    path,
                    old,
                    new,
                    unix_ms,
                    cause,
                    event_seq: 0,
                }
            }));
            *baseline = next;
        }
        records
    }

    /// `output.changed`: an
    /// output whose geometry or usable area moved, or a new output, with its
    /// row. The first pass only seeds; a removed output publishes nothing.
    fn output_edges(&mut self, lp: &Loop, records: &mut Vec<ObservationRecord>) {
        let (rows, _, _) = crate::project::project_outputs(lp);
        if let Some(previous) = &self.outputs {
            for (key, row) in &rows {
                let unchanged = previous.get(key).is_some_and(|old| {
                    (old.x, old.y, old.width, old.height) == (row.x, row.y, row.width, row.height)
                        && old.usable == row.usable
                });
                if !unchanged {
                    records.push(ObservationRecord::OutputChanged {
                        output: key.clone(),
                        row: row.clone(),
                        event_seq: 0,
                    });
                }
            }
        }
        self.outputs = Some(rows);
    }

    fn surface_edges(&mut self, lp: &Loop, records: &mut Vec<ObservationRecord>) {
        let mut next: BTreeMap<u64, Published> = lp
            .inner
            .comp
            .registry
            .surface_rows()
            .map(|record| (record.id().0, published(lp, record)))
            .collect();
        // compd's scene surfaces map and unmap like the Quoin layer
        // surfaces they stand for.
        for row in &lp.inner.comp.scenes.rows {
            next.insert(
                row.id.0,
                Published {
                    mapped: true,
                    role: "layer",
                    foreign_id: None,
                    window: SurfaceEdgeWindow { generation: row.generation, app_id: None, title: None },
                },
            );
        }
        let ids: BTreeSet<u64> = self.surfaces.keys().chain(next.keys()).copied().collect();
        for id in ids {
            let old = self.surfaces.get(&id);
            let new = next.get(&id);
            let old_mapped = old.is_some_and(|old| old.mapped);
            let new_mapped = new.is_some_and(|new| new.mapped);
            let replaced = old_mapped
                && new_mapped
                && old.zip(new).is_some_and(|(old, new)| old.window.generation != new.window.generation);
            if old_mapped == new_mapped && !replaced {
                continue;
            }
            if replaced && let Some(old) = old {
                records.push(unmapped(id, old, None));
            }
            match (new, old) {
                (Some(new), _) if new_mapped => records.push(ObservationRecord::SurfaceMapped {
                    id,
                    role: new.role.to_string(),
                    foreign_id: new.foreign_id.clone(),
                    window: new.window.clone(),
                    event_seq: 0,
                }),
                (new, Some(old)) => {
                    records.push(unmapped(id, old, new.and_then(|new| new.foreign_id.clone())))
                }
                (_, None) => {}
            }
        }
        self.surfaces = next;
    }

    fn focus_edge(&mut self, lp: &Loop, records: &mut Vec<ObservationRecord>) {
        // The edge fires when the keyboard OR the exclusive latch moves.
        let focus = project_focus(lp);
        let (keyboard, latch) = (focus.keyboard, focus.exclusive_latch);
        if keyboard == self.keyboard && latch == self.latch {
            return;
        }
        records.push(ObservationRecord::FocusChanged {
            keyboard,
            previous: self.keyboard,
            exclusive_latch: latch,
            event_seq: 0,
        });
        self.keyboard = keyboard;
        self.latch = latch;
    }
}

/// An unmap edge reports what observers knew: the old role and window, the
/// foreign id still known (the identifier is kept through the unmap).
fn unmapped(id: u64, old: &Published, foreign_id: Option<String>) -> ObservationRecord {
    ObservationRecord::SurfaceUnmapped {
        id,
        role: old.role.to_string(),
        foreign_id: foreign_id.or_else(|| old.foreign_id.clone()),
        window: old.window.clone(),
        event_seq: 0,
    }
}

fn published(lp: &Loop, record: &SurfaceRecord<SurfaceHandle>) -> Published {
    // The ext-foreign-toplevel identifier of a mapped xdg toplevel.
    let foreign_id = (record.mapped() && record.role() == SurfaceRole::Toplevel)
        .then(|| wl_surface(lp, record.handle()))
        .flatten()
        .and_then(|surface| lp.state.foreign.identifier_of(&surface));
    Published {
        mapped: record.mapped(),
        role: record.role().kind(),
        foreign_id,
        window: SurfaceEdgeWindow {
            generation: record.generation(),
            app_id: record.app_id().cloned(),
            title: record.title().cloned(),
        },
    }
}

fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}
