// SPDX-License-Identifier: MIT OR Apache-2.0
//! Admission and replies retained through task completion and reaping.
//!
//! Hosts keep their existing runtime, native client, select loop and shutdown
//! deadline. These helpers spawn only on that runtime and interpret no verbs.
use crate::native_queue::{Outbox, Permit, SendError};
use bus::native_client::{ConnState, IncomingCommand, SupervisedClient};
use std::{future::Future, sync::Arc, time::Instant};
use tokio::task::{JoinError, JoinSet};

/// Credit remains owned by this output until the host reaps and records it.
pub struct Completed<T> {
    pub permit: Permit,
    pub value: T,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_queue::Admission;
    use std::{cell::Cell, time::Duration};

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
        assert!(
            tasks
                .try_spawn_with(admission.try_acquire().unwrap(), |permit| (permit, async {
                    panic!("owned task failure");
                }))
                .is_ok()
        );
        // Tokio polls the spawned task before this task resumes. The panic
        // releases admission, but its unreaped JoinError still owns a slot.
        tokio::task::yield_now().await;
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
