// SPDX-License-Identifier: MIT OR Apache-2.0

//! Typed compatibility surface over the canonical native ABP client.

use super::{ClientError, IncomingCommand};
use crate::{
    BusMessage,
    native_client::{NativeIncomingReceiver, NodedClient},
};
use std::collections::BTreeMap;
use std::sync::Mutex;
use tokio::sync::mpsc;

/// One registered broker connection. Explicit close provides deterministic
/// teardown; the native client owns framing, correlation and socket lifetime.
pub struct Connection {
    name: String,
    inner: NodedClient,
    incoming: Mutex<Option<NativeIncomingReceiver>>,
}

#[derive(Clone, Default)]
pub(crate) struct ConnectionOptions {
    pub provenance: Option<crate::RegisterProvenance>,
    pub verbs: Option<Vec<crate::VerbDescriptor>>,
    pub capacity: Option<usize>,
}

impl Connection {
    pub async fn connect(name: &str, url: &str) -> Result<Self, ClientError> {
        Self::connect_with_options(name, url, &ConnectionOptions::default()).await
    }

    pub(crate) async fn connect_with_options(
        name: &str,
        url: &str,
        options: &ConnectionOptions,
    ) -> Result<Self, ClientError> {
        let inner = NodedClient::connect_with_provenance_and_capacity(
            name,
            url,
            options.provenance.clone(),
            options.capacity,
            options.verbs.clone(),
        )
        .await
        .map_err(ClientError::from_native)?;
        let incoming = inner.take_native_incoming().await;
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
        let mut incoming = self
            .incoming
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if matches!(&*incoming, Some(NativeIncomingReceiver::Unbounded(_))) {
            match incoming.take() {
                Some(NativeIncomingReceiver::Unbounded(receiver)) => Some(receiver),
                _ => unreachable!("unbounded receiver checked under lock"),
            }
        } else {
            None
        }
    }

    pub(crate) fn take_native_incoming(&self) -> Option<NativeIncomingReceiver> {
        self.incoming
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    pub async fn call_typed(
        &self,
        to: &str,
        command: &str,
        args: serde_json::Value,
    ) -> Result<crate::PortReply, ClientError> {
        self.inner
            .call_typed(to, command, args)
            .await
            .map_err(ClientError::from_native)
    }

    pub async fn send(
        &self,
        to: &str,
        command: &str,
        args: serde_json::Value,
    ) -> Result<(), ClientError> {
        self.inner
            .send(to, command, args)
            .await
            .map_err(ClientError::from_native)
    }

    pub async fn list_services(&self) -> Result<Vec<String>, ClientError> {
        self.inner
            .list_services()
            .await
            .map_err(ClientError::from_native)
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

    pub async fn send_with_headers(
        &self,
        to: &str,
        command: &str,
        headers: &BTreeMap<String, String>,
        body: &str,
    ) -> Result<(), ClientError> {
        self.inner
            .send_with_headers(to, command, headers, body)
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
