// SPDX-License-Identifier: MIT OR Apache-2.0

use std::fmt;

pub use crate::native_client::{RegistrationRejected, RegistrationRejectionKind};

/// What can go wrong on one [`Connection`](super::Connection). The
/// transport errors are boxed: they carry a whole HTTP response and would
/// otherwise make every `Result` in the client large.
#[derive(Debug)]
pub enum ClientError {
    /// The WebSocket could not be opened.
    Connect(Box<tokio_tungstenite::tungstenite::Error>),
    /// A frame could not be written; the connection is dead.
    Send(Box<tokio_tungstenite::tungstenite::Error>),
    /// The connection closed before the response arrived.
    Closed,
    /// No response within the request timeout.
    Timeout { to: String },
    /// The request body could not be serialised.
    Json(serde_json::Error),
    /// A native ingress or protocol failure retaining its underlying cause.
    Native(anyhow::Error),
    /// The broker refused this connection's `noded.register`.
    Rejected(RegistrationRejected),
    /// The peer answered with `rc >= 10`: an application error, with the
    /// detail the reply carried.
    Refused { rc: u8, message: String },
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::Connect(e) => write!(f, "failed to connect to broker: {e}"),
            ClientError::Send(e) => write!(f, "failed to send message to broker: {e}"),
            ClientError::Closed => write!(f, "broker connection closed before response"),
            ClientError::Timeout { to } => write!(f, "send to '{to}' timed out"),
            ClientError::Json(e) => write!(f, "request body is not serialisable: {e}"),
            ClientError::Native(e) => write!(f, "{e:#}"),
            ClientError::Rejected(r) => write!(f, "{r}"),
            ClientError::Refused { message, .. } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ClientError::Connect(e) | ClientError::Send(e) => Some(e.as_ref()),
            ClientError::Json(e) => Some(e),
            ClientError::Native(e) => Some(e.as_ref()),
            ClientError::Rejected(r) => Some(r),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for ClientError {
    fn from(e: serde_json::Error) -> Self {
        ClientError::Json(e)
    }
}

impl ClientError {
    pub(crate) fn from_native(error: anyhow::Error) -> Self {
        // Carry the typed refusal across the native boundary whole: the
        // classification decoded from the rejection body survives, so
        // nothing downstream ever rebuilds a `RegistrationRejected` from
        // the (rc, text) tuple accessor.
        let error = match error.downcast::<RegistrationRejected>() {
            Ok(rejection) => return Self::Rejected(rejection),
            Err(error) => error,
        };
        match error.downcast::<Self>() {
            Ok(error) => error,
            Err(error) => Self::Native(error),
        }
    }
    /// The broker's registration refusal, when that is what this error is.
    /// The tuple/string compatibility accessor; the typed classification is
    /// [`registration_rejection_typed`](Self::registration_rejection_typed).
    pub fn registration_rejection(&self) -> Option<(u8, &str)> {
        match self {
            ClientError::Rejected(r) => Some((r.rc, r.message.as_str())),
            _ => None,
        }
    }
    /// The structured registration refusal, when that is what this error is,
    /// with the typed [`RegistrationRejected::kind`] classification.
    pub fn registration_rejection_typed(&self) -> Option<&RegistrationRejected> {
        match self {
            ClientError::Rejected(rejection) => Some(rejection),
            _ => None,
        }
    }
}

/// A declared initial topic failed: invalid client configuration, or an
/// explicit broker refusal of the declared subscription.
///
/// A refusal preserves the exact topic, return code and the broker's
/// diagnostic. A validation failure carries the raw input index when one
/// topic is at fault; aggregate bound failures carry `None`. Declaration
/// errors are never manufactured from registration refusals: the typed
/// registration classification stays on [`RegistrationRejected`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubscriptionDeclarationError {
    /// Client-side validation failed before any socket was opened.
    Invalid { index: Option<usize>, message: String },
    /// The broker answered a declared `topic.subscribe` with a nonzero rc.
    /// Declarations are startup requirements: an explicit refusal is
    /// terminal, never retried. A warning rc is a refusal too, not an
    /// acknowledgement.
    Rejected { topic: String, rc: u8, message: String },
}

impl fmt::Display for SubscriptionDeclarationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SubscriptionDeclarationError::Invalid { index, message } => match index {
                Some(index) => write!(f, "invalid initial topic {index}: {message}"),
                None => write!(f, "invalid initial topics: {message}"),
            },
            SubscriptionDeclarationError::Rejected { topic, rc, message } => write!(
                f,
                "broker refused subscription to '{topic}' with rc {rc}: {message}"
            ),
        }
    }
}

impl std::error::Error for SubscriptionDeclarationError {}

/// What can go wrong on a [`SupervisedClient`](super::SupervisedClient).
///
/// A call made while the broker is away is a typed error, never a queued
/// message: the caller decides what to do. Exhausting the initial connect
/// budget is a typed fatal so a service can exit non-zero.
#[derive(Debug)]
pub enum SupervisedError {
    /// The broker connection is down and the supervisor is reconnecting.
    /// There is no outbound queue.
    Disconnected,
    /// The client is shutting down (deregister or close); new outbound work
    /// is refused.
    ShuttingDown,
    /// The initial connect-and-register budget was exhausted, or the broker
    /// rejected the registration and that was configured as fatal. The
    /// whole-attempt deadline contributes a [`ClientError::Timeout`].
    InitialConnectFailed { attempts: u32, source: ClientError },
    /// An initial topic declaration was invalid, or the broker explicitly
    /// refused it. Terminal: no retry, recovery is reconfiguration or a new
    /// client.
    SubscriptionDeclaration(SubscriptionDeclarationError),
    /// A connection error on a call that did reach a live connection.
    Transport(ClientError),
}

impl fmt::Display for SupervisedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SupervisedError::Disconnected => {
                write!(
                    f,
                    "broker disconnected (no outbound queue; caller must retry)"
                )
            }
            SupervisedError::ShuttingDown => write!(f, "supervised client is shutting down"),
            SupervisedError::InitialConnectFailed { attempts, source } => write!(
                f,
                "initial broker connect failed after {attempts} attempt(s): {source}"
            ),
            SupervisedError::SubscriptionDeclaration(e) => {
                write!(f, "subscription declaration failed: {e}")
            }
            SupervisedError::Transport(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SupervisedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SupervisedError::InitialConnectFailed { source, .. } => Some(source),
            SupervisedError::SubscriptionDeclaration(e) => Some(e),
            SupervisedError::Transport(e) => Some(e),
            _ => None,
        }
    }
}

impl SupervisedError {
    /// The broker's structured registration refusal when this error came
    /// from the initial connect-and-register path. The tuple/string
    /// compatibility accessor.
    pub fn registration_rejection(&self) -> Option<(u8, &str)> {
        match self {
            SupervisedError::InitialConnectFailed { source, .. } => source.registration_rejection(),
            _ => None,
        }
    }
    /// The structured registration refusal when this error came from the
    /// initial connect-and-register path.
    pub fn registration_rejection_typed(&self) -> Option<&RegistrationRejected> {
        match self {
            SupervisedError::InitialConnectFailed { source, .. } => {
                source.registration_rejection_typed()
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supervised_error_is_std_error_and_typed() {
        let e = SupervisedError::Disconnected;
        let dyn_err: &dyn std::error::Error = &e;
        assert!(dyn_err.to_string().contains("disconnected"));
        let fatal = SupervisedError::InitialConnectFailed {
            attempts: 5,
            source: ClientError::Closed,
        };
        assert!(fatal.to_string().contains("5 attempt"));
        assert!(std::error::Error::source(&fatal).is_some());
        assert!(fatal.registration_rejection().is_none());
    }

    #[test]
    fn registration_rejection_surfaces_only_from_the_initial_connect() {
        let rejected = || {
            ClientError::Rejected(RegistrationRejected::new(
                10,
                "name held",
                RegistrationRejectionKind::NameTaken,
            ))
        };
        let initial = SupervisedError::InitialConnectFailed {
            attempts: 1,
            source: rejected(),
        };
        assert_eq!(initial.registration_rejection(), Some((10, "name held")));
        assert_eq!(
            initial
                .registration_rejection_typed()
                .map(|rejection| rejection.kind()),
            Some(RegistrationRejectionKind::NameTaken)
        );
        assert!(initial.to_string().contains("rc 10: name held"));
        let later = SupervisedError::Transport(rejected());
        assert!(later.registration_rejection().is_none());
        assert!(later.registration_rejection_typed().is_none());
    }

    #[test]
    fn from_native_carries_the_typed_rejection_kind() {
        let native = RegistrationRejected::new(
            10,
            "held elsewhere",
            RegistrationRejectionKind::NameTaken,
        );
        let mapped = ClientError::from_native(anyhow::Error::from(native.clone()));
        assert_eq!(
            mapped.registration_rejection_typed(),
            Some(&native),
            "the typed rejection crosses the native boundary whole, kind included"
        );
        // A plain native failure is still Native, never a manufactured rejection.
        let plain = ClientError::from_native(anyhow::anyhow!("socket exploded"));
        assert!(plain.registration_rejection_typed().is_none());
        assert!(matches!(plain, ClientError::Native(_)));
    }

    #[test]
    fn declaration_error_is_std_error_and_keeps_its_structured_fields() {
        let refused = SubscriptionDeclarationError::Rejected {
            topic: "scenes.changed".to_string(),
            rc: 10,
            message: "reserved".to_string(),
        };
        let dyn_err: &dyn std::error::Error = &refused;
        assert!(dyn_err.to_string().contains("scenes.changed"));
        assert!(dyn_err.to_string().contains("rc 10"));
        let invalid = SubscriptionDeclarationError::Invalid {
            index: Some(2),
            message: "empty".to_string(),
        };
        assert!(invalid.to_string().contains("topic 2"));
        assert_eq!(
            SubscriptionDeclarationError::Invalid {
                index: None,
                message: "too many".to_string(),
            }
            .to_string(),
            "invalid initial topics: too many"
        );
    }

    #[test]
    fn declaration_errors_never_manufacture_a_registration_rejection() {
        let refused = SupervisedError::SubscriptionDeclaration(
            SubscriptionDeclarationError::Rejected {
                topic: "x.changed".to_string(),
                rc: 10,
                message: "refused".to_string(),
            },
        );
        assert!(refused.registration_rejection().is_none());
        assert!(refused.registration_rejection_typed().is_none());
        assert!(matches!(refused, SupervisedError::SubscriptionDeclaration(_)));
        let invalid = SupervisedError::SubscriptionDeclaration(
            SubscriptionDeclarationError::Invalid {
                index: None,
                message: "bogus".to_string(),
            },
        );
        assert!(invalid.registration_rejection().is_none());
        assert!(invalid.registration_rejection_typed().is_none());
    }
}
