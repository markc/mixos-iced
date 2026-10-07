// SPDX-License-Identifier: MIT
//! Opt-in native acceptance schedule. Stores one real feedback per window;
//! shared control contains only bounded metadata and single-use notifications.

use crate::core::window::presentation::probe::FailurePoint;
use crate::core::window::{
    Id,
    presentation::{FrameBinding, FrameObserver, FrameOutcome, FrameStamp},
};
use crate::futures::futures::channel::oneshot;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use winit::presentation::PresentationFeedback;

pub(crate) mod capacity;
mod recovery;
pub use capacity::{CapacityGuard, CapacityHandle, CapacityReport, install_capacity};
pub use recovery::{
    RecoveryGuard, RecoveryHandle, RecoveryPlan, RecoveryReport, install_after_commit_recovery,
    install_recovery,
};

static INSTALLED: Mutex<Option<Installation>> = Mutex::new(None);

enum Installation {
    Ordering(Weak<Control>),
    Recovery(Weak<recovery::Control>),
    Capacity(Weak<capacity::Control>),
}

impl Installation {
    fn alive(&self) -> bool {
        match self {
            Self::Ordering(control) => control.strong_count() != 0,
            Self::Recovery(control) => control.strong_count() != 0,
            Self::Capacity(control) => control.strong_count() != 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
/// One real-window acceptance schedule, absent from ordinary builds.
pub struct Plan {
    /// Draw whose actual buffer commit is followed by one injected error.
    pub abort_once: FrameStamp,
    /// Later successful draw whose actual feedback lease is held.
    pub hold_once: FrameStamp,
    /// Successful replacement submission that releases the held lease.
    pub release_after_submit: FrameStamp,
    /// Close the owning window while its real feedback is held instead.
    pub close_while_held: bool,
}

#[derive(Clone, Copy, Debug)]
/// Metadata notification after both the aborted terminal and held lease arrive.
pub struct Held {
    /// Actual owning runtime window.
    pub window: Id,
    /// Identity of the real held native request.
    pub request: u64,
}

#[derive(Clone, Copy, Debug)]
/// Completed schedule metadata; contains no native object or feedback lease.
pub struct Report {
    /// Actual owning runtime window.
    pub window: Id,
    /// Actual native request associated with the unsuccessful submission.
    pub aborted: u64,
    /// Actual compositor terminal outcome for that unsuccessful submission.
    pub aborted_terminal: FrameOutcome,
    /// Actual native request held until replacement or window retirement.
    pub held: u64,
    /// Actual successful replacement request, absent for the close schedule.
    pub released_after: Option<u64>,
    /// Whether the actual owning window retired with its feedback still held.
    pub closed_while_held: bool,
}

/// Installation lifetime; dropping it prevents new windows joining the probe.
pub struct Guard(Arc<Control>);
#[derive(Clone)]
/// Single-use metadata notifications for the installed acceptance schedule.
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

/// Install one process-wide schedule before creating its actual window.
pub fn install(plan: Plan) -> Result<(Guard, Handle), String> {
    if plan.abort_once == plan.hold_once
        || plan.abort_once == plan.release_after_submit
        || plan.hold_once == plan.release_after_submit
    {
        return Err("probe stamps must be distinct".into());
    }
    let mut installed = lock(&INSTALLED);
    if installed.as_ref().is_some_and(Installation::alive) {
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
    *installed = Some(Installation::Ordering(Arc::downgrade(&control)));
    Ok((Guard(control.clone()), Handle(control)))
}

impl Handle {
    /// Take the single held-feedback metadata notification.
    pub fn take_held(&self) -> Result<oneshot::Receiver<Result<Held, String>>, String> {
        lock(&self.0.held_receiver)
            .take()
            .ok_or_else(|| "held receiver already taken".into())
    }
    /// Take the single completed-schedule metadata notification.
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
            .and_then(|installation| match installation {
                Installation::Ordering(control) => control.upgrade(),
                Installation::Recovery(_) | Installation::Capacity(_) => None,
            })
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
    recovery: Option<recovery::Gate>,
    submission_hold: Option<u64>,
}

impl Gate {
    pub(crate) fn new() -> Self {
        let installed = lock(&INSTALLED);
        let (control, recovery) = match installed.as_ref() {
            Some(Installation::Ordering(control)) => (control.upgrade(), None),
            Some(Installation::Recovery(control)) => {
                (None, control.upgrade().map(recovery::Gate::new))
            }
            Some(Installation::Capacity(_)) | None => (None, None),
        };
        Self {
            control,
            held: None,
            bound: None,
            recovery,
            submission_hold: None,
        }
    }

    pub(crate) fn submissions_held(&self) -> bool {
        self.submission_hold.is_some()
    }

    pub(crate) fn after_delivery(&mut self, request: u64) -> bool {
        if self.submission_hold == Some(request) {
            self.submission_hold = None;
            return true;
        }
        false
    }

    fn owns(&self, window: Id, binding: &FrameBinding) -> bool {
        self.bound.as_ref().is_some_and(|(owner, observer)| {
            *owner == window && observer.same_owner(&binding.observer)
        })
    }

    pub(crate) fn begin(
        &mut self,
        window: Id,
        binding: Option<&FrameBinding>,
        physical_size: (u32, u32),
    ) -> Option<FailurePoint> {
        capacity::begin(window, binding, physical_size);
        if let Some(recovery) = &mut self.recovery {
            return recovery.begin(window, binding, physical_size);
        }
        self.begin_ordering(window, binding)
            .then_some(FailurePoint::AfterCommit)
    }

    fn begin_ordering(&mut self, window: Id, binding: Option<&FrameBinding>) -> bool {
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
        pre_present_called: bool,
    ) -> Option<PresentationFeedback> {
        capacity::submitted(window, binding, request, successful);
        if let Some(recovery) = &mut self.recovery {
            recovery.submitted(
                window,
                binding,
                request,
                successful,
                fault_consumed,
                pre_present_called,
            );
            return None;
        }
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
            // This strict ordering schedule must observe the failed commit's
            // real terminal before any newer buffer can supersede it. The
            // production retry path still runs; only this probe's submissions
            // are held until normal delivery retires the original lease.
            self.submission_hold = request;
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
        if let Some(recovery) = &mut self.recovery {
            recovery.intercept(window, ledger, &feedback);
            return Some(feedback);
        }
        let Some(control) = &self.control else {
            return Some(feedback);
        };
        let request = feedback.id.get();
        let outcome = crate::frame_feedback_outcome(&feedback);
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
