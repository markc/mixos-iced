// SPDX-License-Identifier: MIT
//! Real foreign-window receipt leases and a finite, native-only release wake.

use super::{lock, Installation, INSTALLED};
use crate::core::window::{Id, presentation::{FrameBinding, FrameObserver, FrameStamp, FrameOutcome}};
use crate::futures::futures::channel::oneshot;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use winit::{window::{Window, WindowId}, presentation::{PresentationCapacity, PresentationFeedback}};

const DONORS: usize = 16;
const RECEIPTS: usize = 128;

/// Metadata proving an unchanged view recovered from real process exhaustion.
#[derive(Clone, Debug)]
pub struct CapacityReport {
    /// Actual target runtime window.
    pub window: Id,
    /// Actual request whose compositor terminal proved the target.
    pub request: u64,
    /// Process generation sampled while all foreign leases remained held.
    pub blocked_epoch: u64,
    /// Process generation sampled by the production retry branch.
    pub retry_epoch: u64,
    /// Unique actual donor requests admitted before donor destruction.
    pub donor_requests: usize,
    /// Actual native donor Destroyed events received before target admission.
    pub destroyed: usize,
}

/// Process installation and release-thread lifetime.
pub struct CapacityGuard(Arc<Control>);
/// Finite notifications containing metadata, never native leases.
#[derive(Clone)]
pub struct CapacityHandle(Arc<Control>);

pub(super) struct Control {
    stamp: FrameStamp,
    state: Mutex<State>,
    populated: Mutex<Option<oneshot::Receiver<Result<(), String>>>>,
    destroyed: Mutex<Option<oneshot::Receiver<Result<(), String>>>>,
    report: Mutex<Option<oneshot::Receiver<Result<CapacityReport, String>>>>,
    release: Mutex<Option<mpsc::SyncSender<Vec<PresentationFeedback>>>>,
    thread: Mutex<Option<JoinHandle<usize>>>,
    donor_ready: Mutex<Vec<Option<oneshot::Receiver<Result<(), String>>>>>,
    donor_retired: Mutex<Vec<Option<oneshot::Receiver<Result<(), String>>>>>,
}

#[derive(Default)]
struct State {
    donors: Vec<(WindowId, Vec<u64>, bool)>,
    leases: Vec<PresentationFeedback>,
    owner: Option<(Id, FrameObserver)>,
    size: Option<(u32, u32)>,
    refused: bool,
    blocked_epoch: Option<u64>,
    retry_epoch: Option<u64>,
    recovered: Option<u64>,
    populated: Option<oneshot::Sender<Result<(), String>>>,
    destroyed: Option<oneshot::Sender<Result<(), String>>>,
    report: Option<oneshot::Sender<Result<CapacityReport, String>>>,
    donor_ready: Vec<Option<oneshot::Sender<Result<(), String>>>>,
    donor_retired: Vec<Option<oneshot::Sender<Result<(), String>>>>,
}

/// Install before opening sixteen unstamped donors and one later stamped target.
pub fn install_capacity(stamp: FrameStamp) -> Result<(CapacityGuard, CapacityHandle), String> {
    let mut installed = lock(&INSTALLED);
    if installed.as_ref().is_some_and(Installation::alive) {
        return Err("native frame probe already installed".into());
    }
    let (populated, populated_rx) = oneshot::channel();
    let (destroyed, destroyed_rx) = oneshot::channel();
    let (report, report_rx) = oneshot::channel();
    let (release, receive) = mpsc::sync_channel::<Vec<PresentationFeedback>>(1);
    let (mut ready_tx, mut ready_rx, mut retired_tx, mut retired_rx) = (Vec::with_capacity(DONORS), Vec::with_capacity(DONORS), Vec::with_capacity(DONORS), Vec::with_capacity(DONORS));
    for _ in 0..DONORS {
        let (sender, receiver) = oneshot::channel(); ready_tx.push(Some(sender)); ready_rx.push(Some(receiver));
        let (sender, receiver) = oneshot::channel(); retired_tx.push(Some(sender)); retired_rx.push(Some(receiver));
    }
    let thread = std::thread::spawn(move || {
        match receive.recv_timeout(std::time::Duration::from_secs(45)) {
            Ok(leases) => { let count = leases.len(); drop(leases); count }
            Err(_) => 0,
        }
    });
    let control = Arc::new(Control {
        stamp,
        state: Mutex::new(State {
            donors: Vec::with_capacity(DONORS), leases: Vec::with_capacity(RECEIPTS),
            populated: Some(populated), destroyed: Some(destroyed), report: Some(report),
            donor_ready: ready_tx, donor_retired: retired_tx,
            ..State::default()
        }),
        populated: Mutex::new(Some(populated_rx)), destroyed: Mutex::new(Some(destroyed_rx)),
        report: Mutex::new(Some(report_rx)), release: Mutex::new(Some(release)),
        thread: Mutex::new(Some(thread)),
        donor_ready: Mutex::new(ready_rx), donor_retired: Mutex::new(retired_rx),
    });
    *installed = Some(Installation::Capacity(Arc::downgrade(&control)));
    Ok((CapacityGuard(control.clone()), CapacityHandle(control)))
}

impl CapacityHandle {
    /// Take one donor's actual eight-terminal notification, in creation order.
    pub fn take_donor_ready(&self, index: usize) -> Result<oneshot::Receiver<Result<(), String>>, String> {
        lock(&self.0.donor_ready).get_mut(index).and_then(Option::take).ok_or_else(|| "invalid or consumed donor receiver".into())
    }
    /// Take one donor's actual native destruction notification.
    pub fn take_donor_retired(&self, index: usize) -> Result<oneshot::Receiver<Result<(), String>>, String> {
        lock(&self.0.donor_retired).get_mut(index).and_then(Option::take).ok_or_else(|| "invalid or consumed donor retirement receiver".into())
    }
    /// Take the notification after 128 actual terminal leases are retained.
    pub fn take_populated(&self) -> Result<oneshot::Receiver<Result<(), String>>, String> {
        lock(&self.0.populated).take().ok_or_else(|| "populated receiver already taken".into())
    }
    /// Take the notification after every donor's actual native destruction.
    pub fn take_destroyed(&self) -> Result<oneshot::Receiver<Result<(), String>>, String> {
        lock(&self.0.destroyed).take().ok_or_else(|| "destroyed receiver already taken".into())
    }
    /// Take the unchanged target's actual Presented notification.
    pub fn take_report(&self) -> Result<oneshot::Receiver<Result<CapacityReport, String>>, String> {
        lock(&self.0.report).take().ok_or_else(|| "capacity report receiver already taken".into())
    }
    /// Join the finite release owner after completion and verify all leases retired.
    pub fn finish(&self) -> Result<(), String> {
        drop(lock(&self.0.release).take());
        let thread = lock(&self.0.thread).take().ok_or("release thread already joined")?;
        if thread.join().map_err(|_| "release thread panicked")? != RECEIPTS {
            return Err("release thread did not retire every actual lease".into());
        }
        if !lock(&self.0.state).leases.is_empty() { return Err("native lease holder was not drained".into()); }
        Ok(())
    }
}

impl Drop for CapacityGuard {
    fn drop(&mut self) {
        let mut installed = lock(&INSTALLED);
        if installed.as_ref().is_some_and(|installation| match installation {
            Installation::Capacity(control) => control.upgrade().is_some_and(|control| Arc::ptr_eq(&control, &self.0)),
            _ => false,
        }) { *installed = None; }
        drop(installed);
        drop(lock(&self.0.release).take());
        if let Some(thread) = lock(&self.0.thread).take() { let _ = thread.join(); }
    }
}

fn installed() -> Option<Arc<Control>> {
    match lock(&INSTALLED).as_ref() {
        Some(Installation::Capacity(control)) => control.upgrade(),
        _ => None,
    }
}

impl Control {
    fn fail(&self, reason: &'static str) {
        let mut state = lock(&self.state);
        if let Some(sender) = state.populated.take() { let _ = sender.send(Err(reason.into())); }
        if let Some(sender) = state.destroyed.take() { let _ = sender.send(Err(reason.into())); }
        if let Some(sender) = state.report.take() { let _ = sender.send(Err(reason.into())); }
        for sender in state.donor_ready.iter_mut().filter_map(Option::take) { let _ = sender.send(Err(reason.into())); }
        for sender in state.donor_retired.iter_mut().filter_map(Option::take) { let _ = sender.send(Err(reason.into())); }
    }
    fn owns(&self, state: &State, id: Id, binding: &FrameBinding) -> bool {
        binding.stamp == self.stamp && state.owner.as_ref().is_some_and(|(owner, observer)| *owner == id && observer.same_owner(&binding.observer))
    }
}

pub(crate) fn pre_present(window: &Window, binding: Option<&FrameBinding>) {
    let Some(control) = installed() else { return; };
    if binding.is_some() { return; }
    let mut state = lock(&control.state);
    if state.donors.iter().any(|(id, _, retired)| *id == window.id() && !retired) { return; }
    if state.donors.len() == DONORS { drop(state); control.fail("too many native donor windows"); return; }
    let mut requests = Vec::with_capacity(8);
    for _ in 0..8 {
        match crate::native_presentation::feedback(window) {
            Ok(id) => requests.push(id),
            Err(_) => { drop(state); control.fail("real donor feedback admission failed"); return; }
        }
    }
    state.donors.push((window.id(), requests, false));
}

pub(crate) fn intercept(window: WindowId, feedback: PresentationFeedback) -> Option<PresentationFeedback> {
    let Some(control) = installed() else { return Some(feedback); };
    let mut state = lock(&control.state);
    let Some(index) = state.donors.iter().position(|(id, requests, retired)| *id == window && !retired && requests.contains(&feedback.id.get())) else {
        return Some(feedback);
    };
    if state.leases.len() == RECEIPTS || state.leases.iter().any(|lease| lease.id == feedback.id) {
        drop(state); control.fail("duplicate or excess donor terminal lease"); return Some(feedback);
    }
    state.leases.push(feedback);
    if state.donors[index].1.iter().all(|request| state.leases.iter().any(|lease| lease.id.get() == *request)) {
        if let Some(sender) = state.donor_ready[index].take() { let _ = sender.send(Ok(())); }
    }
    if state.leases.len() == RECEIPTS {
        if let Some(sender) = state.populated.take() { let _ = sender.send(Ok(())); }
    }
    None
}

pub(crate) fn destroyed(window: WindowId) {
    let Some(control) = installed() else { return; };
    let mut state = lock(&control.state);
    let Some(index) = state.donors.iter().position(|(id, _, retired)| *id == window && !retired) else { return; };
    state.donors[index].2 = true;
    if let Some(sender) = state.donor_retired[index].take() { let _ = sender.send(Ok(())); }
    if state.donors.len() == DONORS && state.donors.iter().all(|(_, _, retired)| *retired) {
        if state.leases.len() != RECEIPTS { drop(state); control.fail("donor close released retained process capacity"); return; }
        if let Some(sender) = state.destroyed.take() { let _ = sender.send(Ok(())); }
    }
}

pub(super) fn begin(id: Id, binding: Option<&FrameBinding>, size: (u32, u32)) {
    let Some(control) = installed() else { return; };
    let Some(binding) = binding else { return; };
    let mut state = lock(&control.state);
    if state.owner.is_none() && binding.stamp == control.stamp {
        state.owner = Some((id, binding.observer.clone())); state.size = Some(size);
    }
    if !control.owns(&state, id, binding) || state.size != Some(size) {
        drop(state); control.fail("capacity target view, owner or physical size changed");
    }
}

pub(crate) fn donor_submitted(window: WindowId, successful: bool, hook_called: bool) {
    let Some(control) = installed() else { return; };
    let state = lock(&control.state);
    if state.donors.iter().any(|(id, _, retired)| *id == window && !retired) && (!successful || !hook_called) {
        drop(state); control.fail("donor requests did not belong to a successful real first submission");
    }
}

pub(crate) fn refused(id: Id, binding: &FrameBinding, capacity: Option<PresentationCapacity>) {
    let Some(control) = installed() else { return; };
    let mut state = lock(&control.state);
    if !control.owns(&state, id, binding) { return; }
    if state.refused || state.leases.len() != RECEIPTS || !state.donors.iter().all(|(_, _, retired)| *retired) || capacity.is_none_or(|capacity| capacity.available) {
        drop(state); control.fail("target Capacity lacked closed foreign lease exhaustion"); return;
    }
    state.refused = true;
}

pub(crate) fn scanned(id: Id, capacity: PresentationCapacity, retry: bool) {
    let Some(control) = installed() else { return; };
    let mut state = lock(&control.state);
    if !state.refused || state.owner.as_ref().is_none_or(|(owner, _)| *owner != id) { return; }
    if !capacity.available && state.blocked_epoch.is_none() {
        state.blocked_epoch = Some(capacity.release_epoch);
        let leases = std::mem::take(&mut state.leases);
        drop(state);
        if let Some(sender) = lock(&control.release).take() {
            if sender.try_send(leases).is_err() { control.fail("native lease release owner retired early"); }
        } else { control.fail("native lease release already consumed"); }
    } else if retry {
        if !capacity.available || state.retry_epoch.is_some() || state.blocked_epoch.is_none_or(|epoch| capacity.release_epoch <= epoch) {
            drop(state); control.fail("production retry did not follow real process release"); return;
        }
        state.retry_epoch = Some(capacity.release_epoch);
    }
}

pub(super) fn submitted(id: Id, binding: &FrameBinding, request: Option<u64>, successful: bool) {
    let Some(control) = installed() else { return; };
    let mut state = lock(&control.state);
    if !control.owns(&state, id, binding) { return; }
    if let Some(request) = request {
        if !successful || state.retry_epoch.is_none() || state.recovered.is_some() {
            drop(state); control.fail("target requested proof outside its one production capacity retry"); return;
        }
        state.recovered = Some(request);
    }
}

pub(crate) fn target_terminal(id: Id, binding: &FrameBinding, request: u64, outcome: FrameOutcome) {
    let Some(control) = installed() else { return; };
    let mut state = lock(&control.state);
    if !control.owns(&state, id, binding) || state.recovered != Some(request) { return; }
    if !matches!(outcome, FrameOutcome::Presented { .. }) {
        drop(state); control.fail("capacity target did not actually present"); return;
    }
    let report = CapacityReport {
        window: id, request, blocked_epoch: state.blocked_epoch.expect("actual blocked scan"),
        retry_epoch: state.retry_epoch.expect("actual retry scan"),
        donor_requests: state.donors.iter().map(|(_, requests, _)| requests.len()).sum(),
        destroyed: state.donors.iter().filter(|(_, _, retired)| *retired).count(),
    };
    if let Some(sender) = state.report.take() { let _ = sender.send(Ok(report)); }
}
