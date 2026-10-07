// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use settings::{
    cache::{self, WriteOutcome, Writer},
    fallback::PresentationKind,
};
use std::time::Duration;

fn activated() -> Session<u64> {
    let mut session = session();
    let (_, jobs) = session.handle(Event::Wake, Some(1));
    session.handle(Event::Prepared(ready(jobs.prepare.unwrap())), Some(1));
    session
}
async fn next(worker: &mut Worker<u64>) -> Event<u64> {
    tokio::time::timeout(Duration::from_secs(5), worker.next())
        .await
        .unwrap()
        .take()
        .unwrap()
}
fn cache_worker(directory: &std::path::Path) -> Worker<u64> {
    install_fonts();
    Worker::offline_with_cache(directory.to_owned(), |_, snapshot| Ok(snapshot.revision.0))
}
fn cache_path(directory: &std::path::Path) -> std::path::PathBuf {
    directory.join(format!(
        "{}.json",
        settings::digest(&(binding(), "app:ced", false)).unwrap()
    ))
}

#[tokio::test]
async fn applied_capture_survives_loss_and_cold_cache_does_not_seed_authority() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("uncreated/settings");
    let mut session = activated();
    let mut worker = cache_worker(&root);
    let (_, jobs) = session.handle(Event::Wake, Some(1));
    worker.replace(jobs);
    // A staged but unapplied newer snapshot must never replace applied A.
    session.host.consumer_mut().observe(1, snapshot(2, true));
    let (_, jobs) = session.handle(Event::Wake, Some(1));
    worker.replace(jobs);
    let (_, jobs) = session.handle(Event::Wake, None);
    worker.replace(jobs);
    let saved = next(&mut worker).await;
    assert!(matches!(&saved, Event::Saved(_, Ok(WriteOutcome::Written))));
    session.handle(saved, None);
    assert!(session.cache_fault().is_none());
    assert_eq!(session.cache_persisted().unwrap().revision, Revision(1));
    assert_eq!(
        cache::load(&root, session.host.consumer())
            .unwrap()
            .snapshot()
            .revision,
        Revision(1)
    );
    drop(worker);

    let mut cold = Session::<u64>::new(Consumer::for_app(binding(), "ced").unwrap());
    assert!(
        Instant::now() < cold.bootstrap,
        "resources start before authority budget expires"
    );
    let mut cold_worker = cache_worker(&root);
    let (_, jobs) = cold.handle(Event::Wake, None);
    assert!(jobs.fallback.is_some());
    cold_worker.replace(jobs);
    assert!(cold.handle(next(&mut cold_worker).await, None).0.is_some());
    assert_eq!(cold.host.kind(), Some(PresentationKind::Cached));
    assert_eq!(*cold.host.presentation().unwrap().content(), 1);
    assert!(!cold.host.consumer().is_confirmed());
    assert!(cold.host.consumer().current().is_none());
    assert!(cold.host.consumer().cache_save().is_none());
}

#[tokio::test]
async fn corrupt_cache_is_visible_and_embedded_remains_resource_checked() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(cache_path(dir.path()), b"corrupt").unwrap();
    let mut cold = Session::<u64>::new(Consumer::for_app(binding(), "ced").unwrap());
    let mut worker = cache_worker(dir.path());
    let (_, jobs) = cold.handle(Event::Wake, None);
    worker.replace(jobs);
    assert!(cold.handle(next(&mut worker).await, None).0.is_some());
    assert_eq!(cold.host.kind(), Some(PresentationKind::Embedded));
    assert_eq!(cold.fallback_diagnostics()[0].code, "invalid_cache");
    assert_eq!(std::fs::read(cache_path(dir.path())).unwrap(), b"corrupt");
}

#[tokio::test]
async fn unavailable_cached_resources_fall_through_to_prepared_embedded() {
    let dir = tempfile::tempdir().unwrap();
    let state = activated();
    let mut writer = Writer::open(dir.path(), state.host.consumer()).unwrap();
    writer
        .write(&state.host.consumer().cache_save().unwrap())
        .unwrap();
    drop(writer);
    install_fonts();
    let mut worker = Worker::offline_with_cache(dir.path().to_owned(), |_, snapshot| {
        if snapshot.revision == Revision(1) {
            Err(Diagnostic::new(
                "fixture_missing_resource",
                "resource",
                "missing",
            ))
        } else {
            Ok(snapshot.revision.0)
        }
    });
    let mut cold = Session::<u64>::new(Consumer::for_app(binding(), "ced").unwrap());
    let (_, jobs) = cold.handle(Event::Wake, None);
    worker.replace(jobs);
    cold.handle(next(&mut worker).await, None);
    assert_eq!(cold.host.kind(), Some(PresentationKind::Embedded));
    assert_eq!(*cold.host.presentation().unwrap().content(), 0);
    assert_eq!(
        cold.fallback_diagnostics()[0].code,
        "fixture_missing_resource"
    );
}

#[tokio::test]
async fn failed_write_retains_lock_and_retries_only_on_explicit_request() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = activated();
    let mut worker = cache_worker(dir.path());
    let path = cache_path(dir.path());
    std::fs::create_dir(&path).unwrap(); // owned obstruction: rename fails
    let (_, jobs) = session.handle(Event::Wake, Some(1));
    worker.replace(jobs);
    session.handle(next(&mut worker).await, Some(1));
    assert!(session.cache_fault().is_some());
    assert!(
        Writer::open(dir.path(), session.host.consumer()).is_err(),
        "failed write must keep its lock"
    );
    let (_, jobs) = session.handle(Event::Wake, Some(1));
    worker.replace(jobs);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), worker.next())
            .await
            .is_err(),
        "no failure retry loop"
    );
    std::fs::remove_dir(&path).unwrap();
    let (_, jobs) = session.handle(Event::RetryCache, Some(1));
    worker.replace(jobs);
    session.handle(next(&mut worker).await, Some(1));
    assert!(session.cache_fault().is_none());
    assert_eq!(
        cache::load(dir.path(), session.host.consumer())
            .unwrap()
            .snapshot()
            .revision,
        Revision(1)
    );
}

#[test]
fn stale_save_reports_cannot_set_or_clear_current_diagnostics() {
    let mut session = activated();
    let old = session.host.consumer().cache_save().unwrap();
    session.host.consumer_mut().observe(1, snapshot(2, false));
    let current = session.host.consumer().cache_save().unwrap();
    session.handle(
        Event::Saved(
            current,
            Err(Diagnostic::new("current_fault", "cache", "failed")),
        ),
        Some(1),
    );
    session.handle(
        Event::Saved(old.clone(), Ok(WriteOutcome::Written)),
        Some(1),
    );
    assert_eq!(session.cache_fault().unwrap().code, "current_fault");
    assert!(
        session.cache_persisted().is_none(),
        "a stale success is not a receipt"
    );
    let current = session.host.consumer().cache_save().unwrap();
    session.handle(
        Event::Saved(current.clone(), Ok(WriteOutcome::Superseded)),
        Some(1),
    );
    assert!(
        session.cache_persisted().is_none(),
        "superseded work did not persist this capture"
    );
    session.handle(
        Event::Saved(current.clone(), Ok(WriteOutcome::Unchanged)),
        Some(1),
    );
    assert_eq!(session.cache_persisted(), Some(&current.identity()));
    let mut replacement = activated();
    replacement.handle(
        Event::Saved(old, Err(Diagnostic::new("old_fault", "cache", "stale"))),
        Some(1),
    );
    assert!(replacement.cache_fault().is_none());
    assert!(replacement.cache_persisted().is_none());
}

#[tokio::test]
async fn producer_replacement_retires_writer_after_inflight_save() {
    let dir = tempfile::tempdir().unwrap();
    let mut old = activated();
    let mut worker = cache_worker(dir.path());
    let (_, jobs) = old.handle(Event::Wake, Some(1));
    worker.replace(jobs);
    worker.start(); // deliberately leave completion unconsumed
    let mut new = activated();
    new.host.consumer_mut().observe(1, snapshot(2, false));
    let (_, jobs) = new.handle(Event::Wake, Some(1));
    worker.replace(jobs);
    new.handle(next(&mut worker).await, Some(1));
    let saved = next(&mut worker).await;
    assert!(matches!(&saved, Event::Saved(_, Ok(WriteOutcome::Written))));
    new.handle(saved, Some(1));
    assert_eq!(
        cache::load(dir.path(), new.host.consumer())
            .unwrap()
            .snapshot()
            .revision,
        Revision(2)
    );
}

#[tokio::test]
async fn bounded_shutdown_drains_the_latest_activated_capture() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = activated();
    let mut worker = cache_worker(dir.path());
    let (_, jobs) = session.handle(Event::Wake, Some(1));
    worker.replace(jobs);
    session.host.consumer_mut().observe(1, snapshot(2, false));
    let (_, jobs) = session.handle(Event::Wake, Some(1));
    worker.replace(jobs);
    worker
        .flush_cache(Instant::now() + Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(
        cache::load(dir.path(), session.host.consumer())
            .unwrap()
            .snapshot()
            .revision,
        Revision(2)
    );
}

#[tokio::test]
async fn bridge_shutdown_flushes_newest_capture_without_consuming_watch_notification() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ui, mut lane) = super::super::bridge(activated(), cache_worker(dir.path()));
    ui.reconcile(Some(1));
    assert_eq!(lane.drive().await, Progress::Updated);
    // Stage a real appearance change, then install its prepared presentation.
    ui.handle_with(
        Event::Delivery(decoded_snapshot(snapshot(2, true))),
        Some(1),
        |_| {},
    );
    let completion = ready(Request {
        update: ui.session().host().consumer().pending().unwrap().clone(),
        context: ui.session().host().consumer().context().to_owned(),
    });
    let mut activated = Vec::new();
    assert!(
        ui.handle_with(Event::Prepared(completion), Some(1), |p| activated
            .push(*p.content()))
            .is_some()
    );
    assert_eq!(activated, [2]);
    assert_eq!(
        ui.session().host().consumer().applied().unwrap().revision,
        Revision(2)
    );
    // Never poll drive after revision 2: the worker still holds revision 1.
    // Shutdown must consume the newer pending UI->worker capture.
    lane.flush_cache(Instant::now() + Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(
        cache::load(dir.path(), ui.session().host().consumer())
            .unwrap()
            .snapshot()
            .revision,
        Revision(2)
    );
}
