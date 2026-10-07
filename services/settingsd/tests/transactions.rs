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
fn validate_reports_its_base_and_rejects_stale_editors_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = authority(dir.path());
    let candidate = request(&state, "candidate", "dark");
    assert_eq!(state.validate(&candidate).unwrap()["revision"], "1");
    assert!(state.accepted.receipts.is_empty());
    state.apply(request(&state, "winner", "dark")).unwrap();
    assert_eq!(
        state.validate(&candidate).unwrap_err()["status"],
        "conflict"
    );
    assert_eq!(state.accepted.receipts.len(), 1);
}
#[test]
fn pinned_source_and_snapshot_survive_restart_and_valid_tampering_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let state = authority(dir.path());
    assert_eq!(
        state.snapshot.source_digest,
        source_digest(&state.accepted.embedded_source)
    );
    let before = state.snapshot.clone();
    drop(state);
    let (store, data) = Store::open(dir.path(), &binding()).unwrap();
    let state = Authority::new(store, data).unwrap();
    assert_eq!(state.snapshot, before);
    drop(state);
    let path = dir.path().join("desktop.conf.mix");
    let mut value: serde_json::Value =
        strict::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    value["desktop"]["appearance"]["mode"] = json!("dark");
    let tampered = strict::to_string_pretty(&value).unwrap();
    std::fs::write(&path, &tampered).unwrap();
    assert!(Store::open(dir.path(), &binding()).is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), tampered);
}
#[test]
fn missing_primary_with_backup_requires_visible_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = authority(dir.path());
    state.apply(request(&state, "change", "dark")).unwrap();
    drop(state);
    let path = dir.path().join("desktop.conf.mix");
    std::fs::remove_file(&path).unwrap();
    assert!(Store::open(dir.path(), &binding()).is_err());
    assert!(!path.exists());
    assert!(dir.path().join("desktop.previous.conf.mix").exists());
}
#[test]
fn session_seed_is_idempotent_and_never_recreates_lost_or_foreign_state() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("profile");
    assert!(Store::seed(&root, binding(), false).is_err());
    assert!(
        !root.exists(),
        "automatic startup cannot create a missing profile"
    );
    let (store, accepted) = Store::seed(&root, binding(), true).unwrap();
    let initial = std::fs::read(root.join("desktop.conf.mix")).unwrap();
    assert!(
        Store::seed(&root, binding(), false).is_err(),
        "active writer must exclude seed"
    );
    drop(store);
    let (store, again) = Store::seed(&root, binding(), false).unwrap();
    assert_eq!(again.incarnation, accepted.incarnation);
    assert_eq!(
        std::fs::read(root.join("desktop.conf.mix")).unwrap(),
        initial
    );
    drop(store);
    let foreign = Binding {
        instance: "other".into(),
        ..binding()
    };
    assert!(Store::seed(&root, foreign, false).is_err());
    assert_eq!(
        std::fs::read(root.join("desktop.conf.mix")).unwrap(),
        initial
    );
    std::fs::remove_file(root.join("desktop.conf.mix")).unwrap();
    assert!(Store::seed(&root, binding(), false).is_err());
    assert!(
        Store::seed(&root, binding(), true).is_err(),
        "a retained lock forbids recreating an established missing primary even with the creation flag"
    );
    assert!(!root.join("desktop.conf.mix").exists());
    std::fs::rename(&root, parent.path().join("detached")).unwrap();
    assert!(Store::seed(&root, binding(), false).is_err());
    assert!(
        !root.exists(),
        "a whole missing directory must remain visibly missing"
    );
}
#[test]
fn directory_or_lock_replacement_fences_the_existing_writer() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("profile");
    let mut state = authority(&root);
    let req = request(&state, "detached", "dark");
    std::fs::rename(&root, parent.path().join("detached")).unwrap();
    std::fs::create_dir(&root).unwrap();
    assert_eq!(state.apply(req).unwrap_err()["status"], "outcome_unknown");
    assert!(!root.join("desktop.conf.mix").exists());
    assert_eq!(state.accepted.revision, Revision(1));
    let other = parent.path().join("other");
    let mut state = authority(&other);
    std::fs::remove_file(other.join("writer.lock")).unwrap();
    std::fs::write(other.join("writer.lock"), "").unwrap();
    let req = request(&state, "replaced-lock", "dark");
    assert_eq!(state.apply(req).unwrap_err()["status"], "outcome_unknown");
    assert_eq!(state.accepted.revision, Revision(1));
}
#[test]
fn canonical_request_digest_ignores_object_key_order_but_binds_fences() {
    let dir = tempfile::tempdir().unwrap();
    let state = authority(dir.path());
    let mut first = request(&state, "canonical", "dark");
    first.changes.insert(
        "apps.term".into(),
        serde_json::from_str(r#"{"mode":"dark","contrast":"normal"}"#).unwrap(),
    );
    let mut second = first.clone();
    second.changes.insert(
        "apps.term".into(),
        serde_json::from_str(r#"{"contrast":"normal","mode":"dark"}"#).unwrap(),
    );
    assert_eq!(first.digest().unwrap(), second.digest().unwrap());
    second.expected_revision = Revision(2);
    assert_ne!(first.digest().unwrap(), second.digest().unwrap());
}
#[test]
fn public_machine_fixtures_exercise_dispatch_and_restartable_float_settings() {
    let fixture: serde_json::Value = strict::from_str(include_str!(
        "../../../docs/spec/settings/authority.spec.mix"
    ))
    .unwrap();
    let binding: Binding = serde_json::from_value(fixture["binding"].clone()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (store, accepted) = Store::create(dir.path(), binding.clone(), Desktop::default()).unwrap();
    let mut state = Authority::new(store, accepted).unwrap();
    assert_eq!(
        settingsd::service::dispatch(&mut state, "settings.describe", "").unwrap()["version"],
        fixture["version"]
    );
    for batch in fixture["batches"].as_array().unwrap() {
        let req = json!({"binding":binding,"expected_incarnation":state.accepted.incarnation,"expected_revision":state.accepted.revision,
            "operation_id":batch["name"],"changes":batch["changes"],"reset":batch.get("reset").cloned().unwrap_or(json!([]))});
        let body = serde_json::to_string(&req).unwrap();
        let before = std::fs::read(dir.path().join("desktop.conf.mix")).unwrap();
        let validation = settingsd::service::dispatch(&mut state, "settings.validate", &body);
        let result = settingsd::service::dispatch(
            &mut state,
            if batch["name"] == "reset_app" {
                "settings.reset"
            } else {
                "settings.apply"
            },
            &body,
        );
        if let Some(expected) = batch.get("expected") {
            assert_eq!(validation.unwrap_err()["status"], *expected);
            assert_eq!(result.unwrap_err()["status"], *expected);
            assert_eq!(
                std::fs::read(dir.path().join("desktop.conf.mix")).unwrap(),
                before
            );
        } else {
            assert_eq!(validation.unwrap()["status"], "valid");
            let receipt = result.unwrap()["receipt"].clone();
            let status = settingsd::service::dispatch(
                &mut state,
                "settings.status",
                &json!({"binding":binding,"operation_id":batch["name"]}).to_string(),
            )
            .unwrap();
            assert_eq!(status["receipt"], receipt);
        }
    }
    let snapshot = settingsd::service::dispatch(
        &mut state,
        "settings.get",
        &json!({"binding":binding}).to_string(),
    )
    .unwrap()["snapshot"]
        .clone();
    assert_eq!(snapshot["desktop"]["ui"]["text_scale"], 1.1);
    drop(state);
    let (store, accepted) = Store::open(dir.path(), &binding).unwrap();
    let restored = Authority::new(store, accepted).unwrap();
    assert_eq!(serde_json::to_value(restored.snapshot).unwrap(), snapshot);
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
#[test]
fn intact_shape_or_compiler_changes_fail_without_automatic_backup_regression() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = authority(dir.path());
    state.apply(request(&state, "change", "dark")).unwrap();
    let mut accepted = state.accepted.clone();
    drop(state);
    let path = dir.path().join("desktop.conf.mix");
    let mut value: serde_json::Value =
        strict::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    value["future_field"] = json!("requires-migration");
    let future = strict::to_string_pretty(&value).unwrap();
    std::fs::write(&path, &future).unwrap();
    assert!(Store::open(dir.path(), &binding()).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), future);
    // Valid integrity but unavailable/changed compiler interpretation is also
    // a migration error, not evidence that an old backup should replace it.
    accepted.embedded_source = "{ requires_new_compiler: true }".into();
    accepted.seal().unwrap();
    let sealed = strict::to_string_pretty(&accepted).unwrap();
    std::fs::write(&path, &sealed).unwrap();
    assert!(Store::open(dir.path(), &binding()).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), sealed);
    accepted.embedded_source = EMBEDDED_DEFAULT_SOURCE.into();
    accepted.effective_digest = "0".repeat(64);
    accepted.seal().unwrap();
    let drift = strict::to_string_pretty(&accepted).unwrap();
    std::fs::write(&path, &drift).unwrap();
    assert!(Store::open(dir.path(), &binding()).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), drift);
}
