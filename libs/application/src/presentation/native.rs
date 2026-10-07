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
mod mailbox;
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
}

pub struct Session<T> {
    host: Host<T>,
    bootstrap: Instant,
    fallback: Option<settings::fallback::Request>,
    fallback_attempted: bool,
    prepare: Option<Request>,
}
impl<T> Session<T> {
    pub fn new(consumer: Consumer) -> Self {
        Self {
            host: Host::new(consumer),
            bootstrap: Instant::now() + BOOTSTRAP_BUDGET,
            fallback: None,
            fallback_attempted: false,
            prepare: None,
        }
    }
    pub fn host(&self) -> &Host<T> {
        &self.host
    }
    /// `live` is the connection's current sampled state, read on the UI loop.
    /// Queued lifecycle notices cannot authorise an activation after real loss.
    pub fn handle(&mut self, event: Event<T>, live: Option<u64>) -> (Option<ChangePlan>, Jobs) {
        self.sync(live);
        let mut changed = None;
        match event {
            Event::Wake => {}
            Event::Refresh => {
                self.host.consumer_mut().refresh();
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
                changed = self.host.complete(ready);
            }
            Event::Fallback(request, result) => match *result {
                Ok((fallback, presentation)) => {
                    if self
                        .host
                        .consumer_mut()
                        .complete_fallback(&request, Ok(fallback))
                    {
                        let capture = self.host.request().expect("staged fallback");
                        changed = self.host.complete(Completion {
                            update: capture.update,
                            result: Ok(presentation),
                        });
                    }
                }
                Err(faults) => {
                    self.host
                        .consumer_mut()
                        .complete_fallback(&request, Err(faults));
                }
            },
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
        if now >= self.bootstrap
            && !self.fallback_attempted
            && self.host.consumer().applied().is_none()
        {
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
                (now < self.bootstrap && self.host.consumer().applied().is_none())
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
struct Running<T> {
    capture: Resource,
    task: tokio::task::JoinHandle<Event<T>>,
    cancel: Arc<AtomicBool>,
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
        }
    }
    /// Attach the host's already established connection, then resend its
    /// desired jobs. This method creates no connection or receiver.
    pub fn connect(&mut self, client: Arc<settings::native::Client>) {
        self.client = Some(client);
        self.work = None;
        self.rpc = None;
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
        if let Some(running) = &self.running
            && !valid(&running.capture, &jobs)
        {
            running.cancel.store(true, Ordering::Release);
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
            return;
        };
        let capture = match &resource {
            Resource::Prepare(request) => Resource::Prepare(request.clone()),
            Resource::Fallback(request) => Resource::Fallback(request.clone()),
        };
        let build = Arc::clone(&self.build);
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&cancel);
        let task = tokio::task::spawn_blocking(move || match resource {
            Resource::Prepare(request) => {
                let snapshot = request.update().snapshot();
                let result = prepare(snapshot, &request.context, &cancelled, &build);
                Event::Prepared(Completion {
                    update: request.update,
                    result,
                })
            }
            Resource::Fallback(request) => {
                let request = *request;
                let mut presentation = None;
                let prepared = request.prepare(None, |snapshot, context, _| {
                    presentation = Some(prepare(snapshot, context, &cancelled, &build)?);
                    Ok(())
                });
                Event::Fallback(
                    request,
                    Box::new(
                        prepared
                            .map(|fallback| (fallback, presentation.expect("validated resources"))),
                    ),
                )
            }
        });
        self.running = Some(Running {
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
            result = async { (&mut self.running.as_mut().expect("guarded resource").task).await }, if self.running.is_some() => {
                let running = self.running.take().unwrap();
                let event = match result {
                    Ok(event) => event,
                    Err(error) => {
                        let fault = Diagnostic::new("preparation_failed", "worker", error.to_string());
                        match running.capture {
                            Resource::Prepare(request) => Event::Prepared(request.failed(fault)),
                            Resource::Fallback(request) => Event::Fallback(*request, Box::new(Err(vec![fault]))),
                        }
                    }
                };
                Once::new(event)
            }
            _ = std::future::pending::<()>() => unreachable!(),
        }
    }
}
impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        if let Some(running) = &self.running {
            running.cancel.store(true, Ordering::Release);
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
