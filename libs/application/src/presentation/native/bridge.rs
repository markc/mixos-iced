// SPDX-License-Identifier: MIT OR Apache-2.0
//! Paired endpoints on the host's existing UI and worker, with no own runtime.
use super::{ChangePlan, Diagnostic, Event, Jobs, Mailbox, PreparationEvidence, PreparationRevision, Presentation, Session, Worker};
use bus::native_client::IncomingCommand;
use std::sync::Arc;
#[cfg(feature = "settings-cache")]
use std::time::Instant;
use tokio::sync::watch;

/// Pair existing authorities. Construction starts no connection, task or I/O.
pub fn bridge<T, C>(session: Session<T, C>, worker: Worker<T, C>) -> (Ui<T, C>, Lane<T, C>) {
    #[cfg(feature = "settings-cache")]
    let session = {
        let mut session = session;
        if worker.cache.is_none() {
            session.cache_configuration = Some(Diagnostic::new(
                "cache_unconfigured",
                "cache",
                "The host supplied no persistent cache root",
            ));
        }
        session
    };
    let binding = session.host().consumer().binding().clone();
    let (jobs, receiver) = watch::channel::<Option<Jobs<C>>>(None);
    let mailbox = Mailbox::default();
    (
        Ui {
            session,
            jobs,
            mailbox: mailbox.clone(),
        },
        Lane {
            worker,
            jobs: receiver,
            mailbox,
            binding,
            jobs_open: true,
        },
    )
}

/// Owned by the UI. Mutation always publishes the resulting desired jobs.
pub struct Ui<T, C = ()> {
    session: Session<T, C>,
    jobs: watch::Sender<Option<Jobs<C>>>,
    mailbox: Mailbox<T>,
}
impl<T, C> Ui<T, C> {
    pub fn session(&self) -> &Session<T, C> {
        &self.session
    }

    pub fn handle_with(
        &mut self,
        event: Event<T>,
        live: Option<u64>,
        activate: impl FnMut(&Presentation<T>),
    ) -> Option<ChangePlan> {
        let (change, jobs) = self.session.handle_with(event, live, activate);
        self.jobs.send_replace(Some(jobs));
        change
    }

    /// Release the mailbox lock before callbacks; resample live for each event.
    pub fn drain_with(
        &mut self,
        mut live: impl FnMut() -> Option<u64>,
        mut activate: impl FnMut(&Presentation<T>),
    ) -> Vec<ChangePlan> {
        let events = self.mailbox.take();
        let mut changes = Vec::new();
        for event in events {
            if let Some(change) = self.handle_with(event, live(), &mut activate) {
                changes.push(change);
            }
        }
        changes
    }

    /// Explicit live-state reconciliation at startup and before evidence reads.
    pub fn reconcile(&mut self, live: Option<u64>) {
        self.handle_with(Event::Wake, live, |_| {});
    }

    pub fn set_context(&mut self, next: C, live: Option<u64>) -> Result<PreparationRevision, Diagnostic>
    where C: PartialEq {
        let (revision, jobs) = self.session.set_context(next, live)?;
        self.jobs.send_replace(Some(jobs));
        Ok(revision)
    }

    pub fn retry_preparation(&mut self, live: Option<u64>) -> Result<PreparationRevision, Diagnostic> {
        let (revision, jobs) = self.session.retry_preparation(live)?;
        self.jobs.send_replace(Some(jobs));
        Ok(revision)
    }

    pub fn preparation_evidence(&self) -> PreparationEvidence<'_> {
        self.session.preparation_evidence()
    }
}

/// A host must deliver an eventual UI wake on `Wake` or a true publish result.
#[derive(Debug, PartialEq, Eq)]
pub enum Progress {
    Updated,
    Wake,
    UiClosed,
}

/// Owned by the host's existing worker; no connection or receiver escapes.
pub struct Lane<T, C = ()> {
    worker: Worker<T, C>,
    jobs: watch::Receiver<Option<Jobs<C>>>,
    mailbox: Mailbox<T>,
    binding: settings::Binding,
    jobs_open: bool,
}
impl<T: Send + 'static, C: Send + Sync + 'static> Lane<T, C> {
    fn replace_latest(&mut self) {
        let jobs = { self.jobs.borrow_and_update().clone() };
        if let Some(jobs) = jobs {
            self.worker.replace(jobs);
        }
    }
    pub fn connect(&mut self, client: Arc<settings::native::Client>) -> bool {
        self.worker.connect(client);
        self.replace_latest();
        self.publish(Event::Wake)
    }
    pub fn publish(&self, event: Event<T>) -> bool {
        self.mailbox.publish(event)
    }

    /// `None` is ordinary host traffic; `Some` is a recognised settings frame.
    pub fn delivery(&self, command: &IncomingCommand) -> Option<bool> {
        settings::native::Decoded::from_command(&self.binding, command)
            .map(|decoded| self.publish(Event::Delivery(decoded)))
    }

    /// Cancellation leaves work in Worker. No await separates consumption and
    /// publication of its completion. A closed UI watch is reported only once.
    pub async fn drive(&mut self) -> Progress {
        tokio::select! {
            changed = self.jobs.changed(), if self.jobs_open => {
                if changed.is_err() {
                    self.jobs_open = false;
                    return Progress::UiClosed;
                }
                self.replace_latest();
                Progress::Updated
            }
            event = self.worker.next() => {
                if event.take().is_some_and(|event| self.publish(event)) {
                    Progress::Wake
                } else {
                    Progress::Updated
                }
            }
        }
    }

    /// Hosts freeze UI activation before draining. Consume the final watch
    /// value even when shutdown won select before its changed notification.
    #[cfg(feature = "settings-cache")]
    pub async fn flush_cache(&mut self, deadline: Instant) -> Result<(), Diagnostic> {
        self.replace_latest();
        self.worker.flush_cache(deadline).await
    }
}
