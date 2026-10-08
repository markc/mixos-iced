//! Content sources: compositor-owned content measured like a window.
//!
//! compd's sources are its own iced surfaces (the Mix Scenes host registers
//! each scene surface as `scene_<name>`). A producer registers an id, bumps
//! its revision when its
//! content changes, and marks it shown on the outputs that drew it. The
//! presentation wiring (`world` `comp::presentation`) drains the
//! registrations into its `SourceLedger`, takes one [`FrameSource`] per source
//! per queued frame of an output, and resolves them when that frame presents.
//!
//! Compositor-thread state, like the iced registry. Nothing here schedules a
//! frame.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use ledger::presentation::FrameSource;

/// A registration change for the ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceEvent {
    Registered {
        id: String,
        output: Option<String>,
    },
    /// Unregistered at `revision` (its unpresented revisions count as
    /// discarded).
    Unregistered {
        id: String,
        revision: u64,
    },
}

#[derive(Default)]
struct Source {
    native_handle: Option<crate::HandleId>,
    output: Option<String>,
    revision: u64,
    /// When the newest revision was taken, and the oldest since the last
    /// report (CLOCK_MONOTONIC µs).
    revised_us: Option<u64>,
    first_revised_us: Option<u64>,
    /// Outputs that drew this source since their last report.
    shown_on: BTreeSet<String>,
    /// Costs since the last report: bytes uploaded CPU to GPU, and physical
    /// px redrawn.
    upload_bytes: u64,
    damage_px: u64,
}

#[derive(Default)]
struct Sources {
    live: BTreeMap<String, Source>,
    events: Vec<SourceEvent>,
}

thread_local! {
    static SOURCES: RefCell<Sources> = RefCell::new(Sources::default());
}

fn now_us() -> u64 {
    smithay::utils::Clock::<smithay::utils::Monotonic>::new()
        .now()
        .as_micros()
}

/// `[a-z0-9_]{1,64}`: the id is a prop path segment (`sources.<id>`) and
/// SPEC 07 paths allow no `-`: an id with one could be registered but never
/// read by path.
pub fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Register `id`, measured on `output` (`None`: any). A live id is left as
/// it is; an invalid one is refused (false).
pub fn register(id: &str, output: Option<String>) -> bool {
    if !valid_id(id) {
        return false;
    }
    SOURCES.with_borrow_mut(|sources| {
        if !sources.live.contains_key(id) {
            sources.live.insert(
                id.to_owned(),
                Source {
                    output: output.clone(),
                    ..Source::default()
                },
            );
            sources.events.push(SourceEvent::Registered {
                id: id.to_owned(),
                output,
            });
        }
    });
    true
}

pub fn unregister(id: &str) {
    SOURCES.with_borrow_mut(|sources| {
        if let Some(source) = sources.live.remove(id) {
            sources.events.push(SourceEvent::Unregistered {
                id: id.to_owned(),
                revision: source.revision,
            });
        }
    });
}

/// `id`'s content is now `revision` (monotonic; a lower one is ignored).
pub fn revise(id: &str, revision: u64) {
    SOURCES.with_borrow_mut(|sources| {
        if let Some(source) = sources.live.get_mut(id)
            && revision > source.revision
        {
            let now = now_us();
            source.revision = revision;
            source.revised_us = Some(now);
            source.first_revised_us.get_or_insert(now);
        }
    });
}

/// Add a content update's cost to `id`: `upload_bytes` copied CPU to GPU and
/// `damage_px` physical pixels redrawn. Summed until the next report, so
/// several updates between two snapshots read as one cost.
pub fn cost(id: &str, upload_bytes: u64, damage_px: u64) {
    SOURCES.with_borrow_mut(|sources| {
        if let Some(source) = sources.live.get_mut(id) {
            source.upload_bytes = source.upload_bytes.saturating_add(upload_bytes);
            source.damage_px = source.damage_px.saturating_add(damage_px);
        }
    });
}

/// `output` drew `id` in the frame being built.
pub fn shown(id: &str, output: &str) {
    SOURCES.with_borrow_mut(|sources| {
        if let Some(source) = sources.live.get_mut(id) {
            source.shown_on.insert(output.to_owned());
        }
    });
}
pub fn set_native_handle(id: &str, handle: crate::HandleId) {
    SOURCES.with_borrow_mut(|sources| {
        if let Some(source) = sources.live.get_mut(id) {
            source.native_handle = Some(handle);
        }
    });
}
pub fn native_handle(id: &str) -> Option<crate::HandleId> {
    SOURCES.with_borrow(|sources| sources.live.get(id).and_then(|source| source.native_handle))
}

/// Registration changes since the last call, in order.
pub fn take_events() -> Vec<SourceEvent> {
    SOURCES.with_borrow_mut(|sources| std::mem::take(&mut sources.events))
}

/// One report per source measured on `output` (its own output, or any) for
/// the frame `output` is queueing: its revision, whether this output drew
/// it, and the revision times since its last report on any output.
pub fn frame(output: &str) -> Vec<FrameSource> {
    SOURCES.with_borrow_mut(|sources| {
        sources
            .live
            .iter_mut()
            .filter(|(_, source)| source.output.as_deref().is_none_or(|own| own == output))
            .map(|(id, source)| FrameSource {
                id: id.clone(),
                revision: source.revision,
                shown: source.shown_on.remove(output),
                upload_bytes: std::mem::take(&mut source.upload_bytes),
                damage_px: std::mem::take(&mut source.damage_px),
                consumed_input: None,
                revised_us: source.revised_us.take(),
                first_revised_us: source.first_revised_us.take(),
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_reports_its_revision_and_where_it_was_shown() {
        assert!(!register("Bad Id", None));
        assert!(
            !register("scene-gate", None),
            "a '-' is no prop path segment"
        );
        assert!(register("scene_gate", Some("DP-1".into())));
        assert!(
            register("scene_gate", Some("DP-1".into())),
            "re-registering a live id is a no-op"
        );
        assert_eq!(
            take_events(),
            [SourceEvent::Registered {
                id: "scene_gate".into(),
                output: Some("DP-1".into())
            }]
        );
        revise("scene_gate", 2);
        revise("scene_gate", 1);
        cost("scene_gate", 0, 100);
        cost("scene_gate", 8, 50);
        cost("nobody", 1, 1);
        shown("scene_gate", "DP-1");
        assert!(
            frame("HDMI-A-1").is_empty(),
            "measured on its own output only"
        );
        let report = frame("DP-1");
        assert_eq!(report.len(), 1);
        assert_eq!((report[0].revision, report[0].shown), (2, true));
        assert!(report[0].revised_us.is_some() && report[0].first_revised_us.is_some());
        assert_eq!(
            (report[0].upload_bytes, report[0].damage_px),
            (8, 150),
            "costs summed until reported"
        );
        // The next frame: not drawn again, no new revision, no new cost.
        let report = frame("DP-1");
        assert_eq!((report[0].shown, report[0].revised_us), (false, None));
        assert_eq!((report[0].upload_bytes, report[0].damage_px), (0, 0));
        unregister("scene_gate");
        assert_eq!(
            take_events(),
            [SourceEvent::Unregistered {
                id: "scene_gate".into(),
                revision: 2
            }]
        );
        assert!(frame("DP-1").is_empty());
    }
}
