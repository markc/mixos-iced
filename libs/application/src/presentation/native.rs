// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared UI scheduling and jobs multiplexed by an existing host worker. No
//! connection, runtime, receiver or loop is created here.
use super::*;
use crate::message::Once;
use settings::{
    Snapshot,
    consumer::Work,
    native::{BOOTSTRAP_BUDGET, Decoded},
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
mod bridge;
#[cfg(feature = "settings-cache")]
mod cache;
mod mailbox;
pub use bridge::{Lane, Progress, Ui, bridge};
pub use mailbox::Mailbox;
#[cfg(test)]
mod tests;

/// Prepared fallback evidence and the whole host presentation, or diagnostics.
pub type FallbackResult<T> =
    Result<(settings::fallback::Prepared, Presentation<T>), Vec<Diagnostic>>;

pub enum Event<T> {
    Wake,
    Refresh,
    Lost,
    Delivery(Decoded),
    Rpc(Work, Result<Option<Snapshot>, Diagnostic>),
    Prepared(Completion<T>),
    Fallback(settings::fallback::Request, Box<FallbackResult<T>>),
    #[cfg(feature = "settings-cache")]
    Saved(
        settings::cache::Save,
        Result<settings::cache::WriteOutcome, Diagnostic>,
    ),
    /// Explicit retry of the latest activated cache capture; no retry timer.
    #[cfg(feature = "settings-cache")]
    RetryCache,
}

/// One desired job set, sent through the host's existing worker command lane.
/// Replacing it cancels obsolete RPCs/timers and coalesces resource requests.
#[derive(Clone)]
pub struct Jobs {
    work: Option<Work>,
    deadline: Instant,
    wake: Option<Instant>,
    prepare: Option<Request>,
    fallback: Option<settings::fallback::Request>,
    valid: Option<settings::consumer::Update>,
    valid_fallback: Option<settings::fallback::Request>,
    #[cfg(feature = "settings-cache")]
    target: settings::cache::Target,
    #[cfg(feature = "settings-cache")]
    save: Option<settings::cache::Save>,
    #[cfg(feature = "settings-cache")]
    retry_save: u64,
}

pub struct Session<T> {
    host: Host<T>,
    bootstrap: Instant,
    fallback: Option<settings::fallback::Request>,
    fallback_attempted: bool,
    prepare: Option<Request>,
    fallback_diagnostics: Vec<Diagnostic>,
    #[cfg(feature = "settings-cache")]
    cache_fault: Option<Diagnostic>,
    #[cfg(feature = "settings-cache")]
    cache_persisted: Option<settings::consumer::SnapshotIdentity>,
    #[cfg(feature = "settings-cache")]
    retry_save: u64,
}

/// Shared cache readback for application and shell property surfaces. A
/// persistence receipt is historical: compare it with current applied evidence
/// before attributing it to the active presentation.
#[cfg(feature = "settings-cache")]
#[derive(serde::Serialize)]
pub struct CacheEvidence<'a> {
    pub persisted: Option<&'a settings::consumer::SnapshotIdentity>,
    pub fault: Option<&'a Diagnostic>,
    pub fallback_diagnostics: &'a [Diagnostic],
}
impl<T> Session<T> {
    pub fn new(consumer: Consumer) -> Self {
        Self {
            host: Host::new(consumer),
            bootstrap: Instant::now() + BOOTSTRAP_BUDGET,
            fallback: None,
            fallback_attempted: false,
            prepare: None,
            fallback_diagnostics: Vec::new(),
            #[cfg(feature = "settings-cache")]
            cache_fault: None,
            #[cfg(feature = "settings-cache")]
            cache_persisted: None,
            #[cfg(feature = "settings-cache")]
            retry_save: 0,
        }
    }
    pub fn host(&self) -> &Host<T> {
        &self.host
    }

    pub fn fallback_diagnostics(&self) -> &[Diagnostic] {
        &self.fallback_diagnostics
    }

    #[cfg(feature = "settings-cache")]
    pub fn cache_fault(&self) -> Option<&Diagnostic> {
        self.cache_fault.as_ref()
    }
    #[cfg(feature = "settings-cache")]
    pub fn cache_persisted(&self) -> Option<&settings::consumer::SnapshotIdentity> {
        self.cache_persisted.as_ref()
    }
    #[cfg(feature = "settings-cache")]
    pub fn cache_evidence(&self) -> CacheEvidence<'_> {
        CacheEvidence {
            persisted: self.cache_persisted(),
            fault: self.cache_fault(),
            fallback_diagnostics: self.fallback_diagnostics(),
        }
    }
    /// `live` is the connection's current sampled state, read on the UI loop.
    /// Queued lifecycle notices cannot authorise an activation after real loss.
    pub fn handle(&mut self, event: Event<T>, live: Option<u64>) -> (Option<ChangePlan>, Jobs) {
        self.handle_with(event, live, |_| {})
    }

    /// Borrow a host activation hook; no host state is stored in this session.
    /// It runs only for a current successful preparation, before applied ACK.
    pub fn handle_with(
        &mut self,
        event: Event<T>,
        live: Option<u64>,
        mut activate: impl FnMut(&Presentation<T>),
    ) -> (Option<ChangePlan>, Jobs) {
        self.sync(live);
        let mut changed = None;
        match event {
            Event::Wake => {}
            Event::Refresh => {
                self.host.consumer_mut().refresh();
                // Failure stays quiescent on ordinary wakes. An explicit
                // refresh may retry resources without a connection edge.
                if self.host.consumer().applied().is_none() && self.fallback.is_none() {
                    self.fallback_attempted = false;
                }
            }
            Event::Lost => {
                self.host.consumer_mut().lost();
            }
            Event::Delivery(delivery) => {
                self.host.consumer_mut().decoded_delivery(delivery);
            }
            Event::Rpc(work, result) => {
                self.host.consumer_mut().complete(&work, result);
            }
            Event::Prepared(ready) => {
                changed = self.host.complete_with(ready, &mut activate);
            }
            Event::Fallback(request, result) => match *result {
                Ok((fallback, presentation)) => {
                    if self.host.consumer().is_fallback_current(&request) {
                        self.fallback_diagnostics = fallback.diagnostics().to_vec();
                    }
                    if self
                        .host
                        .consumer_mut()
                        .complete_fallback(&request, Ok(fallback))
                    {
                        let capture = self.host.request().expect("staged fallback");
                        changed = self.host.complete_with(
                            Completion {
                                update: capture.update,
                                result: Box::new(Ok(presentation)),
                            },
                            &mut activate,
                        );
                    }
                }
                Err(faults) => {
                    if self.host.consumer().is_fallback_current(&request) {
                        self.fallback_diagnostics = faults.clone();
                    }
                    self.host
                        .consumer_mut()
                        .complete_fallback(&request, Err(faults));
                }
            },
            #[cfg(feature = "settings-cache")]
            Event::Saved(save, result) => {
                if self
                    .host
                    .consumer()
                    .cache_save()
                    .as_ref()
                    .is_some_and(|current| current.same_capture(&save))
                {
                    if matches!(
                        result,
                        Ok(settings::cache::WriteOutcome::Written
                            | settings::cache::WriteOutcome::Unchanged)
                    ) {
                        self.cache_persisted = Some(save.identity());
                    }
                    self.cache_fault = result.err();
                }
            }
            #[cfg(feature = "settings-cache")]
            Event::RetryCache => self.retry_save = self.retry_save.wrapping_add(1),
        }
        let now = Instant::now();
        if self
            .host
            .consumer()
            .retry_deadline()
            .is_some_and(|deadline| deadline <= now)
        {
            self.host.consumer_mut().retry();
        }
        if self
            .fallback
            .as_ref()
            .is_some_and(|request| !self.host.consumer().is_fallback_current(request))
        {
            self.fallback = None;
        }
        if !self.fallback_attempted && self.host.consumer().applied().is_none() {
            let request = self.host.consumer_mut().fallback_request();
            if request.is_some() {
                self.fallback_attempted = true;
                self.fallback = request.clone();
            }
        }
        if self
            .prepare
            .as_ref()
            .is_some_and(|request| !self.host.consumer().is_current(request.update()))
        {
            self.prepare = None;
        }
        if let Some(request) = self.host.request() {
            self.prepare = Some(request);
        }
        let prepare = self.prepare.clone();
        let wake = self
            .host
            .consumer()
            .retry_deadline()
            .into_iter()
            .chain(
                (now < self.bootstrap
                    && !self.fallback_attempted
                    && self.host.consumer().applied().is_none())
                .then_some(self.bootstrap),
            )
            .min();
        let deadline = if now < self.bootstrap && !self.host.consumer().is_confirmed() {
            self.bootstrap
        } else {
            now + BOOTSTRAP_BUDGET
        };
        let jobs = Jobs {
            work: self.host.consumer().current_work().cloned(),
            deadline,
            wake,
            prepare,
            fallback: self.fallback.clone(),
            valid: self.host.consumer().pending().cloned(),
            valid_fallback: self.fallback.clone(),
            #[cfg(feature = "settings-cache")]
            target: self.host.consumer().cache_target(),
            #[cfg(feature = "settings-cache")]
            save: self.host.consumer().cache_save(),
            #[cfg(feature = "settings-cache")]
            retry_save: self.retry_save,
        };
        (changed, jobs)
    }
    fn sync(&mut self, live: Option<u64>) {
        if live != self.host.consumer().generation() {
            self.fallback = None;
            self.fallback_attempted = false;
            match live {
                Some(generation) => {
                    self.host.consumer_mut().connected(generation);
                }
                None => self.host.consumer_mut().disconnected(),
            }
        }
    }
}

type Rpc = Pin<Box<dyn Future<Output = Event<()>> + Send>>;
type Builder<T> = Arc<dyn Fn(&Prepared, &Snapshot) -> Result<T, Diagnostic> + Send + Sync>;

enum Resource {
    Prepare(Request),
    Fallback(Box<settings::fallback::Request>),
}
impl Resource {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Prepare(left), Self::Prepare(right)) => left.update().same_stage(right.update()),
            (Self::Fallback(left), Self::Fallback(right)) => left.same_request(right),
            _ => false,
        }
    }
}
enum Running<T> {
    Resource {
        capture: Resource,
        task: tokio::task::JoinHandle<Event<T>>,
        cancel: Arc<AtomicBool>,
    },
    #[cfg(feature = "settings-cache")]
    Save {
        save: settings::cache::Save,
        target: settings::cache::Target,
        task: tokio::task::JoinHandle<cache::Result>,
    },
}
enum Done<T> {
    Resource(Box<Result<Event<T>, tokio::task::JoinError>>),
    #[cfg(feature = "settings-cache")]
    Save(Box<Result<cache::Result, tokio::task::JoinError>>),
}
fn valid(resource: &Resource, jobs: &Jobs) -> bool {
    match resource {
        Resource::Prepare(request) => jobs
            .valid
            .as_ref()
            .is_some_and(|update| update.same_stage(request.update())),
        Resource::Fallback(request) => jobs
            .valid_fallback
            .as_ref()
            .is_some_and(|capture| capture.same_request(request)),
    }
}

/// Called and polled only on the host's existing Tokio worker. At most one
/// blocking preparation exists physically; replacement waits for it to finish.
/// No task is spawned for a native RPC or timer. `next()` is multiplexed with
/// the existing Bus/effect/state receiver; dropping it does not lose job state.
pub struct Worker<T> {
    client: Option<Arc<settings::native::Client>>,
    work: Option<Work>,
    rpc: Option<Rpc>,
    wake: Option<Instant>,
    queued: Option<Resource>,
    running: Option<Running<T>>,
    build: Builder<T>,
    offered: Option<Resource>,
    #[cfg(feature = "settings-cache")]
    cache: Option<cache::Lane>,
}
impl<T: Send + 'static> Worker<T> {
    pub fn new(
        client: Arc<settings::native::Client>,
        build: impl Fn(&Prepared, &Snapshot) -> Result<T, Diagnostic> + Send + Sync + 'static,
    ) -> Self {
        let mut worker = Self::offline(build);
        worker.client = Some(client);
        worker
    }
    /// Resource/fallback jobs may run while the host's existing connection is
    /// still starting. RPCs stay dormant until that same client is attached.
    pub fn offline(
        build: impl Fn(&Prepared, &Snapshot) -> Result<T, Diagnostic> + Send + Sync + 'static,
    ) -> Self {
        Self {
            client: None,
            work: None,
            rpc: None,
            wake: None,
            queued: None,
            running: None,
            build: Arc::new(build),
            offered: None,
            #[cfg(feature = "settings-cache")]
            cache: None,
        }
    }
    /// Attach the host's already established connection, then resend its
    /// desired jobs. This method creates no connection or receiver.
    pub fn connect(&mut self, client: Arc<settings::native::Client>) {
        self.client = Some(client);
        self.work = None;
        self.rpc = None;
    }
    /// Construction-only cache root. All I/O shares the blocking resource lane.
    #[cfg(feature = "settings-cache")]
    pub fn offline_with_cache(
        directory: std::path::PathBuf,
        build: impl Fn(&Prepared, &Snapshot) -> Result<T, Diagnostic> + Send + Sync + 'static,
    ) -> Self {
        let mut worker = Self::offline(build);
        worker.cache = Some(cache::Lane::new(directory));
        worker
    }
    pub fn replace(&mut self, jobs: Jobs) {
        if self
            .offered
            .as_ref()
            .is_some_and(|resource| !valid(resource, &jobs))
        {
            self.offered = None;
        }
        if self
            .queued
            .as_ref()
            .is_some_and(|resource| !valid(resource, &jobs))
        {
            self.queued = None;
        }
        if let Some(Running::Resource {
            capture, cancel, ..
        }) = &self.running
            && !valid(capture, &jobs)
        {
            cancel.store(true, Ordering::Release);
        }
        #[cfg(feature = "settings-cache")]
        if let Some(cache) = &mut self.cache {
            cache.replace(&jobs.target, jobs.save.as_ref(), jobs.retry_save);
        }
        if self.work != jobs.work {
            self.rpc = None;
            self.work = jobs.work.clone();
            if let (Some(work), Some(client)) = (jobs.work, self.client.as_ref()) {
                let client = Arc::clone(client);
                let deadline = tokio::time::Instant::from_std(jobs.deadline);
                self.rpc = Some(Box::pin(async move {
                    let result = settings::native::execute_until(&client, &work, deadline).await;
                    Event::Rpc(work, result)
                }));
            }
        }
        self.wake = jobs.wake;
        let offered = jobs.prepare.map(Resource::Prepare).or_else(|| {
            jobs.fallback
                .map(|request| Resource::Fallback(Box::new(request)))
        });
        if let Some(resource) = offered
            && self
                .offered
                .as_ref()
                .is_none_or(|previous| !previous.same(&resource))
        {
            self.offered = Some(match &resource {
                Resource::Prepare(request) => Resource::Prepare(request.clone()),
                Resource::Fallback(request) => Resource::Fallback(request.clone()),
            });
            self.queued = Some(resource);
        }
    }
    fn start(&mut self) {
        if self.running.is_some() {
            return;
        }
        let Some(resource) = self.queued.take() else {
            #[cfg(feature = "settings-cache")]
            if let Some(cache) = &mut self.cache {
                self.running = cache.start();
            }
            return;
        };
        let capture = match &resource {
            Resource::Prepare(request) => Resource::Prepare(request.clone()),
            Resource::Fallback(request) => Resource::Fallback(request.clone()),
        };
        let build = Arc::clone(&self.build);
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&cancel);
        #[cfg(feature = "settings-cache")]
        let cache = self.cache.as_ref().and_then(cache::Lane::loader);
        let task = tokio::task::spawn_blocking(move || match resource {
            Resource::Prepare(request) => {
                let snapshot = request.update().snapshot();
                let result = prepare(snapshot, &request.context, &cancelled, &build);
                Event::Prepared(Completion {
                    update: request.update,
                    result: Box::new(result),
                })
            }
            Resource::Fallback(request) => {
                let request = *request;
                let mut presentation = None;
                let prepared = request.prepare_with_cache(
                    || {
                        #[cfg(feature = "settings-cache")]
                        if let Some((directory, target)) = cache {
                            return settings::cache::load_for(&directory, &target).map(Some);
                        }
                        Ok(None)
                    },
                    |snapshot, context, _| {
                        presentation = Some(prepare(snapshot, context, &cancelled, &build)?);
                        Ok(())
                    },
                );
                Event::Fallback(
                    request,
                    Box::new(
                        prepared
                            .map(|fallback| (fallback, presentation.expect("validated resources"))),
                    ),
                )
            }
        });
        self.running = Some(Running::Resource {
            capture,
            task,
            cancel,
        });
    }
    pub async fn next(&mut self) -> Once<Event<T>> {
        self.start();
        tokio::select! {
            event = async { self.rpc.as_mut().expect("guarded RPC").await }, if self.rpc.is_some() => {
                self.rpc = None;
                let Event::Rpc(work, result) = event else { unreachable!() };
                Once::new(Event::Rpc(work, result))
            }
            _ = async { tokio::time::sleep_until(tokio::time::Instant::from_std(self.wake.expect("guarded wake"))).await }, if self.wake.is_some() => {
                self.wake = None;
                Once::new(Event::Wake)
            }
            result = async {
                match self.running.as_mut().expect("guarded job") {
                    Running::Resource { task, .. } => Done::Resource(Box::new(task.await)),
                    #[cfg(feature = "settings-cache")]
                    Running::Save { task, .. } => Done::Save(Box::new(task.await)),
                }
            }, if self.running.is_some() => {
                let running = self.running.take().unwrap();
                let event = match (running, result) {
                    (Running::Resource { capture, .. }, Done::Resource(result)) => match *result {
                    Ok(event) => event,
                    Err(error) => {
                        let fault = Diagnostic::new("preparation_failed", "worker", error.to_string());
                        match capture {
                            Resource::Prepare(request) => Event::Prepared(request.failed(fault)),
                            Resource::Fallback(request) => Event::Fallback(*request, Box::new(Err(vec![fault]))),
                        }
                    }
                    },
                    #[cfg(feature = "settings-cache")]
                    (Running::Save { save, target, .. }, Done::Save(result)) => {
                        let result = match *result {
                            Ok(result) => {
                                if let Some(cache) = &mut self.cache {
                                    cache.finish(&target, result.writer);
                                }
                                result.outcome
                            }
                            Err(error) => Err(Diagnostic::new("cache_write_failed", "cache", error.to_string())),
                        };
                        Event::Saved(save, result)
                    },
                    #[cfg(feature = "settings-cache")]
                    _ => unreachable!("job and result agree"),
                };
                Once::new(event)
            }
            _ = std::future::pending::<()>() => unreachable!(),
        }
    }

    /// Drain activated saves on the host's existing shutdown worker. Resources
    /// are cancelled cooperatively; the UI never waits here. A timeout does
    /// not cancel a filesystem operation that has already started. Persistence
    /// is established only by a successful save/drain receipt.
    #[cfg(feature = "settings-cache")]
    pub async fn flush_cache(&mut self, deadline: Instant) -> Result<(), Diagnostic> {
        self.rpc = None;
        self.wake = None;
        self.queued = None;
        if let Some(Running::Resource { cancel, .. }) = &self.running {
            cancel.store(true, Ordering::Release);
        }
        let mut fault = None;
        while self.running.is_some() || self.cache.as_ref().is_some_and(cache::Lane::pending) {
            let event =
                tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), self.next())
                    .await
                    .map_err(|_| {
                        Diagnostic::new("cache_drain_timeout", "cache", "Shutdown budget expired")
                    })?
                    .take()
                    .expect("worker event");
            if let Event::Saved(_, result) = event {
                fault = result.err();
            }
        }
        fault.map_or(Ok(()), Err)
    }
}
impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        if let Some(Running::Resource { cancel, .. }) = &self.running {
            cancel.store(true, Ordering::Release);
        }
    }
}
fn prepare<T>(
    snapshot: &Snapshot,
    context: &str,
    cancel: &AtomicBool,
    build: &Builder<T>,
) -> Result<Presentation<T>, Diagnostic> {
    let check = || {
        if cancel.load(Ordering::Acquire) {
            Err(Diagnostic::new(
                "preparation_cancelled",
                "worker",
                "Preparation superseded",
            ))
        } else {
            Ok(())
        }
    };
    check()?;
    let effective = snapshot
        .effective
        .get(context)
        .ok_or_else(|| Diagnostic::new("missing_context", "effective", "Context missing"))?;
    let appearance = Projection::new(effective)?
        .prepare_registered_checked(snapshot.desktop.appearance.source.is_none(), check)?;
    let content = build(&appearance, snapshot)?;
    check()?;
    Ok(Presentation {
        appearance,
        content,
    })
}
