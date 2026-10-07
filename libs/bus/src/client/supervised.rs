// SPDX-License-Identifier: MIT OR Apache-2.0

//! A broker connection under a reconnect supervisor.
//!
//! A [`Connection`] is one dial: when the socket drops, its incoming stream
//! ends, which is right for a short-lived tool and fatal for a resident
//! service, where one broker restart would end every registration.
//!
//! [`SupervisedClient`] owns the connect, register, replay subscriptions,
//! pump loop:
//!
//! - `connect` has a bounded initial budget ([`MAX_INITIAL_ATTEMPTS`]) so a
//!   misconfigured service fails fast; `start` returns immediately and lets
//!   the supervisor retry initial transport failures indefinitely;
//! - reconnect is unbounded, with exponential backoff and full jitter (base
//!   250 ms, doubling, capped at 30 s), so a resident service waits out a
//!   long broker outage without being restarted by its supervisor;
//! - declared initial topics
//!   ([`SupervisedConnectOptions::with_initial_topics`]) are subscription
//!   requirements of the first establishment, and ordinary
//!   [`SubscriptionRegistry`] entries are replayed in recorded order, all
//!   before the client reports `Connected`: a service that looks healthy
//!   while deaf on a topic is the failure this guards against. One
//!   whole-attempt deadline (default 60 s, configurable) bounds dial,
//!   registration, every subscribe write and ACK wait together;
//! - the outward incoming stream survives reconnects and only ends on a
//!   fatal shutdown;
//! - while disconnected every outbound call fails fast with a typed error.
//!   There is no outbound queue.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::{Mutex as TokioMutex, RwLock, mpsc, oneshot, watch};

use super::IncomingCommand;
use super::connection::{Connection, ConnectionOptions};
use super::error::{
    ClientError, RegistrationRejected, SubscriptionDeclarationError, SupervisedError,
};
use crate::BusMessage;
use crate::native_client::NativeIncomingReceiver;
use crate::native_client::bounded::{
    BoundedIncomingEvent, BoundedIncomingReceiver, BoundedIncomingSender, bounded_incoming_channel,
};

/// Attempt budget for the initial connect-and-register. Exhausting it is a
/// typed fatal: a misconfigured service must fail fast rather than spin
/// against a broker that will never answer.
pub const MAX_INITIAL_ATTEMPTS: u32 = 5;

const BACKOFF_BASE_MS: u64 = 250;
const BACKOFF_CAP_MS: u64 = 30_000;

/// Default whole-attempt establishment deadline: dial, registration, the
/// transaction-lock wait, every subscribe write and every ACK wait together.
/// One total deadline, not one per declaration, so an unresponsive broker
/// does not multiply its budget by the declaration count.
const DEFAULT_ESTABLISHMENT_TIMEOUT: Duration = Duration::from_secs(60);

/// Bound for explicit attempt cleanup: a blocked close frame must not
/// prevent subsequent cancellation or a finite completion. The native close
/// aborts its reader and clears parked callers before any unbounded sink
/// I/O, so abandoning the graceful close frame here is safe.
const ATTEMPT_CLOSE_BOUND: Duration = Duration::from_secs(1);

/// Client-side configuration bounds for declared initial topics. These are
/// documented client limits, not broker limits: raw entries and bytes are
/// counted before deduplication so duplicates cannot bypass them.
const MAX_INITIAL_TOPICS: usize = 64;
const MAX_TOPIC_BYTES: usize = 1024;
const MAX_AGGREGATE_TOPIC_BYTES: usize = 16 * 1024;

/// The upper bound of the full-jitter window for `attempt` (0-based):
/// `min(base * 2^attempt, cap)`. Monotonic non-decreasing, saturating.
fn backoff_ceiling_ms(attempt: u32) -> u64 {
    let factor = 2u64.checked_pow(attempt).unwrap_or(u64::MAX);
    BACKOFF_BASE_MS.saturating_mul(factor).min(BACKOFF_CAP_MS)
}

/// Full-jitter backoff: a uniform draw from `[0, ceiling]`. Full jitter
/// decorrelates a fleet of services reconnecting after one broker bounce.
fn backoff_delay(attempt: u32) -> Duration {
    Duration::from_millis(jitter_below(backoff_ceiling_ms(attempt)))
}

/// A pseudo-random draw from `[0, ceiling]`. Decorrelation is all that is
/// needed, so this is splitmix64 over the clock, the pid and a counter
/// rather than a dependency on a random-number crate.
fn jitter_below(ceiling: u64) -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let salt = COUNTER.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
    let mut z = nanos ^ u64::from(std::process::id()).rotate_left(32) ^ salt;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    z % (ceiling + 1)
}

/// Connection lifecycle state. The watch channel is also the sampled state
/// authority, so edge-triggered and sampled consumers cannot diverge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    /// Initial connect in progress.
    Connecting = 0,
    /// Live broker connection; outbound calls permitted.
    Connected = 1,
    /// Transport lost; the supervisor is backing off and reconnecting.
    /// Outbound calls fail fast with [`SupervisedError::Disconnected`].
    Disconnected = 2,
    /// Graceful shutdown requested; the supervisor will not reconnect.
    ShuttingDown = 3,
    /// Terminal: the initial budget was exhausted, the broker rejected the
    /// registration and that was configured as fatal, or a declared initial
    /// topic was invalid or explicitly refused.
    Fatal = 4,
}

/// Validate declared initial topics: bounded, control-character free,
/// deduplicated exact names in first-seen order. Raw entries and bytes are
/// counted BEFORE deduplication, so duplicates cannot bypass the resource
/// bounds. Names are never trimmed or case-folded.
fn validate_declarations(topics: &[String]) -> Result<Vec<String>, SubscriptionDeclarationError> {
    if topics.len() > MAX_INITIAL_TOPICS {
        return Err(SubscriptionDeclarationError::Invalid {
            index: None,
            message: format!(
                "at most {MAX_INITIAL_TOPICS} initial topics; {} supplied",
                topics.len()
            ),
        });
    }
    let mut total_bytes = 0usize;
    for (index, topic) in topics.iter().enumerate() {
        if topic.is_empty() {
            return Err(SubscriptionDeclarationError::Invalid {
                index: Some(index),
                message: "topic names must not be empty".to_string(),
            });
        }
        if topic.chars().any(|c| matches!(c, '\r' | '\n' | '\0')) {
            return Err(SubscriptionDeclarationError::Invalid {
                index: Some(index),
                message: "topic names must not contain CR, LF or NUL".to_string(),
            });
        }
        if topic.len() > MAX_TOPIC_BYTES {
            return Err(SubscriptionDeclarationError::Invalid {
                index: Some(index),
                message: format!("a topic exceeds {MAX_TOPIC_BYTES} UTF-8 bytes"),
            });
        }
        total_bytes = total_bytes.saturating_add(topic.len());
        if total_bytes > MAX_AGGREGATE_TOPIC_BYTES {
            return Err(SubscriptionDeclarationError::Invalid {
                index: None,
                message: format!(
                    "declared topics exceed {MAX_AGGREGATE_TOPIC_BYTES} bytes in total"
                ),
            });
        }
    }
    let mut seen = BTreeSet::new();
    let mut declared = Vec::with_capacity(topics.len());
    for topic in topics {
        if seen.insert(topic.as_str()) {
            declared.push(topic.clone());
        }
    }
    Ok(declared)
}

/// The broker's diagnostic for a raw reply, using the normal wire-level
/// precedence (error header, then structured body fields, then raw body).
fn reply_error_message(body: &str, error_header: Option<String>) -> String {
    let mut reply = BusMessage::new().with_body(body);
    if let Some(error) = error_header {
        reply.set("error", &error);
    }
    reply.error_message()
}

/// Ordered, deduplicated set of subscribed topic names, replayed on every
/// reconnect.
///
/// It is mutated transactionally: [`record`](Self::record) only after an
/// `rc 0` broker subscribe, [`remove`](Self::remove) only after an `rc 0`
/// unsubscribe, so a never-satisfiable topic is never replayed forever and a
/// deliberately dropped one is not resurrected by a later bounce. Clones
/// share one store.
#[derive(Clone, Default)]
pub struct SubscriptionRegistry {
    inner: Arc<std::sync::Mutex<Vec<String>>>,
}

impl SubscriptionRegistry {
    pub fn new() -> SubscriptionRegistry {
        SubscriptionRegistry::default()
    }

    /// Record a topic. First-seen order is kept; a duplicate is a no-op and
    /// returns `false`.
    pub fn record(&self, topic: &str) -> bool {
        let mut g = self.lock();
        if g.iter().any(|t| t == topic) {
            return false;
        }
        g.push(topic.to_string());
        true
    }

    /// Forget a topic. Returns whether it was present.
    pub fn remove(&self, topic: &str) -> bool {
        let mut g = self.lock();
        match g.iter().position(|t| t == topic) {
            Some(pos) => {
                g.remove(pos);
                true
            }
            None => false,
        }
    }

    /// The recorded topics in replay order.
    pub fn snapshot(&self) -> Vec<String> {
        self.lock().clone()
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<String>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Connection options for a supervised client, from
/// [`SupervisedClient::connect_options`].
///
/// By default a registration the broker rejects is retried like any other
/// failed attempt. A service whose public name must never wait out a
/// rejection opts into a terminal state with
/// [`fatal_on_registration_rejection`](Self::fatal_on_registration_rejection).
///
/// Declared initial topics ([`with_initial_topics`](Self::with_initial_topics))
/// are subscription requirements established before the first `Connected`.
pub struct SupervisedConnectOptions {
    service_name: String,
    noded_url: String,
    fatal_on_registration_rejection: bool,
    connection: ConnectionOptions,
    initial_topics: Vec<String>,
    establishment_timeout: Option<Duration>,
}

impl SupervisedConnectOptions {
    pub fn with_verbs(mut self, verbs: Vec<crate::VerbDescriptor>) -> Self {
        self.connection.verbs = Some(verbs);
        self
    }

    pub fn with_provenance(mut self, provenance: crate::RegisterProvenance) -> Self {
        self.connection.provenance = Some(provenance);
        self
    }

    pub fn bounded_incoming(mut self, capacity: usize) -> Self {
        assert!(capacity > 0, "bounded incoming capacity must be non-zero");
        self.connection.capacity = Some(capacity);
        self
    }
    /// Treat a broker registration rejection (collision or admission) as
    /// terminal, on the initial connect and on every reconnect.
    pub fn fatal_on_registration_rejection(mut self, enabled: bool) -> Self {
        self.fatal_on_registration_rejection = enabled;
        self
    }

    /// Declare the initial topics this client must be subscribed to before
    /// it reports `Connected`, in declaration order. Replaces any previous
    /// list; the default is empty.
    ///
    /// Declarations are validated once, before any socket is opened: at most
    /// 64 entries, 1024 UTF-8 bytes per topic, 16 KiB of raw topic bytes in
    /// total, no empty names, CR, LF or NUL. Exact duplicates are one
    /// subscription; names are never trimmed or case-folded.
    ///
    /// A declared topic the broker refuses (any nonzero rc, warning included)
    /// is terminal: the client reports `Fatal` with the exact topic, rc and
    /// diagnostic, and never retries. A successful later unsubscribe removes
    /// it and it stays removed across reconnects.
    pub fn with_initial_topics(mut self, topics: Vec<String>) -> Self {
        self.initial_topics = topics;
        self
    }

    /// Bound the WHOLE establishment attempt — dial, registration, the
    /// transaction-lock wait, every subscribe write and every ACK wait — to
    /// this duration. Default 60 seconds. Zero and durations that cannot
    /// form a deadline are invalid configuration.
    pub fn establishment_timeout(mut self, timeout: Duration) -> Self {
        self.establishment_timeout = Some(timeout);
        self
    }

    /// Connect with these options. Invalid declarations return a typed
    /// [`SubscriptionDeclarationError`] immediately, before any socket work;
    /// transient establishment failure keeps the five-attempt budget and
    /// [`SupervisedError::InitialConnectFailed`].
    pub async fn connect(self) -> Result<SupervisedClient, SupervisedError> {
        SupervisedClient::connect_with_options(self).await
    }

    /// Start without waiting for a broker. Transport failures are retried
    /// indefinitely by the same supervisor used for reconnects. The client
    /// initially reports `Connecting`, generation zero, and rejects outbound
    /// work until registration and every declared/replayed subscription have
    /// completed.
    ///
    /// Must be called inside the Tokio runtime that will own this client.
    /// Registration rejection follows the configured fatal policy. Invalid
    /// declarations or a zero establishment timeout yield a `Fatal` client
    /// with the diagnostic already sampleable and its incoming producer
    /// closed, without dialing.
    pub fn start(self) -> SupervisedClient {
        let service_name = self.service_name.clone();
        let capacity = self.connection.capacity;
        match self.prepare() {
            Ok(prepared) => SupervisedClient::launch(prepared, None),
            Err(error) => SupervisedClient::invalid(service_name, capacity, error),
        }
    }

    /// Validate once, before any dial or spawn work.
    fn prepare(self) -> Result<PreparedOptions, SubscriptionDeclarationError> {
        let SupervisedConnectOptions {
            service_name,
            noded_url,
            fatal_on_registration_rejection,
            connection,
            initial_topics,
            establishment_timeout,
        } = self;
        let declarations = validate_declarations(&initial_topics)?;
        let establishment_timeout = establishment_timeout.unwrap_or(DEFAULT_ESTABLISHMENT_TIMEOUT);
        if establishment_timeout.is_zero() {
            return Err(SubscriptionDeclarationError::Invalid {
                index: None,
                message: "establishment timeout must be non-zero".to_string(),
            });
        }
        Instant::now()
            .checked_add(establishment_timeout)
            .ok_or_else(|| SubscriptionDeclarationError::Invalid {
                index: None,
                message: "establishment timeout does not form a valid deadline".to_string(),
            })?;
        Ok(PreparedOptions {
            service_name,
            noded_url,
            fatal_on_registration_rejection,
            connection,
            declarations,
            establishment_timeout,
        })
    }
}

/// Validated, ready-to-launch options.
struct PreparedOptions {
    service_name: String,
    noded_url: String,
    fatal_on_registration_rejection: bool,
    connection: ConnectionOptions,
    /// Validated, deduplicated, first-seen-ordered declared initial topics.
    declarations: Vec<String>,
    /// Whole-attempt deadline, checked non-zero and deadline-formable.
    establishment_timeout: Duration,
}

/// A [`Connection`] under a reconnect supervisor.
///
/// Build one with [`connect`](Self::connect) or
/// [`connect_options`](Self::connect_options). Hand the
/// [`incoming`](Self::incoming) receiver to the event loop and make outbound
/// calls through the state-gated methods; the supervisor task reconnects,
/// re-registers and replays the [`SubscriptionRegistry`] underneath.
pub struct SupervisedClient {
    /// The live connection. Swapped by the supervisor on reconnect under a
    /// brief write lock; outbound calls clone the `Arc` under a read lock so
    /// no network await holds it.
    inner: Arc<RwLock<Option<Arc<Connection>>>>,
    state_tx: watch::Sender<ConnState>,
    /// Serialises terminal-state checks with watch publication.
    state_publish: Arc<std::sync::Mutex<()>>,
    registration_rejection: Arc<std::sync::Mutex<Option<RegistrationRejected>>>,
    /// The terminal declared-subscription refusal or validation failure,
    /// published before the `Fatal` edge under the same fence.
    declaration_error: Arc<std::sync::Mutex<Option<SubscriptionDeclarationError>>>,
    /// Count of fully established connections: zero before the first socket,
    /// one for initial success, plus one after every complete reconnect and
    /// replay, so a consumer cannot sample away a fast bounce.
    connection_generation: Arc<AtomicU64>,
    registry: SubscriptionRegistry,
    subscription_transaction: Arc<TokioMutex<()>>,
    /// The outward incoming stream, taken once by the consumer.
    incoming_rx: std::sync::Mutex<Option<mpsc::UnboundedReceiver<IncomingCommand>>>,
    bounded_incoming_rx: std::sync::Mutex<Option<BoundedIncomingReceiver>>,
    /// `true` tells the supervisor to stop and not reconnect.
    shutdown_tx: watch::Sender<bool>,
    supervisor: TokioMutex<Option<tokio::task::JoinHandle<()>>>,
    service_name: String,
}

impl SupervisedClient {
    /// Start building a supervised connection with explicit options.
    pub fn connect_options(service_name: &str, noded_url: &str) -> SupervisedConnectOptions {
        SupervisedConnectOptions {
            service_name: service_name.to_string(),
            noded_url: noded_url.to_string(),
            fatal_on_registration_rejection: false,
            connection: ConnectionOptions::default(),
            initial_topics: Vec::new(),
            establishment_timeout: None,
        }
    }

    /// Connect (bounded budget), register and start the supervisor with the
    /// default options. Returns [`SupervisedError::InitialConnectFailed`]
    /// when the initial connect-and-register cannot succeed within
    /// [`MAX_INITIAL_ATTEMPTS`].
    pub async fn connect(
        service_name: &str,
        noded_url: &str,
    ) -> Result<SupervisedClient, SupervisedError> {
        Self::connect_options(service_name, noded_url)
            .connect()
            .await
    }

    pub async fn connect_supervised(
        service_name: &str,
        noded_url: &str,
    ) -> Result<Self, SupervisedError> {
        Self::connect(service_name, noded_url).await
    }

    pub async fn connect_supervised_with_provenance(
        service_name: &str,
        noded_url: &str,
        provenance: Option<crate::RegisterProvenance>,
    ) -> Result<Self, SupervisedError> {
        let mut options = Self::connect_options(service_name, noded_url);
        options.connection.provenance = provenance;
        options.connect().await
    }

    async fn connect_with_options(
        options: SupervisedConnectOptions,
    ) -> Result<SupervisedClient, SupervisedError> {
        // Validate once, before any dial or spawn work: an invalid
        // declaration or deadline is a typed error with no socket opened.
        let prepared = options
            .prepare()
            .map_err(SupervisedError::SubscriptionDeclaration)?;
        let (result_tx, result_rx) = oneshot::channel();
        let client = Self::launch(prepared, Some(result_tx));
        match result_rx.await {
            Ok(Ok(())) => Ok(client),
            Ok(Err(error)) => {
                // Dropping the client signals shutdown to the same supervisor.
                drop(client);
                Err(error)
            }
            Err(_) => {
                // The supervisor stopped without a result: this caller (or
                // the incoming consumer) went away mid-establishment.
                drop(client);
                Err(SupervisedError::ShuttingDown)
            }
        }
    }

    fn launch(
        prepared: PreparedOptions,
        initial_result: Option<oneshot::Sender<Result<(), SupervisedError>>>,
    ) -> Self {
        let PreparedOptions {
            service_name,
            noded_url,
            fatal_on_registration_rejection,
            connection: connection_options,
            declarations,
            establishment_timeout,
        } = prepared;
        let (state_tx, _) = watch::channel(ConnState::Connecting);
        let state_publish = Arc::new(std::sync::Mutex::new(()));
        let registration_rejection = Arc::new(std::sync::Mutex::new(None));
        let declaration_error = Arc::new(std::sync::Mutex::new(None));

        let inner = Arc::new(RwLock::new(None));
        let connection_generation = Arc::new(AtomicU64::new(0));
        let (out_tx, out_rx, bounded_out_rx) = match connection_options.capacity {
            Some(capacity) => {
                let (sender, receiver) = bounded_incoming_channel(capacity);
                (SupervisorOutgoing::Bounded(sender), None, Some(receiver))
            }
            None => {
                let (sender, receiver) = mpsc::unbounded_channel();
                (SupervisorOutgoing::Unbounded(sender), Some(receiver), None)
            }
        };
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let registry = SubscriptionRegistry::new();
        let subscription_transaction = Arc::new(TokioMutex::new(()));

        let supervisor = tokio::spawn(supervisor_loop(SupervisorCtx {
            inner: inner.clone(),
            state_tx: state_tx.clone(),
            state_publish: state_publish.clone(),
            registration_rejection: registration_rejection.clone(),
            declaration_error: declaration_error.clone(),
            connection_generation: connection_generation.clone(),
            registry: registry.clone(),
            subscription_transaction: subscription_transaction.clone(),
            out_tx,
            shutdown_rx,
            service_name: service_name.clone(),
            noded_url,
            fatal_on_registration_rejection,
            connection_options,
            declarations,
            establishment_timeout,
            initial_result,
        }));

        SupervisedClient {
            inner,
            state_tx,
            state_publish,
            registration_rejection,
            declaration_error,
            connection_generation,
            registry,
            subscription_transaction,
            incoming_rx: std::sync::Mutex::new(out_rx),
            bounded_incoming_rx: std::sync::Mutex::new(bounded_out_rx),
            shutdown_tx,
            supervisor: TokioMutex::new(Some(supervisor)),
            service_name,
        }
    }

    /// A client that failed option validation: `Fatal` from construction,
    /// the diagnostic already sampleable, the incoming producer closed, no
    /// socket opened.
    fn invalid(
        service_name: String,
        capacity: Option<usize>,
        error: SubscriptionDeclarationError,
    ) -> Self {
        let (state_tx, _) = watch::channel(ConnState::Fatal);
        let state_publish = Arc::new(std::sync::Mutex::new(()));
        let registration_rejection = Arc::new(std::sync::Mutex::new(None));
        let declaration_error = Arc::new(std::sync::Mutex::new(Some(error)));
        let inner = Arc::new(RwLock::new(None));
        let connection_generation = Arc::new(AtomicU64::new(0));
        let registry = SubscriptionRegistry::new();
        let subscription_transaction = Arc::new(TokioMutex::new(()));
        // The incoming producer is dropped immediately: a receiver taken
        // from this client ends at once.
        let (out_rx, bounded_out_rx) = match capacity {
            Some(capacity) => {
                let (sender, receiver) = bounded_incoming_channel(capacity);
                drop(sender);
                (None, Some(receiver))
            }
            None => {
                let (sender, receiver) = mpsc::unbounded_channel();
                drop(sender);
                (Some(receiver), None)
            }
        };
        let (shutdown_tx, _) = watch::channel(false);
        SupervisedClient {
            inner,
            state_tx,
            state_publish,
            registration_rejection,
            declaration_error,
            connection_generation,
            registry,
            subscription_transaction,
            incoming_rx: std::sync::Mutex::new(out_rx),
            bounded_incoming_rx: std::sync::Mutex::new(bounded_out_rx),
            shutdown_tx,
            supervisor: TokioMutex::new(None),
            service_name,
        }
    }

    /// The shared [`SubscriptionRegistry`] the supervisor replays.
    pub fn subscription_registry(&self) -> SubscriptionRegistry {
        self.registry.clone()
    }

    /// Take the outward incoming stream (once). It survives reconnects and
    /// only yields `None` on a fatal shutdown.
    pub fn incoming(&self) -> Option<mpsc::UnboundedReceiver<IncomingCommand>> {
        self.incoming_rx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    pub fn incoming_bounded(&self) -> Option<BoundedIncomingReceiver> {
        self.bounded_incoming_rx
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    /// The current connection state.
    pub fn state(&self) -> ConnState {
        *self.state_tx.borrow()
    }

    /// The terminal broker registration refusal, if one was configured as
    /// fatal. This non-consuming sample is published before the `Fatal` edge;
    /// ordinary dial failures never manufacture a registration refusal.
    pub fn registration_rejection(&self) -> Option<RegistrationRejected> {
        self.registration_rejection
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// The terminal declared-subscription refusal or validation failure, if
    /// one was published. Non-consuming; published before the `Fatal` edge
    /// under the same fence, so observing `Fatal` guarantees the diagnostic
    /// is already sampleable. `None` for every other failure mode.
    pub fn subscription_declaration_error(&self) -> Option<SubscriptionDeclarationError> {
        self.declaration_error
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// Subscribe to connection-state transitions. The receiver starts at
    /// the current state and is notified on every edge.
    pub fn subscribe_state(&self) -> watch::Receiver<ConnState> {
        self.state_tx.subscribe()
    }

    /// The count of fully established connections. Unlike sampling
    /// [`state`](Self::state), this cannot miss a disconnect and reconnect
    /// that both happen between two observations. A new value means
    /// registration and subscription replay completed on a new socket.
    pub fn connection_generation(&self) -> u64 {
        self.connection_generation.load(Ordering::SeqCst)
    }

    /// Whether outbound calls will be accepted right now.
    pub fn is_connected(&self) -> bool {
        self.state() == ConnState::Connected
    }

    /// The registered service name.
    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    /// Gate the outbound path: a typed fail-fast unless `Connected`.
    fn gate(&self) -> Result<(), SupervisedError> {
        match self.state() {
            ConnState::Connected => Ok(()),
            ConnState::ShuttingDown => Err(SupervisedError::ShuttingDown),
            _ => Err(SupervisedError::Disconnected),
        }
    }

    /// The live connection, cloned out so no network await holds the lock.
    async fn connection(&self) -> Result<Arc<Connection>, SupervisedError> {
        self.inner
            .read()
            .await
            .clone()
            .ok_or(SupervisedError::Disconnected)
    }

    pub async fn call_typed(
        &self,
        to: &str,
        command: &str,
        args: serde_json::Value,
    ) -> Result<crate::PortReply, SupervisedError> {
        self.gate()?;
        self.connection()
            .await?
            .call_typed(to, command, args)
            .await
            .map_err(SupervisedError::Transport)
    }

    pub async fn send(
        &self,
        to: &str,
        command: &str,
        args: serde_json::Value,
    ) -> Result<(), SupervisedError> {
        self.gate()?;
        self.connection()
            .await?
            .send(to, command, args)
            .await
            .map_err(SupervisedError::Transport)
    }

    /// Send explicit headers and body through the canonical connection.
    pub async fn send_with_headers(
        &self,
        to: &str,
        command: &str,
        headers: &BTreeMap<String, String>,
        body: &str,
    ) -> Result<(), SupervisedError> {
        self.gate()?;
        self.connection()
            .await?
            .send_with_headers(to, command, headers, body)
            .await
            .map_err(SupervisedError::Transport)
    }

    pub async fn list_services(&self) -> Result<Vec<String>, SupervisedError> {
        self.gate()?;
        self.connection()
            .await?
            .list_services()
            .await
            .map_err(SupervisedError::Transport)
    }

    pub async fn respond(
        &self,
        incoming: &IncomingCommand,
        rc: u8,
        body: &str,
    ) -> Result<(), SupervisedError> {
        self.respond_parts(
            incoming.generation,
            &incoming.from,
            &incoming.command,
            incoming.id.as_deref(),
            rc,
            body,
        )
        .await
    }

    /// Only shutdown's synthetic terminal replies bypass the outbound gate.
    /// Ordinary handler replies still fail once the client is shutting down.
    pub async fn respond_parts_shutdown_synth(
        &self,
        generation: u64,
        to: &str,
        command: &str,
        id: Option<&str>,
        rc: u8,
        body: &str,
    ) -> Result<(), SupervisedError> {
        self.reply_connection(generation)
            .await?
            .respond_parts(to, command, id, rc, body)
            .await
            .map_err(SupervisedError::Transport)
    }

    /// See [`Connection::call`].
    pub async fn call(
        &self,
        to: &str,
        command: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, SupervisedError> {
        self.gate()?;
        self.connection()
            .await?
            .call(to, command, args)
            .await
            .map_err(SupervisedError::Transport)
    }

    /// See [`Connection::call_with_headers`].
    pub async fn call_with_headers(
        &self,
        to: &str,
        command: &str,
        headers: &BTreeMap<String, String>,
        body: &str,
    ) -> Result<serde_json::Value, SupervisedError> {
        self.gate()?;
        self.connection()
            .await?
            .call_with_headers(to, command, headers, body)
            .await
            .map_err(SupervisedError::Transport)
    }

    /// See [`Connection::call_with_headers_raw`].
    pub async fn call_with_headers_raw(
        &self,
        to: &str,
        command: &str,
        headers: &BTreeMap<String, String>,
        body: &str,
    ) -> Result<(u8, String, Option<String>), SupervisedError> {
        self.gate()?;
        self.connection()
            .await?
            .call_with_headers_raw(to, command, headers, body)
            .await
            .map_err(SupervisedError::Transport)
    }

    /// See [`Connection::respond_parts`]. Gated like every other outbound
    /// call: a reply attempted while disconnected fails fast.
    pub async fn respond_parts(
        &self,
        generation: u64,
        to: &str,
        command: &str,
        id: Option<&str>,
        rc: u8,
        body: &str,
    ) -> Result<(), SupervisedError> {
        self.gate()?;
        self.reply_connection(generation)
            .await?
            .respond_parts(to, command, id, rc, body)
            .await
            .map_err(SupervisedError::Transport)
    }

    async fn reply_connection(&self, generation: u64) -> Result<Arc<Connection>, SupervisedError> {
        // Reconnect publishes its connection and generation while holding
        // this write lock. Retain the selected connection for the whole send:
        // a later reconnect cannot redirect a reply onto its replacement.
        let connection = self.inner.read().await;
        if generation == 0 || generation != self.connection_generation() {
            return Err(SupervisedError::Disconnected);
        }
        connection.clone().ok_or(SupervisedError::Disconnected)
    }

    /// Subscribe to a topic and record it for replay on reconnect.
    ///
    /// The topic is recorded only after an `rc 0` broker subscribe. A
    /// rejected subscribe (a reserved name, or any transport failure)
    /// returns the typed error and leaves the registry unchanged, so a
    /// never-satisfiable topic is not replayed forever. Fails fast while not
    /// `Connected`; topics already recorded are replayed on the next
    /// reconnect regardless.
    pub async fn subscribe_topic(&self, topic: &str) -> Result<(), SupervisedError> {
        self.gate()?;
        let _transaction = self.subscription_transaction.lock().await;
        self.gate()?;
        self.connection()
            .await?
            .call_with_headers("noded", "topic.subscribe", &topic_headers(topic), "")
            .await
            .map_err(SupervisedError::Transport)?;
        self.registry.record(topic);
        Ok(())
    }

    /// Unsubscribe from a topic and forget it, so a later reconnect does not
    /// re-subscribe a deliberately dropped topic. The entry is removed only
    /// after an `rc 0` unsubscribe; a failed one leaves the topic recorded,
    /// which is the safe direction (over-deliver rather than go silently
    /// deaf).
    pub async fn unsubscribe_topic(&self, topic: &str) -> Result<(), SupervisedError> {
        self.gate()?;
        let _transaction = self.subscription_transaction.lock().await;
        self.gate()?;
        self.connection()
            .await?
            .call_with_headers("noded", "topic.unsubscribe", &topic_headers(topic), "")
            .await
            .map_err(SupervisedError::Transport)?;
        self.registry.remove(topic);
        Ok(())
    }

    /// Stop supervising and close the current socket, including its reader
    /// task. Idempotent and best effort.
    pub async fn close(&self) {
        publish_state(&self.state_tx, &self.state_publish, ConnState::ShuttingDown);
        let _ = self.shutdown_tx.send(true);
        // Close the published socket before joining the supervisor: a bounded
        // caller may abandon this method if the supervisor is wedged, and the
        // detached reader must not keep the registered name alive in that
        // case. A reconnect that completes after the stop edge closes its
        // fresh connection before it can swap it in.
        if let Ok(connection) = self.connection().await {
            connection.close().await;
        }
        if let Some(handle) = self.supervisor.lock().await.take() {
            let _ = handle.await;
        }
    }

    pub async fn shutdown(&self) {
        publish_state(&self.state_tx, &self.state_publish, ConnState::ShuttingDown);
        let _ = self.shutdown_tx.send(true);
        if let Some(handle) = self.supervisor.lock().await.take() {
            let _ = handle.await;
        }
    }

    /// Graceful deregister: mark `ShuttingDown` to fence reconnect publication,
    /// issue `noded.deregister` on the retained connection, then stop and join
    /// the supervisor. If the connection is
    /// already down the broker dropped the name on socket close, and this
    /// reports [`SupervisedError::Disconnected`]: already gone, carry on.
    pub async fn deregister(&self) -> Result<(), SupervisedError> {
        let result = self.deregister_for_drain().await;
        self.close().await;
        result
    }

    /// Remove the broker registration while retaining the delivered-generation
    /// transport for a bounded shutdown reply drain. Ordinary outbound work and
    /// reconnect publication are fenced immediately. The caller must finish
    /// with [`Self::close`], including after an RPC error. Cancelling this future
    /// stops the supervisor and closes the retained transport.
    pub async fn deregister_for_drain(&self) -> Result<(), SupervisedError> {
        publish_state(&self.state_tx, &self.state_publish, ConnState::ShuttingDown);
        // Cancellation must still wake terminal cleanup; signalling before
        // the RPC would let that cleanup close its transport prematurely.
        let mut stop = StopOnDrop(Some(&self.shutdown_tx));
        let connection = self.connection().await?;
        let result = if connection.is_connected() {
            connection
                .deregister()
                .await
                .map_err(SupervisedError::Transport)
        } else {
            Err(SupervisedError::Disconnected)
        };
        stop.0 = None;
        result
    }
}

impl Drop for SupervisedClient {
    fn drop(&mut self) {
        // Stop a supervisor that would otherwise reconnect forever after the
        // handle is dropped without an explicit close.
        publish_state(&self.state_tx, &self.state_publish, ConnState::ShuttingDown);
        let _ = self.shutdown_tx.send(true);
    }
}

fn topic_headers(topic: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("name".to_string(), topic.to_string())])
}

struct StopOnDrop<'a>(Option<&'a watch::Sender<bool>>);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        if let Some(sender) = self.0 {
            let _ = sender.send(true);
        }
    }
}

enum SupervisorOutgoing {
    Unbounded(mpsc::UnboundedSender<IncomingCommand>),
    Bounded(BoundedIncomingSender),
}

impl SupervisorOutgoing {
    async fn closed(&self) {
        match self {
            Self::Unbounded(sender) => sender.closed().await,
            Self::Bounded(sender) => sender.closed().await,
        }
    }

    fn is_closed(&self) -> bool {
        match self {
            Self::Unbounded(sender) => sender.is_closed(),
            Self::Bounded(sender) => sender.is_closed(),
        }
    }

    fn forward(&self, event: BoundedIncomingEvent) -> bool {
        match (self, event) {
            (Self::Unbounded(sender), BoundedIncomingEvent::Command(command)) => {
                sender.send(command).is_ok()
            }
            (Self::Unbounded(_), BoundedIncomingEvent::Overflow { .. }) => {
                unreachable!("unbounded native lane cannot overflow")
            }
            (Self::Bounded(sender), BoundedIncomingEvent::Command(command)) => {
                sender.try_send(command)
            }
            (Self::Bounded(sender), BoundedIncomingEvent::Overflow { dropped }) => {
                sender.record_overflow(dropped);
                true
            }
        }
    }
}

/// Everything the detached supervisor task owns.
struct SupervisorCtx {
    inner: Arc<RwLock<Option<Arc<Connection>>>>,
    state_tx: watch::Sender<ConnState>,
    state_publish: Arc<std::sync::Mutex<()>>,
    registration_rejection: Arc<std::sync::Mutex<Option<RegistrationRejected>>>,
    declaration_error: Arc<std::sync::Mutex<Option<SubscriptionDeclarationError>>>,
    connection_generation: Arc<AtomicU64>,
    registry: SubscriptionRegistry,
    subscription_transaction: Arc<TokioMutex<()>>,
    out_tx: SupervisorOutgoing,
    shutdown_rx: watch::Receiver<bool>,
    service_name: String,
    noded_url: String,
    fatal_on_registration_rejection: bool,
    connection_options: ConnectionOptions,
    /// The original declared identities, retained solely to classify a later
    /// replay refusal of the same name; never unioned back into a snapshot.
    declarations: Vec<String>,
    /// Whole-attempt establishment deadline duration.
    establishment_timeout: Duration,
    /// Finite-connect establishment result; `None` for `start()`.
    initial_result: Option<oneshot::Sender<Result<(), SupervisedError>>>,
}

impl SupervisorCtx {
    /// The topics this attempt must establish: on generation zero the stable
    /// union of declarations first, then acknowledged registry entries not
    /// already present; on later generations the acknowledged registry alone,
    /// so a successfully unsubscribed declaration stays unsubscribed.
    fn attempt_topics(&self) -> Vec<String> {
        let recorded = self.registry.snapshot();
        if self.connection_generation.load(Ordering::SeqCst) != 0 {
            return recorded;
        }
        let mut topics = self.declarations.clone();
        for topic in recorded {
            if !topics.iter().any(|t| t == &topic) {
                topics.push(topic);
            }
        }
        topics
    }
}

/// `true` once a stop has been requested (an explicit shutdown, or the
/// `SupervisedClient` and so the watch sender was dropped).
fn stop_requested(rx: &watch::Receiver<bool>) -> bool {
    *rx.borrow()
}

/// Publish a state edge. Terminal states (`ShuttingDown`, `Fatal`) are never
/// overwritten, so a reconnect that lands during a shutdown cannot flip the
/// client back to `Connected`.
fn publish_state(
    state_tx: &watch::Sender<ConnState>,
    state_publish: &std::sync::Mutex<()>,
    next: ConnState,
) {
    let _publish = state_publish
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous = *state_tx.borrow();
    if matches!(previous, ConnState::ShuttingDown | ConnState::Fatal) {
        return;
    }
    if previous != next {
        state_tx.send_replace(next);
    }
}

fn publish_registration_rejection(ctx: &SupervisorCtx, error: &ClientError) {
    let Some(rejection) = error.registration_rejection_typed() else {
        return;
    };
    // One protocol-bounded diagnostic, never a history of failed attempts.
    // Use the same publication fence as terminal lifecycle transitions so
    // observing Fatal guarantees the diagnostic is already sampleable.
    let _publish = ctx
        .state_publish
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if matches!(
        *ctx.state_tx.borrow(),
        ConnState::ShuttingDown | ConnState::Fatal
    ) {
        return;
    }
    *ctx.registration_rejection
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(rejection.clone());
    ctx.state_tx.send_replace(ConnState::Fatal);
}

/// Publish a declared-subscription terminal diagnostic and `Fatal` under the
/// same fence as every other terminal transition, so a watch observer never
/// sees an empty declaration diagnostic after `Fatal`.
fn publish_declaration_rejection(ctx: &SupervisorCtx, error: &SubscriptionDeclarationError) {
    let _publish = ctx
        .state_publish
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if matches!(
        *ctx.state_tx.borrow(),
        ConnState::ShuttingDown | ConnState::Fatal
    ) {
        return;
    }
    *ctx.declaration_error
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(error.clone());
    ctx.state_tx.send_replace(ConnState::Fatal);
}

/// Deliver a terminal failure to a finite connect and stop the supervisor.
///
/// `attempts` keeps the pre-change finite loop's counting exactly: an
/// in-attempt terminal (a fatal registration rejection) reports the 1-based
/// attempt index (`attempt + 1` for the 0-based `attempt`), and budget
/// exhaustion always reports [`MAX_INITIAL_ATTEMPTS`] — the loop's post-loop
/// value. The count is dial/establishment attempts, never subscribe count.
fn fail_initial(ctx: &mut SupervisorCtx, attempts: u32, source: ClientError) {
    if let Some(result) = ctx.initial_result.take() {
        let _ = result.send(Err(SupervisedError::InitialConnectFailed {
            attempts,
            source,
        }));
    }
}

/// Deliver the finite-connect success. `false` means the waiting caller has
/// cancelled: the supervisor must stop and close the freshly published
/// connection instead of pumping frames nobody consumes.
fn complete_initial(ctx: &mut SupervisorCtx) -> bool {
    match ctx.initial_result.take() {
        Some(result) => result.send(Ok(())).is_ok(),
        None => true,
    }
}

/// Owner of a connection from dial until it is either published under
/// `inner` or torn down. Every exit after acquisition goes through
/// [`abort`](Self::abort), which bounds the native close so a blocked close
/// frame cannot stall teardown, a later cancellation or a finite completion.
struct AttemptOwner {
    connection: Option<Arc<Connection>>,
}

impl AttemptOwner {
    fn new(connection: Arc<Connection>) -> Self {
        Self {
            connection: Some(connection),
        }
    }

    async fn abort(&mut self) {
        if let Some(connection) = self.connection.take() {
            let _ = tokio::time::timeout(ATTEMPT_CLOSE_BOUND, connection.close()).await;
        }
    }
}

/// Why the fenced publication was refused.
enum PublishBlock {
    /// A stop (shutdown, consumer loss, terminal state): the supervisor ends.
    Stop,
    /// A transient establishment failure: retry on a fresh socket.
    Retry(ClientError),
}

enum EstablishOutcome {
    /// Published: generation advanced, `Connected` published, the forward
    /// phase takes this receiver.
    Published(NativeIncomingReceiver),
    /// Transient failure: back off and retry on a fresh socket.
    Retry,
    /// The supervisor must stop.
    Stop,
}

fn attempt_failed(
    ctx: &SupervisorCtx,
    attempt: u32,
    phase: &str,
    topic: Option<&str>,
    error: &ClientError,
) {
    tracing::warn!(
        event = "supervised_establish_failed",
        service = %ctx.service_name,
        attempt = attempt + 1,
        phase,
        topic = topic.unwrap_or(""),
        error = %error,
        "establishment attempt failed; retrying on a fresh socket"
    );
}

/// Classify a transient attempt failure against the initial policy: retry,
/// or deliver the finite budget exhaustion and stop. A deadline is a
/// [`ClientError::Timeout`], never a subscription refusal — `is_connected`
/// is deliberately still true on the native client until close.
///
/// `attempt` is the 0-based index of the failing attempt; exhaustion means
/// `attempt + 1 >= MAX_INITIAL_ATTEMPTS`, reported as exactly
/// [`MAX_INITIAL_ATTEMPTS`] — the pre-change finite loop's own post-loop
/// value, so legacy callers see the same count.
fn retry_or_fail(
    ctx: &mut SupervisorCtx,
    attempt: u32,
    error: ClientError,
    phase: &str,
    topic: Option<&str>,
) -> EstablishOutcome {
    attempt_failed(ctx, attempt, phase, topic, &error);
    if ctx.initial_result.is_some() && attempt + 1 >= MAX_INITIAL_ATTEMPTS {
        fail_initial(ctx, MAX_INITIAL_ATTEMPTS, error);
        EstablishOutcome::Stop
    } else {
        EstablishOutcome::Retry
    }
}

/// An explicit nonzero declared-topic reply is terminal: the wire has no
/// reliable transient/permanent refusal classifier, so the exact topic, rc
/// and diagnostic are published once and never retried.
fn declaration_terminal(
    ctx: &mut SupervisorCtx,
    error: SubscriptionDeclarationError,
) -> EstablishOutcome {
    publish_declaration_rejection(ctx, &error);
    if let Some(result) = ctx.initial_result.take() {
        let _ = result.send(Err(SupervisedError::SubscriptionDeclaration(error)));
    }
    EstablishOutcome::Stop
}

/// One establishment attempt: dial, register, declarations, replay and the
/// fenced publication. No failed attempt changes the generation, the live
/// inner connection or the acknowledged registry.
async fn establish_attempt(ctx: &mut SupervisorCtx, attempt: u32) -> EstablishOutcome {
    let deadline = match Instant::now().checked_add(ctx.establishment_timeout) {
        Some(deadline) => deadline,
        None => {
            return retry_or_fail(
                ctx,
                attempt,
                ClientError::Timeout {
                    to: "noded".to_string(),
                },
                "deadline",
                None,
            );
        }
    };
    tracing::debug!(
        event = "supervised_establish_attempt",
        service = %ctx.service_name,
        attempt = attempt + 1,
        "dialing and establishing on a fresh socket"
    );

    // 1. Dial and register, bounded by the whole-attempt deadline and
    //    selected against shutdown and consumer closure.
    let result = tokio::select! {
        biased;
        _ = ctx.out_tx.closed() => return EstablishOutcome::Stop,
        _ = ctx.shutdown_rx.changed() => return EstablishOutcome::Stop,
        _ = tokio::time::sleep_until(deadline) => {
            return retry_or_fail(
                ctx, attempt,
                ClientError::Timeout { to: "noded".to_string() },
                "dial", None,
            );
        }
        result = Connection::connect_with_options(
            &ctx.service_name, &ctx.noded_url, &ctx.connection_options,
        ) => result,
    };
    let connection = match result {
        Ok(connection) => connection,
        Err(error) => {
            if ctx.fatal_on_registration_rejection && error.registration_rejection_typed().is_some()
            {
                publish_registration_rejection(ctx, &error);
                tracing::debug!(
                    event = "supervised_registration_rejected",
                    service = %ctx.service_name,
                    error = %error,
                    "service registration was rejected; supervisor stopped"
                );
                fail_initial(ctx, attempt + 1, error);
                return EstablishOutcome::Stop;
            }
            return retry_or_fail(ctx, attempt, error, "dial", None);
        }
    };

    // 2. Explicit unpublished-attempt owner: every exit from here on closes.
    let connection = Arc::new(connection);
    let mut owner = AttemptOwner::new(connection.clone());

    // 3. Transaction mutex, preserving the old-socket ACK / new-socket
    //    snapshot ordering, selected against stop and the deadline.
    let _transaction = tokio::select! {
        biased;
        _ = ctx.out_tx.closed() => {
            owner.abort().await;
            return EstablishOutcome::Stop;
        }
        _ = ctx.shutdown_rx.changed() => {
            owner.abort().await;
            return EstablishOutcome::Stop;
        }
        _ = tokio::time::sleep_until(deadline) => {
            owner.abort().await;
            return retry_or_fail(
                ctx, attempt,
                ClientError::Timeout { to: "noded".to_string() },
                "transaction", None,
            );
        }
        guard = ctx.subscription_transaction.lock() => guard,
    };

    // 4. The topics this attempt must establish.
    let topics = ctx.attempt_topics();

    // 5. Sequential subscribes. Declared identities require an exact rc 0;
    //    any explicit nonzero reply is terminal. Ordinary replay keeps the
    //    legacy success/refusal interpretation and retry policy. ACKed
    //    declarations are staged locally; the registry is only mutated at
    //    publication.
    let mut staged: Vec<String> = Vec::new();
    for topic in &topics {
        let declared = ctx.declarations.iter().any(|d| d == topic);
        let headers = topic_headers(topic);
        let reply = tokio::select! {
            biased;
            _ = ctx.out_tx.closed() => {
                owner.abort().await;
                return EstablishOutcome::Stop;
            }
            _ = ctx.shutdown_rx.changed() => {
                owner.abort().await;
                return EstablishOutcome::Stop;
            }
            _ = tokio::time::sleep_until(deadline) => {
                owner.abort().await;
                return retry_or_fail(
                    ctx, attempt,
                    ClientError::Timeout { to: "noded".to_string() },
                    "subscribe", Some(topic),
                );
            }
            result = connection.call_with_headers_raw(
                "noded", "topic.subscribe", &headers, "",
            ) => result,
        };
        match reply {
            // An explicit nonzero declared reply is terminal: the wire has no
            // reliable transient/permanent refusal classifier. A warning rc
            // is a refusal too — a declared topic needs an exact rc 0.
            Ok((rc, body, error_header)) if declared && rc != crate::RC_SUCCESS => {
                let error = SubscriptionDeclarationError::Rejected {
                    topic: topic.clone(),
                    rc,
                    message: reply_error_message(&body, error_header),
                };
                owner.abort().await;
                return declaration_terminal(ctx, error);
            }
            // Ordinary replay keeps the legacy success interpretation: any
            // rc < 10 (a warning rc 5 included) is a successful subscription.
            Ok((rc, _, _)) if rc < crate::RC_ERROR => {
                staged.push(topic.clone());
            }
            // rc >= 10 on an ordinary topic: the legacy refusal contract,
            // retried on a fresh socket.
            Ok((rc, body, error_header)) => {
                let error = ClientError::Refused {
                    rc,
                    message: reply_error_message(&body, error_header),
                };
                owner.abort().await;
                return retry_or_fail(ctx, attempt, error, "replay", Some(topic));
            }
            Err(error) => {
                owner.abort().await;
                return retry_or_fail(ctx, attempt, error, "subscribe", Some(topic));
            }
        }
    }

    // 8. Pre-publication checks: a stop, receiver closure, elapsed deadline
    //    or a dropped native connection during the last ACK must not publish.
    if stop_requested(&ctx.shutdown_rx) || ctx.out_tx.is_closed() {
        owner.abort().await;
        return EstablishOutcome::Stop;
    }
    if Instant::now() >= deadline {
        owner.abort().await;
        return retry_or_fail(
            ctx,
            attempt,
            ClientError::Timeout {
                to: "noded".to_string(),
            },
            "publication",
            None,
        );
    }
    if !connection.is_connected() {
        owner.abort().await;
        return retry_or_fail(ctx, attempt, ClientError::Closed, "publication", None);
    }

    // The native lane (retained publications/commands from this attempt)
    // either publishes with this generation or is discarded with the
    // connection: there is no staging queue. An absent receiver is an
    // internal invariant failure: close the attempt through the owner and
    // retry/fail as a typed Closed rather than leaking the socket behind a
    // panic.
    let Some(rx) = connection.take_native_incoming() else {
        owner.abort().await;
        return retry_or_fail(ctx, attempt, ClientError::Closed, "publication", None);
    };

    let mut live = tokio::select! {
        biased;
        _ = ctx.out_tx.closed() => {
            owner.abort().await;
            return EstablishOutcome::Stop;
        }
        _ = ctx.shutdown_rx.changed() => {
            owner.abort().await;
            return EstablishOutcome::Stop;
        }
        _ = tokio::time::sleep_until(deadline) => {
            owner.abort().await;
            return retry_or_fail(
                ctx, attempt,
                ClientError::Timeout { to: "noded".to_string() },
                "publication", None,
            );
        }
        guard = ctx.inner.write() => guard,
    };

    // 9. Commit the staged declarations, swap the live connection, advance
    //    the generation once and publish Connected in one fenced section,
    //    re-checking stop, deadline and the actual socket inside.
    let blocked = {
        let _fence = ctx
            .state_publish
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let current = *ctx.state_tx.borrow();
        if matches!(current, ConnState::ShuttingDown | ConnState::Fatal)
            || stop_requested(&ctx.shutdown_rx)
            || ctx.out_tx.is_closed()
        {
            Some(PublishBlock::Stop)
        } else if Instant::now() >= deadline {
            Some(PublishBlock::Retry(ClientError::Timeout {
                to: "noded".to_string(),
            }))
        } else if !connection.is_connected() {
            Some(PublishBlock::Retry(ClientError::Closed))
        } else {
            for topic in &staged {
                ctx.registry.record(topic);
            }
            *live = Some(connection.clone());
            ctx.connection_generation.fetch_add(1, Ordering::SeqCst);
            ctx.state_tx.send_replace(ConnState::Connected);
            None
        }
    };
    drop(live);
    match blocked {
        Some(PublishBlock::Stop) => {
            owner.abort().await;
            EstablishOutcome::Stop
        }
        Some(PublishBlock::Retry(error)) => {
            owner.abort().await;
            retry_or_fail(ctx, attempt, error, "publication", None)
        }
        None => {
            tracing::info!(
                event = "supervised_established",
                service = %ctx.service_name,
                attempt = attempt + 1,
                subscriptions = topics.len(),
                "registered and subscribed (declarations and replay complete)"
            );
            // 10. Notify the finite caller; if it cancelled, stop and close
            //     the published connection rather than pump frames nobody
            //     consumes.
            if !complete_initial(ctx) {
                return EstablishOutcome::Stop;
            }
            EstablishOutcome::Published(rx)
        }
    }
}

async fn supervisor_loop(mut ctx: SupervisorCtx) {
    supervisor_run(&mut ctx).await;
    publish_state(&ctx.state_tx, &ctx.state_publish, ConnState::ShuttingDown);
    let connection = ctx.inner.read().await.clone();
    if let Some(connection) = connection {
        let _ = tokio::time::timeout(ATTEMPT_CLOSE_BOUND, connection.close()).await;
    }
}

async fn supervisor_run(ctx: &mut SupervisorCtx) {
    let mut current_rx: Option<NativeIncomingReceiver> = None;
    let mut attempt: u32 = 0;
    loop {
        // Forward phase: pump the live connection's frames outward until it
        // drops or a stop is requested.
        while let Some(receiver) = current_rx.as_mut() {
            tokio::select! {
                _ = ctx.out_tx.closed() => return,
                changed = ctx.shutdown_rx.changed() => {
                    // An `Err` means the sender (the client) is gone: stop.
                    if changed.is_err() || stop_requested(&ctx.shutdown_rx) {
                        tracing::info!(
                            event = "supervised_stop",
                            service = %ctx.service_name,
                            "supervisor stopping (shutdown requested)"
                        );
                        return;
                    }
                }
                maybe = receiver.recv() => {
                    match maybe {
                        Some(mut command) => {
                            if let BoundedIncomingEvent::Command(incoming) = &mut command {
                                incoming.generation = ctx.connection_generation.load(Ordering::SeqCst);
                            }
                            if !ctx.out_tx.forward(command) {
                                tracing::info!(
                                    event = "supervised_stop",
                                    service = %ctx.service_name,
                                    "supervisor stopping (incoming consumer dropped)"
                                );
                                return;
                            }
                        }
                        None => break,
                    }
                }
            }
        }

        if stop_requested(&ctx.shutdown_rx)
            || matches!(
                *ctx.state_tx.borrow(),
                ConnState::ShuttingDown | ConnState::Fatal
            )
        {
            return;
        }

        // Establishment phase. A transport failure after a live generation
        // is Disconnected; the initial Connecting state stays until the
        // first full establishment or a terminal outcome.
        let initial = ctx.connection_generation.load(Ordering::SeqCst) == 0;
        if !initial {
            publish_state(&ctx.state_tx, &ctx.state_publish, ConnState::Disconnected);
        }
        tracing::warn!(
            event = "supervised_disconnect",
            service = %ctx.service_name,
            "broker connection lost; reconnecting (unbounded backoff)"
        );

        let delay = if attempt == 0 {
            Duration::ZERO
        } else {
            backoff_delay(attempt - 1)
        };
        tokio::select! {
            _ = ctx.out_tx.closed() => return,
            changed = ctx.shutdown_rx.changed() => {
                if changed.is_err() || stop_requested(&ctx.shutdown_rx) {
                    return;
                }
            }
            _ = tokio::time::sleep(delay) => {}
        }
        if stop_requested(&ctx.shutdown_rx)
            || matches!(
                *ctx.state_tx.borrow(),
                ConnState::ShuttingDown | ConnState::Fatal
            )
        {
            return;
        }

        match establish_attempt(ctx, attempt).await {
            EstablishOutcome::Published(rx) => {
                attempt = 0;
                current_rx = Some(rx);
            }
            EstablishOutcome::Retry => {
                attempt += 1;
            }
            EstablishOutcome::Stop => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_ceiling_is_monotonic_and_capped() {
        assert_eq!(backoff_ceiling_ms(0), 250);
        assert_eq!(backoff_ceiling_ms(1), 500);
        assert_eq!(backoff_ceiling_ms(2), 1_000);
        assert_eq!(backoff_ceiling_ms(7), 30_000);
        assert_eq!(backoff_ceiling_ms(8), 30_000);
        assert_eq!(backoff_ceiling_ms(64), 30_000);
        assert_eq!(backoff_ceiling_ms(u32::MAX), 30_000);
        let mut prev = 0;
        for a in 0..40 {
            let c = backoff_ceiling_ms(a);
            assert!(c >= prev, "ceiling regressed at attempt {a}");
            prev = c;
        }
    }

    #[test]
    fn backoff_delay_within_full_jitter_window() {
        for attempt in 0..12 {
            let ceiling = backoff_ceiling_ms(attempt);
            for _ in 0..256 {
                let d = backoff_delay(attempt).as_millis() as u64;
                assert!(
                    d <= ceiling,
                    "delay {d} exceeded ceiling {ceiling} at attempt {attempt}"
                );
            }
        }
    }

    #[test]
    fn jitter_varies_between_draws() {
        let draws: std::collections::BTreeSet<u64> =
            (0..64).map(|_| jitter_below(1_000_000)).collect();
        assert!(draws.len() > 1, "consecutive draws must not all coincide");
        assert_eq!(jitter_below(0), 0);
    }

    #[test]
    fn registry_preserves_order_and_dedups() {
        let r = SubscriptionRegistry::new();
        assert!(r.is_empty());
        assert!(r.record("a"));
        assert!(r.record("b"));
        assert!(r.record("c"));
        assert!(!r.record("b"), "duplicate must be a no-op");
        assert_eq!(r.snapshot(), vec!["a", "b", "c"]);
        assert_eq!(r.len(), 3);

        assert!(r.remove("b"));
        assert!(!r.remove("b"), "removing an absent topic is false");
        assert_eq!(r.snapshot(), vec!["a", "c"]);

        // Re-recording after removal appends: replay order is the order the
        // current set was (re-)established in.
        assert!(r.record("b"));
        assert_eq!(r.snapshot(), vec!["a", "c", "b"]);
    }

    #[test]
    fn registry_handle_is_shared() {
        let r1 = SubscriptionRegistry::new();
        let r2 = r1.clone();
        r1.record("x");
        assert_eq!(r2.snapshot(), vec!["x"], "clones share storage");
        r2.remove("x");
        assert!(r1.is_empty());
    }

    #[test]
    fn state_publication_cannot_overwrite_shutdown_with_reconnect() {
        let (state_tx, state_rx) = watch::channel(ConnState::Connected);
        let publish = Arc::new(std::sync::Mutex::new(()));

        let shutdown_tx = state_tx.clone();
        let shutdown_publish = publish.clone();
        let shutdown = std::thread::spawn(move || {
            publish_state(&shutdown_tx, &shutdown_publish, ConnState::ShuttingDown);
        });
        let reconnect_tx = state_tx.clone();
        let reconnect_publish = publish.clone();
        let reconnect = std::thread::spawn(move || {
            publish_state(&reconnect_tx, &reconnect_publish, ConnState::Connected);
        });
        shutdown.join().expect("shutdown publisher");
        reconnect.join().expect("reconnect publisher");

        assert_eq!(*state_rx.borrow(), ConnState::ShuttingDown);
    }

    #[test]
    fn declarations_validate_dedupe_and_keep_first_seen_order() {
        let topics: Vec<String> = ["a", "b", "a", "c", "b"]
            .iter()
            .map(|t| t.to_string())
            .collect();
        assert_eq!(validate_declarations(&topics).unwrap(), vec!["a", "b", "c"]);
        assert_eq!(validate_declarations(&[]).unwrap(), Vec::<String>::new());
        // Names are exact: no trimming, no case folding.
        let spaced: Vec<String> = ["x ", " x", "X"].iter().map(|t| t.to_string()).collect();
        assert_eq!(validate_declarations(&spaced).unwrap(), spaced);
    }

    #[test]
    fn declarations_reject_bad_entries_with_the_reported_index() {
        for bad in ["", "a\rb", "a\nb", "a\0b"] {
            // Every case places the bad topic second: the reported index is 1.
            let topics = vec!["ok".to_string(), bad.to_string()];
            let err = validate_declarations(&topics).unwrap_err();
            match err {
                SubscriptionDeclarationError::Invalid { index, message } => {
                    assert_eq!(index, Some(1), "entry {bad:?}");
                    assert!(!message.is_empty());
                }
                other => panic!("expected Invalid, got {other:?}"),
            }
        }
    }

    #[test]
    fn declarations_enforce_entry_and_byte_bounds_on_raw_input() {
        // 65 raw entries, duplicates included: the count bound is pre-dedup.
        let too_many: Vec<String> = (0..65).map(|i| format!("topic.{i}")).collect();
        assert!(matches!(
            validate_declarations(&too_many),
            Err(SubscriptionDeclarationError::Invalid { index: None, .. })
        ));
        // Exactly 64 entries is fine.
        let boundary: Vec<String> = (0..64).map(|i| format!("topic.{i}")).collect();
        assert_eq!(validate_declarations(&boundary).unwrap().len(), 64);
        // One oversized topic.
        let big = "x".repeat(MAX_TOPIC_BYTES + 1);
        assert_eq!(
            validate_declarations(&[big.clone()]).unwrap_err(),
            SubscriptionDeclarationError::Invalid {
                index: Some(0),
                message: format!("a topic exceeds {MAX_TOPIC_BYTES} UTF-8 bytes"),
            }
        );
        assert!(validate_declarations(&["x".repeat(MAX_TOPIC_BYTES)]).is_ok());
        // The aggregate bound counts raw bytes: 64 topics x 300 bytes is
        // fine per-topic but exceeds 16 KiB in total.
        let aggregate: Vec<String> = (0..64)
            .map(|i| format!("{i:03}-{}", "x".repeat(300)))
            .collect();
        assert!(matches!(
            validate_declarations(&aggregate),
            Err(SubscriptionDeclarationError::Invalid { index: None, .. })
        ));
        // Duplicates cannot bypass the aggregate bound: 40 identical
        // 900-byte entries are 36 KiB raw, though the deduplicated set is
        // one small valid topic.
        let dup_heavy: Vec<String> = (0..40).map(|_| "z".repeat(900)).collect();
        assert!(matches!(
            validate_declarations(&dup_heavy),
            Err(SubscriptionDeclarationError::Invalid { index: None, .. })
        ));
    }

    // ── Establishment cancellation guards ────────────────────────────────
    //
    // These drive the real `establish_attempt` against a real local native
    // WebSocket stub, holding the exact private await the guard protects, so
    // the cancellation is exercised deterministically rather than raced.

    /// A one-connection native stub: ACK `noded.register` and each
    /// `topic.subscribe` with rc 0, signal each receipt, and signal the
    /// socket's close/EOF so the tests observe the owner's teardown.
    async fn mini_stub(
        listener: tokio::net::TcpListener,
        register_seen: oneshot::Sender<()>,
        subscribe_seen: Option<oneshot::Sender<()>>,
        eof: oneshot::Sender<()>,
    ) {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::accept_async;
        use tokio_tungstenite::tungstenite::Message;
        let (socket, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(socket).await.unwrap();
        loop {
            let text = match websocket.next().await {
                Some(Ok(Message::Text(text))) => text.to_string(),
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => continue,
                Some(Err(_)) => break,
            };
            let Ok(request) = crate::parse(&text) else {
                continue;
            };
            let command = request.get("command").unwrap_or("").to_string();
            let mut reply = BusMessage::new()
                .with_header("type", "response")
                .with_header("command", &command)
                .with_header("from", "noded")
                .with_header("rc", "0");
            if let Some(id) = request.get("id") {
                reply = reply.with_header("id", id);
            }
            let send_ok = websocket.send(Message::Text(reply.to_wire().into())).await.is_ok();
            if command == "noded.register" {
                let _ = register_seen.send(());
            } else if command == "topic.subscribe" {
                if let Some(seen) = &subscribe_seen {
                    let _ = seen.send(());
                }
            }
            if !send_ok {
                break;
            }
        }
        let _ = eof.send(());
    }

    /// A supervisor context wired for the guard tests, plus the shutdown
    /// sender and the outward receiver (held so the producer never closes).
    fn guard_test_ctx(
        noded_url: &str,
        declarations: Vec<String>,
    ) -> (
        SupervisorCtx,
        watch::Sender<bool>,
        mpsc::UnboundedReceiver<IncomingCommand>,
    ) {
        let (state_tx, _) = watch::channel(ConnState::Connecting);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        (
            SupervisorCtx {
                inner: Arc::new(RwLock::new(None)),
                state_tx,
                state_publish: Arc::new(std::sync::Mutex::new(())),
                registration_rejection: Arc::new(std::sync::Mutex::new(None)),
                declaration_error: Arc::new(std::sync::Mutex::new(None)),
                connection_generation: Arc::new(AtomicU64::new(0)),
                registry: SubscriptionRegistry::new(),
                subscription_transaction: Arc::new(TokioMutex::new(())),
                out_tx: SupervisorOutgoing::Unbounded(out_tx),
                shutdown_rx,
                service_name: "guard-test".to_string(),
                noded_url: noded_url.to_string(),
                fatal_on_registration_rejection: false,
                connection_options: ConnectionOptions::default(),
                declarations,
                establishment_timeout: Duration::from_secs(60),
                initial_result: None,
            },
            shutdown_tx,
            out_rx,
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn transaction_lock_wait_shutdown_closes_the_attempt() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (register_seen_tx, register_seen_rx) = oneshot::channel();
        let (eof_tx, eof_rx) = oneshot::channel();
        let stub = tokio::spawn(mini_stub(listener, register_seen_tx, None, eof_tx));

        let (mut ctx, shutdown_tx, _out_rx) =
            guard_test_ctx(&format!("ws://{address}/ws"), vec!["decl.a".to_string()]);
        let state_rx = ctx.state_tx.subscribe();
        let inner = ctx.inner.clone();
        let registry = ctx.registry.clone();
        let generation = ctx.connection_generation.clone();
        // Hold the transaction lock: the attempt can never pass the lock
        // wait, so the shutdown that resolves it is consumed exactly there.
        let transaction = ctx.subscription_transaction.clone();
        let held = transaction.lock().await;
        let handle = tokio::spawn(async move { establish_attempt(&mut ctx, 0).await });

        // Registration was received; with the lock held the attempt cannot
        // pass the lock wait, so the shutdown that resolves the attempt is
        // consumed by the lock-wait select itself (or, if the register ACK
        // has not yet been processed, by the equally guarded dial select —
        // both abort the attempt).
        register_seen_rx.await.unwrap();
        shutdown_tx.send(true).unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("the lock wait must resolve on shutdown, not hang")
            .expect("attempt task joined");
        assert!(matches!(outcome, EstablishOutcome::Stop));
        drop(held);

        // Nothing was published and the owner closed the socket.
        assert_eq!(*state_rx.borrow(), ConnState::Connecting);
        assert!(inner.read().await.is_none());
        assert!(registry.is_empty());
        assert_eq!(generation.load(Ordering::SeqCst), 0);
        tokio::time::timeout(Duration::from_secs(5), eof_rx)
            .await
            .expect("the unpublished attempt must close its socket")
            .unwrap();
        stub.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn publication_barrier_deadline_retries_without_publishing() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (register_seen_tx, register_seen_rx) = oneshot::channel();
        let (subscribe_seen_tx, subscribe_seen_rx) = oneshot::channel();
        let (eof_tx, eof_rx) = oneshot::channel();
        let stub = tokio::spawn(mini_stub(
            listener,
            register_seen_tx,
            Some(subscribe_seen_tx),
            eof_tx,
        ));

        let (mut ctx, _shutdown_tx, _out_rx) =
            guard_test_ctx(&format!("ws://{address}/ws"), vec!["decl.a".to_string()]);
        ctx.establishment_timeout = Duration::from_secs(3);
        let state_rx = ctx.state_tx.subscribe();
        let inner = ctx.inner.clone();
        let registry = ctx.registry.clone();
        let generation = ctx.connection_generation.clone();
        // Hold the publication barrier: once the subscribe ACKs, the only
        // await left is the live-write-lock wait (the section between the
        // ACK and that await is synchronous), so the attempt deadline is
        // consumed by the publication select itself.
        let inner_arc = ctx.inner.clone();
        let write_guard = inner_arc.write().await;
        let handle = tokio::spawn(async move { establish_attempt(&mut ctx, 0).await });

        register_seen_rx.await.unwrap();
        subscribe_seen_rx.await.unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(10), handle)
            .await
            .expect("the publication barrier must yield to the attempt deadline")
            .expect("attempt task joined");
        assert!(matches!(outcome, EstablishOutcome::Retry));
        drop(write_guard);

        assert_eq!(*state_rx.borrow(), ConnState::Connecting);
        assert!(inner.read().await.is_none());
        assert!(
            registry.is_empty(),
            "staged declarations never commit on a blocked publication"
        );
        assert_eq!(generation.load(Ordering::SeqCst), 0);
        tokio::time::timeout(Duration::from_secs(5), eof_rx)
            .await
            .expect("the unpublished attempt must close its socket")
            .unwrap();
        stub.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn publication_barrier_shutdown_closes_without_publishing() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (register_seen_tx, register_seen_rx) = oneshot::channel();
        let (subscribe_seen_tx, subscribe_seen_rx) = oneshot::channel();
        let (eof_tx, eof_rx) = oneshot::channel();
        let stub = tokio::spawn(mini_stub(
            listener,
            register_seen_tx,
            Some(subscribe_seen_tx),
            eof_tx,
        ));

        // The default 60s deadline never interferes: only the shutdown
        // resolves the held publication barrier.
        let (mut ctx, shutdown_tx, _out_rx) =
            guard_test_ctx(&format!("ws://{address}/ws"), vec!["decl.a".to_string()]);
        let state_rx = ctx.state_tx.subscribe();
        let inner = ctx.inner.clone();
        let registry = ctx.registry.clone();
        let generation = ctx.connection_generation.clone();
        let inner_arc = ctx.inner.clone();
        let write_guard = inner_arc.write().await;
        let handle = tokio::spawn(async move { establish_attempt(&mut ctx, 0).await });

        register_seen_rx.await.unwrap();
        subscribe_seen_rx.await.unwrap();
        // Yield so the ACKed attempt reaches the write-lock wait; the proof
        // is the held lock plus the bounded join below, not this yield.
        tokio::task::yield_now().await;
        shutdown_tx.send(true).unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("the publication barrier must resolve on shutdown, not hang")
            .expect("attempt task joined");
        assert!(matches!(outcome, EstablishOutcome::Stop));
        drop(write_guard);

        assert_eq!(*state_rx.borrow(), ConnState::Connecting);
        assert!(inner.read().await.is_none());
        assert!(registry.is_empty());
        assert_eq!(generation.load(Ordering::SeqCst), 0);
        tokio::time::timeout(Duration::from_secs(5), eof_rx)
            .await
            .expect("the unpublished attempt must close its socket")
            .unwrap();
        stub.await.unwrap();
    }
}
