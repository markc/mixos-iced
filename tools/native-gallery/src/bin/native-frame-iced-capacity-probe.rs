// SPDX-License-Identifier: MIT OR Apache-2.0
//! Process capacity held by actually destroyed foreign windows, then native recovery.

use iced::{Element, Task, window::{self, Id, presentation::{FrameBinding, FrameStamp, FrameObserver, FrameObservation, FrameOutcome}}};
use iced::native_frame_probe::{CapacityReport, install_capacity};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

const STAMP: FrameStamp = FrameStamp { activation_epoch: 1, local_revision: 0 };
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> { mutex.lock().unwrap_or_else(PoisonError::into_inner) }

#[derive(Default)]
struct Evidence { receipts: Vec<FrameObservation>, overflow: bool }
struct State {
    donors: Vec<Id>, target: Option<Id>, observer: FrameObserver,
    evidence: Arc<Mutex<Evidence>>, result: Arc<Mutex<Option<Result<(), String>>>>,
    populated: bool, destroyed: bool,
}
#[derive(Clone, Debug)]
enum Message { Ready(usize, Result<(), String>), Retired(usize, Result<(), String>), Report(Result<CapacityReport, String>) }

fn donor() -> (Id, Task<Message>) {
    let (id, opened) = window::open(window::Settings {
        size: iced::Size::new(120.0, 96.0), exit_on_close_request: false,
        ..window::Settings::default()
    });
    (id, opened.discard())
}

fn validate(state: &State, report: CapacityReport) -> Result<(), String> {
    let evidence = lock(&state.evidence);
    if evidence.overflow || !state.populated || !state.destroyed || state.target != Some(report.window)
        || report.donor_requests != 128 || report.destroyed != 16 || report.retry_epoch <= report.blocked_epoch {
        return Err("capacity schedule lost real donor retirement or release epoch".into());
    }
    if evidence.receipts.len() != 2 { return Err(format!("expected only Capacity then Presented, got {:?}", evidence.receipts)); }
    let refused = evidence.receipts[0];
    let presented = evidence.receipts[1];
    if refused.window != report.window || refused.stamp != STAMP || refused.request_id.is_some() || refused.outcome != FrameOutcome::Capacity
        || presented.window != report.window || presented.stamp != STAMP || presented.request_id != Some(report.request)
        || !matches!(presented.outcome, FrameOutcome::Presented { .. }) {
        return Err("unchanged target did not obtain exact native proof after actual Capacity".into());
    }
    println!("ICED_CAPACITY RECEIPT {report:?}");
    Ok(())
}

fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::Ready(index, result) => {
            if let Err(reason) = result { *lock(&state.result) = Some(Err(reason)); return iced::exit(); }
            assert_eq!(index + 1, state.donors.len());
            if index == 15 { state.populated = true; }
            window::close(state.donors[index])
        }
        Message::Retired(index, result) => {
            if let Err(reason) = result { *lock(&state.result) = Some(Err(reason)); return iced::exit(); }
            assert_eq!(index + 1, state.donors.len());
            if index == 15 {
                assert!(state.populated);
                state.destroyed = true;
                let (target, opened) = window::open(window::Settings {
                    size: iced::Size::new(480.0, 180.0), exit_on_close_request: false,
                    ..window::Settings::default()
                });
                state.target = Some(target);
                // No Opened message can assist the target after its first Capacity.
                opened.discard()
            } else {
                let (id, opened) = donor(); state.donors.push(id); opened
            }
        }
        Message::Report(report) => {
            *lock(&state.result) = Some(report.and_then(|report| validate(state, report)));
            state.target.map_or_else(iced::exit, |target| window::close(target).chain(iced::exit()))
        }
    }
}

fn view(state: &State, window: Id) -> Element<'_, Message> {
    let label = if state.target == Some(window) { "Native capacity: unchanged target" } else { "Native capacity: donor" };
    iced::widget::container(iced::widget::text(label).size(toolkit::Tokens::dark().metrics.text.md))
        .center(iced::Fill).into()
}

fn main() {
    let (guard, handle) = install_capacity(STAMP).unwrap();
    let finish = handle.clone();
    let evidence = Arc::new(Mutex::new(Evidence::default()));
    let recorded = evidence.clone();
    let observer = FrameObserver::new(move |receipt| {
        let mut recorded = lock(&recorded);
        if recorded.receipts.len() == 8 { recorded.overflow = true; } else { recorded.receipts.push(receipt); }
    });
    let result = Arc::new(Mutex::new(None));
    let boot_result = result.clone();
    iced::daemon(move || {
        let mut donors = Vec::with_capacity(16);
        let mut tasks = Vec::with_capacity(36);
        let (id, opened) = donor(); donors.push(id); tasks.push(opened);
        for index in 0..16 {
            let ready = handle.take_donor_ready(index).unwrap();
            let retired = handle.take_donor_retired(index).unwrap();
            tasks.push(Task::perform(async move { ready.await.unwrap_or_else(|_| Err("donor terminal cancelled".into())) }, move |result| Message::Ready(index, result)));
            tasks.push(Task::perform(async move { retired.await.unwrap_or_else(|_| Err("donor destruction cancelled".into())) }, move |result| Message::Retired(index, result)));
        }
        let report = handle.take_report().unwrap();
        tasks.push(Task::perform(async move { report.await.unwrap_or_else(|_| Err("capacity report cancelled".into())) }, Message::Report));
        (State { donors, target: None, observer: observer.clone(), evidence: evidence.clone(), result: boot_result.clone(), populated: false, destroyed: false }, Task::batch(tasks))
    }, update, view)
    .frame_presentation(|state, window| (state.target == Some(window)).then(|| FrameBinding { stamp: STAMP, observer: state.observer.clone() }))
    .run().unwrap();
    let outcome = lock(&result).take().expect("capacity schedule did not complete");
    if outcome.is_ok() { finish.finish().expect("real lease owner retirement"); }
    drop(guard);
    outcome.expect("native process capacity acceptance");
    println!("ICED_CAPACITY PASS real_leases=128 destroyed_donors=16 unchanged_view=true native_release_retry=true exact_presented=true");
}
