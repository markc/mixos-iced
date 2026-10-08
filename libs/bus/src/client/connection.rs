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

enum Transport {
    Tcp(Box<NodedClient>),
    #[cfg(unix)]
    Unix(std::sync::Arc<crate::native_client::VerifiedConnection>),
}

impl std::ops::Deref for Transport {
    type Target = NodedClient;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Tcp(client) => client,
            #[cfg(unix)]
            Self::Unix(connection) => connection.client(),
        }
    }
}

pub(crate) enum ConnectionIncomingReceiver {
    Tcp(NativeIncomingReceiver),
    #[cfg(unix)]
    Unix(std::sync::Arc<crate::native_client::VerifiedConnection>),
}

impl ConnectionIncomingReceiver {
    pub(crate) fn verified(&self) -> bool {
        match self {
            Self::Tcp(_) => false,
            #[cfg(unix)]
            Self::Unix(_) => true,
        }
    }
    pub async fn recv(&mut self) -> Option<crate::native_client::BoundedIncomingEvent> {
        match self {
            Self::Tcp(receiver) => receiver.recv().await,
            #[cfg(unix)]
            Self::Unix(connection) => loop {
                let delivery = connection.recv_shared().await?;
                match delivery.delivery() {
                    crate::native_client::Delivery::Command => {
                        return Some(crate::native_client::BoundedIncomingEvent::Command(
                            delivery.into_supervised_command(),
                        ));
                    }
                    crate::native_client::Delivery::Gap => {
                        return Some(crate::native_client::BoundedIncomingEvent::Overflow {
                            dropped: 1,
                        });
                    }
                    crate::native_client::Delivery::Refuse => {
                        // The receive owner settles rejected requests; the
                        // socket reader must remain free to deliver RPC replies.
                        let command = delivery.command();
                        if connection
                            .client()
                            .respond_parts(
                                &command.from,
                                &command.command,
                                command.id.as_deref(),
                                crate::RC_ERROR,
                                r#"{"error":"overloaded","error_code":"OVERLOADED"}"#,
                            )
                            .await
                            .is_err()
                        {
                            return None;
                        }
                    }
                }
            },
        }
    }
}

/// One registered broker connection. Explicit close provides deterministic
/// teardown; the native client owns framing, correlation and socket lifetime.
pub struct Connection {
    name: String,
    inner: Transport,
    incoming: Mutex<Option<ConnectionIncomingReceiver>>,
}

#[derive(Clone, Default)]
pub(crate) struct ConnectionOptions {
    pub provenance: Option<crate::RegisterProvenance>,
    pub verbs: Option<Vec<crate::VerbDescriptor>>,
    pub capacity: Option<usize>,
    pub max_delivery_bytes: Option<usize>,
    #[cfg(unix)]
    pub unix: Option<crate::native_client::UnixConnectOptions>,
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
        #[cfg(unix)]
        if let Some(unix) = &options.unix {
            let mut unix = unix.clone();
            unix.incoming_capacity = options.capacity.or(unix.incoming_capacity);
            unix.incoming_max_bytes = options
                .max_delivery_bytes
                .unwrap_or(unix.incoming_max_bytes);
            let outcome = NodedClient::connect_unix_with_verbs(
                name,
                url,
                &unix,
                options.provenance.clone(),
                options.verbs.clone(),
            )
            .await
            .map_err(|error| match error {
                crate::native_client::ConnectError::Protocol(error) => {
                    ClientError::from_native(error)
                }
                error => ClientError::from_native(error.into()),
            })?;
            return match outcome {
                crate::native_client::UnixConnectOutcome::VerifiedUnix(connection) => {
                    let connection = std::sync::Arc::new(connection);
                    Ok(Self {
                        name: name.to_owned(),
                        inner: Transport::Unix(connection.clone()),
                        incoming: Mutex::new(Some(ConnectionIncomingReceiver::Unix(connection))),
                    })
                }
                crate::native_client::UnixConnectOutcome::UnverifiedTcp { client, .. } => {
                    let incoming = client
                        .take_native_incoming()
                        .await
                        .map(ConnectionIncomingReceiver::Tcp);
                    Ok(Self {
                        name: name.to_owned(),
                        inner: Transport::Tcp(Box::new(client)),
                        incoming: Mutex::new(incoming),
                    })
                }
            };
        }
        let inner = NodedClient::connect_with_provenance_and_capacity(
            name,
            url,
            options.provenance.clone(),
            options.capacity,
            options.verbs.clone(),
        )
        .await
        .map_err(ClientError::from_native)?;
        let incoming = inner
            .take_native_incoming()
            .await
            .map(ConnectionIncomingReceiver::Tcp);
        Ok(Self {
            name: name.to_owned(),
            inner: Transport::Tcp(Box::new(inner)),
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
        if matches!(
            &*incoming,
            Some(ConnectionIncomingReceiver::Tcp(
                NativeIncomingReceiver::Unbounded(_)
            ))
        ) {
            match incoming.take() {
                Some(ConnectionIncomingReceiver::Tcp(NativeIncomingReceiver::Unbounded(
                    receiver,
                ))) => Some(receiver),
                _ => unreachable!("unbounded receiver checked under lock"),
            }
        } else {
            None
        }
    }

    pub(crate) fn take_native_incoming(&self) -> Option<ConnectionIncomingReceiver> {
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
