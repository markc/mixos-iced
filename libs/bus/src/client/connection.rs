// SPDX-License-Identifier: MIT OR Apache-2.0

//! One WebSocket connection to the broker.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

use super::command::IncomingCommand;
use super::error::{ClientError, RegistrationRejected};
use crate::wire::{self, BusMessage};

/// How long a request waits for its response. The broker answers unknown
/// targets at once and bounds mesh hops itself, so this only fires when the
/// broker is unresponsive.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The broker's own service name on the Bus.
const BROKER: &str = "noded";

type WsSink = std::pin::Pin<
    Box<dyn futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Send>,
>;
type PendingMap = HashMap<String, oneshot::Sender<BusMessage>>;

/// Aborts a spawned task if construction is cancelled before ownership of
/// the task has transferred to the returned connection.
struct AbortOnDrop {
    handle: Option<tokio::task::AbortHandle>,
}

impl AbortOnDrop {
    fn new(handle: tokio::task::AbortHandle) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    fn disarm(&mut self) {
        self.handle = None;
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

/// Removes a pending-request slot on every exit path that does not consume
/// it, including a caller dropping the awaiting future mid-flight (a
/// `tokio::time::timeout` around a call does exactly that). Without it the
/// slot would live until a late reply happened to land or the connection
/// closed. `pending` is a `std::sync::Mutex` so this `Drop` can lock without
/// an executor; every access is a brief insert, remove or clear with no
/// `.await` under the guard.
struct PendingGuard {
    pending: Arc<StdMutex<PendingMap>>,
    id: Option<String>,
}

impl PendingGuard {
    fn arm(pending: Arc<StdMutex<PendingMap>>, id: String) -> Self {
        Self {
            pending,
            id: Some(id),
        }
    }

    /// The response path already removed the entry; make `Drop` a no-op.
    fn disarm(&mut self) {
        self.id = None;
    }
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            // A poisoned map is still structurally fine (every access is a
            // short, complete mutation), so recover rather than skip the
            // removal this guard exists for.
            let mut p = match self.pending.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            p.remove(&id);
        }
    }
}

/// One registered WebSocket connection to the broker.
///
/// Dropping a `Connection` does not close the socket: the detached reader
/// task owns the read half and keeps it alive until the next inbound frame
/// or error, and the broker keeps the name registered until then. Call
/// [`close`](Self::close) for deterministic teardown.
pub struct Connection {
    service_name: String,
    sink: Arc<Mutex<WsSink>>,
    pending: Arc<StdMutex<PendingMap>>,
    incoming_rx: StdMutex<Option<mpsc::UnboundedReceiver<IncomingCommand>>>,
    next_id: AtomicU64,
    connected: Arc<AtomicBool>,
    reader: StdMutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Connection {
    /// Connect to the broker at `url` and register `service_name`. A
    /// registration the broker refuses is [`ClientError::Rejected`]; the
    /// half-built connection is closed before the error is returned, so a
    /// retry loop against a persistent refusal cannot accumulate sockets.
    pub async fn connect(service_name: &str, url: &str) -> Result<Self, ClientError> {
        let (ws_stream, _) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|e| ClientError::Connect(Box::new(e)))?;

        let (sink, stream) = ws_stream.split();
        let sink = Arc::new(Mutex::new(Box::pin(sink) as WsSink));
        let pending: Arc<StdMutex<PendingMap>> = Arc::new(StdMutex::new(HashMap::new()));
        let connected = Arc::new(AtomicBool::new(true));
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();

        let reader = tokio::spawn(Self::reader_loop(
            stream,
            pending.clone(),
            incoming_tx,
            connected.clone(),
            service_name.to_string(),
        ));
        // `register()` awaits a broker response after the reader has taken
        // the socket's read half. If an outer timeout drops this future, the
        // half-built connection alone cannot abort that detached reader, so
        // an independent abort handle stays armed until registration ends.
        let mut reader_guard = AbortOnDrop::new(reader.abort_handle());

        let connection = Self {
            service_name: service_name.to_string(),
            sink,
            pending,
            incoming_rx: StdMutex::new(Some(incoming_rx)),
            next_id: AtomicU64::new(1),
            connected,
            reader: StdMutex::new(Some(reader)),
        };

        if let Err(e) = connection.register().await {
            connection.close().await;
            return Err(e);
        }
        reader_guard.disarm();
        Ok(connection)
    }

    /// The registered service name. Used in the `from` header; a name is
    /// never authority.
    pub fn name(&self) -> &str {
        &self.service_name
    }

    /// Whether the socket is believed live. Flips to `false` when the reader
    /// sees the socket end or a write fails.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    /// Take the stream of frames addressed to this service. It can be taken
    /// once; it ends when the socket drops.
    pub fn take_incoming(&self) -> Option<mpsc::UnboundedReceiver<IncomingCommand>> {
        lock(&self.incoming_rx).take()
    }

    /// Send a request and wait for its response. A peer reply with
    /// `rc >= 10` is [`ClientError::Refused`] carrying the reply's detail.
    /// The success value is the body parsed as JSON, the body as a string
    /// when it is not JSON, or `Null` for an empty body.
    pub async fn call(
        &self,
        to: &str,
        command: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, ClientError> {
        // Serialise before taking a pending slot: a failure here must not
        // park a sender the response path can never consume.
        let body = if args.is_null() {
            String::new()
        } else {
            serde_json::to_string(&args)?
        };
        self.call_with_headers(to, command, &BTreeMap::new(), &body)
            .await
    }

    /// [`call`](Self::call) with caller-supplied headers and a verbatim body,
    /// for verbs that read their parameters from headers (`topic.subscribe`
    /// reads `name`). The framing headers (`command`, `from`, `to`, `type`,
    /// `id`) are applied after the caller's, so a caller entry can never
    /// override routing or identity.
    pub async fn call_with_headers(
        &self,
        to: &str,
        command: &str,
        headers: &BTreeMap<String, String>,
        body: &str,
    ) -> Result<serde_json::Value, ClientError> {
        let response = self.request(to, command, headers, body).await?;
        let rc = response.rc().unwrap_or(0);
        if rc >= crate::RC_ERROR {
            return Err(ClientError::Refused {
                rc,
                message: response.error_message(),
            });
        }
        Ok(if response.body.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(&response.body)
                .unwrap_or(serde_json::Value::String(response.body))
        })
    }

    /// The same wire path as [`call_with_headers`](Self::call_with_headers),
    /// returning the raw `(rc, body, error header)` triple with no
    /// interpretation of `rc`, for callers to whom an error reply's body is
    /// meaningful. Only a transport failure is an `Err`.
    pub async fn call_with_headers_raw(
        &self,
        to: &str,
        command: &str,
        headers: &BTreeMap<String, String>,
        body: &str,
    ) -> Result<(u8, String, Option<String>), ClientError> {
        let response = self.request(to, command, headers, body).await?;
        let rc = response.rc().unwrap_or(0);
        let error_header = response.get("error").map(str::to_string);
        Ok((rc, response.body, error_header))
    }

    /// Answer a request from its correlation parts: `to` is the requester
    /// (the inbound `from`), `command` echoes the inbound command, `id`
    /// correlates the response (omitted from the wire when `None`), `rc` is
    /// the return code and `body` the payload.
    pub async fn respond_parts(
        &self,
        to: &str,
        command: &str,
        id: Option<&str>,
        rc: u8,
        body: &str,
    ) -> Result<(), ClientError> {
        let mut msg = BusMessage::new()
            .with_header("command", command)
            .with_header("from", &self.service_name)
            .with_header("to", to)
            .with_header("type", "response")
            .with_header("rc", &rc.to_string());
        if let Some(id) = id {
            msg = msg.with_header("id", id);
        }
        if !body.is_empty() {
            msg.body = body.to_string();
        }
        self.send_raw(&msg).await
    }

    /// Give the service name back to the broker and wait for confirmation.
    /// The broker keys the removal off this connection, so it cannot strip a
    /// name a newer connection holds, and it is idempotent.
    pub async fn deregister(&self) -> Result<(), ClientError> {
        self.call(BROKER, "noded.deregister", serde_json::Value::Null)
            .await
            .map(|_| ())
    }

    /// Tear the connection down: abort the reader, send a best-effort
    /// WebSocket Close so the broker reaps the name at once, mark the
    /// connection dead and fail every parked caller. Idempotent.
    pub async fn close(&self) {
        // Reflect the teardown before any await so no concurrent observer
        // reads a closing connection as live.
        self.connected.store(false, Ordering::Release);
        // Abort the reader before any network I/O: even if a bounded caller
        // abandons this method while the sink is wedged, the read half can no
        // longer keep the socket and the registered name alive by itself.
        if let Some(handle) = lock(&self.reader).take() {
            handle.abort();
        }
        {
            let mut sink = self.sink.lock().await;
            let _ = sink.send(Message::Close(None)).await;
            let _ = sink.close().await;
        }
        // The reader's normal exit drains `pending`; after an abort it never
        // runs, so every parked caller is failed here instead of waiting out
        // the request timeout.
        lock(&self.pending).clear();
    }

    /// Write one Bus message to the socket as a single text frame.
    pub async fn send_raw(&self, msg: &BusMessage) -> Result<(), ClientError> {
        let wire = msg.to_wire();
        let result = self.sink.lock().await.send(Message::Text(wire.into())).await;
        if let Err(e) = result {
            // A sink failure is proof the connection is dead; mark it now
            // rather than when the reader notices.
            self.connected.store(false, Ordering::Release);
            return Err(ClientError::Send(Box::new(e)));
        }
        Ok(())
    }

    /// Send a `type: request` frame and wait for the correlated response.
    async fn request(
        &self,
        to: &str,
        command: &str,
        headers: &BTreeMap<String, String>,
        body: &str,
    ) -> Result<BusMessage, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(id.clone(), tx);
        let mut guard = PendingGuard::arm(self.pending.clone(), id.clone());

        let mut msg = BusMessage::new();
        for (k, v) in headers {
            msg = msg.with_header(k, v);
        }
        msg = msg
            .with_header("command", command)
            .with_header("from", &self.service_name)
            .with_header("to", to)
            .with_header("type", "request")
            .with_header("id", &id);
        if !body.is_empty() {
            msg.body = body.to_string();
        }
        self.send_raw(&msg).await?;

        let response = match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => return Err(ClientError::Closed),
            Err(_) => return Err(ClientError::Timeout { to: to.to_string() }),
        };
        // The reader removed the entry when it resolved the oneshot.
        guard.disarm();
        Ok(response)
    }

    async fn register(&self) -> Result<(), ClientError> {
        match self
            .call(BROKER, "noded.register", serde_json::Value::Null)
            .await
        {
            Ok(_) => Ok(()),
            Err(ClientError::Refused { rc, message }) => {
                Err(ClientError::Rejected(RegistrationRejected { rc, message }))
            }
            Err(e) => Err(e),
        }
    }

    async fn reader_loop<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
        mut stream: futures_util::stream::SplitStream<WebSocketStream<S>>,
        pending: Arc<StdMutex<PendingMap>>,
        incoming_tx: mpsc::UnboundedSender<IncomingCommand>,
        connected: Arc<AtomicBool>,
        service_name: String,
    ) {
        while let Some(result) = stream.next().await {
            let data = match result {
                Ok(Message::Text(text)) => text.to_string(),
                Ok(Message::Close(_)) => break,
                Ok(_) => continue,
                Err(e) => {
                    tracing::warn!("{service_name}: WebSocket error: {e}");
                    break;
                }
            };

            let msg = match wire::parse(&data) {
                Ok(m) => m,
                Err(e) => {
                    tracing::debug!("{service_name}: failed to parse Bus message: {e}");
                    continue;
                }
            };

            let msg_id = msg.get("id").map(|s| s.to_string());
            tracing::debug!(
                "[{service_name}] recv type={} cmd={} from={} id={msg_id:?}",
                msg.get("type").unwrap_or("unknown"),
                msg.get("command").unwrap_or("?"),
                msg.get("from").unwrap_or("?"),
            );

            // A response resolves its pending caller and is never an incoming
            // command: an orphan response (no id, or no caller waiting) is
            // dropped, not redispatched, or two peers can bounce it forever.
            if msg.message_type() == Some("response") {
                match &msg_id {
                    Some(id) => {
                        if let Some(tx) = lock(&pending).remove(id) {
                            let _ = tx.send(msg);
                        } else {
                            tracing::debug!(
                                "{service_name}: dropping orphan response id={id} (no pending caller)"
                            );
                        }
                    }
                    None => tracing::debug!("{service_name}: dropping orphan response (no id)"),
                }
                continue;
            }

            // A topic delivery is identified by its `topic` header; its
            // envelope need not carry a command.
            if msg.get("command").is_some() || msg.get("topic").is_some() {
                let command = IncomingCommand {
                    from: msg.get("from").unwrap_or("").to_string(),
                    command: msg.get("command").unwrap_or("").to_string(),
                    id: msg_id,
                    args: if msg.body.is_empty() {
                        serde_json::Value::Null
                    } else {
                        serde_json::from_str(&msg.body).unwrap_or(serde_json::Value::Null)
                    },
                    body: msg.body,
                    headers: msg.headers,
                };
                if incoming_tx.send(command).is_err() {
                    tracing::debug!("{service_name}: incoming channel closed");
                    break;
                }
            }
        }

        connected.store(false, Ordering::Release);
        tracing::info!("{service_name}: disconnected from broker");
        // Every parked caller gets `Closed` now.
        lock(&pending).clear();
    }
}

/// Lock a `std` mutex, recovering from poisoning: every guarded value here
/// is left consistent by each short critical section.
fn lock<T>(mutex: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod pending_guard_tests {
    use super::*;

    fn pending() -> Arc<StdMutex<PendingMap>> {
        Arc::new(StdMutex::new(HashMap::new()))
    }

    #[test]
    fn armed_drop_removes_the_pending_entry() {
        let pending = pending();
        let (tx, _rx) = oneshot::channel();
        pending.lock().unwrap().insert("42".to_string(), tx);
        {
            let _guard = PendingGuard::arm(pending.clone(), "42".to_string());
        }
        assert!(!pending.lock().unwrap().contains_key("42"));
    }

    #[test]
    fn disarmed_drop_leaves_the_pending_entry_in_place() {
        let pending = pending();
        let (tx, _rx) = oneshot::channel();
        pending.lock().unwrap().insert("7".to_string(), tx);
        {
            let mut guard = PendingGuard::arm(pending.clone(), "7".to_string());
            guard.disarm();
        }
        assert!(pending.lock().unwrap().contains_key("7"));
    }

    #[test]
    fn drop_after_response_path_removal_is_a_silent_no_op() {
        let pending = pending();
        {
            let _guard = PendingGuard::arm(pending.clone(), "missing".to_string());
        }
        assert!(pending.lock().unwrap().is_empty());
    }
}

#[cfg(test)]
mod connect_cancellation_tests {
    use super::*;

    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    #[tokio::test]
    async fn cancelling_registration_aborts_reader_and_closes_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (register_seen_tx, register_seen_rx) = oneshot::channel();
        let (disconnected_tx, disconnected_rx) = oneshot::channel();
        let broker = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut websocket = accept_async(socket).await.unwrap();
            let request = websocket.next().await.expect("registration request");
            assert!(request.is_ok());
            let _ = register_seen_tx.send(());
            // Withhold the registration response. Dropping the connect
            // future must still close this socket promptly.
            let _ = websocket.next().await;
            let _ = disconnected_tx.send(());
        });

        let connect = tokio::spawn(async move {
            Connection::connect("cancelled-service", &format!("ws://{address}")).await
        });
        register_seen_rx.await.unwrap();
        connect.abort();
        let _ = connect.await;

        tokio::time::timeout(Duration::from_secs(60), disconnected_rx)
            .await
            .expect("cancelled connect must not retain the reader/socket")
            .unwrap();
        broker.await.unwrap();
    }
}
