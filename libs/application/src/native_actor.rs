// SPDX-License-Identifier: MIT OR Apache-2.0
//! Admission and replies retained through task completion and reaping.
//!
//! Hosts keep their existing runtime, native client, select loop and shutdown
//! deadline. These helpers spawn only on that runtime and interpret no verbs.
use crate::native_queue::{Admission, Outbox, Permit, SendError};
use bus::native_client::{ConnState, IncomingCommand, SupervisedClient};
use std::{future::Future, sync::Arc, time::Instant};
use tokio::task::{JoinError, JoinSet};

/// Bounded lifetime diagnostics shared by native actors. UTF-8 truncation
/// retains valid text; the total counter saturates independently of retention.
#[derive(Default, serde::Serialize)]
pub struct Faults {
    count: u64,
    recent: std::collections::VecDeque<String>,
}

impl Faults {
    pub fn count(&self) -> u64 {
        self.count
    }
    pub fn recent(&self) -> &std::collections::VecDeque<String> {
        &self.recent
    }
    pub fn push(&mut self, mut error: String) {
        if error.len() > 4096 {
            let mut end = 4096;
            while !error.is_char_boundary(end) {
                end -= 1;
            }
            error.truncate(end);
        }
        self.count = self.count.saturating_add(1);
        if self.recent.len() == 32 {
            self.recent.pop_front();
        }
        self.recent.push_back(error);
    }
}

/// Record a reaped task's actual outcome before releasing its admission credit.
pub fn reap<T>(
    label: &str,
    result: Result<Completed<T>, JoinError>,
    faults: &mut Faults,
    record: impl FnOnce(T, &mut Faults),
) {
    match result {
        Ok(Completed { permit, value }) => {
            record(value, faults);
            permit.finish();
        }
        Err(error) => faults.push(format!("{label}: {error}")),
    }
}

/// Request cancellation once and record ready outcomes separately from work
/// whose destruction has not yet been observed. This never extends a deadline.
pub fn cancel<T: Send + 'static>(
    label: &str,
    tasks: TaskSet<T>,
    faults: &mut Faults,
    mut record: impl FnMut(T, &mut Faults),
) {
    let report = tasks.abort_and_report();
    for result in report.ready {
        reap(label, result, faults, |value, faults| record(value, faults));
    }
    if report.unconfirmed > 0 {
        faults.push(format!(
            "{label}: cancellation requested with {} unconfirmed tasks",
            report.unconfirmed
        ));
    }
}

/// Credit remains owned by this output until the host reaps and records it.
pub struct Completed<T> {
    pub permit: Permit,
    pub value: T,
}

/// Immediately available outcomes after cancellation, plus work whose
/// destruction has not been observed. Abort does not prove completion.
pub struct AbortReport<T> {
    pub ready: Vec<Result<Completed<T>, JoinError>>,
    pub unconfirmed: usize,
}

/// A finite set includes running and completed but unreaped tasks.
pub struct TaskSet<T> {
    capacity: usize,
    tasks: JoinSet<Completed<T>>,
}

impl<T: Send + 'static> TaskSet<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            tasks: JoinSet::new(),
        }
    }
    pub fn len(&self) -> usize {
        self.tasks.len()
    }
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
    pub fn is_full(&self) -> bool {
        self.len() >= self.capacity
    }
    /// Check capacity before invoking the factory. Full returns the untouched
    /// retained item; no readiness or cancellation future can consume it.
    pub fn try_spawn_with<I, F>(
        &mut self,
        item: I,
        make: impl FnOnce(I) -> (Permit, F),
    ) -> Result<(), I>
    where
        F: Future<Output = T> + Send + 'static,
    {
        if self.is_full() {
            return Err(item);
        }
        let (permit, future) = make(item);
        self.tasks.spawn(async move {
            Completed {
                permit,
                value: future.await,
            }
        });
        Ok(())
    }
    pub async fn join_next(&mut self) -> Option<Result<Completed<T>, JoinError>> {
        self.tasks.join_next().await
    }
    pub fn try_join_next(&mut self) -> Option<Result<Completed<T>, JoinError>> {
        self.tasks.try_join_next()
    }
    pub fn abort_all(&mut self) {
        self.tasks.abort_all();
    }
    /// Freeze admission and request cancellation without awaiting beyond the
    /// host's deadline. Dropping the set requests abort for unreaped work;
    /// the owning runtime still controls when those futures are destroyed.
    pub fn abort_and_report(mut self) -> AbortReport<T> {
        self.tasks.abort_all();
        let mut ready = Vec::with_capacity(self.tasks.len());
        while let Some(result) = self.tasks.try_join_next() {
            ready.push(result);
        }
        AbortReport {
            ready,
            unconfirmed: self.tasks.len(),
        }
    }
}

/// One accepted command always retains its receiving supervisor and credit.
pub struct Accepted {
    client: Arc<SupervisedClient>,
    command: IncomingCommand,
    permit: Permit,
    admitted_at: Instant,
}

impl Accepted {
    pub fn new(
        client: Arc<SupervisedClient>,
        command: IncomingCommand,
        permit: Permit,
        admitted_at: Instant,
    ) -> Self {
        Self {
            client,
            command,
            permit,
            admitted_at,
        }
    }
    pub fn command(&self) -> &IncomingCommand {
        &self.command
    }
    pub fn admitted_at(&self) -> Instant {
        self.admitted_at
    }
    /// Build an owner-specific operation without exposing or replacing its
    /// origin. The task set retains the returned credit through reaping.
    pub fn into_task<F>(
        self,
        make: impl FnOnce(Arc<SupervisedClient>, IncomingCommand, Instant) -> F,
    ) -> (Permit, F) {
        let Self {
            client,
            command,
            permit,
            admitted_at,
        } = self;
        (permit, make(client, command, admitted_at))
    }
    /// Generation alone cannot identify distinct supervisor incarnations.
    pub fn is_current(&self, current: &Arc<SupervisedClient>) -> bool {
        if !Arc::ptr_eq(&self.client, current) {
            return false;
        }
        let lifecycle = current.subscribe_state();
        let state = lifecycle.borrow();
        *state == ConnState::Connected && current.connection_generation() == self.command.generation
    }
    pub fn reply(self, rc: u8, body: String, deadline: Instant) -> Reply {
        Reply {
            accepted: Box::new(self),
            rc,
            body,
            deadline,
        }
    }
    /// The host records a terminal retirement before explicitly finishing it.
    pub fn retire(self) -> Permit {
        self.permit
    }
}

/// A reply keeps its origin and absolute deadline during retained queue delay.
pub struct Reply {
    accepted: Box<Accepted>,
    rc: u8,
    body: String,
    deadline: Instant,
}

impl Reply {
    /// Retire an unsent retained reply during the host's bounded shutdown.
    pub fn retire(self) -> Permit {
        self.accepted.retire()
    }
    pub fn into_task(
        self,
    ) -> (
        Permit,
        impl Future<Output = Result<(), String>> + Send + 'static,
    ) {
        let Self {
            accepted,
            rc,
            body,
            deadline,
        } = self;
        let Accepted {
            client,
            command,
            permit,
            ..
        } = *accepted;
        (permit, async move {
            let deadline = tokio::time::Instant::from_std(deadline);
            if tokio::time::Instant::now() >= deadline {
                return Err("Bus reply timed out".to_owned());
            }
            tokio::time::timeout_at(
                deadline,
                SupervisedClient::respond(&client, &command, rc, &body),
            )
            .await
            .map_err(|_| "Bus reply timed out".to_owned())?
            .map_err(|error| format!("Bus reply: {error}"))
        })
    }
}

/// Preserve every accepted reply at its original position when tasks are full.
pub fn submit_replies(retained: &mut Outbox<Reply, 0>, tasks: &mut TaskSet<Result<(), String>>) {
    retained.flush_with(|reply| {
        tasks
            .try_spawn_with(reply, Reply::into_task)
            .map_err(SendError::Full)
    });
}

/// A small refusal lane on the host's existing runtime. Running and completed
/// replies retain their original supervisor, command and credit through reap.
/// Saturation returns the untouched command; it never starts overflow work.
pub struct Refusals {
    admission: Admission,
    tasks: TaskSet<Result<(), String>>,
}
impl Refusals {
    pub fn new(capacity: usize) -> Self {
        Self {
            admission: Admission::new(capacity),
            tasks: TaskSet::new(capacity),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
    pub fn counts(&self) -> crate::native_queue::Counts {
        self.admission.counts()
    }
    pub fn try_reply(
        &mut self,
        client: Arc<SupervisedClient>,
        command: IncomingCommand,
        rc: u8,
        body: String,
        deadline: Instant,
    ) -> Result<(), IncomingCommand> {
        if command.id.is_none() {
            return Ok(());
        }
        if self.tasks.is_full() {
            return Err(command);
        }
        let Some(permit) = self.admission.try_acquire() else {
            return Err(command);
        };
        let reply =
            Accepted::new(client, command, permit, Instant::now()).reply(rc, body, deadline);
        // Exclusive access to this set keeps the capacity check valid until
        // submission. No await or factory runs between these operations.
        match self.tasks.try_spawn_with(reply, Reply::into_task) {
            Ok(()) => Ok(()),
            Err(reply) => {
                let Accepted {
                    command, permit, ..
                } = *reply.accepted;
                permit.finish();
                Err(command)
            }
        }
    }
    pub async fn join_next(&mut self) -> Option<Result<Completed<Result<(), String>>, JoinError>> {
        self.tasks.join_next().await
    }
    pub fn record(result: Result<Completed<Result<(), String>>, JoinError>, faults: &mut Faults) {
        reap("native refusal", result, faults, |value, faults| {
            if let Err(error) = value {
                faults.push(error);
            }
        });
    }
    /// Drain only within the host's existing absolute shutdown deadline.
    /// Cancellation reports unconfirmed work separately from sent replies.
    pub async fn drain(mut self, deadline: Instant, faults: &mut Faults) {
        while !self.tasks.is_empty() {
            match tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.tasks.join_next(),
            )
            .await
            {
                Ok(Some(result)) => Self::record(result, faults),
                Ok(None) => break,
                Err(_) => {
                    faults.push("native refusal drain timed out".into());
                    break;
                }
            }
        }
        cancel("native refusal", self.tasks, faults, |value, faults| {
            if let Err(error) = value {
                faults.push(error);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_queue::Admission;
    use std::{cell::Cell, time::Duration};

    #[test]
    fn diagnostics_bound_utf8_storage_and_saturate_the_lifetime_count() {
        let mut faults = Faults {
            count: u64::MAX - 1,
            recent: Default::default(),
        };
        for _ in 0..40 {
            faults.push("€".repeat(2000));
        }
        assert_eq!(faults.count(), u64::MAX);
        assert_eq!(faults.recent().len(), 32);
        assert!(
            faults
                .recent()
                .iter()
                .all(|text| text.len() == 4095 && text.chars().all(|character| character == '€'))
        );
        faults.push("latest failure".into());
        assert_eq!(faults.recent().back().unwrap(), "latest failure");
        assert_eq!(faults.recent().len(), 32);
    }

    #[tokio::test]
    async fn synchronous_abort_reports_unconfirmed_work_before_destruction() {
        let admission = Admission::new(1);
        let mut tasks = TaskSet::<()>::new(1);
        let (started, entered) = tokio::sync::oneshot::channel();
        let (mut release, waiting) = tokio::sync::oneshot::channel::<()>();
        assert!(
            tasks
                .try_spawn_with(admission.try_acquire().unwrap(), |permit| (
                    permit,
                    async move {
                        started.send(()).unwrap();
                        let _ = waiting.await;
                    }
                ))
                .is_ok()
        );
        entered.await.unwrap();
        let report = tasks.abort_and_report();
        assert!(report.ready.is_empty());
        assert_eq!(report.unconfirmed, 1);
        assert_eq!(admission.counts().active, 1);
        assert_eq!(admission.counts().abandoned, 0);
        release.closed().await;
        assert_eq!(admission.counts().active, 0);
        assert_eq!(admission.counts().abandoned, 1);
        assert_eq!(admission.counts().finished, 0);
        drop(release);
    }

    #[tokio::test]
    async fn finished_tasks_retain_admission_and_capacity_until_reaped() {
        let admission = Admission::new(1);
        let permit = admission.try_acquire().unwrap();
        let (done, observed) = tokio::sync::oneshot::channel();
        let mut tasks = TaskSet::new(1);
        assert!(
            tasks
                .try_spawn_with(permit, |permit| (permit, async move {
                    done.send(()).unwrap();
                    7
                }))
                .is_ok()
        );
        observed.await.unwrap();
        assert!(tasks.is_full());
        assert!(admission.try_acquire().is_none());
        let completed = tasks.join_next().await.unwrap().unwrap();
        assert_eq!(completed.value, 7);
        assert!(tasks.is_empty());
        assert!(admission.try_acquire().is_none());
        completed.permit.finish();
        assert_eq!(admission.counts().finished, 1);
        assert_eq!(admission.counts().active, 0);
    }

    #[tokio::test]
    async fn full_returns_the_original_item_without_calling_its_factory() {
        let mut tasks = TaskSet::<()>::new(0);
        let invoked = Cell::new(false);
        let item = Box::new(31);
        let original = &*item as *const i32;
        let returned = tasks
            .try_spawn_with(item, |_| {
                invoked.set(true);
                (Admission::new(1).try_acquire().unwrap(), async {})
            })
            .unwrap_err();
        assert_eq!(&*returned as *const i32, original);
        assert!(!invoked.get());
        assert!(tasks.is_empty());
    }

    #[tokio::test]
    async fn cancelled_readiness_preserves_work_and_abort_records_abandonment() {
        let admission = Admission::new(1);
        let mut tasks = TaskSet::new(1);
        let (release, waiting) = tokio::sync::oneshot::channel::<()>();
        assert!(
            tasks
                .try_spawn_with(admission.try_acquire().unwrap(), |permit| (
                    permit,
                    async move {
                        let _ = waiting.await;
                    }
                ))
                .is_ok()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(1), tasks.join_next())
                .await
                .is_err()
        );
        assert!(tasks.is_full());
        assert_eq!(admission.counts().active, 1);
        tasks.abort_all();
        assert!(matches!(tasks.join_next().await, Some(Err(error)) if error.is_cancelled()));
        drop(release);
        assert_eq!(admission.counts().active, 0);
        assert_eq!(admission.counts().abandoned, 1);
        assert_eq!(admission.counts().finished, 0);
    }

    #[tokio::test]
    async fn a_panicking_task_is_abandoned_and_never_reported_finished() {
        let admission = Admission::new(1);
        let mut tasks = TaskSet::<()>::new(1);
        let (ended, destroyed) = tokio::sync::oneshot::channel::<()>();
        assert!(
            tasks
                .try_spawn_with(admission.try_acquire().unwrap(), |permit| (
                    permit,
                    async move {
                        let _ended = ended;
                        panic!("owned task failure");
                    }
                ))
                .is_ok()
        );
        // The real panic destroys its captured sender. On this current-thread
        // runtime the wrapper's unwind completes before this task resumes.
        assert!(destroyed.await.is_err());
        assert!(tasks.is_full());
        assert_eq!(admission.counts().abandoned, 1);
        assert_eq!(admission.counts().finished, 0);
        assert!(matches!(tasks.join_next().await, Some(Err(error)) if error.is_panic()));
        assert!(tasks.is_empty());
        assert_eq!(admission.counts().active, 0);
        assert_eq!(admission.counts().abandoned, 1);
        assert_eq!(admission.counts().finished, 0);
    }
}
