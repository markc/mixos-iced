// SPDX-License-Identifier: MIT OR Apache-2.0

//! Native ABP client, including authenticated Unix broker ingress.
//! All requests pass through noded; no application socket transport is provided.

mod bounded;
mod native;
pub mod session;
mod supervised;
mod types;
mod unix;

pub use crate::PortReply;
pub use bounded::{BoundedIncomingEvent, BoundedIncomingReceiver};
pub use native::{NameCollision, NodedClient, RegistrationRejected};
pub use supervised::{
    ConnState, MAX_INITIAL_ATTEMPTS, SubscriptionRegistry, SupervisedClient, SupervisedError,
};
pub use types::IncomingCommand;
pub use unix::{
    BrokerAccount, ConnectError, Delivery, UnixConnectOptions, UnixConnectOutcome, VerifiedCommand,
    VerifiedConnection,
};
