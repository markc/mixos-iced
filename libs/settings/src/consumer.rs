// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared bounded consumer engine. Hosts execute returned work on their existing
//! worker, feed deliveries/lifecycle/loss, and acknowledge stages on their UI
//! event loop. This engine owns neither a connection nor a renderer runtime.
use crate::{
    domains::ChangePlan,
    reducer::{Decision, Reducer},
    *,
};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

static NEXT_CONSUMER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkKind {
    Subscribe,
    Read,
}

/// At most one current work ticket. Superseded completions are harmless.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Work {
    owner: u64,
    serial: u64,
    generation: u64,
    binding: Binding,
    kind: WorkKind,
}
impl Work {
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn binding(&self) -> &Binding {
        &self.binding
    }
    pub fn kind(&self) -> WorkKind {
        self.kind
    }
}

/// A complete immutable generation to stage before a single UI-thread swap.
/// A stage is not evidence of application or presentation.
#[derive(Clone, Debug)]
pub struct Update {
    owner: u64,
    serial: u64,
    generation: u64,
    snapshot: Arc<Snapshot>,
    changes: ChangePlan,
}
impl Update {
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
    pub fn changes(&self) -> ChangePlan {
        self.changes
    }
}

pub struct Consumer {
    owner: u64,
    binding: Binding,
    context: String,
    shell: bool,
    reducer: Reducer,
    generation: Option<u64>,
    latest_generation: u64,
    serial: u64,
    subscribed: bool,
    confirmed: bool,
    work: Option<Work>,
    read_again: bool,
    buffered: Option<Snapshot>,
    confirming: Option<String>,
    rejected: VecDeque<String>,
    retry_delay: Option<Duration>,
    failures: u8,
    pending: Option<Update>,
    applied: Option<Arc<Snapshot>>,
    fault: Option<Diagnostic>,
}
impl Consumer {
    pub fn for_app(binding: Binding, app: &str) -> Result<Self, Diagnostic> {
        Self::new(binding, &format!("app:{app}"), false)
    }
    pub fn for_shell(binding: Binding) -> Result<Self, Diagnostic> {
        Self::new(binding, "desktop", true)
    }
    pub fn new(binding: Binding, context: &str, shell: bool) -> Result<Self, Diagnostic> {
        binding.validate()?;
        let identifier = context.strip_prefix("app:").or_else(|| (context == "desktop").then_some(context));
        if identifier.is_none_or(|id| id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')) {
            return Err(Diagnostic::new(
                "invalid_context",
                "context",
                "Use a declared context",
            ));
        }
        Ok(Self {
            owner: NEXT_CONSUMER
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                .expect("settings consumer identities exhausted"),
            reducer: Reducer::new(binding.clone()),
            binding,
            context: context.into(),
            shell,
            generation: None,
            latest_generation: 0,
            serial: 0,
            subscribed: false,
            confirmed: false,
            work: None,
            read_again: false,
            buffered: None,
            confirming: None,
            rejected: VecDeque::new(),
            retry_delay: None,
            failures: 0,
            pending: None,
            applied: None,
            fault: None,
        })
    }
    pub fn binding(&self) -> &Binding {
        &self.binding
    }
    pub fn context(&self) -> &str { &self.context }
    pub fn current(&self) -> Option<&Snapshot> {
        self.reducer.current()
    }
    pub fn applied(&self) -> Option<&Snapshot> {
        self.applied.as_deref()
    }
    pub fn pending(&self) -> Option<&Update> {
        self.pending.as_ref()
    }
    pub fn fault(&self) -> Option<&Diagnostic> {
        self.fault.as_ref()
    }
    pub fn retry_delay(&self) -> Option<Duration> {
        self.retry_delay
    }
    pub fn generation(&self) -> Option<u64> {
        self.generation
    }
    fn serial(&mut self) -> u64 {
        self.serial = self
            .serial
            .checked_add(1)
            .expect("settings consumer serial exhausted");
        self.serial
    }
    fn start(&mut self, kind: WorkKind) -> Option<Work> {
        if self.work.is_some() {
            return None;
        }
        let generation = self.generation?;
        self.retry_delay = None;
        let work = Work {
            owner: self.owner,
            serial: self.serial(),
            generation,
            binding: self.binding.clone(),
            kind,
        };
        self.work = Some(work.clone());
        Some(work)
    }
    /// Feed the sampled connection generation, even if a watch coalesced edges.
    pub fn connected(&mut self, generation: u64) -> Option<Work> {
        if generation == 0 || generation <= self.latest_generation {
            return None;
        }
        self.disconnected();
        self.latest_generation = generation;
        self.generation = Some(generation);
        self.start(WorkKind::Subscribe)
    }
    pub fn disconnected(&mut self) {
        self.serial();
        self.reducer.invalidate_work();
        self.generation = None;
        self.work = None;
        self.pending = None;
        self.subscribed = false;
        self.confirmed = false;
        self.read_again = false;
        self.buffered = None;
        self.confirming = None;
        self.rejected.clear();
        self.retry_delay = None;
        self.failures = 0;
        // Last usable applied/current data remains available, labelled offline
        // by the absent connection generation; it is not current read evidence.
    }
    /// Called only at the deadline of this specific failed recovery job.
    /// Success/disconnect removes the deadline; no idle timer exists.
    pub fn retry(&mut self) -> Option<Work> {
        self.retry_delay?;
        self.start(if self.subscribed {
            WorkKind::Read
        } else {
            WorkKind::Subscribe
        })
    }
    pub fn refresh(&mut self) -> Option<Work> {
        if self.generation.is_none() {
            return None;
        }
        if self.work.is_some() {
            self.read_again = true;
            return None;
        }
        self.start(if self.subscribed {
            WorkKind::Read
        } else {
            WorkKind::Subscribe
        })
    }
    /// Any delivered-lane loss invalidates stages and coalesces one full read.
    pub fn lost(&mut self) -> Option<Work> {
        self.pending = None;
        self.confirmed = false;
        self.buffered = None;
        self.reducer.invalidate_work();
        self.refresh()
    }
    fn fail(&mut self, fault: Diagnostic) {
        let terminal = matches!(
            fault.code.as_str(),
            "wrong_target"
                | "unsupported_schema"
                | "invalid_snapshot"
                | "snapshot_too_large"
                | "invalid_completion"
                | "authority_refused"
                | "invalid_read"
        );
        self.fault = Some(fault);
        if terminal {
            self.retry_delay = None;
            return;
        }
        self.failures = self.failures.saturating_add(1);
        let delay = (250 * (1u64 << (self.failures - 1).min(7))).min(30_000);
        self.retry_delay = Some(Duration::from_millis(delay));
    }
    /// Subscribe acknowledgement precedes read. Read completions are confirmed
    /// only for this exact connection/job; an event may already be newer.
    pub fn complete(
        &mut self,
        work: &Work,
        result: Result<Option<Snapshot>, Diagnostic>,
    ) -> Option<Work> {
        if self.work.as_ref() != Some(work) || self.generation != Some(work.generation) {
            return None;
        }
        self.work = None;
        match (work.kind, result) {
            (_, Err(fault)) => {
                self.fail(fault);
                None
            }
            (WorkKind::Subscribe, Ok(None)) => {
                self.subscribed = true;
                self.read_again = false;
                self.start(WorkKind::Read)
            }
            (WorkKind::Read, Ok(Some(snapshot))) => {
                // A loss/confirmation request after this read began requires
                // one later bound read. Do not activate a pre-gap result.
                if self.read_again {
                    self.read_again = false;
                    return self.start(WorkKind::Read);
                }
                if let Err(fault) = self.check(&snapshot) {
                    self.fail(fault);
                    return None;
                }
                let digest = crate::digest(&snapshot).ok();
                let decision = self.reducer.install(snapshot, true, self.reducer.ticket());
                if let Some(candidate) = self.confirming.take()
                    && (digest.as_ref() != Some(&candidate) || decision == Decision::Contradiction)
                {
                    if self.rejected.len() == 16 {
                        self.rejected.pop_front();
                    }
                    self.rejected.push_back(candidate);
                }
                match decision {
                    Decision::Install | Decision::Duplicate | Decision::Stale => {
                        self.confirmed = true;
                        self.fault = None;
                        self.failures = 0;
                        self.retry_delay = None;
                        self.stage();
                    }
                    _ => {
                        // Same-revision contradictions are terminal evidence
                        // faults, not an infinite automatic fresh-read loop.
                        self.fault = Some(Diagnostic::new(
                            "authority_contradiction",
                            "snapshot",
                            format!("{decision:?}"),
                        ));
                        self.pending = None;
                        self.confirmed = false;
                    }
                }
                if self.confirmed
                    && let Some(buffered) = self.buffered.take()
                {
                    return self.observe(work.generation, buffered);
                }
                None
            }
            _ => {
                self.fail(Diagnostic::new(
                    "invalid_completion",
                    "work",
                    "Unexpected result kind",
                ));
                None
            }
        }
    }
    fn check(&self, snapshot: &Snapshot) -> Result<(), Diagnostic> {
        if snapshot.binding != self.binding {
            return Err(Diagnostic::new(
                "wrong_target",
                "binding",
                "Snapshot binding differs",
            ));
        }
        if snapshot.schema != SCHEMA || snapshot.effective.values().any(|e| e.design.schema != 1) {
            return Err(Diagnostic::new(
                "unsupported_schema",
                "schema",
                "Unsupported settings/design schema",
            ));
        }
        if snapshot.incarnation.is_empty()
            || snapshot.incarnation.len() > 128
            || !snapshot.effective.contains_key(&self.context)
        {
            return Err(Diagnostic::new(
                "invalid_snapshot",
                "snapshot",
                "Missing incarnation or declared context",
            ));
        }
        if snapshot
            .encoded_len()
            .map_err(|e| Diagnostic::new("invalid_snapshot", "snapshot", e.to_string()))?
            > MAX_SNAPSHOT_BYTES
        {
            return Err(Diagnostic::new(
                "snapshot_too_large",
                "snapshot",
                "Inline byte budget exceeded",
            ));
        }
        Ok(())
    }
    /// Transport adapters authenticate origin before feeding an inline snapshot.
    /// Old connection deliveries cannot confirm or replace a new connection.
    pub fn observe(&mut self, generation: u64, snapshot: Snapshot) -> Option<Work> {
        if self.generation != Some(generation) {
            return None;
        }
        if let Err(fault) = self.check(&snapshot) {
            self.fault = Some(fault);
            return self.lost();
        }
        let candidate = crate::digest(&snapshot).ok();
        if candidate
            .as_ref()
            .is_some_and(|d| self.rejected.contains(d))
        {
            return None;
        }
        if !self.confirmed {
            // Keep one latest candidate during bootstrap, not one read demand
            // per retained event. The fresh read establishes the history first.
            if self.buffered.as_ref().is_none_or(|old| {
                old.incarnation != snapshot.incarnation || old.revision <= snapshot.revision
            }) {
                self.buffered = Some(snapshot);
            }
            if self.work.is_some() {
                return None;
            }
            return self.refresh();
        }
        match self.reducer.observe(snapshot) {
            Decision::Install => self.stage(),
            Decision::ConfirmAuthority => {
                if candidate.as_ref().is_some_and(|d| {
                    self.rejected.contains(d) || self.confirming.as_ref() == Some(d)
                }) {
                    return None;
                }
                self.confirming = candidate;
                return self.refresh();
            }
            _ => {}
        }
        None
    }
    fn stage(&mut self) {
        let Some(snapshot) = self.reducer.current().cloned() else {
            return;
        };
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.snapshot.as_ref() == &snapshot)
        {
            return;
        }
        let changes = ChangePlan::between(
            self.applied.as_deref(),
            &snapshot,
            &self.context,
            self.shell,
        );
        let snapshot = Arc::new(snapshot);
        if changes.is_empty() {
            // Identical render input requires no UI swap or redraw. Revision
            // evidence may advance, but this never claims a presented frame.
            self.applied = Some(snapshot);
            self.pending = None;
        } else if let Some(generation) = self.generation {
            let serial = self.serial();
            self.pending = Some(Update {
                owner: self.owner,
                serial,
                generation,
                snapshot,
                changes,
            });
        }
    }
    /// Check immediately BEFORE the synchronous UI-thread swap, with no await
    /// between this check, activation and acknowledgement. Fencing a report
    /// after a stale swap cannot undo the renderer mutation.
    pub fn is_current(&self, update: &Update) -> bool {
        update.owner == self.owner
            && self.generation == Some(update.generation)
            && self
                .pending
                .as_ref()
                .is_some_and(|p| p.serial == update.serial)
    }
    /// Invoke after the host atomically activated all staged resources/defaults.
    pub fn acknowledge(&mut self, update: &Update) -> bool {
        if !self.is_current(update) {
            return false;
        }
        self.applied = Some(update.snapshot.clone());
        self.pending = None;
        self.fault = None;
        true
    }
    pub fn failed(&mut self, update: &Update, fault: Diagnostic) -> bool {
        if !self.is_current(update) {
            return false;
        }
        self.pending = None;
        self.fault = Some(fault);
        // Preserve last-good applied data. Resource retry belongs to the host's
        // pending resource job, not a settings heartbeat.
        true
    }
}
