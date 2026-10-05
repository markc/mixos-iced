// SPDX-License-Identifier: MIT OR Apache-2.0

//! Typed compatibility surface over the canonical native ABP client.

use super::{ClientError, IncomingCommand};
use crate::{BusMessage, native_client::NodedClient};
use std::collections::BTreeMap;
use std::sync::Mutex;
use tokio::sync::mpsc;

/// One registered broker connection. Explicit close provides deterministic
/// teardown; the native client owns framing, correlation and socket lifetime.
pub struct Connection {
    name: String,
    inner: NodedClient,
    incoming: Mutex<Option<mpsc::UnboundedReceiver<IncomingCommand>>>,
}

impl Connection {
    pub async fn connect(name: &str, url: &str) -> Result<Self, ClientError> {
        let inner = NodedClient::connect(name, url)
            .await
            .map_err(ClientError::from_native)?;
        let incoming = inner.incoming_async().await;
        Ok(Self {
            name: name.to_owned(),
            inner,
            incoming: Mutex::new(incoming),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn is_connected(&self) -> bool {
        self.inner.is_connected()
    }
    pub fn take_incoming(&self) -> Option<mpsc::UnboundedReceiver<IncomingCommand>> {
        self.incoming
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    pub async fn call(
        &self,
        to: &str,
        command: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, ClientError> {
        let body = if args.is_null() {
            String::new()
        } else {
            serde_json::to_string(&args)?
        };
        self.call_with_headers(to, command, &BTreeMap::new(), &body)
            .await
    }

    pub async fn call_with_headers(
        &self,
        to: &str,
        command: &str,
        headers: &BTreeMap<String, String>,
        body: &str,
    ) -> Result<serde_json::Value, ClientError> {
        let (rc, body, error) = self
            .call_with_headers_raw(to, command, headers, body)
            .await?;
        if rc >= crate::RC_ERROR {
            let mut response = BusMessage::new().with_body(&body);
            if let Some(error) = error {
                response.set("error", &error);
            }
            return Err(ClientError::Refused {
                rc,
                message: response.error_message(),
            });
        }
        Ok(if body.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(&body).unwrap_or(serde_json::Value::String(body))
        })
    }

    pub async fn call_with_headers_raw(
        &self,
        to: &str,
        command: &str,
        headers: &BTreeMap<String, String>,
        body: &str,
    ) -> Result<(u8, String, Option<String>), ClientError> {
        self.inner
            .call_with_headers_raw(to, command, headers, body)
            .await
            .map_err(ClientError::from_native)
    }

    pub async fn respond_parts(
        &self,
        to: &str,
        command: &str,
        id: Option<&str>,
        rc: u8,
        body: &str,
    ) -> Result<(), ClientError> {
        self.inner
            .respond_parts(to, command, id, rc, body)
            .await
            .map_err(ClientError::from_native)
    }
    pub async fn deregister(&self) -> Result<(), ClientError> {
        self.inner
            .deregister()
            .await
            .map_err(ClientError::from_native)
    }
    pub async fn close(&self) {
        self.inner.close().await;
    }
    pub async fn send_raw(&self, message: &BusMessage) -> Result<(), ClientError> {
        self.inner
            .send_raw(message)
            .await
            .map_err(ClientError::from_native)
    }
}
