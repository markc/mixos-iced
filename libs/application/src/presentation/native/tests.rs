// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use settings::{Binding, Desktop, Revision};
use toolkit::fonts::{FontChoice, FontSelection, FontSet};

fn binding() -> Binding {
    Binding {
        instance: "fixture".into(),
        profile: "default".into(),
    }
}
fn snapshot(revision: u64, dark: bool) -> Snapshot {
    let mut desktop = Desktop::default();
    if dark {
        desktop.appearance.mode = "dark".into();
    }
    Snapshot {
        schema: 1,
        binding: binding(),
        incarnation: "fixture".into(),
        revision: Revision(revision),
        design_revision: Revision(revision),
        source_digest: settings::source_digest(settings::EMBEDDED_DEFAULT_SOURCE),
        effective: settings::resolve(&desktop).unwrap(),
        desktop,
    }
}
fn session() -> Session<u64> {
    let mut consumer = Consumer::for_app(binding(), "ced").unwrap();
    let subscribe = consumer.connected(1).unwrap();
    let read = consumer.complete(&subscribe, Ok(None)).unwrap();
    consumer.complete(&read, Ok(Some(snapshot(1, false))));
    Session::new(consumer)
}
fn ready(request: Request) -> Completion<u64> {
    let rev = request.update().snapshot().revision.0;
    request.prepare(
        |_, _| {
            Ok(FontSelection {
                font: crate::iced::Font::DEFAULT,
                choice: FontChoice::Declared,
            })
        },
        |_| Ok(rev),
    )
}
fn decoded(rev: u64) -> Decoded {
    decoded_snapshot(snapshot(rev, false))
}
fn decoded_snapshot(snapshot: Snapshot) -> Decoded {
    let command = bus::native_client::IncomingCommand {
        generation: 1,
        from: "settingsd".into(),
        command: String::new(),
        id: None,
        args: serde_json::Value::Null,
        body: serde_json::to_string(&snapshot).unwrap(),
        headers: std::collections::BTreeMap::from([
            ("topic".into(), settings::topic("default")),
            ("broker_service".into(), "settingsd".into()),
        ]),
    };
    Decoded::from_command(&binding(), &command).unwrap()
}
#[test]
fn exact_replay_does_not_mark_a_gap_but_same_revision_contradiction_does() {
    let mailbox = Mailbox::<u64>::default();
    mailbox.publish(Event::Delivery(decoded(1)));
    for _ in 0..100 {
        assert!(!mailbox.publish(Event::Delivery(decoded(1))));
    }
    assert_eq!(mailbox.take().len(), 1);
    mailbox.publish(Event::Delivery(decoded(1)));
    mailbox.publish(Event::Delivery(decoded_snapshot(snapshot(1, true))));
    let events = mailbox.take();
    assert!(matches!(events[0], Event::Lost));
}
#[test]
fn foreign_snapshot_binding_cannot_activate_despite_matching_broker_stamp() {
    let mut session = session();
    let mut foreign = snapshot(2, true);
    foreign.binding.instance = "other".into();
    let (change, jobs) = session.handle(Event::Delivery(decoded_snapshot(foreign)), Some(1));
    assert!(change.is_none());
    assert!(jobs.prepare.is_none());
    assert_eq!(
        session.host().consumer().fault().unwrap().code,
        "wrong_target"
    );
    assert!(session.host().presentation().is_none());
}
#[test]
fn live_stage_fences_a_ready_fallback_on_the_same_connection() {
    let mut session = Session::<u64>::new(Consumer::for_app(binding(), "ced").unwrap());
    session.bootstrap = Instant::now();
    let (_, jobs) = session.handle(Event::Wake, Some(1));
    let request = jobs.fallback.unwrap();
    let mut presentation = None;
    let fallback = request
        .prepare(None, |snapshot, context, _| {
            let appearance = Projection::new(&snapshot.effective[context])?.prepare(|_, _| {
                Ok(FontSelection {
                    font: crate::iced::Font::DEFAULT,
                    choice: FontChoice::Declared,
                })
            })?;
            presentation = Some(Presentation {
                appearance,
                content: 77,
            });
            Ok(())
        })
        .unwrap();
    let subscribe = session.host.consumer().current_work().unwrap().clone();
    let (_, jobs) = session.handle(Event::Rpc(subscribe, Ok(None)), Some(1));
    let (_, jobs) = session.handle(
        Event::Rpc(jobs.work.unwrap(), Ok(Some(snapshot(1, false)))),
        Some(1),
    );
    let ready = ready(jobs.prepare.unwrap());
    let (change, jobs) = session.handle(
        Event::Fallback(request, Ok((fallback, presentation.unwrap()))),
        Some(1),
    );
    assert!(change.is_none());
    assert!(jobs.prepare.is_some());
    let (change, _) = session.handle(Event::Prepared(ready), Some(1));
    assert!(change.is_some());
    assert_eq!(*session.host().presentation().unwrap().content(), 1);
}
#[test]
fn mailbox_bounds_a_delivery_storm_and_marks_the_gap_before_latest() {
    let mailbox = Mailbox::<u64>::default();
    assert!(mailbox.publish(Event::Delivery(decoded(1))));
    for rev in 2..100 {
        assert!(!mailbox.publish(Event::Delivery(decoded(rev))));
    }
    assert!(!mailbox.publish(Event::Wake));
    let events = mailbox.clone().take();
    assert_eq!(events.len(), 3);
    assert!(matches!(events[0], Event::Lost));
    assert!(matches!(events[1], Event::Delivery(_)));
    assert!(mailbox.take().is_empty());
    assert!(mailbox.publish(Event::Wake));
}
#[test]
fn decoded_event_queued_before_loss_cannot_reenter_a_new_generation() {
    let mut session = session();
    let delayed = decoded(99);
    session.handle(Event::Wake, None);
    session.handle(Event::Delivery(delayed), Some(2));
    assert!(session.host().consumer().pending().is_none());
    assert_eq!(session.host().consumer().generation(), Some(2));
}
#[test]
fn coalesced_jobs_retain_capture_and_atomic_loss_rejects_queued_ready() {
    let mut session = session();
    let (_, first) = session.handle(Event::Wake, Some(1));
    let (_, repeated) = session.handle(Event::Wake, Some(1));
    assert!(
        first
            .prepare
            .unwrap()
            .update()
            .same_stage(repeated.prepare.as_ref().unwrap().update())
    );
    let result = ready(repeated.prepare.unwrap());
    let (change, after_loss) = session.handle(Event::Prepared(result), None);
    assert!(change.is_none());
    assert!(session.host().presentation().is_none());
    assert!(after_loss.prepare.is_none());
}
#[test]
fn no_op_revision_advances_evidence_without_a_resource_job() {
    let mut session = session();
    let (_, jobs) = session.handle(Event::Wake, Some(1));
    let (change, _) = session.handle(Event::Prepared(ready(jobs.prepare.unwrap())), Some(1));
    assert!(change.is_some());
    session.host.consumer_mut().observe(1, snapshot(2, false));
    let (change, jobs) = session.handle(Event::Wake, Some(1));
    assert!(change.is_none());
    assert!(jobs.prepare.is_none());
    assert_eq!(
        session.host().consumer().applied().unwrap().revision,
        Revision(2)
    );
    assert_eq!(*session.host().presentation().unwrap().content(), 1);
}
#[test]
fn fallback_attempt_is_fenced_and_does_not_retry_a_failed_resource_in_a_loop() {
    let mut session = Session::<u64>::new(Consumer::for_app(binding(), "ced").unwrap());
    session.bootstrap = Instant::now();
    let (_, jobs) = session.handle(Event::Wake, None);
    let request = jobs.fallback.unwrap();
    session.handle(
        Event::Fallback(
            request,
            Err(vec![Diagnostic::new(
                "font_unavailable",
                "fixture",
                "missing",
            )]),
        ),
        None,
    );
    let (_, jobs) = session.handle(Event::Wake, None);
    assert!(jobs.fallback.is_none());
    assert!(jobs.wake.is_none());
    assert!(session.host().consumer().fallback_fault().is_some());
}
#[tokio::test]
async fn superseded_blocking_jobs_are_physically_serial_and_keep_only_latest() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        toolkit::fonts::install(
            FontSet::new().sans(
                include_bytes!("../../../../../vendor/font/Inter-VariableFont_opsz,wght.ttf")
                    .as_slice(),
            ),
            None,
        )
        .unwrap()
    });
    let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = std::sync::Mutex::new(release_rx);
    let mut worker = Worker::offline(move |_, snapshot| {
        entered_tx.send(snapshot.revision.0).unwrap();
        release_rx.lock().unwrap().recv().unwrap();
        Ok(snapshot.revision.0)
    });
    let mut session = session();
    let (_, jobs) = session.handle(Event::Wake, Some(1));
    worker.replace(jobs);
    tokio::select! {
        event = worker.next() => panic!("completed before release: {}", event.take().is_some()),
        started = entered_rx.recv() => assert_eq!(started, Some(1)),
        _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => panic!("resource did not start"),
    }
    for (rev, dark) in [(2, true), (3, false)] {
        session.host.consumer_mut().observe(1, snapshot(rev, dark));
        let (_, jobs) = session.handle(Event::Wake, Some(1));
        worker.replace(jobs.clone());
        worker.replace(jobs);
    }
    assert!(entered_rx.try_recv().is_err());
    release_tx.send(()).unwrap();
    let old = tokio::time::timeout(std::time::Duration::from_secs(5), worker.next())
        .await
        .unwrap()
        .take()
        .unwrap();
    let (change, jobs) = session.handle(old, Some(1));
    assert!(change.is_none());
    worker.replace(jobs);
    tokio::select! {
        _ = worker.next() => panic!("latest completed before release"),
        started = entered_rx.recv() => assert_eq!(started, Some(3)),
        _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => panic!("latest did not start"),
    }
    release_tx.send(()).unwrap();
    let fresh = tokio::time::timeout(std::time::Duration::from_secs(5), worker.next())
        .await
        .unwrap()
        .take()
        .unwrap();
    let (change, jobs) = session.handle(fresh, Some(1));
    assert!(change.is_some());
    assert_eq!(*session.host().presentation().unwrap().content(), 3);
    worker.replace(jobs);
    assert!(worker.running.is_none() && worker.queued.is_none());
    assert!(entered_rx.try_recv().is_err());
}
