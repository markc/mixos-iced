//! The occlusion props, reported from the renderer's draw-time cull.
//!
//! compd reports what its renderer actually did, not a separate
//! opaque-coverage model: a window on a pane but not drawn there because
//! opaque windows in front covered it ([`Drawn`](super::occlude::Drawn):
//! `on_pane && !visible`) is `opaque-coverage`; one drawn anywhere is
//! `exposed`; one no pane evaluated this pass (off every output, hidden by
//! the comp policy, under the lock) is `unknown`: the report fails open.
//! Where format/region rules and the renderer's strict deposits disagree,
//! compd reports what it drew.
//!
//! Per-pane votes are replaced each time that pane renders, so an output that
//! is not redrawn keeps its last decisions. The record never schedules a
//! frame; it is written by the renderer and read by the projection and the
//! frame-callback trickle.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use smithay::desktop::Window;
use uuid::Uuid;

use super::occlude::Visible;
use crate::window::interface::record::window::LoopWindow;

/// One window's occlusion decision.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Decision {
    #[default]
    Unknown,
    Exposed,
    Occluded,
}

impl Decision {
    /// The `occlusion.reason` leaf.
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Exposed => "exposed",
            Self::Occluded => "opaque-coverage",
        }
    }
}

/// The `occlusion.counters` leaves (volatile; never diffed).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Counters {
    /// Occluded roots whose frame callbacks a pulse withheld.
    pub withheld_opportunities: u64,
    /// Occluded → exposed transitions.
    pub resumes: u64,
    /// Decision passes (one per pane render).
    pub recomputes: u64,
    /// `unknown` results in those passes.
    pub conservative_fallbacks: u64,
}

/// What the projection reads for one window.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Props {
    pub occluded: bool,
    pub reason: &'static str,
    /// The record revision at which this window's decision last changed.
    pub revision: u64,
}

#[derive(Default)]
struct Record {
    /// (output key, pane slot) → window → drawn there?
    votes: HashMap<(String, Option<u64>), HashMap<Uuid, bool>>,
    decisions: HashMap<Uuid, (Decision, u64)>,
    revision: u64,
    counters: Counters,
    /// Windows whose decision changed since the last `take_changed`.
    changed: Vec<Uuid>,
}

thread_local! {
    static RECORD: RefCell<Record> = RefCell::new(Record::default());
}

impl Record {
    fn record(&mut self, key: (String, Option<u64>), votes: HashMap<Uuid, bool>, live: &HashSet<Uuid>) {
        self.votes.insert(key, votes);
        self.votes.retain(|_, pane| {
            pane.retain(|uuid, _| live.contains(uuid));
            !pane.is_empty()
        });
        self.decisions.retain(|uuid, _| live.contains(uuid));
        self.counters.recomputes = self.counters.recomputes.saturating_add(1);
        let mut next: HashMap<Uuid, Decision> = HashMap::new();
        for pane in self.votes.values() {
            for (uuid, drawn) in pane {
                let entry = next.entry(*uuid).or_default();
                *entry = match (*entry, *drawn) {
                    (_, true) | (Decision::Exposed, false) => Decision::Exposed,
                    _ => Decision::Occluded,
                };
            }
        }
        let mut changed = Vec::new();
        for uuid in live {
            let decision = next.get(uuid).copied().unwrap_or_default();
            if decision == Decision::Unknown {
                self.counters.conservative_fallbacks = self.counters.conservative_fallbacks.saturating_add(1);
            }
            let previous = self.decisions.get(uuid).map_or(Decision::Unknown, |(d, _)| *d);
            if previous != decision {
                if previous == Decision::Occluded && decision == Decision::Exposed {
                    self.counters.resumes = self.counters.resumes.saturating_add(1);
                }
                changed.push((*uuid, decision));
            }
        }
        if !changed.is_empty() {
            self.revision = self.revision.saturating_add(1);
            for (uuid, decision) in changed {
                self.decisions.insert(uuid, (decision, self.revision));
                if !self.changed.contains(&uuid) {
                    self.changed.push(uuid);
                }
            }
        }
    }
}

/// One pane's render: the windows it noted (drawn, and on the pane) for
/// output `output` and pane `slot`, against the windows that exist now.
pub fn record(output: &str, slot: Option<u64>, visible: &Visible, live: impl IntoIterator<Item = Uuid>) {
    let mut votes = HashMap::new();
    for window in &visible.on_pane {
        if let Some(uuid) = window.uuid() {
            votes.insert(uuid, false);
        }
    }
    for window in &visible.drawn {
        if let Some(uuid) = window.uuid() {
            votes.insert(uuid, true);
        }
    }
    let live: HashSet<Uuid> = live.into_iter().collect();
    RECORD.with_borrow_mut(|record| record.record((output.to_owned(), slot), votes, &live));
}

/// `window`'s props (unknown/0 before any pane evaluated it).
pub fn props(uuid: &Uuid) -> Props {
    RECORD.with_borrow(|record| {
        let (decision, revision) = record.decisions.get(uuid).copied().unwrap_or_default();
        Props { occluded: decision == Decision::Occluded, reason: decision.reason(), revision }
    })
}

pub fn decision(window: &Window) -> Decision {
    window.uuid().map_or(Decision::Unknown, |uuid| {
        RECORD.with_borrow(|record| record.decisions.get(&uuid).map_or(Decision::Unknown, |(d, _)| *d))
    })
}

pub fn counters() -> Counters {
    RECORD.with_borrow(|record| record.counters)
}

/// The record's revision (moves when any decision changes).
pub fn revision() -> u64 {
    RECORD.with_borrow(|record| record.revision)
}

/// Windows whose decision changed since the last call (for `props.changed`
/// cause `wayland.occlusion`).
pub fn take_changed() -> Vec<Uuid> {
    RECORD.with_borrow_mut(|record| std::mem::take(&mut record.changed))
}

/// The occluded windows and the outputs they were occluded on.
pub fn occluded() -> Vec<(Uuid, Vec<String>)> {
    RECORD.with_borrow(|record| {
        let mut out: HashMap<Uuid, Vec<String>> = HashMap::new();
        for ((output, _), pane) in &record.votes {
            for (uuid, drawn) in pane {
                if !drawn && record.decisions.get(uuid).is_some_and(|(d, _)| *d == Decision::Occluded) {
                    let outputs = out.entry(*uuid).or_default();
                    if !outputs.contains(output) {
                        outputs.push(output.clone());
                    }
                }
            }
        }
        out.into_iter().collect()
    })
}

/// A frame-callback pulse withheld callbacks from `n` occluded windows.
pub fn note_withheld(n: usize) {
    RECORD.with_borrow_mut(|record| {
        record.counters.withheld_opportunities = record.counters.withheld_opportunities.saturating_add(n as u64);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn pane(votes: &[(u128, bool)]) -> HashMap<Uuid, bool> {
        votes.iter().map(|(n, drawn)| (id(*n), *drawn)).collect()
    }

    #[test]
    fn decisions_fold_across_panes_and_revisions_move_only_on_change() {
        let mut r = Record::default();
        let live: HashSet<Uuid> = [id(1), id(2), id(3)].into();
        r.record(("A".into(), None), pane(&[(1, true), (2, false)]), &live);
        assert_eq!(r.decisions[&id(1)].0, Decision::Exposed);
        assert_eq!(r.decisions[&id(2)].0, Decision::Occluded);
        assert!(r.decisions.get(&id(3)).is_none(), "never evaluated: unknown, not stored");
        let rev = r.revision;
        assert_eq!(r.counters.conservative_fallbacks, 1);
        // Same votes again: nothing changes, the revision holds.
        r.record(("A".into(), None), pane(&[(1, true), (2, false)]), &live);
        assert_eq!(r.revision, rev);
        // Window 2 drawn on another output: exposed wins, a resume.
        r.record(("B".into(), None), pane(&[(2, true)]), &live);
        assert_eq!(r.decisions[&id(2)], (Decision::Exposed, rev + 1));
        assert_eq!(r.counters.resumes, 1);
        assert_eq!(r.counters.recomputes, 3);
    }

    #[test]
    fn a_pane_rerender_replaces_its_votes_and_dead_windows_go() {
        let mut r = Record::default();
        let live: HashSet<Uuid> = [id(1)].into();
        r.record(("A".into(), None), pane(&[(1, false)]), &live);
        assert_eq!(r.decisions[&id(1)].0, Decision::Occluded);
        // Off every pane now: unknown (the report fails open).
        r.record(("A".into(), None), pane(&[]), &live);
        assert_eq!(r.decisions[&id(1)].0, Decision::Unknown);
        // Destroyed: forgotten.
        r.record(("A".into(), None), pane(&[]), &HashSet::new());
        assert!(r.decisions.is_empty() && r.votes.is_empty());
    }
}
