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
//! - on every reconnect the [`SubscriptionRegistry`] is replayed in recorded
//!   order before the client reports `Connected`: a service that looks
//!   healthy while deaf on a topic is the failure this guards against;
//! - the outward incoming stream survives reconnects and only ends on a
//!   fatal shutdown;
//! - while disconnected every outbound call fails fast with a typed error.
//!   There is no outbound queue.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::{Mutex as TokioMutex, RwLock, mpsc, watch};

use super::IncomingCommand;
use super::connection::{Connection, ConnectionOptions};
use super::error::{ClientError, RegistrationRejected, SupervisedError};
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
    /// Terminal: the initial budget was exhausted, or the broker rejected the
    /// registration and that was configured as fatal.
    Fatal = 4,
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
pub struct SupervisedConnectOptions {
    service_name: String,
    noded_url: String,
    fatal_on_registration_rejection: bool,
    connection: ConnectionOptions,
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

    /// Connect with these options.
    pub async fn connect(self) -> Result<SupervisedClient, SupervisedError> {
        SupervisedClient::connect_with_options(self).await
    }

    /// Start without waiting for a broker. Transport failures are retried
    /// indefinitely by the same supervisor used for reconnects. The client
    /// initially reports `Connecting`, generation zero, and rejects outbound
    /// work until registration and subscription replay have completed.
    ///
    /// Must be called inside the Tokio runtime that will own this client.
    /// Registration rejection follows the configured fatal policy.
    pub fn start(self) -> SupervisedClient {
        SupervisedClient::launch(self, None)
    }
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
        let mut last_err: Option<ClientError> = None;
        let mut connection: Option<Connection> = None;
        for attempt in 0..MAX_INITIAL_ATTEMPTS {
            match Connection::connect_with_options(
                &options.service_name,
                &options.noded_url,
                &options.connection,
            )
            .await
            {
                Ok(c) => {
                    connection = Some(c);
                    break;
                }
                Err(e) => {
                    tracing::debug!(
                        event = "supervised_initial_attempt_failed",
                        service = %options.service_name,
                        attempt,
                        error = %e,
                        "initial broker connect attempt failed"
                    );
                    if options.fatal_on_registration_rejection
                        && e.registration_rejection().is_some()
                    {
                        return Err(SupervisedError::InitialConnectFailed {
                            attempts: attempt + 1,
                            source: e,
                        });
                    }
                    last_err = Some(e);
                    // No sleep after the final attempt: fail fast.
                    if attempt + 1 < MAX_INITIAL_ATTEMPTS {
                        tokio::time::sleep(backoff_delay(attempt)).await;
                    }
                }
            }
        }

        let connection = match connection {
            Some(c) => c,
            None => {
                return Err(SupervisedError::InitialConnectFailed {
                    attempts: MAX_INITIAL_ATTEMPTS,
                    source: last_err.unwrap_or(ClientError::Closed),
                });
            }
        };

        Ok(Self::launch(options, Some(connection)))
    }

    fn launch(options: SupervisedConnectOptions, connection: Option<Connection>) -> Self {
        let SupervisedConnectOptions {
            service_name,
            noded_url,
            fatal_on_registration_rejection,
            connection: connection_options,
        } = options;
        let established = connection.is_some();
        let (state_tx, _) = watch::channel(if established {
            ConnState::Connected
        } else {
            ConnState::Connecting
        });
        let state_publish = Arc::new(std::sync::Mutex::new(()));
        let registration_rejection = Arc::new(std::sync::Mutex::new(None));
        // The supervisor forwards from the first connection's receiver, and
        // every later one, into the single outward channel.
        let first_rx = connection.as_ref().map(|connection| {
            connection
                .take_native_incoming()
                .expect("a fresh connection has its incoming receiver")
        });

        let inner = Arc::new(RwLock::new(connection.map(Arc::new)));
        let connection_generation = Arc::new(AtomicU64::new(u64::from(established)));
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
            connection_generation: connection_generation.clone(),
            registry: registry.clone(),
            subscription_transaction: subscription_transaction.clone(),
            out_tx,
            shutdown_rx,
            service_name: service_name.clone(),
            noded_url,
            fatal_on_registration_rejection,
            connection_options,
            first_rx,
        }));

        SupervisedClient {
            inner,
            state_tx,
            state_publish,
            registration_rejection,
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
    connection_generation: Arc<AtomicU64>,
    registry: SubscriptionRegistry,
    subscription_transaction: Arc<TokioMutex<()>>,
    out_tx: SupervisorOutgoing,
    shutdown_rx: watch::Receiver<bool>,
    service_name: String,
    noded_url: String,
    fatal_on_registration_rejection: bool,
    connection_options: ConnectionOptions,
    first_rx: Option<NativeIncomingReceiver>,
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

async fn supervisor_loop(mut ctx: SupervisorCtx) {
    let current_rx = ctx.first_rx.take();
    supervisor_run(&mut ctx, current_rx).await;
    publish_state(&ctx.state_tx, &ctx.state_publish, ConnState::ShuttingDown);
    let connection = ctx.inner.read().await.clone();
    if let Some(connection) = connection {
        connection.close().await;
    }
}

async fn supervisor_run(ctx: &mut SupervisorCtx, mut current_rx: Option<NativeIncomingReceiver>) {
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

        // Reconnect phase: unbounded backoff.
        let initial = ctx.connection_generation.load(Ordering::SeqCst) == 0;
        if !initial {
            publish_state(&ctx.state_tx, &ctx.state_publish, ConnState::Disconnected);
        }
        let down_since = Instant::now();
        tracing::warn!(
            event = "supervised_disconnect",
            service = %ctx.service_name,
            "broker connection lost; reconnecting (unbounded backoff)"
        );

        let mut attempt: u32 = 0;
        let new_rx = loop {
            let delay = if initial && attempt == 0 {
                Duration::ZERO
            } else {
                backoff_delay(attempt)
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

            let connected = tokio::select! {
                biased;
                _ = ctx.out_tx.closed() => return,
                _ = ctx.shutdown_rx.changed() => return,
                result = Connection::connect_with_options(
                    &ctx.service_name, &ctx.noded_url, &ctx.connection_options,
                ) => result,
            };
            match connected {
                Ok(connection) => {
                    // A stop requested during the connect must not leave a
                    // registered connection behind: a bare drop would keep
                    // the detached reader, and so the name, alive while
                    // `deregister()` reports `Disconnected`.
                    if stop_requested(&ctx.shutdown_rx) || ctx.out_tx.is_closed() {
                        connection.close().await;
                        return;
                    }

                    // Replay the whole registry in recorded order before
                    // declaring Connected. Any failure fails the whole
                    // attempt: close, stay Disconnected, back off, retry.
                    // A successful old-socket acknowledgement commits its
                    // registry update before this snapshot, or a waiting
                    // operation uses the fully published new connection.
                    let _transaction = tokio::select! {
                        _ = ctx.out_tx.closed() => {
                            connection.close().await;
                            return;
                        }
                        _ = ctx.shutdown_rx.changed() => {
                            connection.close().await;
                            return;
                        }
                        guard = ctx.subscription_transaction.lock() => guard,
                    };
                    let topics = ctx.registry.snapshot();
                    let mut replay_ok = true;
                    for topic in &topics {
                        let headers = topic_headers(topic);
                        let replay = tokio::select! {
                            biased;
                            _ = ctx.out_tx.closed() => {
                                connection.close().await;
                                return;
                            }
                            _ = ctx.shutdown_rx.changed() => {
                                connection.close().await;
                                return;
                            }
                            result = connection.call_with_headers(
                                "noded", "topic.subscribe", &headers, "",
                            ) => result,
                        };
                        if let Err(e) = replay {
                            tracing::warn!(
                                event = "supervised_replay_failed",
                                service = %ctx.service_name,
                                topic = %topic,
                                error = %e,
                                "subscription replay failed; failing the whole reconnect attempt"
                            );
                            replay_ok = false;
                            break;
                        }
                    }
                    if !replay_ok {
                        connection.close().await;
                        attempt += 1;
                        continue;
                    }

                    let rx = connection
                        .take_native_incoming()
                        .expect("a fresh connection has its incoming receiver");
                    // Final stop check before the swap, so a stop that landed
                    // during replay does not publish a live connection.
                    if stop_requested(&ctx.shutdown_rx) || ctx.out_tx.is_closed() {
                        connection.close().await;
                        return;
                    }
                    let connection = Arc::new(connection);
                    let mut live = ctx.inner.write().await;
                    let published = {
                        // Close publishes its terminal state under this same
                        // fence before selecting a socket. It therefore sees
                        // this new socket, or prevents this swap entirely.
                        let _fence = ctx
                            .state_publish
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        let current = *ctx.state_tx.borrow();
                        if matches!(current, ConnState::ShuttingDown | ConnState::Fatal)
                            || stop_requested(&ctx.shutdown_rx)
                        {
                            false
                        } else {
                            *live = Some(connection.clone());
                            ctx.connection_generation.fetch_add(1, Ordering::SeqCst);
                            ctx.state_tx.send_replace(ConnState::Connected);
                            true
                        }
                    };
                    drop(live);
                    if !published {
                        connection.close().await;
                        return;
                    }
                    tracing::info!(
                        event = "supervised_reconnect",
                        service = %ctx.service_name,
                        attempts = attempt + 1,
                        downtime_ms = down_since.elapsed().as_millis() as u64,
                        replayed_subscriptions = topics.len(),
                        "reconnected to broker (full registry replayed)"
                    );
                    break rx;
                }
                Err(e) => {
                    if ctx.fatal_on_registration_rejection && e.registration_rejection().is_some() {
                        publish_registration_rejection(ctx, &e);
                        tracing::debug!(
                            event = "supervised_registration_rejected",
                            service = %ctx.service_name,
                            error = %e,
                            "service registration was rejected during reconnect; supervisor stopped"
                        );
                        return;
                    }
                    tracing::debug!(
                        event = "supervised_reconnect_attempt_failed",
                        service = %ctx.service_name,
                        attempt = attempt + 1,
                        error = %e,
                        "reconnect attempt failed"
                    );
                    attempt += 1;
                    continue;
                }
            }
        };

        current_rx = Some(new_rx);
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
}
