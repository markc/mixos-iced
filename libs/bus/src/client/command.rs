// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::BTreeMap;

/// A frame from the broker that is not a response to one of our requests:
/// a request from another service, or a topic delivery.
///
/// `headers` carries every Bus header of the original message; the named
/// fields are shortcuts into it.
#[derive(Debug)]
pub struct IncomingCommand {
    /// The `from` header: the requesting service, or the publisher.
    pub from: String,
    /// The `command` header. Empty for a topic delivery whose envelope
    /// carries no `command`; a consumer that dispatches on `command` must
    /// treat `""` as "not a verb" and look at [`topic`](Self::topic).
    pub command: String,
    /// The `id` header, which a response echoes back for correlation.
    pub id: Option<String>,
    /// The body parsed as JSON, or `Null` when the body is empty or is not
    /// JSON. The verbatim text is in `body`.
    pub args: serde_json::Value,
    pub body: String,
    /// Every Bus header of the original message.
    pub headers: BTreeMap<String, String>,
}

impl IncomingCommand {
    /// A header by name.
    pub fn header(&self, key: &str) -> Option<&str> {
        self.headers.get(key).map(|s| s.as_str())
    }

    /// Whether this frame is a broker topic delivery (it carries a `topic`
    /// header). Dispatch subscriptions on this, never on `command`, which
    /// may be empty for a delivery.
    pub fn is_topic_delivery(&self) -> bool {
        self.headers.contains_key("topic")
    }

    /// The `topic` header of a delivery.
    pub fn topic(&self) -> Option<&str> {
        self.header("topic")
    }
}
