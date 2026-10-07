// SPDX-License-Identifier: MIT
//! Per-window correlation of drawn interfaces and native commit observations.

use std::collections::BTreeMap;
use crate::core::window::{Id, presentation::{FrameBinding, FrameOutcome}};

const CAP: usize = 8;

struct Submission {
    binding: FrameBinding,
    successful: bool,
}

pub(crate) struct Ledger {
    window: Id,
    pending: BTreeMap<u64, Submission>,
    proven: Option<(u64, FrameBinding)>,
}

impl Ledger {
    pub fn new(window: Id) -> Self { Self {window, pending: BTreeMap::new(), proven: None} }
    pub fn needs(&self, binding: &FrameBinding) -> bool {
        self.pending.len() < CAP
            && !self.proven.as_ref().is_some_and(|(_, proven)| proven.same_presentation(binding))
            && !self.pending.values().any(|entry| entry.successful && entry.binding.same_presentation(binding))
    }
    pub fn submitted(&mut self, id: u64, binding: FrameBinding, successful: bool) {
        debug_assert!(self.pending.len() < CAP && !self.pending.contains_key(&id));
        self.pending.insert(id, Submission {binding, successful});
    }
    /// Mutate the correlation table before returning its owned observer.
    pub fn resolve(&mut self, id: u64, outcome: FrameOutcome) -> Option<FrameBinding> {
        let entry = self.pending.remove(&id)?;
        if !entry.successful { return None; }
        if matches!(outcome, FrameOutcome::Presented {..})
            && self.proven.as_ref().is_none_or(|(previous, _)| id > *previous) {
            self.proven = Some((id, entry.binding.clone()));
        }
        Some(entry.binding)
    }
}

impl Drop for Ledger {
    fn drop(&mut self) {
        for (id, entry) in std::mem::take(&mut self.pending) {
            entry.binding.observe(self.window, Some(id), FrameOutcome::Closed);
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
    fn binding(epoch: u64, observer: FrameObserver) -> FrameBinding {
        FrameBinding {stamp:FrameStamp {activation_epoch:epoch, local_revision:0}, observer}
    }
    fn presented() -> FrameOutcome {
        FrameOutcome::Presented {clock_id:Some(1),seconds:2,nanoseconds:3,refresh_ns:4,output_sequence:5,flags:0}
    }
    #[test]
    fn old_or_aborted_feedback_cannot_certify_the_new_interface() {
        let observer = FrameObserver::new(|_| {});
        let old = binding(1, observer.clone());
        let current = binding(2, observer);
        let mut ledger = Ledger::new(Id::unique());
        ledger.submitted(1, old.clone(), true);
        ledger.submitted(2, current.clone(), false);
        assert!(ledger.resolve(1, presented()).unwrap().same_presentation(&old));
        assert!(ledger.needs(&current));
        assert!(ledger.resolve(2, presented()).is_none());
        assert!(ledger.needs(&current));
        ledger.submitted(3, current.clone(), true);
        assert!(ledger.resolve(3, presented()).unwrap().same_presentation(&current));
        assert!(!ledger.needs(&current));
        assert!(ledger.resolve(3, presented()).is_none());
        let replacement = binding(2, FrameObserver::new(|_| {}));
        assert!(ledger.needs(&replacement), "same stamp with a new owner still needs proof");
    }
    #[test]
    fn late_older_receipt_does_not_regress_proven_identity() {
        let observer = FrameObserver::new(|_| {});
        let old = binding(1, observer.clone());
        let current = binding(2, observer);
        let mut ledger = Ledger::new(Id::unique());
        ledger.submitted(1, old, true);
        ledger.submitted(2, current.clone(), true);
        ledger.resolve(2, presented()).unwrap();
        ledger.resolve(1, presented()).unwrap();
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
        assert!(ledger.needs(&next), "only terminal feedback releases aborted correlation capacity");
    }
}
