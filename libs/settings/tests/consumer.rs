// SPDX-License-Identifier: MIT OR Apache-2.0
use settings::{
    consumer::{Consumer, Work, WorkKind},
    domains::ChangePlan,
    *,
};
use std::{collections::BTreeMap, sync::OnceLock, time::Duration};

fn binding() -> Binding {
    Binding {
        instance: "fixture".into(),
        profile: "default".into(),
    }
}
fn snapshot(revision: u64, incarnation: &str) -> Snapshot {
    static EFFECTIVE: OnceLock<BTreeMap<String, Effective>> = OnceLock::new();
    Snapshot {
        schema: SCHEMA,
        binding: binding(),
        incarnation: incarnation.into(),
        revision: Revision(revision),
        design_revision: Revision(1),
        source_digest: "source".into(),
        desktop: Desktop::default(),
        effective: EFFECTIVE
            .get_or_init(|| resolve(&Desktop::default()).unwrap())
            .clone(),
    }
}
fn consumer() -> Consumer {
    Consumer::for_app(binding(), "ced").unwrap()
}
fn read_work(state: &mut Consumer, generation: u64) -> Work {
    let subscribe = state.connected(generation).unwrap();
    assert_eq!(subscribe.kind(), WorkKind::Subscribe);
    let read = state.complete(&subscribe, Ok(None)).unwrap();
    assert_eq!(read.kind(), WorkKind::Read);
    read
}
fn activate(state: &mut Consumer, snapshot: Snapshot) {
    let read = read_work(state, 1);
    assert!(state.complete(&read, Ok(Some(snapshot))).is_none());
    assert!(state.applied().is_none(), "accepted is not applied");
    assert!(state.acknowledge(&state.pending().unwrap().clone()));
}
#[test]
fn read_event_race_keeps_newest_and_fences_old_renderer_completion() {
    let mut state = consumer();
    activate(&mut state, snapshot(1, "a"));
    let read = state.refresh().unwrap();
    let mut latest = snapshot(3, "a");
    latest.effective.get_mut("app:ced").unwrap().ui.density = 1.5;
    assert!(state.observe(1, latest).is_none());
    let staged = state.pending().unwrap().clone();
    assert_eq!(state.applied().unwrap().revision, Revision(1));
    state.complete(&read, Ok(Some(snapshot(2, "a"))));
    assert_eq!(state.current().unwrap().revision, Revision(3));
    assert!(state.acknowledge(&staged));
    let mut fourth = snapshot(4, "a");
    fourth.effective.get_mut("app:ced").unwrap().ui.density = 2.0;
    state.observe(1, fourth);
    let old = state.pending().unwrap().clone();
    let mut fifth = snapshot(5, "a");
    fifth.effective.get_mut("app:ced").unwrap().ui.text_scale = 1.2;
    state.observe(1, fifth);
    assert!(!state.acknowledge(&old));
    assert_eq!(state.applied().unwrap().revision, Revision(3));
    let current = state.pending().unwrap().clone();
    assert!(state.failed(
        &current,
        Diagnostic::new("resource_failed", "font", "Missing face")
    ));
    assert_eq!(state.applied().unwrap().revision, Revision(3));
}
#[test]
fn reconnect_loss_and_foreign_consumer_tickets_are_fenced_and_coalesced() {
    let mut state = consumer();
    let old = read_work(&mut state, 1);
    let mut other = consumer();
    let foreign = read_work(&mut other, 1);
    assert!(
        state
            .complete(&foreign, Ok(Some(snapshot(99, "wrong-history"))))
            .is_none()
    );
    assert!(state.current().is_none());
    let current = read_work(&mut state, 2);
    state.complete(&old, Ok(Some(snapshot(99, "old"))));
    state.observe(1, snapshot(99, "old"));
    assert!(state.current().is_none());
    for _ in 0..100 {
        assert!(state.lost().is_none());
    }
    let followup = state
        .complete(&current, Ok(Some(snapshot(8, "old"))))
        .unwrap();
    assert!(state.current().is_none());
    state.complete(&followup, Ok(Some(snapshot(1, "new"))));
    let update = state.pending().unwrap().clone();
    other.complete(&foreign, Ok(Some(snapshot(1, "new"))));
    assert!(!other.acknowledge(&update));
    state.disconnected();
    assert!(!state.acknowledge(&update));
    assert!(state.retry_delay().is_none());
}
#[test]
fn retained_new_incarnation_requires_read_and_confirmed_contradiction_does_not_loop() {
    let mut state = consumer();
    activate(&mut state, snapshot(7, "a"));
    let read = state.observe(1, snapshot(1, "b")).unwrap();
    assert_eq!(state.current().unwrap().incarnation, "a");
    state.complete(&read, Ok(Some(snapshot(1, "b"))));
    assert_eq!(
        state.applied().unwrap().incarnation,
        "b",
        "unchanged render data advances evidence"
    );
    let mut contradiction = snapshot(1, "b");
    contradiction.source_digest = "contradiction".into();
    let read = state.observe(1, contradiction.clone()).unwrap();
    assert!(state.complete(&read, Ok(Some(contradiction))).is_none());
    assert_eq!(state.fault().unwrap().code, "authority_contradiction");
    assert!(state.retry_delay().is_none());
    assert_eq!(state.current().unwrap().source_digest, "source");
}
#[test]
fn failed_recovery_has_one_bounded_retry_and_success_or_disconnect_removes_it() {
    let mut state = consumer();
    let mut work = state.connected(1).unwrap();
    for _ in 0..25 {
        state.complete(&work, Err(Diagnostic::new("outage", "native", "Offline")));
        assert!(state.retry_delay().unwrap() <= Duration::from_secs(30));
        work = state.retry().unwrap();
        assert!(state.retry().is_none());
    }
    let read = state.complete(&work, Ok(None)).unwrap();
    state.complete(&read, Ok(Some(snapshot(1, "a"))));
    assert!(state.retry_delay().is_none());
    assert!(state.retry().is_none());
    let work = state.refresh().unwrap();
    state.complete(&work, Err(Diagnostic::new("outage", "native", "Offline")));
    state.disconnected();
    assert!(state.retry_delay().is_none());
    assert!(state.retry().is_none());
}
#[test]
fn provenance_source_revision_and_other_app_changes_do_not_invalidate_ced() {
    let initial = snapshot(1, "a");
    let mut next = snapshot(2, "a");
    next.design_revision = Revision(2);
    next.source_digest = "other-source".into();
    let ced = next.effective.get_mut("app:ced").unwrap();
    ced.design.source = "renamed-source".into();
    ced.provenance.insert("mode".into(), "app".into());
    ced.design.pairs.get_mut("base").unwrap().contrast_ratio += 0.1;
    next.desktop.apps.insert(
        "term".into(),
        AppOverride {
            mode: Some("dark".into()),
            ..Default::default()
        },
    );
    next.effective.get_mut("app:term").unwrap().mode = "dark".into();
    assert!(ChangePlan::between(Some(&initial), &next, "app:ced", false).is_empty());
    let mut state = consumer();
    activate(&mut state, initial);
    state.observe(1, next);
    assert!(state.pending().is_none());
    assert_eq!(state.applied().unwrap().revision, Revision(2));
}
#[test]
fn domain_plan_is_shared_and_shell_geometry_is_only_for_shell_consumers() {
    let initial = snapshot(1, "a");
    let mut next = initial.clone();
    next.desktop
        .shell
        .panels
        .get_mut("bottom")
        .unwrap()
        .thickness = 48;
    assert!(ChangePlan::between(Some(&initial), &next, "app:ced", false).is_empty());
    let shell = ChangePlan::between(Some(&initial), &next, "desktop", true);
    assert!(shell.shell && shell.layout && !shell.paint && !shell.text);
    next = initial.clone();
    next.effective.get_mut("app:ced").unwrap().ui.text_scale = 1.2;
    let scale = ChangePlan::between(Some(&initial), &next, "app:ced", false);
    assert!(scale.text && scale.layout && !scale.resources && !scale.paint);
    next = initial.clone();
    next.effective
        .get_mut("app:ced")
        .unwrap()
        .design
        .typography
        .get_mut("ui")
        .unwrap()
        .family = "Another face".into();
    let face = ChangePlan::between(Some(&initial), &next, "app:ced", false);
    assert!(face.resources && face.text && face.layout);
}
#[test]
fn wrong_target_unsupported_schema_and_missing_context_never_activate() {
    let mut state = consumer();
    let read = read_work(&mut state, 1);
    let mut invalid = snapshot(1, "a");
    invalid.effective.remove("app:ced");
    state.complete(&read, Ok(Some(invalid)));
    assert!(state.current().is_none());
    assert!(state.pending().is_none());
    let mut state = consumer();
    let read = read_work(&mut state, 1);
    let mut wrong = snapshot(1, "a");
    wrong.binding.profile = "other".into();
    state.complete(&read, Ok(Some(wrong)));
    assert_eq!(state.fault().unwrap().code, "wrong_target");
    assert!(state.retry_delay().is_none());
    let read = state.refresh().unwrap();
    let mut future = snapshot(1, "a");
    future.schema += 1;
    state.complete(&read, Ok(Some(future)));
    assert_eq!(state.fault().unwrap().code, "unsupported_schema");
    assert!(state.current().is_none());
}
#[test]
fn bootstrap_delivery_flood_is_one_buffer_and_retired_candidate_does_not_amplify_reads() {
    let mut state = consumer();
    let read = read_work(&mut state, 1);
    for _ in 0..100 {
        assert!(state.observe(1, snapshot(3, "a")).is_none());
    }
    assert!(state.complete(&read, Ok(Some(snapshot(2, "a")))).is_none());
    assert_eq!(state.current().unwrap().revision, Revision(3));
    assert!(state.acknowledge(&state.pending().unwrap().clone()));
    let read = state.observe(1, snapshot(90, "retired")).unwrap();
    for _ in 0..100 {
        assert!(state.observe(1, snapshot(90, "retired")).is_none());
    }
    assert!(state.complete(&read, Ok(Some(snapshot(3, "a")))).is_none());
    for _ in 0..100 {
        assert!(state.observe(1, snapshot(90, "retired")).is_none());
    }
    assert_eq!(state.current().unwrap().incarnation, "a");
    assert!(state.connected(0).is_none());
    assert!(state.connected(1).is_none());
    assert!(state.connected(2).is_some());
    assert!(state.connected(1).is_none());
    assert_eq!(state.generation(), Some(2));
}
