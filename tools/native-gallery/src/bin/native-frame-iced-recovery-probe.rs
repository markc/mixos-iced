// SPDX-License-Identifier: MIT OR Apache-2.0
//! Real pre-commit failures and unchanged-view recovery, without fixture redraws.

use iced::{Element, Task, window::{self, Id, presentation::{FrameBinding, FrameObservation, FrameObserver, FrameOutcome, FrameStamp}}};
use iced::native_frame_probe::{install_recovery, install_after_commit_recovery, RecoveryPlan, RecoveryReport};
use iced::window::presentation::probe::FailurePoint;
use iced::futures::channel::oneshot;
use std::{collections::BTreeSet, sync::{Arc, Mutex, MutexGuard, PoisonError}};

const BASELINE: FrameStamp = FrameStamp { activation_epoch: 1, local_revision: 0 };
const TARGET: FrameStamp = FrameStamp { activation_epoch: 2, local_revision: 0 };

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> { mutex.lock().unwrap_or_else(PoisonError::into_inner) }

#[derive(Default)]
struct Observations {
    receipts: Vec<FrameObservation>,
    overflow: bool,
    baseline: Option<oneshot::Sender<FrameObservation>>,
}

struct State {
    window: Id,
    stamp: FrameStamp,
    observer: FrameObserver,
    observed: Arc<Mutex<Observations>>,
    failures: u8,
    failure_point: FailurePoint,
    opened: bool,
    baseline_ready: bool,
    result: Arc<Mutex<Option<Result<(), String>>>>,
}

#[derive(Clone, Debug)]
enum Message { Opened(Id), Baseline(FrameObservation), Report(Result<RecoveryReport, String>) }

fn validate(state: &State, report: RecoveryReport) -> Result<(), String> {
    let observed = lock(&state.observed);
    if observed.overflow || report.window != state.window || state.stamp != TARGET || report.failure_point != state.failure_point || !state.opened || !state.baseline_ready {
        return Err("recovery observation cap, window or unchanged stamp mismatch".into());
    }
    if report.failed_requests.len() != state.failures as usize {
        return Err("not every actual renderer fault was recorded".into());
    }
    let requested: BTreeSet<_> = report.failed_requests.iter().flatten().copied().collect();
    let expected_requested = state.failures.min(8) as usize;
    if requested.len() != expected_requested
        || report.failed_requests[..expected_requested].iter().any(Option::is_none)
        || report.failed_requests[expected_requested..].iter().any(Option::is_some)
        || requested.contains(&report.recovered)
    {
        return Err("failed/native/untracked attempts lost their actual bounded identities".into());
    }
    if state.failures == 9 && report.untracked_commits == 0 {
        return Err("capacity recovery did not commit the actual untracked buffer".into());
    }
    let terminals: BTreeSet<_> = report.failed_terminals.iter().map(|(request, _)| *request).collect();
    if terminals != requested || report.failed_terminals.len() != expected_requested {
        return Err("failed requests did not reach their own actual native terminal".into());
    }
    if report.failed_terminals.iter().any(|(_, outcome)| !matches!(outcome, FrameOutcome::Presented { .. } | FrameOutcome::Discarded)) {
        return Err("recovery reported a fabricated native terminal".into());
    }
    let receipts = &observed.receipts;
    if receipts.iter().any(|receipt| receipt.window != state.window) {
        return Err("recovery receipts crossed actual window identity".into());
    }
    let baseline = receipts.iter().position(|receipt| receipt.stamp == BASELINE && matches!(receipt.outcome, FrameOutcome::Presented { .. }))
        .ok_or("actual baseline did not present before recovery")?;
    let baseline_id = receipts[baseline].request_id.ok_or("baseline lacked an actual request")?;
    if requested.contains(&baseline_id) || baseline_id == report.recovered {
        return Err("baseline request identity was reused".into());
    }
    for request in &requested {
        let failed: Vec<_> = receipts.iter().enumerate().filter(|(_, receipt)| {
            receipt.request_id == Some(*request) && receipt.stamp == TARGET && receipt.outcome == FrameOutcome::SubmissionFailed
        }).collect();
        if failed.len() != 1 || failed[0].0 <= baseline
            || receipts.iter().any(|receipt| receipt.request_id == Some(*request) && matches!(receipt.outcome, FrameOutcome::Presented { .. })) {
            return Err("failed request proved success or lacked its unique post-baseline failure".into());
        }
    }
    let proven: Vec<_> = receipts.iter().filter(|receipt| receipt.stamp == TARGET && matches!(receipt.outcome, FrameOutcome::Presented { .. })).collect();
    if proven.len() != 1 || proven[0].request_id != Some(report.recovered) {
        return Err("unchanged recovery view did not obtain exactly one fresh actual Presented receipt".into());
    }
    println!("ICED_RECOVERY RECEIPT {report:?}");
    Ok(())
}

fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::Opened(window) => {
            assert_eq!(window, state.window);
            state.opened = true;
            if state.baseline_ready { state.stamp = TARGET; }
        }
        Message::Baseline(receipt) => {
            assert_eq!(receipt.window, state.window);
            assert_eq!(receipt.stamp, BASELINE);
            assert!(matches!(receipt.outcome, FrameOutcome::Presented { .. }));
            // The sole view update. Every subsequent retry belongs to the
            // production renderer/error/capacity path, with this stamp fixed.
            state.baseline_ready = true;
            if state.opened { state.stamp = TARGET; }
        }
        Message::Report(report) => {
            *lock(&state.result) = Some(report.and_then(|report| validate(state, report)));
            return window::close(state.window).chain(iced::exit());
        }
    }
    Task::none()
}

fn view(state: &State, _: Id) -> Element<'_, Message> {
    let label = if state.stamp == BASELINE { "Native recovery: presented baseline" } else { "Native recovery: unchanged failed view" };
    iced::widget::container(iced::widget::text(label).size(toolkit::Tokens::dark().metrics.text.md))
        .width(iced::Fill).height(iced::Fill).center(iced::Fill).into()
}

fn main() {
    let failures: u8 = std::env::var("ICED_PROBE_FAILURES").unwrap_or_else(|_| "1".into()).parse().unwrap();
    assert!(matches!(failures, 1 | 9), "native fixture runs one or nine actual failures");
    let after_commit = std::env::var("ICED_PROBE_AFTER_COMMIT").is_ok_and(|value| value == "1");
    let failure_point = if after_commit { FailurePoint::AfterCommit } else { FailurePoint::BeforeCommit };
    let plan = RecoveryPlan { fault_stamp: TARGET, failures };
    let (guard, handle) = if after_commit { install_after_commit_recovery(plan) } else { install_recovery(plan) }.unwrap();
    let observed = Arc::new(Mutex::new(Observations::default()));
    let (baseline_sender, baseline_receiver) = oneshot::channel();
    lock(&observed).baseline = Some(baseline_sender);
    let recorded = observed.clone();
    let observer = FrameObserver::new(move |receipt| {
        let sender = {
            let mut recorded = lock(&recorded);
            if recorded.receipts.len() == 48 { recorded.overflow = true; }
            else { recorded.receipts.push(receipt); }
            if receipt.stamp == BASELINE && matches!(receipt.outcome, FrameOutcome::Presented { .. }) { recorded.baseline.take() }
            else { None }
        };
        if let Some(sender) = sender { let _ = sender.send(receipt); }
    });
    let result = Arc::new(Mutex::new(None));
    let boot_result = result.clone();
    let baseline_receiver = Mutex::new(Some(baseline_receiver));
    iced::daemon(move || {
        let (window, opened) = window::open(window::Settings {
            size: iced::Size::new(480.0, 180.0), exit_on_close_request: false,
            ..window::Settings::default()
        });
        let baseline = lock(&baseline_receiver).take().unwrap();
        let report = handle.take_report().unwrap();
        (
            State { window, stamp: BASELINE, observer: observer.clone(), observed: observed.clone(), failures, failure_point, opened: false, baseline_ready: false, result: boot_result.clone() },
            Task::batch([
                opened.map(Message::Opened),
                Task::perform(async move { baseline.await.expect("baseline observer retired") }, Message::Baseline),
                Task::perform(async move { report.await.unwrap_or_else(|_| Err("recovery notification cancelled".into())) }, Message::Report),
            ]),
        )
    }, update, view)
        .frame_presentation(|state, window| (window == state.window).then(|| FrameBinding { stamp: state.stamp, observer: state.observer.clone() }))
        .run().unwrap();
    drop(guard);
    lock(&result).take().expect("native recovery did not complete").expect("native recovery validation");
    let point = if after_commit { "after_commit" } else { "before_commit" };
    println!("ICED_RECOVERY PASS {point}=true failures={failures} unchanged_view=true failed_success_suppressed=true exact_retry=true");
}
