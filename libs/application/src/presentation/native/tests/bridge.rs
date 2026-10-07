// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use std::time::Duration;

fn pair() -> (Ui<u64>, Lane<u64>) {
    super::super::bridge(session(), Worker::offline(|_, snapshot| Ok(snapshot.revision.0)))
}

#[test]
fn bridge_fences_completion_and_activates_current_content_once() {
    let mut original = session();
    let (_, jobs) = original.handle(Event::Wake, Some(1));
    let stale = ready(jobs.prepare.unwrap());
    original.host.consumer_mut().observe(1, snapshot(2, true));
    let (_, jobs) = original.handle(Event::Wake, Some(1));
    let fresh = ready(jobs.prepare.unwrap());
    let (mut ui, lane) = super::super::bridge(original, Worker::offline(|_, s| Ok(s.revision.0)));
    let mut installed = Vec::new();
    assert!(ui.handle_with(Event::Prepared(stale), Some(1), |p| installed.push(*p.content())).is_none());
    assert!(installed.is_empty());
    assert!(lane.publish(Event::Prepared(fresh)));
    assert_eq!(ui.drain_with(|| Some(1), |p| installed.push(*p.content())).len(), 1);
    assert_eq!(installed, [2]);
    assert_eq!(ui.session().host().consumer().applied().unwrap().revision, Revision(2));
    assert!(ui.drain_with(|| Some(1), |_| panic!("duplicate activation")).is_empty());
}

#[tokio::test]
async fn cancelled_bridge_drive_keeps_one_resource_job_and_completion() {
    install_fonts();
    let (entered, mut started) = tokio::sync::mpsc::unbounded_channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let blocked = std::sync::Mutex::new(blocked);
    let worker = Worker::offline(move |_, snapshot| {
        entered.send(snapshot.revision.0).unwrap();
        blocked.lock().unwrap().recv().unwrap();
        Ok(snapshot.revision.0)
    });
    let (mut ui, mut lane) = super::super::bridge(session(), worker);
    ui.reconcile(Some(1));
    assert_eq!(lane.drive().await, Progress::Updated);
    // This polls and then drops drive while the blocking preparation is alive.
    tokio::select! {
        _ = lane.drive() => panic!("resource completed before release"),
        revision = started.recv() => assert_eq!(revision, Some(1)),
        _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("resource never started"),
    }
    release.send(()).unwrap();
    assert_eq!(tokio::time::timeout(Duration::from_secs(5), lane.drive()).await.unwrap(), Progress::Wake);
    assert_eq!(ui.drain_with(|| Some(1), |_| {}).len(), 1);
    assert_eq!(*ui.session().host().presentation().unwrap().content(), 1);
    assert!(started.try_recv().is_err(), "cancelling drive restarted preparation");
    assert!(ui.drain_with(|| Some(1), |_| panic!("duplicate")).is_empty());
}

#[test]
fn bridge_coalesces_wakes_and_samples_generation_for_each_event() {
    let mut original = session();
    let (_, jobs) = original.handle(Event::Wake, Some(1));
    let completion = ready(jobs.prepare.unwrap());
    let (mut ui, lane) = super::super::bridge(original, Worker::offline(|_, s| Ok(s.revision.0)));
    assert!(lane.publish(Event::Wake));
    for _ in 0..100 { assert!(!lane.publish(Event::Wake)); }
    assert!(!lane.publish(Event::Prepared(completion)));
    let mut samples = [Some(1), None].into_iter();
    assert!(ui.drain_with(|| samples.next().expect("bounded events"), |_| panic!("loss must fence activation")).is_empty());
    assert!(samples.next().is_none());
    assert!(ui.session().host().presentation().is_none());
    assert!(lane.publish(Event::Wake), "draining did not release notification edge");
}

#[tokio::test]
async fn closing_ui_reports_once_without_a_ready_loop() {
    let (ui, mut lane) = pair();
    drop(ui);
    assert_eq!(lane.drive().await, Progress::UiClosed);
    assert!(tokio::time::timeout(Duration::from_millis(30), lane.drive()).await.is_err());
}

#[tokio::test]
async fn explicit_reconcile_fences_readback_before_queued_loss_notice() {
    let (mut ui, _lane) = pair();
    ui.reconcile(Some(1));
    assert!(ui.session().host().consumer().is_confirmed());
    ui.reconcile(None);
    assert!(!ui.session().host().consumer().is_confirmed());
    assert_eq!(ui.session().host().consumer().generation(), None);
}
