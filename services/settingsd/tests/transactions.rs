// SPDX-License-Identifier: MIT OR Apache-2.0
use serde_json::json;
use settings::*;
use settingsd::{authority::Authority, store::Store};
use std::collections::BTreeMap;

fn binding() -> Binding {
    Binding {
        instance: "fixture".into(),
        profile: "default".into(),
    }
}
fn authority(dir: &std::path::Path) -> Authority {
    let (store, data) = Store::create(dir, binding(), Desktop::default()).unwrap();
    Authority::new(store, data).unwrap()
}
fn request(authority: &Authority, id: &str, mode: &str) -> ApplyRequest {
    ApplyRequest {
        binding: binding(),
        expected_incarnation: authority.accepted.incarnation.clone(),
        expected_revision: authority.accepted.revision,
        operation_id: id.into(),
        changes: BTreeMap::from([("appearance.mode".into(), json!(mode))]),
        reset: vec![],
        request_digest: None,
    }
}
#[test]
fn lost_reply_and_later_edit_return_original_receipt_before_revision_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let mut authority = authority(dir.path());
    let first = request(&authority, "first", "dark");
    assert_eq!(
        authority.apply(first.clone()).unwrap()["receipt"]["revision"],
        "2"
    );
    let second = request(&authority, "second", "light");
    authority.apply(second).unwrap();
    let retry = authority.apply(first.clone()).unwrap();
    assert_eq!(retry["receipt"]["revision"], "2");
    assert_eq!(retry["replayed"], true);
    assert_eq!(authority.accepted.revision, Revision(3));
    let mut reused = first;
    reused
        .changes
        .insert("appearance.mode".into(), json!("light"));
    assert_eq!(
        authority.apply(reused).unwrap_err()["status"],
        "operation_id_reused"
    );
}
#[test]
fn no_op_is_durable_without_a_revision_or_snapshot_change() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = authority(dir.path());
    let before = state.snapshot.clone();
    let req = request(&state, "noop", "light");
    assert_eq!(state.apply(req.clone()).unwrap()["status"], "unchanged");
    assert_eq!(state.snapshot, before);
    drop(state);
    let (store, data) = Store::open(dir.path(), &binding()).unwrap();
    let mut restored = Authority::new(store, data).unwrap();
    assert_eq!(restored.apply(req).unwrap()["replayed"], true);
    assert_eq!(restored.accepted.revision, Revision(1));
    assert_eq!(
        restored.status(&binding(), Some("never-seen")).unwrap()["status"],
        "unknown_operation"
    );
}
#[test]
fn invalid_batch_conflicting_editor_and_wrong_target_preserve_accepted_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = authority(dir.path());
    let before = std::fs::read(dir.path().join("desktop.conf.mix")).unwrap();
    let mut bad = request(&state, "bad", "dark");
    bad.changes
        .insert("shell.panels.bottom.thickness".into(), json!(999));
    assert_eq!(state.apply(bad).unwrap_err()["status"], "validation_failed");
    assert_eq!(
        std::fs::read(dir.path().join("desktop.conf.mix")).unwrap(),
        before
    );
    let winner = request(&state, "winner", "dark");
    let loser = request(&state, "loser", "dark");
    state.apply(winner).unwrap();
    assert_eq!(state.apply(loser).unwrap_err()["status"], "conflict");
    let mut wrong = request(&state, "wrong", "light");
    wrong.binding.instance = "other".into();
    assert_eq!(state.apply(wrong).unwrap_err()["status"], "wrong_target");
    assert_eq!(state.accepted.revision, Revision(2));
}
#[test]
fn bad_request_digest_and_reset_conflict_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = authority(dir.path());
    let mut wrong = request(&state, "digest", "dark");
    wrong.request_digest = Some("forged".into());
    assert!(state.apply(wrong).is_err());
    let mut wrong = request(&state, "overlap", "dark");
    wrong.reset.push("appearance.mode".into());
    assert!(state.apply(wrong).is_err());
    assert_eq!(state.accepted.receipts.len(), 0);
}
#[test]
fn receipt_eviction_is_unknown_not_invented_expiry_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = authority(dir.path());
    for n in 0..=MAX_RECEIPTS {
        let req = request(&state, &format!("noop-{n}"), "light");
        state.apply(req).unwrap();
    }
    assert_eq!(state.accepted.receipts.len(), MAX_RECEIPTS);
    assert_eq!(state.accepted.revision, Revision(1));
    assert_eq!(
        state.status(&binding(), Some("noop-0")).unwrap()["status"],
        "unknown_operation"
    );
    drop(state);
    let (_, data) = Store::open(dir.path(), &binding()).unwrap();
    assert_eq!(data.receipts.len(), MAX_RECEIPTS);
}
#[test]
fn corrupt_primary_is_preserved_and_backup_restore_fences_old_history() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = authority(dir.path());
    let old_incarnation = state.accepted.incarnation.clone();
    let change = request(&state, "change", "dark");
    state.apply(change).unwrap();
    drop(state);
    std::fs::write(dir.path().join("desktop.conf.mix"), "{ incomplete").unwrap();
    let (store, data) = Store::open(dir.path(), &binding()).unwrap();
    assert!(store.restored);
    assert_ne!(data.incarnation, old_incarnation);
    assert_eq!(data.desktop.appearance.mode, "light");
    assert!(data.receipts.is_empty());
    assert!(std::fs::read_dir(dir.path()).unwrap().any(|f| {
        f.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("corrupt-")
    }));
}
#[test]
fn newer_schema_never_falls_back_over_user_data() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = authority(dir.path());
    let change = request(&state, "change", "dark");
    state.apply(change).unwrap();
    drop(state);
    let path = dir.path().join("desktop.conf.mix");
    let mut value: serde_json::Value =
        strict::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    value["schema"] = json!(2);
    value["future_field"] = json!({"new":"keep"});
    let future = strict::to_string_pretty(&value).unwrap();
    std::fs::write(&path, &future).unwrap();
    assert!(Store::open(dir.path(), &binding()).is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), future);
}
