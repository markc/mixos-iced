// SPDX-License-Identifier: MIT OR Apache-2.0
//! Latest-value observations multiplexed by the existing native settings owner.
//! No connection, task, timer, authority or redraw belongs to this publisher.
use crate::frames::{Handle, Snapshot};
use bus::native_client::SupervisedClient;
use serde_json::Value;
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::sync::watch;

type Sending = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

pub(super) struct Publisher {
    metadata: watch::Receiver<Value>,
    sources: watch::Receiver<Option<Handle>>,
    frames: Option<watch::Receiver<Snapshot>>,
    client: Option<Arc<SupervisedClient>>,
    sending: Option<Sending>,
    queued: bool,
    open: bool,
}

impl Publisher {
    pub(super) fn new(
        metadata: watch::Receiver<Value>,
        sources: watch::Receiver<Option<Handle>>,
    ) -> Self {
        Self {
            metadata,
            sources,
            frames: None,
            client: None,
            sending: None,
            queued: false,
            open: true,
        }
    }
    pub(super) fn connect(&mut self, client: Arc<SupervisedClient>) {
        self.sending = None;
        self.client = Some(client);
        self.queued = true;
    }
    fn stage(&mut self) {
        if self.sending.is_some() || !self.queued {
            return;
        }
        self.queued = false;
        let Some(client) = self.client.as_ref().cloned() else {
            return;
        };
        let Some(generation) = settings::native::live_generation(&client) else {
            return;
        };
        let mut metadata = self.metadata.borrow().clone();
        if metadata.is_null() {
            return;
        }
        metadata["service"] = Value::String(client.service_name().to_owned());
        metadata["connection_generation"] = Value::from(generation);
        metadata["native_frames"] = self
            .frames
            .as_ref()
            .map(|frames| crate::frames::snapshot_json(&frames.borrow()))
            .unwrap_or(Value::Null);
        let Ok(body) = serde_json::to_string(&metadata) else {
            return;
        };
        let mut message = bus::wire::BusMessage::new();
        message.set(
            "command",
            &format!("{}.presentation.changed", client.service_name()),
        );
        message.body = body;
        let wire = message.to_wire();
        let mut headers = BTreeMap::new();
        headers.insert(
            "name".into(),
            format!("{}.presentation.changed", client.service_name()),
        );
        headers.insert("retain".into(), "true".into());
        self.sending = Some(Box::pin(async move {
            let reply = tokio::time::timeout(
                Duration::from_secs(1),
                client.call_with_headers_raw_at_generation(
                    generation,
                    "noded",
                    "topic.publish",
                    &headers,
                    &wire,
                ),
            )
            .await
            .map_err(|_| "presentation publication deadline elapsed".to_owned())?
            .map_err(|error| error.to_string())?;
            if reply.0 != 0 {
                return Err(format!("presentation publication refused: {}", reply.0));
            }
            Ok(())
        }));
    }
    pub(super) async fn drive(&mut self) {
        loop {
            self.stage();
            tokio::select! {
                changed = self.metadata.changed(), if self.open => {
                    if changed.is_err() {self.open = false;} else {self.metadata.borrow_and_update(); self.queued = true;}
                    return;
                }
                changed = self.sources.changed(), if self.open => {
                    if changed.is_err() {self.open = false;} else {
                        self.frames = self.sources.borrow_and_update().as_ref().map(Handle::subscribe_observations);
                        self.queued = true;
                    }
                    return;
                }
                changed = async {self.frames.as_mut().expect("guarded frame source").changed().await}, if self.frames.is_some() => {
                    if changed.is_err() {self.frames = None;} else {
                        self.frames.as_mut().unwrap().borrow_and_update();
                        self.queued = true;
                    }
                    return;
                }
                result = async {self.sending.as_mut().expect("guarded publication").await}, if self.sending.is_some() => {
                    self.sending = None;
                    if let Err(error) = result {tracing::warn!(%error, "native presentation observation not published");}
                    return;
                }
                else => std::future::pending::<()>().await,
            }
        }
    }
}
