// SPDX-License-Identifier: MIT OR Apache-2.0

use std::fmt;

/// The broker refused `noded.register`.
///
/// Only the return code and the broker's diagnostic text are kept. A name
/// collision and an admission refusal both arrive as `rc=10` with different
/// wording, so this never claims to know which it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationRejected {
    pub rc: u8,
    pub message: String,
}

impl fmt::Display for RegistrationRejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Bus registration rejected with rc {}: {}",
            self.rc, self.message
        )
    }
}

impl std::error::Error for RegistrationRejected {}

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
        if let Some((rc, message)) =
            crate::native_client::NodedClient::registration_rejection(&error)
        {
            return Self::Rejected(RegistrationRejected {
                rc,
                message: message.to_owned(),
            });
        }
        match error.downcast::<Self>() {
            Ok(error) => error,
            Err(error) => Self::Native(error),
        }
    }
    /// The broker's structured registration refusal, when that is what this
    /// error is.
    pub fn registration_rejection(&self) -> Option<(u8, &str)> {
        match self {
            ClientError::Rejected(r) => Some((r.rc, r.message.as_str())),
            _ => None,
        }
    }
}

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
    /// rejected the registration and that was configured as fatal.
    InitialConnectFailed { attempts: u32, source: ClientError },
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
            SupervisedError::Transport(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SupervisedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SupervisedError::InitialConnectFailed { source, .. } => Some(source),
            SupervisedError::Transport(e) => Some(e),
            _ => None,
        }
    }
}

impl SupervisedError {
    /// The broker's structured registration refusal when this error came
    /// from the initial connect-and-register path.
    pub fn registration_rejection(&self) -> Option<(u8, &str)> {
        match self {
            SupervisedError::InitialConnectFailed { source, .. } => source.registration_rejection(),
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
            ClientError::Rejected(RegistrationRejected {
                rc: 10,
                message: "name held".into(),
            })
        };
        let initial = SupervisedError::InitialConnectFailed {
            attempts: 1,
            source: rejected(),
        };
        assert_eq!(initial.registration_rejection(), Some((10, "name held")));
        assert!(initial.to_string().contains("rc 10: name held"));
        let later = SupervisedError::Transport(rejected());
        assert!(later.registration_rejection().is_none());
    }
}
