// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared UI scheduling and jobs multiplexed by an existing host worker. No
//! connection, runtime, receiver or loop is created here.
use super::*;
use crate::message::Once;
use appearance::resources::{ResourceHost, ResourceRequirements};
use settings::{
    Snapshot,
    consumer::Work,
    native::{BOOTSTRAP_BUDGET, Decoded},
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
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
    /// Opaque worker result fenced by the captured local preparation revision.
    Resource(ResourceCompletion<T>),
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
pub struct Jobs<C = ()> {
    work: Option<Work>,
    deadline: Instant,
    wake: Option<Instant>,
    resource: Option<Resource<C>>,
    #[cfg(feature = "settings-cache")]
    target: settings::cache::Target,
    #[cfg(feature = "settings-cache")]
    save: Option<settings::cache::Save>,
    #[cfg(feature = "settings-cache")]
    retry_save: u64,
}

pub struct Session<T, C = ()> {
    host: Host<T>,
    bootstrap: Instant,
    fallback: Option<settings::fallback::Request>,
    fallback_attempted: bool,
    prepare: Option<Request>,
    fallback_diagnostics: Vec<Diagnostic>,
    local: LocalCapture<C>,
    applied_revision: Option<PreparationRevision>,
    active: Option<ActiveSource>,
    activation_epoch: u64,
    activation_exhausted: bool,
    local_fault: Option<Diagnostic>,
    failed_local: Option<LocalKey>,
    legacy_completion: bool,
    #[cfg(feature = "settings-cache")]
    cache_fault: Option<Diagnostic>,
    #[cfg(feature = "settings-cache")]
    cache_persisted: Option<settings::consumer::SnapshotIdentity>,
    #[cfg(feature = "settings-cache")]
    cache_configuration: Option<Diagnostic>,
    #[cfg(feature = "settings-cache")]
    retry_save: u64,
}

/// Shared cache readback for application and shell property surfaces. A
/// persistence receipt is historical: compare it with current applied evidence
/// before attributing it to the active presentation.
#[cfg(feature = "settings-cache")]
#[derive(serde::Serialize)]
pub struct CacheEvidence<'a> {
    /// Construction policy, distinct from write faults and historical receipts.
    pub configuration: Option<&'a Diagnostic>,
    pub persisted: Option<&'a settings::consumer::SnapshotIdentity>,
    pub fault: Option<&'a Diagnostic>,
    pub fallback_diagnostics: &'a [Diagnostic],
}
impl<T> Session<T, ()> {
    pub fn new(consumer: Consumer) -> Self {
        let mut session = Self::with_context(consumer, ());
        session.legacy_completion = true;
        session
    }
}

/// A process-local resource preparation identity, independent of settingsd.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreparationRevision(u64);
impl PreparationRevision {
    const INITIAL: Self = Self(0);
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Local presentation readback; authoritative settings evidence stays separate.
pub struct PreparationEvidence<'a> {
    pub desired: PreparationRevision,
    pub applied: Option<PreparationRevision>,
    pub current: bool,
    pub fault: Option<&'a Diagnostic>,
}

struct LocalCapture<C> {
    revision: PreparationRevision,
    value: Arc<C>,
}
impl<C> Clone for LocalCapture<C> {
    fn clone(&self) -> Self {
        Self {
            revision: self.revision,
            value: Arc::clone(&self.value),
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct LocalKey {
    epoch: u64,
    revision: PreparationRevision,
}
#[derive(Clone)]
struct ActiveSource {
    epoch: u64,
    request: Request,
    binding: Option<settings::ResourceBinding>,
    generic: Option<Prepared>,
}
pub struct ResourceCompletion<T> {
    revision: PreparationRevision,
    outcome: ResourceOutcome<T>,
}
enum ResourceOutcome<T> {
    Prepared(Completion<T>),
    Fallback(Box<settings::fallback::Request>, Box<FallbackResult<T>>),
    Reprepared(Box<ActiveSource>, Box<Result<Presentation<T>, Diagnostic>>),
}
impl<T, C> Session<T, C> {
    pub fn with_context(consumer: Consumer, initial: C) -> Self {
        Self {
            host: Host::new(consumer),
            bootstrap: Instant::now() + BOOTSTRAP_BUDGET,
            fallback: None,
            fallback_attempted: false,
            prepare: None,
            fallback_diagnostics: Vec::new(),
            local: LocalCapture {
                revision: PreparationRevision::INITIAL,
                value: Arc::new(initial),
            },
            applied_revision: None,
            active: None,
            activation_epoch: 0,
            activation_exhausted: false,
            local_fault: None,
            failed_local: None,
            legacy_completion: false,
            #[cfg(feature = "settings-cache")]
            cache_fault: None,
            #[cfg(feature = "settings-cache")]
            cache_persisted: None,
            #[cfg(feature = "settings-cache")]
            cache_configuration: None,
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
            configuration: self.cache_configuration.as_ref(),
            persisted: self.cache_persisted(),
            fault: self.cache_fault(),
            fallback_diagnostics: self.fallback_diagnostics(),
        }
    }
    /// `live` is the connection's current sampled state, read on the UI loop.
    /// Queued lifecycle notices cannot authorise an activation after real loss.
    pub fn handle(&mut self, event: Event<T>, live: Option<u64>) -> (Option<ChangePlan>, Jobs<C>) {
        self.handle_with(event, live, |_| {})
    }

    /// Borrow a host activation hook; no host state is stored in this session.
    /// It runs only for a current successful preparation, before applied ACK.
    pub fn handle_with(
        &mut self,
        event: Event<T>,
        live: Option<u64>,
        mut activate: impl FnMut(&Presentation<T>),
    ) -> (Option<ChangePlan>, Jobs<C>) {
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
            Event::Prepared(ready) if self.accepts_legacy() => {
                changed = self.complete_resource(ResourceOutcome::Prepared(ready), &mut activate);
            }
            Event::Fallback(request, result) if self.accepts_legacy() => {
                changed = self.complete_resource(
                    ResourceOutcome::Fallback(Box::new(request), result),
                    &mut activate,
                );
            }
            Event::Prepared(_) | Event::Fallback(..) => {}
            Event::Resource(completion) => {
                if completion.revision == self.local.revision {
                    changed = self.complete_resource(completion.outcome, &mut activate);
                }
            }
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
        self.advance_settings();
        self.capture_authority();
        (changed, self.jobs())
    }

    fn accepts_legacy(&self) -> bool {
        self.legacy_completion && self.local.revision == PreparationRevision::INITIAL
    }

    fn complete_resource(
        &mut self,
        outcome: ResourceOutcome<T>,
        activate: &mut impl FnMut(&Presentation<T>),
    ) -> Option<ChangePlan> {
        match outcome {
            ResourceOutcome::Prepared(ready) => {
                let source = Request {
                    update: ready.update.clone(),
                    context: self.host.consumer().context().to_owned(),
                };
                self.complete_authority(source, ready, activate)
            }
            ResourceOutcome::Fallback(request, result) => match *result {
                Ok((fallback, presentation)) => {
                    if !self.host.consumer().is_fallback_current(&request) {
                        return None;
                    }
                    // Check the activation identity before staging or acknowledging.
                    if self.activation_epoch.checked_add(1).is_none() {
                        self.activation_exhausted = true;
                        self.local_fault = Some(exhausted("activation"));
                        return None;
                    }
                    self.fallback_diagnostics = fallback.diagnostics().to_vec();
                    if self
                        .host
                        .consumer_mut()
                        .complete_fallback(&request, Ok(fallback))
                    {
                        let capture = self.host.request().expect("staged fallback");
                        let completion = Completion {
                            update: capture.update.clone(),
                            result: Box::new(Ok(presentation)),
                        };
                        self.complete_authority(capture, completion, activate)
                    } else {
                        None
                    }
                }
                Err(faults) => {
                    if self.host.consumer().is_fallback_current(&request) {
                        self.fallback_diagnostics = faults.clone();
                    }
                    self.host
                        .consumer_mut()
                        .complete_fallback(&request, Err(faults));
                    None
                }
            },
            ResourceOutcome::Reprepared(source, result) => {
                let key = LocalKey {
                    epoch: source.epoch,
                    revision: self.local.revision,
                };
                let current = !self.activation_exhausted
                    && self.active.as_ref().is_some_and(|active| {
                        active.epoch == source.epoch
                            && active.request.update.same_stage(&source.request.update)
                            && active.request.context == source.request.context
                            && active.binding == source.binding
                    })
                    && self.host.consumer().pending().is_none();
                if !current {
                    return None;
                }
                match *result {
                    Ok(presentation) => {
                        let binding = presentation
                            .appearance()
                            .resources()
                            .and_then(|r| r.binding().cloned());
                        if binding != source.binding {
                            self.local_fault = Some(Diagnostic::new(
                                "resource_binding",
                                "preparation",
                                "Local preparation changed the active resource identity",
                            ));
                            self.failed_local = Some(key);
                            return None;
                        }
                        self.host.replace_local(presentation, activate);
                        self.applied_revision = Some(self.local.revision);
                        self.local_fault = None;
                        self.failed_local = None;
                        Some(ChangePlan {
                            paint: true,
                            text: true,
                            layout: true,
                            resources: true,
                            motion: false,
                            shell: false,
                        })
                    }
                    Err(fault) => {
                        self.local_fault = Some(fault);
                        self.failed_local = Some(key);
                        None
                    }
                }
            }
        }
    }

    fn complete_authority(
        &mut self,
        request: Request,
        completion: Completion<T>,
        activate: &mut impl FnMut(&Presentation<T>),
    ) -> Option<ChangePlan> {
        let epoch = self.activation_epoch.checked_add(1);
        if epoch.is_none()
            && completion.result.is_ok()
            && self.host.consumer().is_current(&completion.update)
        {
            self.activation_exhausted = true;
            self.local_fault = Some(exhausted("activation"));
            return None;
        }
        let changed = self.host.complete_with(completion, activate);
        if changed.is_some() {
            self.activation_epoch = epoch.expect("activation checked before replacement");
            let appearance = self
                .host
                .presentation()
                .expect("activated presentation")
                .appearance();
            let binding = appearance.resources().and_then(|r| r.binding().cloned());
            self.active = Some(ActiveSource {
                epoch: self.activation_epoch,
                request,
                generic: binding.is_none().then(|| appearance.clone()),
                binding,
            });
            self.applied_revision = Some(self.local.revision);
            self.local_fault = None;
            self.failed_local = None;
        }
        changed
    }

    pub fn preparation_evidence(&self) -> PreparationEvidence<'_> {
        PreparationEvidence {
            desired: self.local.revision,
            applied: self.applied_revision,
            current: self.active.is_some() && self.applied_revision == Some(self.local.revision),
            fault: self.local_fault.as_ref(),
        }
    }

    pub fn set_context(
        &mut self,
        next: C,
        live: Option<u64>,
    ) -> Result<(PreparationRevision, Jobs<C>), Diagnostic>
    where
        C: PartialEq,
    {
        if self.local.value.as_ref() == &next {
            self.sync(live);
            self.capture_authority();
            return Ok((self.local.revision, self.jobs()));
        }
        let revision = self.next_revision()?;
        self.local = LocalCapture {
            revision,
            value: Arc::new(next),
        };
        self.local_fault = None;
        self.failed_local = None;
        self.sync(live);
        self.capture_authority();
        Ok((revision, self.jobs()))
    }

    pub fn retry_preparation(
        &mut self,
        live: Option<u64>,
    ) -> Result<(PreparationRevision, Jobs<C>), Diagnostic> {
        let revision = self.next_revision()?;
        self.local.revision = revision;
        self.local_fault = None;
        self.failed_local = None;
        self.sync(live);
        self.capture_authority();
        Ok((revision, self.jobs()))
    }

    fn next_revision(&self) -> Result<PreparationRevision, Diagnostic> {
        if self.activation_exhausted {
            return Err(exhausted("activation"));
        }
        self.local
            .revision
            .0
            .checked_add(1)
            .map(PreparationRevision)
            .ok_or_else(|| exhausted("revision"))
    }

    fn advance_settings(&mut self) {
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
    }

    fn capture_authority(&mut self) {
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
    }

    fn jobs(&self) -> Jobs<C> {
        let now = Instant::now();
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
        let kind = if self.activation_exhausted {
            None
        } else {
            self.prepare
                .clone()
                .map(ResourceKind::Prepare)
                .or_else(|| {
                    self.fallback
                        .clone()
                        .filter(|_| self.active.is_none())
                        .map(|r| ResourceKind::Fallback(Box::new(r)))
                })
                .or_else(|| {
                    self.active
                        .as_ref()
                        .filter(|source| {
                            self.host.consumer().pending().is_none()
                                && self.applied_revision != Some(self.local.revision)
                                && self.failed_local
                                    != Some(LocalKey {
                                        epoch: source.epoch,
                                        revision: self.local.revision,
                                    })
                        })
                        .cloned()
                        .map(|source| ResourceKind::Reprepare(Box::new(source)))
                })
        };
        Jobs {
            work: self.host.consumer().current_work().cloned(),
            deadline,
            wake,
            resource: kind.map(|kind| Resource {
                kind,
                local: self.local.clone(),
            }),
            #[cfg(feature = "settings-cache")]
            target: self.host.consumer().cache_target(),
            #[cfg(feature = "settings-cache")]
            save: self.host.consumer().cache_save(),
            #[cfg(feature = "settings-cache")]
            retry_save: self.retry_save,
        }
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
type Builder<T, C> = Arc<dyn Fn(&Prepared, &Snapshot, &C) -> Result<T, Diagnostic> + Send + Sync>;
/// Pure per-snapshot icon requirements, built on the worker before any
/// renderer mutation. Applications collect their real static menu/control
/// catalogue and bounded dynamic snapshot here; the default requests no icons.
type Requirements<C> = Arc<
    dyn Fn(&Projection, &Snapshot, &C) -> Result<ResourceRequirements, Diagnostic> + Send + Sync,
>;

#[derive(Clone)]
enum ResourceKind {
    Prepare(Request),
    Fallback(Box<settings::fallback::Request>),
    Reprepare(Box<ActiveSource>),
}
struct Resource<C> {
    kind: ResourceKind,
    local: LocalCapture<C>,
}
impl<C> Clone for Resource<C> {
    fn clone(&self) -> Self {
        Self {
            kind: self.kind.clone(),
            local: self.local.clone(),
        }
    }
}
impl<C> Clone for Jobs<C> {
    fn clone(&self) -> Self {
        Self {
            work: self.work.clone(),
            deadline: self.deadline,
            wake: self.wake,
            resource: self.resource.clone(),
            #[cfg(feature = "settings-cache")]
            target: self.target.clone(),
            #[cfg(feature = "settings-cache")]
            save: self.save.clone(),
            #[cfg(feature = "settings-cache")]
            retry_save: self.retry_save,
        }
    }
}
#[cfg(test)]
impl<C> Jobs<C> {
    fn prepare_request(&self) -> Option<Request> {
        match &self.resource.as_ref()?.kind {
            ResourceKind::Prepare(request) => Some(request.clone()),
            _ => None,
        }
    }
    fn fallback_request(&self) -> Option<settings::fallback::Request> {
        match &self.resource.as_ref()?.kind {
            ResourceKind::Fallback(request) => Some((**request).clone()),
            _ => None,
        }
    }
}
impl<C> Resource<C> {
    fn same(&self, other: &Self) -> bool {
        if self.local.revision != other.local.revision {
            return false;
        }
        match (&self.kind, &other.kind) {
            (ResourceKind::Prepare(left), ResourceKind::Prepare(right)) => {
                left.update().same_stage(right.update())
            }
            (ResourceKind::Fallback(left), ResourceKind::Fallback(right)) => {
                left.same_request(right)
            }
            (ResourceKind::Reprepare(left), ResourceKind::Reprepare(right)) => {
                left.epoch == right.epoch
            }
            _ => false,
        }
    }

    fn failed<T>(self, fault: Diagnostic) -> Event<T> {
        let outcome = match self.kind {
            ResourceKind::Prepare(request) => ResourceOutcome::Prepared(request.failed(fault)),
            ResourceKind::Fallback(request) => {
                ResourceOutcome::Fallback(request, Box::new(Err(vec![fault])))
            }
            ResourceKind::Reprepare(source) => {
                ResourceOutcome::Reprepared(source, Box::new(Err(fault)))
            }
        };
        Event::Resource(ResourceCompletion {
            revision: self.local.revision,
            outcome,
        })
    }
}
enum Running<T, C> {
    Resource {
        capture: Resource<C>,
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
fn valid<C>(resource: &Resource<C>, jobs: &Jobs<C>) -> bool {
    jobs.resource
        .as_ref()
        .is_some_and(|desired| desired.same(resource))
}

/// Called and polled only on the host's existing Tokio worker. At most one
/// blocking preparation exists physically; replacement waits for it to finish.
/// No task is spawned for a native RPC or timer. `next()` is multiplexed with
/// the existing Bus/effect/state receiver; dropping it does not lose job state.
pub struct Worker<T, C = ()> {
    client: Option<Arc<settings::native::Client>>,
    work: Option<Work>,
    rpc: Option<Rpc>,
    wake: Option<Instant>,
    queued: Option<Resource<C>>,
    running: Option<Running<T, C>>,
    build: Builder<T, C>,
    offered: Option<Resource<C>>,
    /// The one verified resource host. Locked only inside the serial blocking
    /// closure and released before the event returns; UI and async scheduling
    /// never touch it. A poisoned lock becomes a preparation diagnostic.
    host: Arc<Mutex<ResourceHost>>,
    requirements: Requirements<C>,
    #[cfg(feature = "settings-cache")]
    cache: Option<cache::Lane>,
}
impl<T: Send + 'static> Worker<T, ()> {
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
    /// The resource host captures the normal MixOS lookup policy.
    pub fn offline(
        build: impl Fn(&Prepared, &Snapshot) -> Result<T, Diagnostic> + Send + Sync + 'static,
    ) -> Self {
        Self::offline_with_host(build, ResourceHost::new(assets::mixos::lookup()))
    }
    /// An explicit host policy for tests and embeddings: the approved roots
    /// only, never the process environment. This is host configuration, never
    /// an authored settings field.
    pub fn offline_with_host(
        build: impl Fn(&Prepared, &Snapshot) -> Result<T, Diagnostic> + Send + Sync + 'static,
        host: ResourceHost,
    ) -> Self {
        Self::contextual_with_host(
            move |prepared, snapshot, _: &()| build(prepared, snapshot),
            host,
        )
    }
    /// Replace the empty icon requirements with the application's own: a pure
    /// projection/snapshot collection, evaluated before any renderer mutation.
    pub fn with_resource_requirements(
        self,
        build: impl Fn(&Projection, &Snapshot) -> Result<ResourceRequirements, Diagnostic>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.with_contextual_resource_requirements(move |projection, snapshot, _: &()| {
            build(projection, snapshot)
        })
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
    /// Construction-only cache root with an explicit host policy.
    #[cfg(feature = "settings-cache")]
    pub fn offline_with_cache_and_host(
        directory: std::path::PathBuf,
        build: impl Fn(&Prepared, &Snapshot) -> Result<T, Diagnostic> + Send + Sync + 'static,
        host: ResourceHost,
    ) -> Self {
        let mut worker = Self::offline_with_host(build, host);
        worker.cache = Some(cache::Lane::new(directory));
        worker
    }
}
impl<T: Send + 'static, C: Send + Sync + 'static> Worker<T, C> {
    pub fn contextual(
        build: impl Fn(&Prepared, &Snapshot, &C) -> Result<T, Diagnostic> + Send + Sync + 'static,
    ) -> Self {
        Self::contextual_with_host(build, ResourceHost::new(assets::mixos::lookup()))
    }

    pub fn contextual_with_host(
        build: impl Fn(&Prepared, &Snapshot, &C) -> Result<T, Diagnostic> + Send + Sync + 'static,
        host: ResourceHost,
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
            host: Arc::new(Mutex::new(host)),
            requirements: Arc::new(|_, _, _| Ok(ResourceRequirements::empty())),
            #[cfg(feature = "settings-cache")]
            cache: None,
        }
    }

    pub fn with_contextual_resource_requirements(
        mut self,
        build: impl Fn(&Projection, &Snapshot, &C) -> Result<ResourceRequirements, Diagnostic>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.requirements = Arc::new(build);
        self
    }

    #[cfg(feature = "settings-cache")]
    pub fn with_cache_directory(mut self, directory: std::path::PathBuf) -> Self {
        self.cache = Some(cache::Lane::new(directory));
        self
    }

    /// Attach the host's already established connection without another task.
    pub fn connect(&mut self, client: Arc<settings::native::Client>) {
        self.client = Some(client);
        self.work = None;
        self.rpc = None;
    }
    pub fn replace(&mut self, jobs: Jobs<C>) {
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
        let offered = jobs.resource;
        if let Some(resource) = offered
            && self
                .offered
                .as_ref()
                .is_none_or(|previous| !previous.same(&resource))
        {
            self.offered = Some(resource.clone());
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
        let capture = resource.clone();
        let build = Arc::clone(&self.build);
        let host = Arc::clone(&self.host);
        let requirements = Arc::clone(&self.requirements);
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&cancel);
        #[cfg(feature = "settings-cache")]
        let cache = self.cache.as_ref().and_then(cache::Lane::loader);
        let task = tokio::task::spawn_blocking(move || {
            let local = resource.local;
            let outcome = match resource.kind {
                ResourceKind::Prepare(request) => {
                    let snapshot = request.update().snapshot();
                    let result = prepare(
                        snapshot,
                        host,
                        None,
                        &PreparationInputs {
                            context: &request.context,
                            cancel: &cancelled,
                            build: &build,
                            requirements: &requirements,
                            local: local.value.as_ref(),
                        },
                    );
                    ResourceOutcome::Prepared(Completion {
                        update: request.update,
                        result: Box::new(result),
                    })
                }
                ResourceKind::Fallback(request) => {
                    let request = *request;
                    let mut presentation = None;
                    let prepared = request.prepare_resources_with_cache(
                        || {
                            #[cfg(feature = "settings-cache")]
                            if let Some((directory, target)) = cache {
                                return settings::cache::load_for(&directory, &target).map(Some);
                            }
                            Ok(None)
                        },
                        |snapshot, context, _, expected| {
                            presentation = Some(prepare(
                                snapshot,
                                Arc::clone(&host),
                                expected,
                                &PreparationInputs {
                                    context,
                                    cancel: &cancelled,
                                    build: &build,
                                    requirements: &requirements,
                                    local: local.value.as_ref(),
                                },
                            )?);
                            // The typed binding the fallback ladder records: exactly
                            // what this preparation verified, so the later cache
                            // capture cannot diverge from the activation.
                            Ok(presentation
                                .as_ref()
                                .and_then(|prepared| prepared.appearance().resources())
                                .and_then(|resources| resources.binding().cloned()))
                        },
                    );
                    ResourceOutcome::Fallback(
                        Box::new(request),
                        Box::new(prepared.map(|fallback| {
                            (fallback, presentation.expect("validated resources"))
                        })),
                    )
                }
                ResourceKind::Reprepare(source) => {
                    let snapshot = source.request.update.snapshot();
                    let inputs = PreparationInputs {
                        context: &source.request.context,
                        cancel: &cancelled,
                        build: &build,
                        requirements: &requirements,
                        local: local.value.as_ref(),
                    };
                    let result = if let Some(generic) = &source.generic {
                        build_generic(generic, snapshot, &inputs)
                    } else {
                        prepare(snapshot, host, source.binding.as_ref(), &inputs)
                    };
                    ResourceOutcome::Reprepared(source, Box::new(result))
                }
            };
            Event::Resource(ResourceCompletion {
                revision: local.revision,
                outcome,
            })
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
                    (Running::Resource { capture, cancel, .. }, Done::Resource(result)) => if cancel.load(Ordering::Acquire) {
                        // Supersession is sticky even if the same source becomes
                        // desired again. Its newly queued capture must remain
                        // eligible, rather than inherit the obsolete failure.
                        Event::Wake
                    } else { match *result {
                    Ok(event) => event,
                    Err(error) => {
                        let fault = Diagnostic::new("preparation_failed", "worker", error.to_string());
                        capture.failed(fault)
                    }
                    } },
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
impl<T, C> Drop for Worker<T, C> {
    fn drop(&mut self) {
        if let Some(Running::Resource { cancel, .. }) = &self.running {
            cancel.store(true, Ordering::Release);
        }
    }
}
/// One serial worker preparation: pure requirement collection, then the
/// verified resource host under its lock, then the deliberate content builder.
/// The host lock is released before the content builder runs and before the
/// event returns; a poisoned lock is a diagnostic, never an unwrap. Cancellation
/// is checked at the start, through every bounded host stage and again after
/// the content builder, immediately before the candidate is returned.
struct PreparationInputs<'a, T, C> {
    context: &'a str,
    cancel: &'a AtomicBool,
    build: &'a Builder<T, C>,
    requirements: &'a Requirements<C>,
    local: &'a C,
}

impl<T, C> PreparationInputs<'_, T, C> {
    fn check(&self) -> Result<(), Diagnostic> {
        if self.cancel.load(Ordering::Acquire) {
            Err(Diagnostic::new(
                "preparation_cancelled",
                "worker",
                "Preparation superseded",
            ))
        } else {
            Ok(())
        }
    }
}

fn prepare<T, C>(
    snapshot: &Snapshot,
    host: Arc<Mutex<ResourceHost>>,
    expected: Option<&settings::ResourceBinding>,
    inputs: &PreparationInputs<'_, T, C>,
) -> Result<Presentation<T>, Diagnostic> {
    let mut check = || inputs.check();
    check()?;
    let effective = snapshot
        .effective
        .get(inputs.context)
        .ok_or_else(|| Diagnostic::new("missing_context", "effective", "Context missing"))?;
    let projection = Projection::new(effective)?;
    let required = (inputs.requirements)(&projection, snapshot, inputs.local)?;
    let reference = snapshot.desktop.appearance.resources.as_ref();
    let appearance = host
        .lock()
        .map_err(|_| {
            Diagnostic::new(
                "resource_host_poisoned",
                "worker",
                "Resource host lock poisoned",
            )
        })?
        .prepare(projection, reference, expected, required, &mut check)?;
    let content = (inputs.build)(&appearance, snapshot, inputs.local)?;
    check()?;
    Ok(Presentation {
        appearance,
        content,
    })
}

fn exhausted(field: &str) -> Diagnostic {
    Diagnostic::new(
        "preparation_exhausted",
        field,
        "Preparation identity exhausted",
    )
}

fn build_generic<T, C>(
    appearance: &Prepared,
    snapshot: &Snapshot,
    inputs: &PreparationInputs<'_, T, C>,
) -> Result<Presentation<T>, Diagnostic> {
    let check = || inputs.check();
    check()?;
    let effective = snapshot
        .effective
        .get(inputs.context)
        .ok_or_else(|| Diagnostic::new("missing_context", "effective", "Context missing"))?;
    (inputs.requirements)(&Projection::new(effective)?, snapshot, inputs.local)?;
    let content = (inputs.build)(appearance, snapshot, inputs.local)?;
    check()?;
    Ok(Presentation {
        appearance: appearance.clone(),
        content,
    })
}
