// SPDX-License-Identifier: MIT
//! Bounded metadata for actual renderer faults and unheld native recovery.

use super::{FailurePoint, INSTALLED, Installation, lock};
use crate::core::window::{
    Id,
    presentation::{FrameBinding, FrameObserver, FrameOutcome, FrameStamp},
};
use crate::futures::futures::channel::oneshot;
use std::sync::{Arc, Mutex};
use winit::presentation::PresentationFeedback;

const MAX_FAILURES: u8 = 9;

/// One stable draw whose real submissions fail at the configured boundary.
#[derive(Clone, Copy, Debug)]
pub struct RecoveryPlan {
    /// Immutable draw identity; the fixture first presents a separate baseline.
    pub fault_stamp: FrameStamp,
    /// Actual failures: one through nine before commit, or one after commit.
    pub failures: u8,
}

/// Completed recovery metadata; no native receipt lease is retained here.
#[derive(Clone, Debug)]
pub struct RecoveryReport {
    /// Actual renderer scope selected for every injected draw.
    pub failure_point: FailurePoint,
    /// Actual physical draw size, held unchanged through every fault and retry.
    pub physical_size: (u32, u32),
    /// Actual window that performed every failed and successful draw.
    pub window: Id,
    /// Actual fault attempts, including untracked attempts after capacity fills.
    pub failed_requests: Vec<Option<u64>>,
    /// Actual terminal outcomes of the admitted unsuccessful request IDs.
    pub failed_terminals: Vec<(u64, FrameOutcome)>,
    /// Successful buffer commits made while feedback admission was unavailable.
    pub untracked_commits: u8,
    /// Distinct native request that successfully proves the unchanged draw.
    pub recovered: u64,
}

/// Process installation lifetime for an unheld recovery schedule.
pub struct RecoveryGuard(Arc<Control>);
/// Single-use completion metadata for the installed recovery schedule.
#[derive(Clone)]
pub struct RecoveryHandle(Arc<Control>);

pub(super) struct Control {
    plan: RecoveryPlan,
    point: FailurePoint,
    state: Mutex<State>,
    receiver: Mutex<Option<oneshot::Receiver<Result<RecoveryReport, String>>>>,
}

#[derive(Default)]
struct State {
    owner: Option<(Id, FrameObserver)>,
    physical_size: Option<(u32, u32)>,
    failed: Vec<Option<u64>>,
    terminals: Vec<(u64, FrameOutcome)>,
    untracked_commits: u8,
    recovered: Option<u64>,
    presented: bool,
    sender: Option<oneshot::Sender<Result<RecoveryReport, String>>>,
}

/// Install a bounded pre-commit failure schedule before opening its window.
pub fn install_recovery(plan: RecoveryPlan) -> Result<(RecoveryGuard, RecoveryHandle), String> {
    install_at(plan, FailurePoint::BeforeCommit)
}

/// Install one unheld after-commit failure; either real native terminal is valid.
/// Successful recovery still requires a distinct actual Presented request.
pub fn install_after_commit_recovery(
    plan: RecoveryPlan,
) -> Result<(RecoveryGuard, RecoveryHandle), String> {
    if plan.failures != 1 {
        return Err("unheld after-commit recovery requires one failure".into());
    }
    install_at(plan, FailurePoint::AfterCommit)
}

fn install_at(
    plan: RecoveryPlan,
    point: FailurePoint,
) -> Result<(RecoveryGuard, RecoveryHandle), String> {
    if !(1..=MAX_FAILURES).contains(&plan.failures) {
        return Err("recovery failures must be between one and nine".into());
    }
    let mut installed = lock(&INSTALLED);
    if installed.as_ref().is_some_and(Installation::alive) {
        return Err("native frame probe already installed".into());
    }
    let (sender, receiver) = oneshot::channel();
    let control = Arc::new(Control {
        plan,
        point,
        state: Mutex::new(State {
            failed: Vec::with_capacity(MAX_FAILURES as usize),
            terminals: Vec::with_capacity(MAX_FAILURES as usize),
            sender: Some(sender),
            ..State::default()
        }),
        receiver: Mutex::new(Some(receiver)),
    });
    *installed = Some(Installation::Recovery(Arc::downgrade(&control)));
    Ok((RecoveryGuard(control.clone()), RecoveryHandle(control)))
}

impl RecoveryHandle {
    /// Take the one completed-schedule notification.
    pub fn take_report(&self) -> Result<oneshot::Receiver<Result<RecoveryReport, String>>, String> {
        lock(&self.0.receiver)
            .take()
            .ok_or_else(|| "recovery receiver already taken".into())
    }
}

impl Drop for RecoveryGuard {
    fn drop(&mut self) {
        let mut installed = lock(&INSTALLED);
        if installed
            .as_ref()
            .is_some_and(|installation| match installation {
                Installation::Recovery(control) => control
                    .upgrade()
                    .is_some_and(|control| Arc::ptr_eq(&control, &self.0)),
                Installation::Ordering(_) | Installation::Capacity(_) => false,
            })
        {
            *installed = None;
        }
    }
}

impl Control {
    fn fail(&self, reason: &'static str) {
        let sender = lock(&self.state).sender.take();
        if let Some(sender) = sender {
            let _ = sender.send(Err(reason.into()));
        }
    }

    fn report(&self) {
        let send = {
            let mut state = lock(&self.state);
            let requested = state.failed.iter().flatten().count();
            if !state.presented
                || state.failed.len() != self.plan.failures as usize
                || state.terminals.len() != requested
            {
                return;
            }
            match (state.owner.as_ref(), state.recovered, state.physical_size) {
                (Some((window, _)), Some(recovered), Some(physical_size)) => {
                    let report = RecoveryReport {
                        failure_point: self.point,
                        physical_size,
                        window: *window,
                        failed_requests: state.failed.clone(),
                        failed_terminals: state.terminals.clone(),
                        untracked_commits: state.untracked_commits,
                        recovered,
                    };
                    state.sender.take().map(|sender| (sender, report))
                }
                _ => None,
            }
        };
        if let Some((sender, report)) = send {
            let _ = sender.send(Ok(report));
        }
    }
}

pub(super) struct Gate {
    control: Arc<Control>,
    bound: Option<(Id, FrameObserver)>,
}

impl Gate {
    pub(super) fn new(control: Arc<Control>) -> Self {
        Self {
            control,
            bound: None,
        }
    }

    fn owns(&self, window: Id, binding: &FrameBinding) -> bool {
        self.bound.as_ref().is_some_and(|(owner, observer)| {
            *owner == window && observer.same_owner(&binding.observer)
        })
    }

    pub(super) fn begin(
        &mut self,
        window: Id,
        binding: Option<&FrameBinding>,
        physical_size: (u32, u32),
    ) -> Option<FailurePoint> {
        let binding = binding?;
        if self.bound.is_none() && binding.stamp == self.control.plan.fault_stamp {
            let mut state = lock(&self.control.state);
            if state.owner.is_none() {
                state.owner = Some((window, binding.observer.clone()));
                state.physical_size = Some(physical_size);
                self.bound = Some((window, binding.observer.clone()));
            }
        }
        if !self.owns(window, binding) {
            if self
                .bound
                .as_ref()
                .is_some_and(|(owner, _)| *owner == window)
            {
                self.control.fail("recovery observer owner changed");
            }
            return None;
        }
        if lock(&self.control.state).physical_size != Some(physical_size) {
            self.control
                .fail("recovery used a changed physical draw size");
            return None;
        }
        if binding.stamp != self.control.plan.fault_stamp {
            self.control
                .fail("recovery binding changed before completion");
            return None;
        }
        (lock(&self.control.state).failed.len() < self.control.plan.failures as usize)
            .then_some(self.control.point)
    }

    pub(super) fn submitted(
        &mut self,
        window: Id,
        binding: &FrameBinding,
        request: Option<u64>,
        successful: bool,
        fault: Option<bool>,
        hook_called: bool,
    ) {
        if !self.owns(window, binding) {
            return;
        }
        if binding.stamp != self.control.plan.fault_stamp {
            self.control.fail("recovery submission changed its binding");
            return;
        }
        if let Some(consumed) = fault {
            if !consumed || successful || !hook_called {
                self.control
                    .fail("configured fault scope did not fail a real post-hook submission");
                return;
            }
            let mut state = lock(&self.control.state);
            if state.failed.len() == self.control.plan.failures as usize {
                drop(state);
                self.control
                    .fail("pre-commit failure count exceeded its bound");
                return;
            }
            if request.is_some_and(|request| state.failed.contains(&Some(request))) {
                drop(state);
                self.control.fail("failed native request identity reused");
                return;
            }
            state.failed.push(request);
        } else if successful && hook_called {
            let mut state = lock(&self.control.state);
            if let Some(request) = request {
                if state.recovered.is_none() {
                    state.recovered = Some(request);
                }
            } else {
                state.untracked_commits = state.untracked_commits.saturating_add(1);
            }
        }
    }

    pub(super) fn intercept(
        &mut self,
        window: Id,
        ledger: &crate::presentation::Ledger,
        feedback: &PresentationFeedback,
    ) {
        let request = feedback.id.get();
        let Some((binding, successful)) = ledger.pending_binding(request) else {
            return;
        };
        if !self.owns(window, binding) {
            return;
        }
        let outcome = crate::frame_feedback_outcome(feedback);
        let mut state = lock(&self.control.state);
        if state.failed.contains(&Some(request)) {
            if successful
                || state
                    .terminals
                    .iter()
                    .any(|(previous, _)| *previous == request)
            {
                drop(state);
                self.control
                    .fail("unsuccessful native request was reused or relabelled successful");
                return;
            }
            state.terminals.push((request, outcome));
        } else if state.recovered == Some(request) {
            if !successful || !matches!(outcome, FrameOutcome::Presented { .. }) {
                drop(state);
                self.control
                    .fail("recovery native request did not actually present");
                return;
            }
            state.presented = true;
        }
        drop(state);
        self.control.report();
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        if self.bound.is_some() {
            self.control
                .fail("recovery window retired before completion");
        }
    }
}
