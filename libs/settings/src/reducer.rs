// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pure ordering/fencing reducer, reusable by renderer-specific async adapters.
//! Only a fresh bound read confirms a new incarnation; topic delivery alone
//! cannot roll a consumer back to a retired store or complete superseded work.
use crate::*;

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Install,
    Duplicate,
    Stale,
    ConfirmAuthority,
    WrongTarget,
    Unsupported,
    /// A fresh authority read contradicted an already installed identity.
    /// Preserve current values and report corruption; do not loop fresh reads.
    Contradiction,
}
pub struct Reducer {
    binding: Binding,
    current: Option<Snapshot>,
    ticket: u64,
}
impl Reducer {
    pub fn new(binding: Binding) -> Self {
        Self {
            binding,
            current: None,
            ticket: 0,
        }
    }
    pub fn current(&self) -> Option<&Snapshot> {
        self.current.as_ref()
    }
    pub fn invalidate_work(&mut self) -> u64 {
        self.ticket = self
            .ticket
            .checked_add(1)
            .expect("settings work ticket exhausted");
        self.ticket
    }
    pub fn ticket(&self) -> u64 {
        self.ticket
    }
    /// A freshly received, already validated delivery uses the current ticket.
    /// Asynchronous decode/resolve completions must instead call install with
    /// the ticket captured when that work began.
    pub fn observe(&mut self, incoming: Snapshot) -> Decision {
        self.install(incoming, false, self.ticket)
    }
    pub fn examine(&self, incoming: &Snapshot, confirmed: bool) -> Decision {
        if incoming.binding != self.binding {
            return Decision::WrongTarget;
        }
        if incoming.schema != SCHEMA || incoming.effective.values().any(|e| e.design.schema != 1) {
            return Decision::Unsupported;
        }
        match &self.current {
            None if !confirmed => Decision::ConfirmAuthority,
            Some(current) if current.incarnation != incoming.incarnation && !confirmed => {
                Decision::ConfirmAuthority
            }
            Some(current)
                if current.incarnation == incoming.incarnation
                    && incoming.revision < current.revision =>
            {
                Decision::Stale
            }
            Some(current)
                if current.incarnation == incoming.incarnation
                    && incoming.revision == current.revision =>
            {
                if current == incoming {
                    Decision::Duplicate
                } else if confirmed {
                    Decision::Contradiction
                } else {
                    Decision::ConfirmAuthority
                }
            }
            _ => Decision::Install,
        }
    }
    pub fn install(&mut self, incoming: Snapshot, confirmed: bool, ticket: u64) -> Decision {
        if ticket != self.ticket {
            return Decision::Stale;
        }
        let decision = self.examine(&incoming, confirmed);
        if decision == Decision::Install {
            self.current = Some(incoming);
        }
        decision
    }
    pub fn effective_changed(&self, incoming: &Snapshot, context: &str) -> bool {
        let current = self.current.as_ref();
        current.and_then(|s| s.effective.get(context)) != incoming.effective.get(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(rev: u64, incarnation: &str) -> Snapshot {
        Snapshot {
            schema: SCHEMA,
            binding: Binding {
                instance: "test".into(),
                profile: "default".into(),
            },
            incarnation: incarnation.into(),
            revision: Revision(rev),
            design_revision: Revision(1),
            source_digest: "source".into(),
            desktop: Desktop::default(),
            effective: Default::default(),
        }
    }
    #[test]
    fn confirmed_contradiction_is_reported_without_installing_or_read_loop() {
        let mut state = Reducer::new(snapshot(1, "a").binding);
        state.install(snapshot(1, "a"), true, 0);
        let mut changed = snapshot(1, "a");
        changed.source_digest = "changed".into();
        assert_eq!(
            state.install(changed.clone(), false, 0),
            Decision::ConfirmAuthority
        );
        assert_eq!(state.install(changed, true, 0), Decision::Contradiction);
        assert_eq!(state.current().unwrap().source_digest, "source");
        let mut wrong = snapshot(2, "a");
        wrong.binding.instance = "other".into();
        assert_eq!(state.install(wrong, true, 0), Decision::WrongTarget);
        let mut future = snapshot(2, "a");
        future.schema = SCHEMA + 1;
        assert_eq!(state.install(future, true, 0), Decision::Unsupported);
    }
    #[test]
    fn new_delivery_during_pending_read_wins_and_old_async_work_stays_fenced() {
        let mut state = Reducer::new(snapshot(1, "a").binding);
        state.install(snapshot(4, "a"), true, 0);
        let ticket = state.invalidate_work();
        assert_eq!(state.observe(snapshot(6, "a")), Decision::Install);
        assert_eq!(
            state.install(snapshot(5, "a"), true, ticket),
            Decision::Stale
        );
        assert_eq!(state.install(snapshot(7, "a"), false, 0), Decision::Stale);
        assert_eq!(state.current().unwrap().revision, Revision(6));
    }
    #[test]
    fn queued_incarnations_and_superseded_completion_cannot_replace_current() {
        let mut state = Reducer::new(snapshot(1, "a").binding);
        assert_eq!(state.install(snapshot(4, "a"), true, 0), Decision::Install);
        assert_eq!(state.install(snapshot(3, "a"), false, 0), Decision::Stale);
        assert_eq!(
            state.install(snapshot(1, "b"), false, 0),
            Decision::ConfirmAuthority
        );
        let ticket = state.invalidate_work();
        assert_eq!(
            state.install(snapshot(1, "b"), true, ticket),
            Decision::Install
        );
        assert_eq!(
            state.install(snapshot(5, "a"), false, ticket),
            Decision::ConfirmAuthority
        );
        assert_eq!(state.install(snapshot(2, "b"), true, 0), Decision::Stale);
        assert_eq!(state.current().unwrap().incarnation, "b");
    }
}
