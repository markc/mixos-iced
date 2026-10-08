// SPDX-License-Identifier: MIT
//! Per-window correlation of drawn interfaces and native commit observations.

use crate::core::window::{
    Id,
    presentation::{FrameBinding, FrameOutcome},
};
use std::collections::BTreeMap;

const CAP: usize = 8;

/// One process generation per runtime; no timer or query-driven redraw.
#[derive(Default)]
pub(crate) struct CapacityEpoch(Option<u64>);

impl CapacityEpoch {
    pub(crate) fn idle(&mut self) {
        self.0 = None;
    }
    pub(crate) fn should_scan(&mut self, epoch: u64) -> bool {
        let changed = self.0 != Some(epoch) || epoch == u64::MAX;
        self.0 = Some(epoch);
        changed
    }
}

struct Submission {
    binding: FrameBinding,
    successful: bool,
}

pub(crate) struct Ledger {
    window: Id,
    pending: BTreeMap<u64, Submission>,
    proven: Option<(u64, FrameBinding)>,
    latest: Option<FrameBinding>,
    capacity_blocked: bool,
}

impl Ledger {
    pub fn new(window: Id) -> Self {
        Self {
            window,
            pending: BTreeMap::new(),
            proven: None,
            latest: None,
            capacity_blocked: false,
        }
    }
    pub fn drawn(&mut self, binding: Option<FrameBinding>) {
        self.latest = binding;
        if self.latest.is_none() {
            self.capacity_blocked = false;
        }
    }
    pub fn needs(&self, binding: &FrameBinding) -> bool {
        self.pending.len() < CAP && self.requires_evidence(binding)
    }
    fn requires_evidence(&self, binding: &FrameBinding) -> bool {
        !self
            .proven
            .as_ref()
            .is_some_and(|(_, proven)| proven.same_presentation(binding))
            && !self
                .pending
                .values()
                .any(|entry| entry.successful && entry.binding.same_presentation(binding))
    }
    pub(crate) fn feedback_candidate(&mut self) -> Option<FrameBinding> {
        let binding = self.latest.clone()?;
        if !self.requires_evidence(&binding) {
            self.capacity_blocked = false;
            return None;
        }
        self.capacity_blocked = !self.needs(&binding);
        (!self.capacity_blocked).then(|| binding.captured())
    }
    pub(crate) fn native_capacity_blocked(&mut self) {
        self.capacity_blocked = true;
    }
    pub(crate) fn is_capacity_blocked(&self) -> bool {
        self.capacity_blocked
    }
    pub(crate) fn take_capacity_retry(&mut self, native_available: bool) -> bool {
        if !self.capacity_blocked {
            return false;
        }
        let Some(binding) = &self.latest else {
            self.capacity_blocked = false;
            return false;
        };
        if !self.requires_evidence(binding) {
            self.capacity_blocked = false;
            return false;
        }
        if native_available && self.pending.len() < CAP {
            self.capacity_blocked = false;
            return true;
        }
        false
    }
    pub fn submitted(&mut self, id: u64, binding: FrameBinding, successful: bool) {
        debug_assert!(self.pending.len() < CAP && !self.pending.contains_key(&id));
        let previous = self.pending.insert(
            id,
            Submission {
                binding,
                successful,
            },
        );
        debug_assert!(
            previous.is_none(),
            "checked native request IDs must be unique"
        );
    }
    #[cfg(feature = "native-frame-probe")]
    pub(crate) fn pending_binding(&self, id: u64) -> Option<(&FrameBinding, bool)> {
        self.pending
            .get(&id)
            .map(|entry| (&entry.binding, entry.successful))
    }
    /// Mutate the correlation table before returning its owned observer.
    pub fn resolve(&mut self, id: u64, outcome: FrameOutcome) -> Option<FrameBinding> {
        let entry = self.pending.remove(&id)?;
        if !entry.successful {
            return None;
        }
        if matches!(outcome, FrameOutcome::Presented { .. })
            && self
                .proven
                .as_ref()
                .is_none_or(|(previous, _)| id > *previous)
        {
            self.proven = Some((id, entry.binding.clone()));
        }
        Some(entry.binding)
    }
}

impl Drop for Ledger {
    fn drop(&mut self) {
        if let Some(binding) = self.latest.take() {
            binding.observe(self.window, None, FrameOutcome::Closed);
        }
        for (id, entry) in std::mem::take(&mut self.pending) {
            entry
                .binding
                .observe(self.window, Some(id), FrameOutcome::Closed);
        }
        if let Some((id, binding)) = self.proven.take() {
            binding.observe(self.window, Some(id), FrameOutcome::Closed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::window::presentation::{FrameObserver, FrameStamp};

    #[test]
    fn retained_view_admits_fresh_scope_after_late_old_request() {
        use std::sync::{Arc, Mutex};
        let receipts = Arc::new(Mutex::new(Vec::new()));
        let make = |generation| {
            let receipts = Arc::clone(&receipts);
            FrameObserver::new(move |receipt| receipts.lock().unwrap().push((generation, receipt)))
        };
        let current = Arc::new(Mutex::new(make(1)));
        let provider = Arc::clone(&current);
        let retained = binding(
            1,
            FrameObserver::with_capture(move || provider.lock().unwrap().clone()),
        );
        let window = Id::unique();
        let mut ledger = Ledger::new(window);
        ledger.drawn(Some(retained.clone()));
        let old = ledger.feedback_candidate().unwrap();
        ledger.submitted(1, old, true);
        *current.lock().unwrap() = make(2);
        // No replacement view or stamp is supplied. The pending old callback
        // remains generation 1, while the unchanged view can admit generation 2.
        let old = ledger.resolve(1, presented()).unwrap();
        old.observe(window, Some(1), presented());
        let fresh = ledger.feedback_candidate().unwrap();
        assert_eq!(fresh.stamp, retained.stamp);
        assert!(!fresh.same_presentation(&old));
        ledger.submitted(2, fresh, true);
        let fresh = ledger.resolve(2, presented()).unwrap();
        fresh.observe(window, Some(2), presented());
        assert!(ledger.feedback_candidate().is_none());
        let receipts = receipts.lock().unwrap();
        assert_eq!(receipts[0].0, 1);
        assert_eq!(receipts[1].0, 2);
        assert_eq!(receipts[1].1.request_id, Some(2));
    }

    #[test]
    fn capacity_epoch_scans_initial_wait_and_changes_without_repeated_generation_work() {
        let mut epoch = CapacityEpoch::default();
        assert!(epoch.should_scan(0));
        assert!(!epoch.should_scan(0));
        assert!(epoch.should_scan(1));
        assert!(!epoch.should_scan(1));
        epoch.idle();
        assert!(
            epoch.should_scan(1),
            "a newly blocked runtime gets its initial availability check"
        );
        assert!(epoch.should_scan(u64::MAX));
        assert!(
            epoch.should_scan(u64::MAX),
            "terminal epoch must reconcile each real native wake"
        );
    }

    #[test]
    fn local_capacity_wait_keeps_failed_ids_and_queues_one_retry_after_retirement() {
        let target = binding(9, FrameObserver::new(|_| {}));
        let mut ledger = Ledger::new(Id::unique());
        ledger.drawn(Some(target.clone()));
        for id in 1..=CAP as u64 {
            ledger.submitted(id, target.clone(), false);
        }
        assert!(ledger.feedback_candidate().is_none());
        assert!(ledger.is_capacity_blocked());
        assert!(
            !ledger.take_capacity_retry(true),
            "local entries must actually retire"
        );
        assert!(
            ledger.resolve(1, presented()).is_none(),
            "failed ID cannot prove its eventual native commit"
        );
        assert!(!ledger.take_capacity_retry(false));
        assert!(ledger.take_capacity_retry(true));
        assert!(
            !ledger.take_capacity_retry(true),
            "repeated wakes cannot enqueue another retry"
        );
        let candidate = ledger.feedback_candidate().unwrap();
        assert!(candidate.same_presentation(&target));
        ledger.submitted(9, candidate, true);
        ledger.native_capacity_blocked();
        assert!(
            !ledger.take_capacity_retry(true),
            "successful pending proof suppresses redundant work"
        );
        assert!(!ledger.is_capacity_blocked());
    }

    #[test]
    fn native_capacity_refusal_rearms_after_an_admission_race_and_binding_retirement_clears_it() {
        let mut ledger = Ledger::new(Id::unique());
        ledger.drawn(Some(binding(1, FrameObserver::new(|_| {}))));
        assert!(ledger.feedback_candidate().is_some());
        ledger.native_capacity_blocked();
        assert!(!ledger.take_capacity_retry(false));
        assert!(ledger.take_capacity_retry(true));
        assert!(ledger.feedback_candidate().is_some());
        ledger.native_capacity_blocked();
        assert!(!ledger.take_capacity_retry(false));
        assert!(ledger.is_capacity_blocked());
        ledger.drawn(None);
        assert!(!ledger.is_capacity_blocked());
        assert!(!ledger.take_capacity_retry(true));
    }

    #[cfg(feature = "native-frame-probe")]
    #[test]
    fn probe_reads_the_actual_immutable_pending_binding_and_success_flag() {
        let target = binding(1, FrameObserver::new(|_| {}));
        let mut ledger = Ledger::new(Id::unique());
        ledger.submitted(1, target.clone(), false);
        let (captured, successful) = ledger.pending_binding(1).unwrap();
        assert!(captured.same_presentation(&target));
        assert!(!successful);
        assert!(ledger.resolve(1, presented()).is_none());
        assert!(ledger.pending_binding(1).is_none());
    }

    #[test]
    fn an_unsupported_only_window_still_notifies_its_drawn_observer_on_retirement() {
        let closed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = closed.clone();
        let observer = FrameObserver::new(move |receipt| {
            if receipt.outcome == FrameOutcome::Closed {
                observed.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });
        let window = Id::unique();
        let current = binding(1, observer);
        current.observe(window, None, FrameOutcome::Unsupported);
        let mut ledger = Ledger::new(window);
        ledger.drawn(Some(current));
        drop(ledger);
        assert!(closed.load(std::sync::atomic::Ordering::SeqCst));
    }
    fn binding(epoch: u64, observer: FrameObserver) -> FrameBinding {
        FrameBinding {
            stamp: FrameStamp {
                activation_epoch: epoch,
                local_revision: 0,
            },
            observer,
        }
    }
    fn presented() -> FrameOutcome {
        FrameOutcome::Presented {
            clock_id: Some(1),
            seconds: 2,
            nanoseconds: 3,
            refresh_ns: 4,
            output_sequence: 5,
            flags: 0,
        }
    }
    #[test]
    fn old_or_aborted_feedback_cannot_certify_the_new_interface() {
        let observer = FrameObserver::new(|_| {});
        let old = binding(1, observer.clone());
        let current = binding(2, observer);
        let mut ledger = Ledger::new(Id::unique());
        ledger.submitted(1, old.clone(), true);
        ledger.submitted(2, current.clone(), false);
        assert!(
            ledger
                .resolve(1, presented())
                .unwrap()
                .same_presentation(&old)
        );
        assert!(ledger.needs(&current));
        assert!(ledger.resolve(2, presented()).is_none());
        assert!(ledger.needs(&current));
        ledger.submitted(3, current.clone(), true);
        assert!(
            ledger
                .resolve(3, presented())
                .unwrap()
                .same_presentation(&current)
        );
        assert!(!ledger.needs(&current));
        assert!(ledger.resolve(3, presented()).is_none());
        let replacement = binding(2, FrameObserver::new(|_| {}));
        assert!(
            ledger.needs(&replacement),
            "same stamp with a new owner still needs proof"
        );
    }
    #[test]
    fn late_older_receipt_does_not_regress_proven_identity() {
        let observer = FrameObserver::new(|_| {});
        let old = binding(1, observer.clone());
        let current = binding(2, observer);
        let mut ledger = Ledger::new(Id::unique());
        ledger.submitted(1, old.clone(), true);
        ledger.submitted(2, current.clone(), true);
        let newer = ledger.resolve(2, presented()).unwrap();
        let older = ledger.resolve(1, presented()).unwrap();
        assert!(newer.same_presentation(&current));
        assert!(older.same_presentation(&old));
        assert!(!ledger.needs(&current));
    }
    #[test]
    fn pending_and_aborted_submissions_keep_the_eight_request_bound() {
        let observer = FrameObserver::new(|_| {});
        let mut ledger = Ledger::new(Id::unique());
        for id in 1..=CAP as u64 {
            let next = binding(id, observer.clone());
            assert!(ledger.needs(&next));
            ledger.submitted(id, next, id != 1);
        }
        let next = binding(9, observer);
        assert!(!ledger.needs(&next));
        assert!(ledger.resolve(1, presented()).is_none());
        assert!(
            ledger.needs(&next),
            "only terminal feedback releases aborted correlation capacity"
        );
    }
}
