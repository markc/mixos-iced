// SPDX-License-Identifier: MIT OR Apache-2.0
//! Real Iced tiny-skia after-commit failure, deferred feedback and retirement.

use iced::futures::channel::oneshot;
use iced::native_frame_probe::{self as probe, Held, Plan, Report};
use iced::{
    Element, Task,
    window::{
        self, Id,
        presentation::{FrameBinding, FrameObservation, FrameObserver, FrameOutcome, FrameStamp},
    },
};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

const X: FrameStamp = FrameStamp {
    activation_epoch: 1,
    local_revision: 0,
};
const A: FrameStamp = FrameStamp {
    activation_epoch: 2,
    local_revision: 0,
};
const B: FrameStamp = FrameStamp {
    activation_epoch: 3,
    local_revision: 0,
};

#[derive(Default)]
struct Observations {
    events: Vec<FrameObservation>,
    overflow: bool,
    retry: Option<oneshot::Sender<FrameObservation>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct State {
    window: Id,
    stamp: FrameStamp,
    observer: FrameObserver,
    observations: Arc<Mutex<Observations>>,
    close_while_held: bool,
    result: Arc<Mutex<Option<Result<(), String>>>>,
}

#[derive(Debug, Clone)]
enum Message {
    Opened(Id),
    Retry(FrameObservation),
    Held(Result<Held, String>),
    Report(Result<Report, String>),
}

fn validate(state: &State, report: Report) -> Result<(), String> {
    let observations = lock(&state.observations);
    let events = &observations.events;
    if observations.overflow {
        return Err("fixture observation cap exceeded".into());
    }
    if report.window != state.window || report.closed_while_held != state.close_while_held {
        return Err("probe window/mode mismatch".into());
    }
    if !matches!(report.aborted_terminal, FrameOutcome::Presented { .. }) {
        return Err(
            "failed commit did not receive actual Presented; narrower Discarded case only".into(),
        );
    }
    let matching = |stamp, request, outcome: fn(FrameOutcome) -> bool| {
        events
            .iter()
            .filter(move |event| {
                event.stamp == stamp && event.request_id == Some(request) && outcome(event.outcome)
            })
            .count()
    };
    let presented = |outcome| matches!(outcome, FrameOutcome::Presented { .. });
    if matching(X, report.aborted, |outcome| {
        outcome == FrameOutcome::SubmissionFailed
    }) != 1
        || events
            .iter()
            .any(|event| event.request_id == Some(report.aborted) && presented(event.outcome))
    {
        return Err(
            "failed native request reached observer as Presented or lacked unique failure".into(),
        );
    }
    if report.held == report.aborted || events.iter().any(|event| event.window != state.window) {
        return Err("native request or window identity was reused".into());
    }
    if !events.iter().any(|event| {
        event.stamp == X
            && event.request_id.is_some_and(|id| id != report.aborted)
            && presented(event.outcome)
    }) {
        return Err("ordinary retry did not present the original view".into());
    }
    if state.close_while_held {
        if report.released_after.is_some()
            || matching(A, report.held, presented) != 0
            || matching(A, report.held, |outcome| outcome == FrameOutcome::Closed) == 0
        {
            return Err("held native receipt was not retired with its actual window".into());
        }
    } else {
        let Some(replacement) = report.released_after else {
            return Err("held feedback lacked successful replacement submission".into());
        };
        if replacement == report.held
            || replacement == report.aborted
            || matching(A, report.held, presented) != 1
            || matching(B, replacement, presented) != 1
        {
            return Err("old and replacement receipts lost their exact native identities".into());
        }
        let old = events
            .iter()
            .position(|event| {
                event.stamp == A
                    && event.request_id == Some(report.held)
                    && presented(event.outcome)
            })
            .unwrap();
        let new = events
            .iter()
            .position(|event| {
                event.stamp == B
                    && event.request_id == Some(replacement)
                    && presented(event.outcome)
            })
            .unwrap();
        if old >= new {
            return Err(
                "held old feedback did not precede replacement terminal observation".into(),
            );
        }
    }
    println!("ICED_FRAME RECEIPT {report:?}");
    Ok(())
}

fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::Opened(window) => assert_eq!(window, state.window),
        Message::Retry(receipt) => {
            assert_eq!(receipt.window, state.window);
            assert_eq!(receipt.stamp, X);
            assert!(matches!(receipt.outcome, FrameOutcome::Presented { .. }));
            state.stamp = A;
        }
        Message::Held(Ok(held)) => {
            assert_eq!(held.window, state.window);
            assert!(
                !lock(&state.observations)
                    .events
                    .iter()
                    .any(|event| event.request_id == Some(held.request)
                        && matches!(event.outcome, FrameOutcome::Presented { .. })),
                "held native lease must not already reach the observer"
            );
            if state.close_while_held {
                return window::close(held.window);
            }
            state.stamp = B;
        }
        Message::Held(Err(error)) => {
            *lock(&state.result) = Some(Err(error));
            return iced::exit();
        }
        Message::Report(report) => {
            let result = report.and_then(|report| validate(state, report));
            *lock(&state.result) = Some(result);
            return if state.close_while_held {
                iced::exit()
            } else {
                window::close(state.window).chain(iced::exit())
            };
        }
    }
    Task::none()
}

fn view(state: &State, _: Id) -> Element<'_, Message> {
    let label = match state.stamp {
        X => "Native frame X: ordinary retry",
        A => "Native frame A: retained feedback",
        _ => "Native frame B: replacement commit",
    };
    iced::widget::container(iced::widget::text(label).size(toolkit::Tokens::dark().metrics.text.md))
        .width(iced::Fill)
        .height(iced::Fill)
        .center(iced::Fill)
        .into()
}

fn main() {
    let close_while_held = std::env::var("ICED_PROBE_CLOSE_HELD").is_ok_and(|value| value == "1");
    let (guard, handle) = probe::install(Plan {
        abort_once: X,
        hold_once: A,
        release_after_submit: B,
        close_while_held,
    })
    .unwrap();
    let observations = Arc::new(Mutex::new(Observations::default()));
    let (retry_sender, retry_receiver) = oneshot::channel();
    lock(&observations).retry = Some(retry_sender);
    let recorded = observations.clone();
    let observer = FrameObserver::new(move |receipt| {
        let sender = {
            let mut recorded = lock(&recorded);
            if recorded.events.len() == 16 {
                recorded.overflow = true;
            } else {
                recorded.events.push(receipt);
            }
            if receipt.stamp == X && matches!(receipt.outcome, FrameOutcome::Presented { .. }) {
                recorded.retry.take()
            } else {
                None
            }
        };
        if let Some(sender) = sender {
            let _ = sender.send(receipt);
        }
    });
    let result = Arc::new(Mutex::new(None));
    let boot_result = result.clone();
    let retry_receiver = Mutex::new(Some(retry_receiver));
    iced::daemon(
        move || {
            let (window, opened) = window::open(window::Settings {
                size: iced::Size::new(480.0, 180.0),
                exit_on_close_request: false,
                ..window::Settings::default()
            });
            let held = handle.take_held().unwrap();
            let report = handle.take_report().unwrap();
            let retry = lock(&retry_receiver).take().unwrap();
            (
                State {
                    window,
                    stamp: X,
                    observer: observer.clone(),
                    observations: observations.clone(),
                    close_while_held,
                    result: boot_result.clone(),
                },
                Task::batch([
                    opened.map(Message::Opened),
                    Task::perform(
                        async move { retry.await.expect("fixture retry sender retired") },
                        Message::Retry,
                    ),
                    Task::perform(
                        async move {
                            held.await
                                .unwrap_or_else(|_| Err("held notification cancelled".into()))
                        },
                        Message::Held,
                    ),
                    Task::perform(
                        async move {
                            report
                                .await
                                .unwrap_or_else(|_| Err("report notification cancelled".into()))
                        },
                        Message::Report,
                    ),
                ]),
            )
        },
        update,
        view,
    )
    .frame_presentation(|state, window| {
        (window == state.window).then(|| FrameBinding {
            stamp: state.stamp,
            observer: state.observer.clone(),
        })
    })
    .run()
    .unwrap();
    drop(guard);
    lock(&result)
        .take()
        .expect("native schedule did not finish")
        .expect("native schedule validation");
    if close_while_held {
        println!(
            "ICED_FRAME PASS failed_native_presented_suppressed=true ordinary_retry=true close_held_retired=true"
        );
    } else {
        println!(
            "ICED_FRAME PASS failed_native_presented_suppressed=true ordinary_retry=true old_stamp_after_new_submit=true exact_ids=true"
        );
    }
}
