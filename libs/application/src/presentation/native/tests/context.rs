// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use std::time::Duration;

// Deliberately not Clone, Default, Debug or serialisable.
#[derive(PartialEq)]
struct Context(u64);

fn contextual() -> Session<u64, Context> {
    Session::with_context(session().host.consumer, Context(1))
}

fn captured(jobs: &Jobs<Context>) -> Resource<Context> {
    jobs.resource.as_ref().expect("resource selected").clone()
}

fn authority(resource: Resource<Context>) -> Event<u64> {
    let ResourceKind::Prepare(request) = resource.kind else {
        panic!("authority request")
    };
    Event::Resource(ResourceCompletion {
        revision: resource.local.revision,
        outcome: ResourceOutcome::Prepared(ready(request)),
    })
}

fn local(resource: Resource<Context>, result: Result<Presentation<u64>, Diagnostic>) -> Event<u64> {
    let ResourceKind::Reprepare(source) = resource.kind else {
        panic!("local request")
    };
    Event::Resource(ResourceCompletion {
        revision: resource.local.revision,
        outcome: ResourceOutcome::Reprepared(source, Box::new(result)),
    })
}

fn activate_first(session: &mut Session<u64, Context>) {
    let (_, jobs) = session.handle(Event::Wake, Some(1));
    assert!(
        session
            .handle(authority(captured(&jobs)), Some(1))
            .0
            .is_some()
    );
}

#[test]
fn non_clone_context_equal_input_is_a_no_op_and_revisions_do_not_wrap() {
    let mut session = contextual();
    let (_, first) = session.handle(Event::Wake, Some(1));
    let (same, unchanged) = session.set_context(Context(1), Some(1)).unwrap();
    assert_eq!(same.get(), 0);
    assert!(captured(&first).same(&captured(&unchanged)));
    let (next, changed) = session.set_context(Context(2), Some(1)).unwrap();
    assert_eq!(next.get(), 1);
    assert!(!captured(&first).same(&captured(&changed)));
    session.local.revision = PreparationRevision(u64::MAX);
    assert!(session.set_context(Context(3), Some(1)).is_err());
    assert_eq!(session.local.value.0, 2);
    assert!(session.retry_preparation(Some(1)).is_err());
    assert_eq!(session.local.revision.get(), u64::MAX);
}

#[test]
fn contextual_sessions_reject_unstamped_success_and_failure() {
    let mut session = contextual();
    let (_, jobs) = session.handle(Event::Wake, Some(1));
    let request = jobs.prepare_request().unwrap();
    assert!(
        session
            .handle(Event::Prepared(ready(request.clone())), Some(1))
            .0
            .is_none()
    );
    assert!(
        session
            .handle(
                Event::Prepared(request.failed(Diagnostic::new("stale", "worker", "unstamped"))),
                Some(1)
            )
            .0
            .is_none()
    );
    assert!(session.host.presentation().is_none());
    assert!(session.host.consumer().fault().is_none());
    assert!(session.host.consumer().pending().is_some());
}

#[test]
fn old_local_revision_cannot_activate_fail_or_capture_a_save() {
    for failure in [false, true] {
        let mut session = contextual();
        let (_, jobs) = session.handle(Event::Wake, Some(1));
        let capture = captured(&jobs);
        let event = if failure {
            capture.failed(Diagnostic::new("old", "worker", "stale failure"))
        } else {
            authority(capture)
        };
        session.set_context(Context(2), Some(1)).unwrap();
        assert!(
            session
                .handle_with(event, Some(1), |_| panic!("stale activation"))
                .0
                .is_none()
        );
        assert!(session.host.presentation().is_none());
        assert!(session.host.consumer().applied().is_none());
        assert!(session.host.consumer().fault().is_none());
        #[cfg(feature = "settings-cache")]
        assert!(session.host.consumer().cache_save().is_none());
    }
}

#[test]
fn local_rebuild_preserves_authority_and_generic_binding_without_resource_discovery() {
    let mut session = contextual();
    activate_first(&mut session);
    let applied = session.host.consumer().applied().unwrap().clone();
    let work = session.host.consumer().current_work().cloned();
    #[cfg(feature = "settings-cache")]
    let save = session.host.consumer().cache_save().unwrap();
    let (_, jobs) = session.set_context(Context(2), Some(1)).unwrap();
    assert_eq!(jobs.work, work);
    let capture = captured(&jobs);
    let ResourceKind::Reprepare(source) = &capture.kind else {
        panic!("local rebuild")
    };
    assert!(source.binding.is_none());
    assert!(source.generic.is_some());
    let appearance = source.generic.as_ref().unwrap().clone();
    let mut installed = None;
    assert!(
        session
            .handle_with(
                local(
                    capture,
                    Ok(Presentation {
                        appearance,
                        content: 22
                    })
                ),
                Some(1),
                |p| installed = Some(*p.content())
            )
            .0
            .is_some()
    );
    assert_eq!(installed, Some(22));
    assert_eq!(
        session.host.consumer().applied().unwrap().revision,
        applied.revision
    );
    assert_eq!(session.host.consumer().current_work(), work.as_ref());
    assert_eq!(session.preparation_evidence().applied.unwrap().get(), 1);
    assert!(session.preparation_evidence().current);
    assert!(
        session
            .host
            .presentation()
            .unwrap()
            .appearance
            .resources()
            .and_then(|r| r.binding())
            .is_none()
    );
    #[cfg(feature = "settings-cache")]
    assert!(save.same_capture(&session.host.consumer().cache_save().unwrap()));
}

#[test]
fn failed_local_rebuild_is_quiescent_and_retry_keeps_the_same_context() {
    let mut session = contextual();
    activate_first(&mut session);
    let (_, jobs) = session.set_context(Context(2), Some(1)).unwrap();
    session.handle(
        local(
            captured(&jobs),
            Err(Diagnostic::new("local", "resources", "refused")),
        ),
        Some(1),
    );
    assert_eq!(*session.host.presentation().unwrap().content(), 1);
    assert_eq!(session.preparation_evidence().fault.unwrap().code, "local");
    assert!(session.host.consumer().fault().is_none());
    for _ in 0..4 {
        assert!(session.handle(Event::Wake, Some(1)).1.resource.is_none());
    }
    let context = Arc::clone(&session.local.value);
    let (revision, jobs) = session.retry_preparation(Some(1)).unwrap();
    assert_eq!(revision.get(), 2);
    assert!(Arc::ptr_eq(&context, &session.local.value));
    assert!(matches!(captured(&jobs).kind, ResourceKind::Reprepare(_)));
}

#[test]
fn pending_authority_and_new_activation_epoch_reject_old_local_results() {
    let mut session = contextual();
    activate_first(&mut session);
    let (_, jobs) = session.set_context(Context(2), Some(1)).unwrap();
    let old = captured(&jobs);
    let appearance = session.host.presentation().unwrap().appearance.clone();
    session.host.consumer_mut().observe(1, snapshot(2, true));
    let (_, current) = session.handle(Event::Wake, Some(1));
    assert!(matches!(captured(&current).kind, ResourceKind::Prepare(_)));
    assert!(
        session
            .handle(
                local(
                    old.clone(),
                    Ok(Presentation {
                        appearance: appearance.clone(),
                        content: 99
                    })
                ),
                Some(1)
            )
            .0
            .is_none()
    );
    assert_eq!(*session.host.presentation().unwrap().content(), 1);
    session.handle(authority(captured(&current)), Some(1));
    assert_eq!(session.activation_epoch, 2);
    assert!(
        session
            .handle(
                local(
                    old,
                    Ok(Presentation {
                        appearance,
                        content: 99
                    })
                ),
                Some(1)
            )
            .0
            .is_none()
    );
    assert_eq!(*session.host.presentation().unwrap().content(), 2);
}

#[test]
fn obsolete_fallback_failure_does_not_consume_the_original_capture() {
    let mut session = Session::<u64, Context>::with_context(
        Consumer::for_app(binding(), "ced").unwrap(),
        Context(1),
    );
    let (_, jobs) = session.handle(Event::Wake, None);
    let old = captured(&jobs);
    let ResourceKind::Fallback(request) = &old.kind else {
        panic!("bootstrap fallback")
    };
    let request = request.clone();
    session.set_context(Context(2), None).unwrap();
    let (_, jobs) = session.handle(
        old.failed(Diagnostic::new("old", "fallback", "stale")),
        None,
    );
    assert!(session.fallback_diagnostics().is_empty());
    assert!(session.host.consumer().fallback_fault().is_none());
    assert!(jobs.fallback_request().unwrap().same_request(&request));
}

#[tokio::test]
async fn held_physical_job_is_not_replaced_until_it_finishes_and_latest_context_runs_next() {
    install_fonts();
    let (entered, mut arrivals) = tokio::sync::mpsc::unbounded_channel();
    let (release, held) = std::sync::mpsc::channel();
    let held = Mutex::new(held);
    let worker = Worker::contextual_with_host(
        move |_, _, context: &Context| {
            entered.send(context.0).unwrap();
            if context.0 == 1 {
                held.lock().unwrap().recv().unwrap();
            }
            Ok(context.0)
        },
        hermetic_host(),
    );
    let (mut ui, mut lane) = super::super::bridge(contextual(), worker);
    ui.reconcile(Some(1));
    assert_eq!(lane.drive().await, Progress::Updated);
    tokio::select! {
        _ = lane.drive() => panic!("held builder returned early"),
        started = arrivals.recv() => assert_eq!(started, Some(1)),
        _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("builder did not start"),
    }
    for value in 2..=40 {
        ui.set_context(Context(value), Some(1)).unwrap();
    }
    assert_eq!(lane.drive().await, Progress::Updated);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), arrivals.recv())
            .await
            .is_err()
    );
    release.send(()).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), lane.drive())
            .await
            .unwrap(),
        Progress::Wake
    );
    assert!(
        ui.drain_with(|| Some(1), |_| panic!("obsolete physical result activated"))
            .is_empty()
    );
    // Draining publishes the desired capture again, but keeps the one offer.
    assert_eq!(lane.drive().await, Progress::Updated);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), lane.drive())
            .await
            .unwrap(),
        Progress::Wake
    );
    assert_eq!(arrivals.recv().await, Some(40));
    assert_eq!(ui.drain_with(|| Some(1), |_| {}).len(), 1);
    assert_eq!(*ui.session().host().presentation().unwrap().content(), 40);
    assert!(ui.preparation_evidence().current);
    assert!(arrivals.try_recv().is_err());
}

#[tokio::test]
async fn requirements_and_content_use_one_immutable_context_and_generic_rebuild_does_no_lookup() {
    install_fonts();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let requirements_seen = Arc::clone(&seen);
    let content_seen = Arc::clone(&seen);
    let worker = Worker::contextual_with_host(
        move |_, _, context: &Context| {
            content_seen.lock().unwrap().push(("build", context.0));
            Ok(context.0)
        },
        hermetic_host(),
    )
    .with_contextual_resource_requirements(move |_, _, context: &Context| {
        requirements_seen
            .lock()
            .unwrap()
            .push(("requirements", context.0));
        Ok(ResourceRequirements::empty())
    });
    let (mut ui, mut lane) = super::super::bridge(contextual(), worker);
    ui.reconcile(Some(1));
    assert_eq!(lane.drive().await, Progress::Updated);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), lane.drive())
            .await
            .unwrap(),
        Progress::Wake
    );
    ui.drain_with(|| Some(1), |_| {});
    // No approved filesystem root existed at the first activation. There is
    // no new resource lookup or mutation for a generic local rebuild.
    let version = toolkit::fonts::registry::registry().usage();
    ui.set_context(Context(7), Some(1)).unwrap();
    assert_eq!(lane.drive().await, Progress::Updated);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), lane.drive())
            .await
            .unwrap(),
        Progress::Wake
    );
    ui.drain_with(|| Some(1), |_| {});
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [
            ("requirements", 1),
            ("build", 1),
            ("requirements", 7),
            ("build", 7)
        ]
    );
    assert_eq!(toolkit::fonts::registry::registry().usage(), version);
    assert_eq!(*ui.session().host().presentation().unwrap().content(), 7);
}

#[tokio::test]
async fn superseded_physical_failure_or_panic_cannot_poison_the_same_requeued_local_key() {
    for panic_first in [false, true] {
        let mut session = contextual();
        activate_first(&mut session);
        let (entered, mut arrivals) = tokio::sync::mpsc::unbounded_channel();
        let (release, held) = std::sync::mpsc::channel();
        let held = Mutex::new(held);
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        let mut worker = Worker::contextual_with_host(
            move |_, _, context: &Context| {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                entered.send(attempt).unwrap();
                if attempt == 0 {
                    held.lock().unwrap().recv().unwrap();
                    assert!(!panic_first, "retired physical panic");
                    return Err(Diagnostic::new(
                        "retired",
                        "worker",
                        "retired physical failure",
                    ));
                }
                Ok(context.0)
            },
            hermetic_host(),
        );
        let (_, first) = session.set_context(Context(2), Some(1)).unwrap();
        worker.replace(first);
        tokio::select! {
            _ = worker.next() => panic!("held task returned early"),
            attempt = arrivals.recv() => assert_eq!(attempt, Some(0)),
            _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("task never started"),
        }
        session.host.consumer_mut().observe(1, snapshot(2, true));
        let (_, authority) = session.handle(Event::Wake, Some(1));
        assert!(matches!(
            captured(&authority).kind,
            ResourceKind::Prepare(_)
        ));
        worker.replace(authority);
        // Loss invalidates the authority stage and makes the original logical
        // local key eligible again, while the first physical task still lives.
        let (_, restored) = session.handle(Event::Wake, None);
        assert!(matches!(
            captured(&restored).kind,
            ResourceKind::Reprepare(_)
        ));
        worker.replace(restored);
        release.send(()).unwrap();
        let retired = tokio::time::timeout(Duration::from_secs(5), worker.next())
            .await
            .unwrap()
            .take()
            .unwrap();
        let (_, latest) = session.handle(retired, None);
        assert!(session.preparation_evidence().fault.is_none());
        assert!(matches!(captured(&latest).kind, ResourceKind::Reprepare(_)));
        worker.replace(latest);
        let result = tokio::time::timeout(Duration::from_secs(5), worker.next())
            .await
            .unwrap()
            .take()
            .unwrap();
        assert_eq!(arrivals.recv().await, Some(1));
        assert!(session.handle(result, None).0.is_some());
        assert_eq!(*session.host.presentation().unwrap().content(), 2);
        assert!(session.preparation_evidence().current);
    }
}
