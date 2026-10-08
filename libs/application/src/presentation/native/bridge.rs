// SPDX-License-Identifier: MIT OR Apache-2.0
//! Paired endpoints on the host's existing UI and worker, with no own runtime.
use super::{
    ChangePlan, Diagnostic, Event, Jobs, Mailbox, PreparationEvidence, PreparationRevision,
    Presentation, Session, Worker,
};
use bus::native_client::IncomingCommand;
use std::sync::Arc;
#[cfg(feature = "settings-cache")]
use std::time::Instant;
use tokio::sync::watch;
#[path = "observation.rs"]
mod observation;

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
    let (observations, observed) = watch::channel(serde_json::Value::Null);
    let (frame_source, frame_sources) = watch::channel(None);
    let mut observation = observation::Publisher::new(observed, frame_sources);
    if let Some(client) = worker.client.as_ref() {
        observation.connect(Arc::clone(client));
    }
    (
        Ui {
            session,
            jobs,
            mailbox: mailbox.clone(),
            observations,
            frame_source,
        },
        Lane {
            worker,
            jobs: receiver,
            mailbox,
            binding,
            jobs_open: true,
            observation,
        },
    )
}

/// Owned by the UI. Mutation always publishes the resulting desired jobs.
pub struct Ui<T, C = ()> {
    session: Session<T, C>,
    jobs: watch::Sender<Option<Jobs<C>>>,
    mailbox: Mailbox<T>,
    observations: watch::Sender<serde_json::Value>,
    frame_source: watch::Sender<Option<crate::frames::Handle>>,
}
impl<T, C> Ui<T, C> {
    /// Bind the actual existing window owner; creates no frame request.
    pub fn bind_frames(&mut self, frames: crate::frames::Handle) {
        self.frame_source.send_replace(Some(frames));
        self.refresh_observation();
    }

    fn refresh_observation(&self) {
        let stamp = self.session.frame_stamp().map(|stamp| serde_json::json!({"activation_epoch":stamp.activation_epoch,"local_revision":stamp.local_revision}));
        let next = serde_json::json!({"contract":"application.presentation.v1", "pid":std::process::id(),
            "settings":self.session.host().consumer().evidence(),
            "settings_observation":self.session.host().consumer().observations(),
            "installed_frame_stamp":stamp});
        self.observations.send_if_modified(|current| {
            if *current == next {
                false
            } else {
                *current = next;
                true
            }
        });
    }
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
        self.refresh_observation();
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

    pub fn set_context(
        &mut self,
        next: C,
        live: Option<u64>,
    ) -> Result<PreparationRevision, Diagnostic>
    where
        C: PartialEq,
    {
        let (revision, jobs) = self.session.set_context(next, live)?;
        self.refresh_observation();
        self.jobs.send_replace(Some(jobs));
        Ok(revision)
    }

    pub fn retry_preparation(
        &mut self,
        live: Option<u64>,
    ) -> Result<PreparationRevision, Diagnostic> {
        let (revision, jobs) = self.session.retry_preparation(live)?;
        self.refresh_observation();
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
    observation: observation::Publisher,
}
impl<T: Send + 'static, C: Send + Sync + 'static> Lane<T, C> {
    fn replace_latest(&mut self) {
        let jobs = { self.jobs.borrow_and_update().clone() };
        if let Some(jobs) = jobs {
            self.worker.replace(jobs);
        }
    }
    pub fn connect(&mut self, client: Arc<settings::native::Client>) -> bool {
        self.observation.connect(Arc::clone(&client));
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
            () = self.observation.drive() => Progress::Updated,
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
