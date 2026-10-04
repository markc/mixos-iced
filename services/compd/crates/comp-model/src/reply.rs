// The bodies are built with `json!` (serde_json without `preserve_order`:
// keys sort), so the bytes are stable.

//! The `comp.*` reply shape: `(rc, body)` with `rc` 0 for success and 10 for
//! a refusal whose body is `{"error": code, ...detail}`.

use std::sync::Arc;

use serde_json::{Value, json};

use surfaces::{SeatKind, WindowTargetError};

use crate::observation::{PropValue, SetValidationError};

/// rc 0: success.
pub const RC_OK: u8 = 0;
/// rc 10: a structured refusal.
pub const RC_REFUSED: u8 = 10;

// Both caps track libs/bus: change them when the broker's change.
/// The largest Bus message read from a local transport (`bus::MAX_MESSAGE_BYTES`).
pub const BUS_MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
/// The largest WebSocket frame on the broker path
/// (`bus::WS_MAX_FRAME_BYTES`). A Bus message is one frame there.
pub const BUS_WS_MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Effective Bus message ceiling on the broker WebSocket path. The client
/// writes each Bus message as one frame, so both transport caps apply.
pub const MAX_REPLY_WIRE_BYTES: usize = if BUS_MAX_MESSAGE_BYTES < BUS_WS_MAX_FRAME_BYTES {
    BUS_MAX_MESSAGE_BYTES
} else {
    BUS_WS_MAX_FRAME_BYTES
};

/// Upper bound reserved inside [`MAX_REPLY_WIRE_BYTES`] for canonical Bus
/// framing and response headers (`command`, `from`, `to`, `type`, `rc`, and
/// broker correlation `id`). The transport also measures those actual bytes
/// immediately before sending.
pub const REPLY_WIRE_HEADROOM_BYTES: usize = 4 * 1024;
pub const MAX_REPLY_BODY_BYTES: usize = MAX_REPLY_WIRE_BYTES - REPLY_WIRE_HEADROOM_BYTES;

/// A bare refusal: `{"error": reason}` (rc 10).
pub fn error(reason: &'static str) -> (u8, Arc<str>) {
    (RC_REFUSED, Arc::from(json!({"error": reason}).to_string()))
}

/// The reply that replaces a body over the limit.
pub fn too_large(limit_bytes: usize) -> (u8, Arc<str>) {
    (
        RC_REFUSED,
        Arc::from(
            json!({
                "error": "too_large",
                "limit_bytes": limit_bytes,
                "hint": "read a subtree",
            })
            .to_string(),
        ),
    )
}

/// Every refusal carries `error_code` beside `error` (the 0.58.x alias), so
/// Mix `send` hands a script the whole structured body. The transport applies
/// this to every reply before it is sent.
pub fn with_error_code(rc: u8, body: Arc<str>) -> (u8, Arc<str>) {
    if rc == 0 {
        return (rc, body);
    }
    let Ok(Value::Object(mut fields)) = serde_json::from_str::<Value>(&body) else {
        return (rc, body);
    };
    if fields.contains_key("error_code") {
        return (rc, body);
    }
    let Some(code) = fields.get("error").filter(|code| code.is_string()).cloned() else {
        return (rc, body);
    };
    fields.insert("error_code".into(), code);
    (rc, Arc::from(Value::Object(fields).to_string()))
}

#[derive(Clone, Debug, PartialEq)]
pub enum ControlReply {
    WithInputSeat {
        seat: SeatKind,
        reply: Box<ControlReply>,
    },
    PointerWatch {
        topic: String,
        lease_ms: u64,
    },
    Watch {
        topic: String,
        event_seq: u64,
        lost_count: u64,
    },
    Set {
        path: String,
        old: PropValue,
        new: PropValue,
        /// Durability report for file-persisted leaves: `Some(false)` means
        /// the in-memory change and the changed event stand but the write
        /// to disk FAILED and the value will not survive restart. `None` for
        /// process-lifetime leaves (field absent on the wire).
        persisted: Option<bool>,
    },
    Validation(SetValidationError),
    /// A `comp.window.*` success.
    Window {
        id: u64,
        generation: u64,
        title: Option<Arc<str>>,
        app_id: Option<Arc<str>>,
        minimized: bool,
        changed: bool,
    },
    WindowTarget {
        id: u64,
        error: WindowTargetError,
    },
    NotFound {
        minimized_count: usize,
    },
    /// A verb argument the verb does not define (a typo must not be
    /// silently ignored, or `{"gen": 3}` would act unfenced).
    InvalidArgs {
        field: String,
        allowed: &'static [&'static str],
    },
    Locked,
    Busy,
    /// A verb's success body (rc 0).
    Body(Value),
    /// A refusal: `{"error": error, ...detail}` (rc 10). `detail` is an
    /// object or null.
    Refused {
        error: &'static str,
        detail: Value,
    },
}

impl ControlReply {
    /// The reply body as a JSON value, `error_code` included.
    pub fn wire_json(self) -> Value {
        let (rc, body) = self.into_wire();
        let (_, body) = with_error_code(rc, body);
        serde_json::from_str(&body).unwrap_or(Value::Null)
    }

    pub fn refused(error: &'static str, detail: Value) -> Self {
        Self::Refused { error, detail }
    }

    pub fn into_wire(self) -> (u8, Arc<str>) {
        match self {
            Self::WithInputSeat { seat, reply } => {
                let (rc, body) = reply.into_wire();
                let mut body: Value = serde_json::from_str(&body).expect("control replies are JSON");
                body["seat"] = json!(seat.name());
                (rc, Arc::from(body.to_string()))
            }
            Self::PointerWatch { topic, lease_ms } => (
                0,
                Arc::from(json!({"version":1,"topic":topic,"lease_ms":lease_ms}).to_string()),
            ),
            Self::Watch {
                topic,
                event_seq,
                lost_count,
            } => (
                0,
                Arc::from(
                    json!({
                        "topic": topic,
                        "event_seq": event_seq,
                        "lost_count": lost_count,
                    })
                    .to_string(),
                ),
            ),
            Self::Set {
                path,
                old,
                new,
                persisted,
            } => (
                0,
                Arc::from(
                    {
                        let mut body = json!({
                            "path": path,
                            "old": old.wire_value(),
                            "new": new.wire_value(),
                        });
                        if let Some(persisted) = persisted {
                            body["persisted"] = json!(persisted);
                        }
                        body
                    }
                    .to_string(),
                ),
            ),
            Self::Validation(SetValidationError::UnknownPath) => error("unknown_path"),
            Self::Validation(SetValidationError::ReadOnly) => error("read_only"),
            Self::Validation(SetValidationError::InvalidValue {
                path,
                expected,
                range,
            }) => (
                10,
                Arc::from(
                    json!({
                        "error": "invalid_value",
                        "path": path,
                        "expected": expected,
                        "range": range,
                    })
                    .to_string(),
                ),
            ),
            Self::Window {
                id,
                generation,
                title,
                app_id,
                minimized,
                changed,
            } => (
                0,
                Arc::from(
                    json!({
                        "id": id,
                        "generation": generation,
                        "title": title.as_deref(),
                        "app_id": app_id.as_deref(),
                        "minimized": minimized,
                        "changed": changed,
                    })
                    .to_string(),
                ),
            ),
            Self::WindowTarget { id, error } => {
                let body = match error {
                    WindowTargetError::UnknownWindow => {
                        json!({"error": "unknown_window", "id": id})
                    }
                    WindowTargetError::StaleTarget { requested, current } => json!({
                        "error": "stale_target",
                        "id": id,
                        "generation": requested,
                        "current": current,
                    }),
                    WindowTargetError::NotManaged => json!({"error": "not_managed", "id": id}),
                    WindowTargetError::NotMapped => json!({"error": "not_mapped", "id": id}),
                };
                (10, Arc::from(body.to_string()))
            }
            Self::NotFound { minimized_count } => (
                10,
                Arc::from(
                    json!({"error": "not_found", "minimized_count": minimized_count}).to_string(),
                ),
            ),
            Self::InvalidArgs { field, allowed } => (
                10,
                Arc::from(
                    json!({"error": "invalid_args", "field": field, "allowed": allowed})
                        .to_string(),
                ),
            ),
            Self::Locked => error("locked"),
            Self::Busy => error("busy"),
            Self::Body(body) => (0, Arc::from(body.to_string())),
            Self::Refused {
                error: code,
                detail,
            } => {
                let mut body = serde_json::Map::new();
                body.insert("error".into(), json!(code));
                if let Value::Object(fields) = detail {
                    for (name, value) in fields {
                        if name != "error" {
                            body.insert(name, value);
                        }
                    }
                }
                (10, Arc::from(Value::Object(body).to_string()))
            }
        }
    }
}

/// The body a reply over [`MAX_REPLY_WIRE_BYTES`] is replaced with, given the
/// measured wire size the transport computed (the measuring itself needs the
/// Bus framing and stays with the transport).
pub fn enforce_wire_limit(wire_bytes: usize, rc: u8, body: Arc<str>) -> (u8, Arc<str>) {
    if wire_bytes > MAX_REPLY_WIRE_BYTES {
        let (rc, body) = too_large(MAX_REPLY_BODY_BYTES);
        with_error_code(rc, body)
    } else {
        (rc, body)
    }
}

/// `^[a-z][a-z0-9-]{1,30}$`: the ABP service-name grammar.
pub fn validate_service_name(name: &str) -> Result<(), String> {
    let valid = (2..=31).contains(&name.len())
        && name
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "invalid Bus service name '{name}': expected ^[a-z][a-z0-9-]{{1,30}}$"
        ))
    }
}
