// SPDX-License-Identifier: MIT
//! Opt-in native acceptance schedule. Stores one real feedback per window;
//! shared control contains only bounded metadata and single-use notifications.

use crate::core::window::{
    Id,
    presentation::{FrameBinding, FrameObserver, FrameOutcome, FrameStamp},
};
use crate::futures::futures::channel::oneshot;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use winit::presentation::PresentationFeedback;

static INSTALLED: Mutex<Option<Weak<Control>>> = Mutex::new(None);

#[derive(Clone, Copy, Debug)]
pub struct Plan {
    pub abort_once: FrameStamp,
    pub hold_once: FrameStamp,
    pub release_after_submit: FrameStamp,
    pub close_while_held: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Held {
    pub window: Id,
    pub request: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Report {
    pub window: Id,
    pub aborted: u64,
    pub aborted_terminal: FrameOutcome,
    pub held: u64,
    pub released_after: Option<u64>,
    pub closed_while_held: bool,
}

pub struct Guard(Arc<Control>);
#[derive(Clone)]
pub struct Handle(Arc<Control>);

struct Control {
    plan: Plan,
    state: Mutex<State>,
    held_receiver: Mutex<Option<oneshot::Receiver<Result<Held, String>>>>,
    report_receiver: Mutex<Option<oneshot::Receiver<Result<Report, String>>>>,
}

#[derive(Default)]
struct State {
    owner: Option<(Id, FrameObserver)>,
    abort_attempted: bool,
    aborted: Option<u64>,
    aborted_terminal: Option<FrameOutcome>,
    held: Option<u64>,
    released_after: Option<u64>,
    replacement_presented: bool,
    held_sender: Option<oneshot::Sender<Result<Held, String>>>,
    report_sender: Option<oneshot::Sender<Result<Report, String>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn install(plan: Plan) -> Result<(Guard, Handle), String> {
    if plan.abort_once == plan.hold_once
        || plan.abort_once == plan.release_after_submit
        || plan.hold_once == plan.release_after_submit
    {
        return Err("probe stamps must be distinct".into());
    }
    let mut installed = lock(&INSTALLED);
    if installed.as_ref().and_then(Weak::upgrade).is_some() {
        return Err("native frame probe already installed".into());
    }
    let (held_sender, held_receiver) = oneshot::channel();
    let (report_sender, report_receiver) = oneshot::channel();
    let control = Arc::new(Control {
        plan,
        state: Mutex::new(State {
            held_sender: Some(held_sender),
            report_sender: Some(report_sender),
            ..State::default()
        }),
        held_receiver: Mutex::new(Some(held_receiver)),
        report_receiver: Mutex::new(Some(report_receiver)),
    });
    *installed = Some(Arc::downgrade(&control));
    Ok((Guard(control.clone()), Handle(control)))
}

impl Handle {
    pub fn take_held(&self) -> Result<oneshot::Receiver<Result<Held, String>>, String> {
        lock(&self.0.held_receiver)
            .take()
            .ok_or_else(|| "held receiver already taken".into())
    }
    pub fn take_report(&self) -> Result<oneshot::Receiver<Result<Report, String>>, String> {
        lock(&self.0.report_receiver)
            .take()
            .ok_or_else(|| "report receiver already taken".into())
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let mut installed = lock(&INSTALLED);
        if installed
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some_and(|control| Arc::ptr_eq(&control, &self.0))
        {
            *installed = None;
        }
    }
}

impl Control {
    fn notify_held(&self) {
        let send = {
            let mut state = lock(&self.state);
            match (&state.owner, state.aborted_terminal, state.held) {
                (Some((window, _)), Some(_), Some(request)) => {
                    let held = Held {
                        window: *window,
                        request,
                    };
                    state.held_sender.take().map(|sender| (sender, held))
                }
                _ => None,
            }
        };
        if let Some((sender, held)) = send {
            let _ = sender.send(Ok(held));
        }
    }
    fn fail(&self, reason: &'static str) {
        let (held, report) = {
            let mut state = lock(&self.state);
            (state.held_sender.take(), state.report_sender.take())
        };
        if let Some(sender) = held {
            let _ = sender.send(Err(reason.into()));
        }
        if let Some(sender) = report {
            let _ = sender.send(Err(reason.into()));
        }
    }

    fn report(&self, closed: bool) {
        let send = {
            let mut state = lock(&self.state);
            if !closed && !state.replacement_presented {
                return;
            }
            match (
                &state.owner,
                state.aborted,
                state.aborted_terminal,
                state.held,
            ) {
                (Some((window, _)), Some(aborted), Some(aborted_terminal), Some(held)) => {
                    let report = Report {
                        window: *window,
                        aborted,
                        aborted_terminal,
                        held,
                        released_after: state.released_after,
                        closed_while_held: closed,
                    };
                    state.report_sender.take().map(|sender| (sender, report))
                }
                _ => None,
            }
        };
        if let Some((sender, report)) = send {
            let _ = sender.send(Ok(report));
        }
    }
}

pub(crate) struct Gate {
    control: Option<Arc<Control>>,
    held: Option<PresentationFeedback>,
    bound: Option<(Id, FrameObserver)>,
}

impl Gate {
    pub(crate) fn new() -> Self {
        Self {
            control: lock(&INSTALLED).as_ref().and_then(Weak::upgrade),
            held: None,
            bound: None,
        }
    }

    fn owns(&self, window: Id, binding: &FrameBinding) -> bool {
        self.bound.as_ref().is_some_and(|(owner, observer)| {
            *owner == window && observer.same_owner(&binding.observer)
        })
    }

    pub(crate) fn begin(&mut self, window: Id, binding: Option<&FrameBinding>) -> bool {
        let Some(control) = &self.control else {
            return false;
        };
        let Some(binding) = binding else {
            return false;
        };
        if self.bound.is_none() && binding.stamp == control.plan.abort_once {
            let mut state = lock(&control.state);
            if state.owner.is_none() {
                state.owner = Some((window, binding.observer.clone()));
                self.bound = Some((window, binding.observer.clone()));
            }
        }
        if self.bound.is_some() && !self.owns(window, binding) {
            control.fail("probe window or observation owner changed");
            return false;
        }
        if !self.owns(window, binding) || binding.stamp != control.plan.abort_once {
            return false;
        }
        let mut state = lock(&control.state);
        if state.abort_attempted {
            return false;
        }
        state.abort_attempted = true;
        true
    }

    pub(crate) fn submitted(
        &mut self,
        window: Id,
        binding: &FrameBinding,
        request: Option<u64>,
        successful: bool,
        fault_consumed: Option<bool>,
    ) -> Option<PresentationFeedback> {
        let Some(control) = &self.control else {
            return None;
        };
        if !self.owns(window, binding) {
            return None;
        }
        if let Some(consumed) = fault_consumed {
            if !consumed || successful || request.is_none() {
                control.fail("fault did not follow a real requested buffer commit");
                return None;
            }
            lock(&control.state).aborted = request;
        }
        if binding.stamp == control.plan.release_after_submit
            && successful
            && !control.plan.close_while_held
            && self.held.is_some()
        {
            let Some(request) = request else {
                control.fail("replacement submission had no actual feedback request");
                return None;
            };
            lock(&control.state).released_after = Some(request);
            return self.held.take();
        }
        None
    }

    pub(crate) fn intercept(
        &mut self,
        window: Id,
        ledger: &crate::presentation::Ledger,
        feedback: PresentationFeedback,
    ) -> Option<PresentationFeedback> {
        let Some(control) = &self.control else {
            return Some(feedback);
        };
        let request = feedback.id.get();
        let outcome = crate::presentation::outcome(&feedback);
        let Some((binding, successful)) = ledger.pending_binding(request) else {
            return Some(feedback);
        };
        if !self.owns(window, binding) {
            return Some(feedback);
        }
        let (aborted, hold, released) = {
            let state = lock(&control.state);
            (
                state.aborted == Some(request),
                state.held.is_none() && binding.stamp == control.plan.hold_once,
                state.released_after == Some(request),
            )
        };
        if aborted {
            lock(&control.state).aborted_terminal = Some(outcome);
            control.notify_held();
        }
        if hold && successful {
            if !matches!(outcome, FrameOutcome::Presented { .. }) {
                control.fail("held frame was not actually presented");
                return Some(feedback);
            }
            lock(&control.state).held = Some(request);
            self.held = Some(feedback);
            // Both native arrivals are required before the ordinary fixture
            // update can advance or close, in either valid dispatch order.
            control.notify_held();
            return None;
        }
        if released {
            if matches!(outcome, FrameOutcome::Presented { .. }) {
                lock(&control.state).replacement_presented = true;
            } else {
                control.fail("replacement frame was not actually presented");
            }
        }
        if aborted || released {
            control.report(false);
        }
        Some(feedback)
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        let held = self.held.take();
        let had_held = held.is_some();
        drop(held);
        if let Some(control) = &self.control {
            if had_held && control.plan.close_while_held {
                control.report(true);
            }
            // Normal completion already consumed its senders. Otherwise
            // window retirement cancels both finite fixture waiters.
            if self.bound.is_some() {
                control.fail("probe window retired before its schedule completed");
            }
        }
    }
}
