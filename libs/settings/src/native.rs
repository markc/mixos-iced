// SPDX-License-Identifier: MIT OR Apache-2.0
//! Optional native executor over the host's EXISTING supervised connection.
//! It never connects, takes the incoming receiver, spawns a task or polls.
use crate::{
    consumer::{Consumer, Work, WorkKind},
    *,
};
use bus::native_client::{IncomingCommand, SupervisedClient};
use std::time::Duration;

pub const BOOTSTRAP_BUDGET: Duration = Duration::from_secs(1);

/// Execute one fenced action. The host can multiplex this future with incoming
/// events and its existing work. A bootstrap host caps the combined initial
/// subscribe/read sequence at BOOTSTRAP_BUDGET before showing a labelled fallback.
/// Each individual recovery call is also bounded by that budget.
pub async fn execute(
    client: &SupervisedClient,
    work: &Work,
) -> Result<Option<Snapshot>, Diagnostic> {
    execute_until(client, work, tokio::time::Instant::now() + BOOTSTRAP_BUDGET).await
}

/// Pass the same initial deadline for subscribe AND read, so their combined
/// bootstrap cannot spend a second budget. Recovery uses execute() instead.
pub async fn execute_until(
    client: &SupervisedClient,
    work: &Work,
    deadline: tokio::time::Instant,
) -> Result<Option<Snapshot>, Diagnostic> {
    fn fault(code: &str, message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(code, "native", message)
    }
    if !client.is_connected() || client.connection_generation() != work.generation() {
        return Err(fault(
            "stale_connection",
            "Work belongs to another connection",
        ));
    }
    let deadline = deadline.min(tokio::time::Instant::now() + BOOTSTRAP_BUDGET);
    if deadline <= tokio::time::Instant::now() {
        return Err(fault("read_timeout", "Native work deadline already elapsed"));
    }
    let result = tokio::time::timeout_at(deadline, async {
        match work.kind() {
            WorkKind::Subscribe => {
                client
                    .subscribe_topic(&topic(&work.binding().profile))
                    .await
                    .map_err(|e| fault("subscribe_failed", e.to_string()))?;
                Ok(None)
            }
            WorkKind::Read => {
                let body = serde_json::to_value(ReadRequest {
                    binding: work.binding().clone(),
                })
                .map_err(|e| fault("invalid_request", e.to_string()))?;
                let value = match client
                    .call_typed("settingsd", "settings.get", body)
                    .await
                    .map_err(|e| fault("read_failed", e.to_string()))?
                {
                    bus::PortReply::Ok { value, .. } => value,
                    bus::PortReply::AppError { message, .. } => {
                        let structured = serde_json::from_str::<serde_json::Value>(&message).ok();
                        let status = structured.as_ref().and_then(|v| v.get("status")).and_then(|s| s.as_str());
                        let code = if matches!(status, Some("wrong_target" | "validation_failed" | "unsupported_schema" | "snapshot_too_large" | "not_served")) {
                            "authority_refused"
                        } else {
                            "read_failed"
                        };
                        return Err(fault(code, message));
                    }
                };
                let encoded =
                    serde_json::to_vec(&value).map_err(|e| fault("invalid_read", e.to_string()))?;
                if encoded.len() > MAX_SNAPSHOT_BYTES + 64 * 1024 {
                    return Err(fault(
                        "snapshot_too_large",
                        "Read response exceeds envelope budget",
                    ));
                }
                if value.get("status").and_then(|s| s.as_str()) != Some("current") {
                    return Err(fault(
                        "invalid_read",
                        "Authority did not return current state",
                    ));
                }
                serde_json::from_value(
                    value
                        .get("snapshot")
                        .cloned()
                        .ok_or_else(|| fault("invalid_read", "Missing snapshot"))?,
                )
                .map(Some)
                .map_err(|e| fault("invalid_snapshot", e.to_string()))
            }
        }
    })
    .await
    .map_err(|_| fault("read_timeout", "Native work exceeded its bootstrap/recovery deadline"))?;
    if !client.is_connected() || client.connection_generation() != work.generation() {
        return Err(fault(
            "stale_connection",
            "Connection changed during native work",
        ));
    }
    result
}

impl Consumer {
    /// Feed a multiplexed native command without taking over the app's receiver.
    /// Unrelated commands are ignored. The broker-authenticated authority stamp,
    /// exact topic and connection generation are checked before decoding.
    pub fn native_delivery(&mut self, command: &IncomingCommand) -> Option<Work> {
        if command.topic() != Some(topic(&self.binding().profile).as_str())
            || command.header("broker_service") != Some("settingsd")
            || self.generation() != Some(command.generation)
        {
            return None;
        }
        if command.body.len() > MAX_SNAPSHOT_BYTES {
            return self.rejected_delivery(Diagnostic::new("invalid_delivery", "snapshot", "Canonical delivery exceeds inline budget"));
        }
        match serde_json::from_str::<Snapshot>(&command.body) {
            Ok(snapshot) => self.observe(command.generation, snapshot),
            Err(error) => self.rejected_delivery(Diagnostic::new("invalid_delivery", "snapshot", error.to_string())),
        }
    }
}
