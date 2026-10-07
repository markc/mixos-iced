// SPDX-License-Identifier: MIT OR Apache-2.0

//! The Bus: MixOS's message bus wire format and broker client.
//!
//! - [`wire`] is the frozen wire format every Bus peer speaks: `---`-fenced
//!   headers plus an optional body ([`BusMessage`], [`parse`]). It needs
//!   only `serde_json` and is always compiled.
//! - [`client`] (the default `client` feature) is the WebSocket client that
//!   registers a service with the local broker, noded, and keeps it
//!   registered across broker restarts ([`SupervisedClient`]).
//!
//! The wire format and the verbs the client speaks (`noded.register`,
//! `topic.subscribe`, …) are contracts with a running broker: their bytes
//! never change.

pub mod wire;
// Source compatibility names refer to the same message and parser implementation.
pub use wire as bus;
pub mod native_session;
mod protocol;
pub mod service_info;
pub use protocol::*;
pub use service_info::{
    NodeInfo, REGISTRATION_REJECTION_NAME_TAKEN, REGISTRATION_REJECTION_SCHEMA, RegisterProvenance,
    SCHEMA_VERSION, ServiceInfo,
};

#[cfg(feature = "client")]
pub mod client_helpers;
#[cfg(feature = "client")]
pub mod native_client;

pub use wire::{
    BusMessage, EMPTY_MESSAGE, MAX_HEADERS, MAX_MESSAGE_BYTES, ParseError, ParseReport,
    WS_MAX_FRAME_BYTES, parse, parse_lenient, parse_strict,
};

/// Return codes carried in the `rc` header. The convention is ARexx's:
/// 0 succeeded, 5 succeeded with a warning, 10 is an error the caller can
/// act on, 20 a failure of the responder itself.
pub const RC_SUCCESS: u8 = 0;
pub const RC_WARNING: u8 = 5;
pub const RC_ERROR: u8 = 10;
pub const RC_FAILURE: u8 = 20;

#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "client")]
pub use client::{
    ClientError, ConnState, Connection, DEFAULT_NODED_URL, IncomingCommand, MAX_INITIAL_ATTEMPTS,
    RegistrationRejected, RegistrationRejectionKind, SubscriptionRegistry, SupervisedClient,
    SupervisedConnectOptions, SupervisedError, noded_url,
};
