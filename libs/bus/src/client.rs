// SPDX-License-Identifier: MIT OR Apache-2.0

//! The broker client.
//!
//! A [`Connection`] is one WebSocket to the broker (noded): it registers a
//! service name, correlates requests with their responses and surfaces
//! everything else the broker sends as [`IncomingCommand`]s. It is a single
//! dial: when the socket drops, its incoming stream ends.
//!
//! A [`SupervisedClient`] wraps a connection and owns the connect, register,
//! replay-subscriptions, pump loop. Its incoming stream survives broker
//! restarts, its outbound calls fail fast with a typed error while the
//! broker is away, and every recorded topic subscription is replayed before
//! it reports `Connected` again. Services that must stay registered for the
//! life of a session use it.
//!
//! [`noded_url`] finds the broker: `MIXOS_NODED_URL`, else the node
//! configuration file, else the loopback broker.

mod connection;
mod error;
mod locate;
mod supervised;

pub use crate::native_client::IncomingCommand;
pub use connection::Connection;
pub use error::{ClientError, RegistrationRejected, RegistrationRejectionKind, SupervisedError};
pub use locate::{DEFAULT_NODED_URL, node_config_path, noded_url, url_from_node_config};
pub use supervised::{
    ConnState, MAX_INITIAL_ATTEMPTS, SubscriptionRegistry, SupervisedClient,
    SupervisedConnectOptions,
};
