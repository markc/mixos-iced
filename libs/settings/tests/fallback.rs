// SPDX-License-Identifier: MIT OR Apache-2.0
use settings::{consumer::Consumer, fallback::PresentationKind, *};

fn binding() -> Binding {
    Binding {
        instance: "fixture".into(),
        profile: "default".into(),
    }
}
fn snapshot(revision: u64, density: f64) -> Snapshot {
    let mut desktop = Desktop::default();
    desktop.ui.density = density;
    Snapshot {
        schema: SCHEMA,
        binding: binding(),
        incarnation: "authority".into(),
        revision: Revision(revision),
        design_revision: Revision(1),
        source_digest: source_digest(EMBEDDED_DEFAULT_SOURCE),
        effective: resolve(&desktop).unwrap(),
        desktop,
    }
}
fn resources(snapshot: &Snapshot, context: &str, _: bool) -> Result<(), Diagnostic> {
    // Headless fixture inventory: every role must identify its font family.
    assert!(
        snapshot.effective[context]
            .design
            .typography
            .values()
            .all(|role| !role.family.is_empty())
    );
    Ok(())
}
#[test]
fn embedded_is_staged_not_authority_and_unchanged_fresh_read_promotes_evidence() {
    let mut state = Consumer::for_app(binding(), "ced").unwrap();
    let request = state.fallback_request().unwrap();
    assert!(state.fallback_request().is_none());
    let prepared = request.prepare(None, resources).unwrap();
    assert_eq!(prepared.kind(), PresentationKind::Embedded);
    assert!(state.complete_fallback(&request, Ok(prepared)));
    assert!(state.current().is_none());
    assert!(state.applied().is_none());
    let update = state.pending().unwrap().clone();
    assert_eq!(update.kind(), PresentationKind::Embedded);
    assert!(state.is_current(&update));
    assert!(state.acknowledge(&update));
    assert_eq!(state.presentation_kind(), Some(PresentationKind::Embedded));
    assert!(state.current().is_none());
    assert!(!state.is_confirmed());
    #[cfg(feature = "cache")]
    assert!(state.cache_save().is_none());
    let subscribe = state.connected(1).unwrap();
    let read = state.complete(&subscribe, Ok(None)).unwrap();
    state.complete(&read, Ok(Some(snapshot(1, 1.0))));
    assert!(state.pending().is_none(), "same defaults require no swap");
    assert_eq!(state.presentation_kind(), Some(PresentationKind::Current));
    assert_eq!(state.current().unwrap().revision, Revision(1));
    state.disconnected();
    assert_eq!(state.presentation_kind(), Some(PresentationKind::LastGood));
    assert!(
        state.fallback_request().is_none(),
        "preserve usable installed state"
    );
}
#[test]
fn retained_fallback_does_not_seed_revision_fences_and_live_read_supersedes_it() {
    let mut state = Consumer::for_app(binding(), "ced").unwrap();
    let subscribe = state.connected(1).unwrap();
    let read = state.complete(&subscribe, Ok(None)).unwrap();
    state.observe(1, snapshot(99, 1.4));
    state.complete(
        &read,
        Err(Diagnostic::new("read_timeout", "native", "Deadline")),
    );
    let request = state.fallback_request().unwrap();
    let prepared = request
        .prepare_with_cache(
            || panic!("valid retained data must precede cache I/O"),
            resources,
        )
        .unwrap();
    assert_eq!(prepared.kind(), PresentationKind::Retained);
    assert!(state.complete_fallback(&request, Ok(prepared)));
    assert!(state.acknowledge(&state.pending().unwrap().clone()));
    assert_eq!(state.applied().unwrap().revision, Revision(99));
    assert_eq!(state.presentation_kind(), Some(PresentationKind::Retained));
    assert!(state.current().is_none());
    // A reconnect discards the old retained candidate, then fresh history can
    // be below the presentation revision without triggering a rollback fence.
    let subscribe = state.connected(2).unwrap();
    let read = state.complete(&subscribe, Ok(None)).unwrap();
    state.complete(&read, Ok(Some(snapshot(1, 1.0))));
    assert_eq!(state.current().unwrap().revision, Revision(1));
    assert!(state.fault().is_none());
    assert!(state.acknowledge(&state.pending().unwrap().clone()));
    assert_eq!(state.presentation_kind(), Some(PresentationKind::Current));
}
#[test]
fn offline_request_and_stages_are_fenced_by_reconnect_and_foreign_owner() {
    let mut state = Consumer::for_app(binding(), "ced").unwrap();
    let request = state.fallback_request().unwrap();
    let prepared = request.prepare(None, resources).unwrap();
    let mut other = Consumer::for_app(binding(), "ced").unwrap();
    let foreign = other.fallback_request().unwrap();
    assert!(!other.complete_fallback(&foreign, Ok(prepared.clone())));
    assert!(state.complete_fallback(&request, Ok(prepared.clone())));
    let update = state.pending().unwrap().clone();
    let subscribe = state.connected(1).unwrap();
    assert!(!state.is_current(&update));
    assert!(!state.acknowledge(&update));
    let read = state.complete(&subscribe, Ok(None)).unwrap();
    state.complete(&read, Ok(Some(snapshot(2, 1.2))));
    assert!(!state.complete_fallback(&request, Ok(prepared)));
    assert_eq!(state.pending().unwrap().kind(), PresentationKind::Current);
}
#[test]
fn resources_fail_over_from_retained_and_complete_failure_never_stages() {
    let mut state = Consumer::for_app(binding(), "ced").unwrap();
    let subscribe = state.connected(1).unwrap();
    state.observe(1, snapshot(9, 1.4));
    let request = state.fallback_request().unwrap();
    let prepared = request
        .prepare(None, |snapshot, context, shell| {
            if snapshot.revision.0 != 0 {
                Err(Diagnostic::new(
                    "missing_font",
                    "font",
                    "Fixture resource unavailable",
                ))
            } else {
                resources(snapshot, context, shell)
            }
        })
        .unwrap();
    assert_eq!(prepared.kind(), PresentationKind::Embedded);
    assert_eq!(prepared.diagnostics()[0].code, "missing_font");
    assert!(state.complete_fallback(&request, Ok(prepared)));
    state.complete(
        &subscribe,
        Err(Diagnostic::new("subscribe_failed", "native", "Outage")),
    );
    let request = state.fallback_request().unwrap();
    let result = request.prepare(None, |_, _, _| {
        Err(Diagnostic::new("missing_resources", "assets", "None ready"))
    });
    assert!(!state.complete_fallback(&request, result));
    assert!(state.pending().is_none());
    assert!(state.applied().is_none());
    assert!(state.current().is_none());
    assert!(state.retry_deadline().is_some());
}
#[test]
fn failed_first_live_resource_stage_allows_usable_fallback_without_losing_authority_readback() {
    let mut state = Consumer::for_app(binding(), "ced").unwrap();
    let subscribe = state.connected(1).unwrap();
    let read = state.complete(&subscribe, Ok(None)).unwrap();
    state.complete(&read, Ok(Some(snapshot(2, 1.4))));
    assert!(
        state.fallback_request().is_none(),
        "fresh stage already awaits activation"
    );
    let failed = state.pending().unwrap().clone();
    assert!(state.failed(
        &failed,
        Diagnostic::new("resource_failed", "font", "Live resources unusable")
    ));
    let request = state.fallback_request().unwrap();
    let prepared = request.prepare(None, resources).unwrap();
    assert!(state.complete_fallback(&request, Ok(prepared)));
    assert!(!state.is_current(&failed));
    assert!(state.acknowledge(&state.pending().unwrap().clone()));
    assert_eq!(state.presentation_kind(), Some(PresentationKind::Embedded));
    assert_eq!(
        state.current().unwrap().revision,
        Revision(2),
        "only real readback supplies mutation fences"
    );
    assert_eq!(state.applied().unwrap().revision, Revision(0));
    assert_eq!(state.fault().unwrap().code, "resource_failed");
}
#[test]
fn successful_fallback_retry_clears_its_fault_and_preserves_authority_outage() {
    let mut state = Consumer::for_app(binding(), "ced").unwrap();
    let subscribe = state.connected(1).unwrap();
    state.complete(
        &subscribe,
        Err(Diagnostic::new("subscribe_failed", "native", "Outage")),
    );
    let request = state.fallback_request().unwrap();
    let failed = request.prepare(None, |_, _, _| {
        Err(Diagnostic::new("missing_resources", "font", "Not ready"))
    });
    assert!(!state.complete_fallback(&request, failed));
    assert_eq!(state.fallback_fault().unwrap().code, "missing_resources");
    assert_eq!(state.fault().unwrap().code, "subscribe_failed");
    let request = state.fallback_request().unwrap();
    let prepared = request.prepare(None, resources).unwrap();
    assert!(state.complete_fallback(&request, Ok(prepared)));
    assert!(state.acknowledge(&state.pending().unwrap().clone()));
    assert!(state.fallback_fault().is_none());
    assert_eq!(state.fault().unwrap().code, "subscribe_failed");
    assert_eq!(state.presentation_kind(), Some(PresentationKind::Embedded));
}
