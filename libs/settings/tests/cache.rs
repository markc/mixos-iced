// SPDX-License-Identifier: MIT OR Apache-2.0
#![cfg(feature = "cache")]
use settings::{
    cache::{self, WriteOutcome, Writer},
    consumer::Consumer,
    fallback::PresentationKind,
    *,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn binding() -> Binding {
    Binding {
        instance: "fixture".into(),
        profile: "default".into(),
    }
}
fn consumer() -> Consumer {
    Consumer::for_app(binding(), "ced").unwrap()
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
fn activate(state: &mut Consumer, revision: u64, density: f64) {
    let value = snapshot(revision, density);
    if state.generation().is_none() {
        let subscribe = state.connected(1).unwrap();
        let read = state.complete(&subscribe, Ok(None)).unwrap();
        state.complete(&read, Ok(Some(value)));
    } else {
        state.observe(1, value);
    }
    if let Some(update) = state.pending().cloned() {
        assert!(state.acknowledge(&update));
    }
}
fn path(dir: &Path) -> PathBuf {
    dir.join(format!(
        "{}.json",
        digest(&(binding(), "app:ced", false)).unwrap()
    ))
}
fn populated() -> (tempfile::TempDir, Consumer, Writer) {
    let dir = tempfile::tempdir().unwrap();
    let mut state = consumer();
    activate(&mut state, 1, 1.0);
    let mut writer = Writer::open(dir.path(), &state).unwrap();
    assert_eq!(
        writer.write(&state.cache_save().unwrap()).unwrap(),
        WriteOutcome::Written
    );
    (dir, state, writer)
}
#[test]
fn cache_roundtrip_is_resource_checked_presentation_and_never_authority() {
    let (dir, state, _) = populated();
    let mut fresh = consumer();
    let candidate = cache::load(dir.path(), &fresh).unwrap();
    assert_eq!(candidate.snapshot(), state.applied().unwrap());
    let request = fresh.fallback_request().unwrap();
    let mut checks = 0;
    let prepared = request
        .prepare(Some(candidate), |snapshot, context, shell| {
            assert_eq!(context, "app:ced");
            assert!(!shell);
            assert!(
                snapshot.effective[context]
                    .design
                    .typography
                    .values()
                    .all(|role| !role.family.is_empty())
            );
            checks += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(checks, 1);
    assert_eq!(prepared.kind(), PresentationKind::Cached);
    assert!(fresh.complete_fallback(&request, Ok(prepared)));
    assert!(fresh.acknowledge(&fresh.pending().unwrap().clone()));
    assert_eq!(fresh.presentation_kind(), Some(PresentationKind::Cached));
    assert!(fresh.current().is_none());
    assert!(!fresh.is_confirmed());
    assert!(fresh.cache_save().is_none());
}
#[test]
fn cache_missing_or_resource_failure_uses_embedded_without_rewriting_cache() {
    let (dir, _, _) = populated();
    let before = fs::read(path(dir.path())).unwrap();
    let mut fresh = consumer();
    let candidate = cache::load(dir.path(), &fresh).unwrap();
    let request = fresh.fallback_request().unwrap();
    let prepared = request
        .prepare(Some(candidate), |snapshot, _, _| {
            if snapshot.revision.0 > 0 {
                Err(Diagnostic::new(
                    "missing_font",
                    "font",
                    "Fixture cache resource lost",
                ))
            } else {
                Ok(())
            }
        })
        .unwrap();
    assert_eq!(prepared.kind(), PresentationKind::Embedded);
    assert_eq!(prepared.diagnostics()[0].code, "missing_font");
    assert!(fresh.complete_fallback(&request, Ok(prepared)));
    assert!(fresh.acknowledge(&fresh.pending().unwrap().clone()));
    assert!(fresh.cache_save().is_none());
    assert_eq!(fs::read(path(dir.path())).unwrap(), before);
    let missing = tempfile::tempdir().unwrap();
    assert!(cache::load(missing.path(), &fresh).is_err());
    assert!(
        fs::read_dir(missing.path()).unwrap().next().is_none(),
        "read-only bootstrap creates nothing"
    );
}
#[test]
fn stale_saves_and_duplicate_writers_cannot_replace_newer_data() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = consumer();
    assert!(state.cache_save().is_none());
    let subscribe = state.connected(1).unwrap();
    let read = state.complete(&subscribe, Ok(None)).unwrap();
    state.complete(&read, Ok(Some(snapshot(1, 1.0))));
    assert!(state.cache_save().is_none(), "readback is not applied");
    assert!(state.acknowledge(&state.pending().unwrap().clone()));
    let old = state.cache_save().unwrap();
    let mut writer = Writer::open(dir.path(), &state).unwrap();
    assert!(Writer::open(dir.path(), &state).is_err());
    activate(&mut state, 2, 1.4);
    let new = state.cache_save().unwrap();
    assert_eq!(writer.write(&new).unwrap(), WriteOutcome::Written);
    assert_eq!(writer.write(&old).unwrap(), WriteOutcome::Superseded);
    assert_eq!(writer.write(&new).unwrap(), WriteOutcome::Unchanged);
    assert_eq!(
        cache::load(dir.path(), &state).unwrap().snapshot().revision,
        Revision(2)
    );
    // Staging and RPC work do not relabel old applied data with a new serial.
    state.observe(1, snapshot(3, 1.7));
    assert_eq!(
        writer.write(&state.cache_save().unwrap()).unwrap(),
        WriteOutcome::Unchanged
    );
    assert!(state.acknowledge(&state.pending().unwrap().clone()));
    assert_eq!(
        writer.write(&state.cache_save().unwrap()).unwrap(),
        WriteOutcome::Written
    );
    state.observe(1, snapshot(4, 1.7));
    assert!(state.pending().is_none());
    assert_eq!(
        writer.write(&state.cache_save().unwrap()).unwrap(),
        WriteOutcome::Written,
        "unchanged rendering still advances cache evidence"
    );
    let mut foreign = consumer();
    activate(&mut foreign, 3, 1.0);
    assert_eq!(
        writer
            .write(&foreign.cache_save().unwrap())
            .unwrap_err()
            .code,
        "wrong_cache_producer"
    );
}
#[test]
fn cache_refuses_tampering_including_resealed_projection_and_foreign_targets() {
    let (dir, state, _) = populated();
    let file = path(dir.path());
    let original: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    let mut future = original.clone();
    future["schema"] = 2.into();
    future["new_future_field"] = true.into();
    fs::write(&file, serde_json::to_vec(&future).unwrap()).unwrap();
    assert_eq!(
        cache::load(dir.path(), &state).unwrap_err().code,
        "unsupported_cache"
    );
    future["schema"] = 1.into();
    fs::write(&file, serde_json::to_vec(&future).unwrap()).unwrap();
    assert_eq!(
        cache::load(dir.path(), &state).unwrap_err().code,
        "invalid_cache"
    );
    for field in ["schema", "interpretation", "context", "shell", "digest"] {
        let mut value = original.clone();
        value[field] = match field {
            "schema" => 2.into(),
            "shell" => true.into(),
            _ => "foreign".into(),
        };
        fs::write(&file, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(cache::load(dir.path(), &state).is_err(), "{field}");
    }
    for changed in ["effective", "source_digest", "binding", "revision"] {
        let mut value = original.clone();
        match changed {
            "effective" => value["snapshot"]["effective"]["app:ced"]["ui"]["density"] = 9.0.into(),
            "source_digest" => value["snapshot"]["source_digest"] = "absent-old-package".into(),
            "binding" => value["snapshot"]["binding"]["profile"] = "foreign".into(),
            _ => value["snapshot"]["revision"] = "0".into(),
        }
        let snapshot: Snapshot = serde_json::from_value(value["snapshot"].clone()).unwrap();
        value["digest"] = digest(&snapshot).unwrap().into();
        fs::write(&file, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(
            cache::load(dir.path(), &state).is_err(),
            "resealed {changed}"
        );
    }
    fs::write(&file, b"{invalid").unwrap();
    assert!(cache::load(dir.path(), &state).is_err());
    fs::write(&file, vec![b' '; cache::MAX_CACHE_BYTES + 1]).unwrap();
    assert!(cache::load(dir.path(), &state).is_err());
}
#[test]
fn cache_paths_refuse_symlinks_fifo_and_write_failure_preserves_old_bytes() {
    use std::os::unix::{ffi::OsStrExt, fs::symlink};
    let (dir, mut state, mut writer) = populated();
    let file = path(dir.path());
    let outside = dir.path().join("outside");
    fs::rename(&file, &outside).unwrap();
    let before = fs::read(&outside).unwrap();
    symlink(&outside, &file).unwrap();
    assert!(cache::load(dir.path(), &state).is_err());
    activate(&mut state, 2, 1.4);
    let new = state.cache_save().unwrap();
    assert_eq!(writer.write(&new).unwrap_err().code, "cache_write_failed");
    assert_eq!(fs::read(&outside).unwrap(), before);
    fs::remove_file(&file).unwrap();
    let cfile = std::ffi::CString::new(file.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(cfile.as_ptr(), 0o600) }, 0);
    assert!(
        cache::load(dir.path(), &state).is_err(),
        "FIFO must not block"
    );
    fs::remove_file(&file).unwrap();
    assert_eq!(
        writer.write(&new).unwrap(),
        WriteOutcome::Written,
        "latest failed save can retry"
    );
    let parent = tempfile::tempdir().unwrap();
    let link = parent.path().join("linked");
    symlink(dir.path(), &link).unwrap();
    assert!(cache::load(&link, &state).is_err());
    assert!(Writer::open(&link, &state).is_err());
    let nested = dir.path().join("nested");
    fs::create_dir(&nested).unwrap();
    assert!(
        cache::load(&link.join("nested"), &state).is_err(),
        "ancestor symlink refused too"
    );
}
#[test]
fn replaced_lock_fences_the_old_writer() {
    let (dir, mut state, mut writer) = populated();
    let lock = dir.path().join(format!(
        "{}.lock",
        path(dir.path()).file_name().unwrap().to_str().unwrap()
    ));
    fs::rename(&lock, dir.path().join("retired-lock")).unwrap();
    let mut replacement = Writer::open(dir.path(), &state).unwrap();
    activate(&mut state, 2, 1.4);
    let save = state.cache_save().unwrap();
    assert_eq!(writer.write(&save).unwrap_err().code, "cache_lock_replaced");
    assert_eq!(replacement.write(&save).unwrap(), WriteOutcome::Written);
}
