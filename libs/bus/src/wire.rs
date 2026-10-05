// SPDX-License-Identifier: MIT OR Apache-2.0

//! The Bus wire format: frontmatter-framed headers with an optional body.
//!
//! Every Bus message is a `---\n` delimited header block followed by an
//! optional body:
//!
//! ```text
//! ---
//! command: get
//! rc: 0
//! ---
//! {"key": "value"}
//! ```
//!
//! The same framing is used on every transport: local sockets, the broker's
//! WebSocket relay and log files. Headers serialise in sorted key order
//! (`BTreeMap`), so two messages with the same headers and body are
//! byte-identical on the wire.

use std::collections::BTreeMap;
use std::fmt;

/// The minimum valid Bus message: a heartbeat, ACK or keepalive.
pub const EMPTY_MESSAGE: &str = "---\n---\n";

/// Maximum size of a single Bus message read from a local transport. A
/// larger message is rejected rather than buffered in full, which bounds a
/// local memory-exhaustion attack. 16 MiB is generous headroom over the
/// largest legitimate bodies (subscription snapshots, stats).
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// Maximum size of one WebSocket frame on the broker path. A Bus message is
/// written as a single WebSocket frame there, so its effective per-message
/// ceiling is `min(MAX_MESSAGE_BYTES, WS_MAX_FRAME_BYTES)`.
pub const WS_MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Maximum number of header lines parsed from a single Bus message. Bounds
/// the map a hostile frame can force the parser to build: a 16 MiB frame of
/// one-byte header lines would otherwise allocate millions of entries. A
/// legitimate message carries a handful of headers. Excess lines are
/// recorded in the [`ParseReport`] (so [`parse_strict`] rejects them) and
/// parsing stops.
pub const MAX_HEADERS: usize = 4096;

/// A parsed Bus message: sorted headers plus an optional body.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BusMessage {
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

impl BusMessage {
    /// An empty message.
    pub fn new() -> Self {
        Self::default()
    }

    /// The empty Bus message (heartbeat/keepalive).
    pub fn empty() -> Self {
        Self::new()
    }

    /// Construct a command from header pairs, preserving the frozen wire shape.
    pub fn command(headers: impl IntoIterator<Item = (&'static str, String)>) -> Self {
        Self {
            headers: headers
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
            body: String::new(),
        }
    }

    pub fn from_addr(&self) -> Option<&str> {
        self.get("from")
    }
    pub fn to_addr(&self) -> Option<&str> {
        self.get("to")
    }
    pub fn args(&self) -> Option<serde_json::Value> {
        self.get("args").and_then(|s| serde_json::from_str(s).ok())
    }
    pub fn json_payload(&self) -> Option<serde_json::Value> {
        self.get("json").and_then(|s| serde_json::from_str(s).ok())
    }
    pub fn ui_id(&self) -> Option<&str> {
        self.get("id")
    }
    pub fn target(&self) -> Option<&str> {
        self.get("target")
    }
    pub fn parent(&self) -> Option<&str> {
        self.get("parent")
    }
    pub fn source(&self) -> Option<&str> {
        self.get("source")
    }
    pub fn is_ui_command(&self) -> bool {
        self.command_name().is_some_and(|c| c.starts_with("ui."))
    }

    /// Add a header (builder form).
    pub fn with_header(mut self, key: &str, value: &str) -> Self {
        self.headers.insert(key.to_string(), value.to_string());
        self
    }

    /// Set the body (builder form).
    pub fn with_body(mut self, body: &str) -> Self {
        self.body = body.to_string();
        self
    }

    /// Add or replace a header.
    pub fn set(&mut self, key: &str, value: &str) {
        self.headers.insert(key.to_string(), value.to_string());
    }

    /// A header value.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.headers.get(key).map(|s| s.as_str())
    }

    /// Whether this is the empty message (heartbeat/keepalive).
    pub fn is_empty_message(&self) -> bool {
        self.headers.is_empty() && self.body.is_empty()
    }

    /// The `command` header.
    pub fn command_name(&self) -> Option<&str> {
        self.get("command")
    }

    /// The `type` header (`request`, `response`, `event` or `stream`).
    pub fn message_type(&self) -> Option<&str> {
        self.get("type")
    }

    /// The `rc` header as a number; `None` when absent or not an integer.
    pub fn rc(&self) -> Option<u8> {
        self.get("rc").and_then(|rc| rc.parse().ok())
    }

    /// Human-readable detail for an error reply (`rc >= 10`), in priority
    /// order: the `error` header; a `{error_code, message}` body rendered
    /// as `CODE: message`; an `{"error": …}` body field; the raw body text;
    /// and finally `"unknown error"` when the reply carries no detail.
    pub fn error_message(&self) -> String {
        if let Some(e) = self.get("error") {
            return e.to_string();
        }
        let body = self.body.trim();
        if !body.is_empty() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
                if let (Some(code), Some(message)) = (
                    v.get("error_code").and_then(|c| c.as_str()),
                    v.get("message").and_then(|m| m.as_str()),
                ) {
                    return format!("{code}: {message}");
                }
                if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
                    return e.to_string();
                }
            }
            return body.to_string();
        }
        "unknown error".to_string()
    }

    /// Serialise to wire bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.to_wire().into_bytes()
    }

    /// Serialise to the wire string. The body gets a trailing newline when
    /// it lacks one; an empty body adds nothing after the closing `---`.
    pub fn to_wire(&self) -> String {
        let mut out = String::from("---\n");
        for (k, v) in &self.headers {
            out.push_str(k);
            out.push_str(": ");
            out.push_str(v);
            out.push('\n');
        }
        out.push_str("---\n");
        if !self.body.is_empty() {
            out.push_str(&self.body);
            if !self.body.ends_with('\n') {
                out.push('\n');
            }
        }
        out
    }
}

impl fmt::Display for BusMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_wire())
    }
}

/// Why a string is not a Bus message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The text does not start with the `---\n` opening fence.
    MissingOpeningFence { head: String },
    /// `parse_strict` found content a strict reader rejects.
    NonCompliant {
        skipped_lines: usize,
        json_parse_errors: usize,
        first: String,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::MissingOpeningFence { head } => {
                write!(f, "Bus message must start with '---\\n', got: {head:?}")
            }
            ParseError::NonCompliant {
                skipped_lines,
                json_parse_errors,
                first,
            } => write!(
                f,
                "Bus message non-compliant: {skipped_lines} skipped line(s), \
                 {json_parse_errors} json parse error(s); first: {first}"
            ),
        }
    }
}

impl std::error::Error for ParseError {}

/// What the parser could not strictly interpret. Canonical Bus consumers
/// require this to be empty; readers of legacy or external content may
/// accept a non-empty report but should log it.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ParseReport {
    /// Header block lines that did not strictly match `key: value`:
    /// typically YAML list items, indented continuations, nested-object
    /// children, anchors or bare scalars. Entries are `(line_number,
    /// content)` with 1-indexed line numbers within the header block. The
    /// headers map still contains anything that split on `": "`; this list
    /// surfaces what a strict parser would reject.
    pub skipped_lines: Vec<(usize, String)>,
    /// Header values starting with `[` or `{` that are not valid JSON, as
    /// `(key, raw_value)`. The raw value stays in the headers map as a
    /// string.
    pub json_parse_errors: Vec<(String, String)>,
}

impl ParseReport {
    pub fn is_empty(&self) -> bool {
        self.skipped_lines.is_empty() && self.json_parse_errors.is_empty()
    }
}

/// Parse a Bus message and also return what the parser could not strictly
/// interpret. The headers map is populated identically to [`parse`]; the
/// report lets a caller police strictness at its own layer.
pub fn parse_lenient(raw: &str) -> Result<(BusMessage, ParseReport), ParseError> {
    let content = raw
        .strip_prefix("---\n")
        .ok_or_else(|| ParseError::MissingOpeningFence {
            head: raw.chars().take(40).collect(),
        })?;

    let (header_block, body) = match content.split_once("\n---\n") {
        Some((h, b)) => (h, b),
        None => {
            let h = content
                .strip_suffix("\n---\n")
                .or_else(|| content.strip_suffix("\n---"))
                .or_else(|| content.strip_suffix("---\n"))
                .or_else(|| content.strip_suffix("---"))
                .unwrap_or(content);
            (h, "")
        }
    };

    let mut headers = BTreeMap::new();
    let mut skipped_lines = Vec::new();
    let mut json_parse_errors = Vec::new();

    // Count every non-empty line processed, not distinct map entries: each
    // processed line can grow `headers`, `skipped_lines` or
    // `json_parse_errors`, and duplicate keys (which overwrite) or repeated
    // malformed lines would otherwise bypass a map-size cap.
    let mut processed = 0usize;
    for (idx, line) in header_block.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        if processed >= MAX_HEADERS {
            skipped_lines.push((
                idx + 1,
                format!("header line count exceeds {MAX_HEADERS}; remaining lines skipped"),
            ));
            break;
        }
        processed += 1;
        match line.split_once(": ") {
            Some((k, v)) => {
                let key = k.trim().to_string();
                let val = v.trim().to_string();
                // A line with leading whitespace, or an empty or spaced key,
                // is still inserted (lenient) but flagged for strict readers.
                if line.starts_with(|c: char| c.is_whitespace())
                    || key.is_empty()
                    || key.contains(char::is_whitespace)
                {
                    skipped_lines.push((idx + 1, line.to_string()));
                }
                // A value starting with `[` or `{` may be JSON; validate it
                // opportunistically and record a failure without discarding
                // the raw value.
                if let Some(first) = val.chars().find(|c| !c.is_whitespace())
                    && (first == '[' || first == '{')
                    && serde_json::from_str::<serde_json::Value>(&val).is_err()
                {
                    json_parse_errors.push((key.clone(), val.clone()));
                }
                headers.insert(key, val);
            }
            None => {
                // No `": "` on the line: a comment, list item, bare scalar
                // or anchor. Record and skip.
                skipped_lines.push((idx + 1, line.to_string()));
            }
        }
    }

    Ok((
        BusMessage {
            headers,
            body: body.trim_end().to_string(),
        },
        ParseReport {
            skipped_lines,
            json_parse_errors,
        },
    ))
}

/// Parse a Bus message, rejecting any header line that does not strictly
/// conform and any `[`/`{` value that is not valid JSON. For canonical Bus
/// content.
pub fn parse_strict(raw: &str) -> Result<BusMessage, ParseError> {
    let (msg, report) = parse_lenient(raw)?;
    if !report.is_empty() {
        let first = report
            .skipped_lines
            .iter()
            .map(|(n, l)| format!("line {n}: {l:?}"))
            .chain(
                report
                    .json_parse_errors
                    .iter()
                    .map(|(k, v)| format!("json-parse {k}: {v:?}")),
            )
            .next()
            .unwrap_or_default();
        return Err(ParseError::NonCompliant {
            skipped_lines: report.skipped_lines.len(),
            json_parse_errors: report.json_parse_errors.len(),
            first,
        });
    }
    Ok(msg)
}

/// Parse a Bus message from raw text. Non-compliant lines are skipped
/// silently. Use [`parse_strict`] for canonical content and
/// [`parse_lenient`] to inspect what was skipped.
pub fn parse(raw: &str) -> Result<BusMessage, ParseError> {
    parse_lenient(raw).map(|(msg, _report)| msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_with_body() {
        let msg = BusMessage::new()
            .with_header("command", "get")
            .with_header("rc", "0")
            .with_body(r#"{"key": "value"}"#);

        let raw = String::from_utf8(msg.to_bytes()).unwrap();
        let parsed = parse(&raw).unwrap();

        assert_eq!(parsed.get("command"), Some("get"));
        assert_eq!(parsed.get("rc"), Some("0"));
        assert_eq!(parsed.rc(), Some(0));
        assert_eq!(parsed.body, r#"{"key": "value"}"#);
    }

    #[test]
    fn round_trip_no_body() {
        let msg = BusMessage::new()
            .with_header("command", "ping")
            .with_header("rc", "0");

        let parsed = parse(&msg.to_wire()).unwrap();
        assert_eq!(parsed.get("command"), Some("ping"));
        assert!(parsed.body.is_empty());
    }

    #[test]
    fn round_trip_error_response() {
        let msg = BusMessage::new()
            .with_header("rc", "10")
            .with_header("error", "Port not found");

        let parsed = parse(&msg.to_wire()).unwrap();
        assert_eq!(parsed.rc(), Some(10));
        assert_eq!(parsed.get("error"), Some("Port not found"));
        assert!(parsed.body.is_empty());
    }

    #[test]
    fn wire_bytes_are_exact() {
        // Headers in sorted key order, `: ` separator, body newline-terminated.
        let msg = BusMessage::new()
            .with_header("to", "noded")
            .with_header("command", "topic.publish")
            .with_header("from", "compd")
            .with_body("{\"a\":1}");
        assert_eq!(
            msg.to_wire(),
            "---\ncommand: topic.publish\nfrom: compd\nto: noded\n---\n{\"a\":1}\n"
        );
        // A body that already ends in a newline gains nothing.
        let msg = BusMessage::new().with_body("x\n");
        assert_eq!(msg.to_wire(), "---\n---\nx\n");
    }

    #[test]
    fn parse_minimal() {
        let parsed = parse(EMPTY_MESSAGE).unwrap();
        assert!(parsed.headers.is_empty());
        assert!(parsed.body.is_empty());
        assert!(parsed.is_empty_message());
        assert_eq!(BusMessage::empty().to_wire(), EMPTY_MESSAGE);
    }

    #[test]
    fn missing_fence_is_an_error() {
        let err = parse("command: ping\n").unwrap_err();
        assert!(matches!(err, ParseError::MissingOpeningFence { .. }));
        assert!(err.to_string().contains("must start with"));
    }

    #[test]
    fn header_only_message_without_closing_newline() {
        let msg = parse("---\ncommand: ping\n---").unwrap();
        assert_eq!(msg.command_name(), Some("ping"));
        assert!(msg.body.is_empty());
    }

    #[test]
    fn display_trait() {
        let msg = BusMessage::new().with_header("command", "ping");
        let display = format!("{msg}");
        assert!(display.starts_with("---\n"));
        assert!(display.contains("command: ping"));
    }

    #[test]
    fn convenience_accessors() {
        let raw = "---\ntype: request\nfrom: mix\nto: maild\ncommand: status\n---\n";
        let msg = parse(raw).unwrap();
        assert_eq!(msg.message_type(), Some("request"));
        assert_eq!(msg.get("from"), Some("mix"));
        assert_eq!(msg.get("to"), Some("maild"));
        assert_eq!(msg.command_name(), Some("status"));
        assert_eq!(msg.rc(), None);
    }

    #[test]
    fn parse_lenient_clean_message() {
        let (msg, report) = parse_lenient("---\ncommand: status\nrc: 0\n---\n").unwrap();
        assert_eq!(msg.get("command"), Some("status"));
        assert!(
            report.is_empty(),
            "clean Bus should report empty: {report:?}"
        );
    }

    #[test]
    fn parse_lenient_yaml_list() {
        // A bare-colon key and two indented list items: three non-Bus lines.
        let raw = "---\ntitle: foo\ndraws_from:\n  - A\n  - B\n---\n";
        let (msg, report) = parse_lenient(raw).unwrap();
        assert_eq!(msg.get("title"), Some("foo"));
        assert_eq!(msg.get("draws_from"), None);
        assert_eq!(report.skipped_lines.len(), 3);
        assert!(report.skipped_lines.iter().any(|(_, l)| l == "draws_from:"));
        assert!(report.skipped_lines.iter().any(|(_, l)| l.contains("- A")));
        assert!(report.skipped_lines.iter().any(|(_, l)| l.contains("- B")));
    }

    #[test]
    fn parse_lenient_comment_line() {
        let (msg, report) = parse_lenient("---\n# a comment\ncommand: ping\n---\n").unwrap();
        assert_eq!(msg.get("command"), Some("ping"));
        assert_eq!(report.skipped_lines.len(), 1);
        assert!(report.skipped_lines[0].1.starts_with('#'));
    }

    #[test]
    fn parse_lenient_json_value_valid() {
        let (msg, report) = parse_lenient("---\nargs: {\"limit\": 10}\n---\n").unwrap();
        assert_eq!(msg.get("args"), Some(r#"{"limit": 10}"#));
        assert!(report.json_parse_errors.is_empty());
    }

    #[test]
    fn parse_lenient_flow_syntax_fails_json() {
        // Unquoted flow syntax is not JSON; the raw value is kept as a string.
        let (msg, report) = parse_lenient("---\ndraws_from: [A, B, C]\n---\n").unwrap();
        assert_eq!(msg.get("draws_from"), Some("[A, B, C]"));
        assert_eq!(report.json_parse_errors.len(), 1);
        assert_eq!(report.json_parse_errors[0].0, "draws_from");
        assert_eq!(report.json_parse_errors[0].1, "[A, B, C]");
    }

    #[test]
    fn parse_strict_accepts_clean() {
        let msg = parse_strict("---\ncommand: ping\nrc: 0\n---\n").unwrap();
        assert_eq!(msg.get("command"), Some("ping"));
    }

    #[test]
    fn parse_strict_rejects_yaml_list() {
        let err = parse_strict("---\ntitle: foo\ndraws_from:\n  - A\n---\n").unwrap_err();
        assert!(matches!(err, ParseError::NonCompliant { .. }));
        assert!(err.to_string().contains("non-compliant"), "{err}");
    }

    #[test]
    fn parse_strict_rejects_flow_syntax() {
        let err = parse_strict("---\ndraws_from: [A, B, C]\n---\n").unwrap_err();
        assert!(err.to_string().contains("non-compliant"), "{err}");
    }

    #[test]
    fn parse_matches_lenient_headers() {
        let raw = "---\nkey: val\n  indented: foo\n---\n";
        let a = parse(raw).unwrap();
        let (b, _report) = parse_lenient(raw).unwrap();
        assert_eq!(a.headers, b.headers);
    }

    #[test]
    fn error_message_prefers_header() {
        let mut msg = BusMessage::new().with_header("error", "header wins");
        msg.body = r#"{"error":"body loses"}"#.to_string();
        assert_eq!(msg.error_message(), "header wins");
    }

    #[test]
    fn error_message_falls_back_to_json_body() {
        let msg = BusMessage::new()
            .with_body(r#"{"error":"invalid type: floating point `2.0`, expected usize"}"#);
        assert_eq!(
            msg.error_message(),
            "invalid type: floating point `2.0`, expected usize"
        );
    }

    #[test]
    fn error_message_renders_the_unified_shape_and_keeps_legacy_text() {
        let unified = BusMessage::new().with_body(
            r#"{"error_code":"EMPTY_EDGE","message":"edge has no registered pages","edge":"top"}"#,
        );
        assert_eq!(
            unified.error_message(),
            "EMPTY_EDGE: edge has no registered pages"
        );
        // A reply with a code beside `error` keeps its exact text.
        let legacy = BusMessage::new().with_body(
            r#"{"error_code":"PANEL_THICKNESS_BUDGET","error":"panel thickness exceeds output budget"}"#,
        );
        assert_eq!(
            legacy.error_message(),
            "panel thickness exceeds output budget"
        );
        // `message` without a code is not the unified shape; the raw body stands.
        let bare = BusMessage::new().with_body(r#"{"message":"hello"}"#);
        assert_eq!(bare.error_message(), r#"{"message":"hello"}"#);
    }

    #[test]
    fn error_message_falls_back_to_raw_body_then_unknown() {
        assert_eq!(
            BusMessage::new()
                .with_body("plain text failure")
                .error_message(),
            "plain text failure"
        );
        assert_eq!(BusMessage::new().error_message(), "unknown error");
    }

    #[test]
    fn header_count_is_capped() {
        let mut raw = String::from("---\n");
        for i in 0..(MAX_HEADERS + 50) {
            raw.push_str(&format!("k{i}: v\n"));
        }
        raw.push_str("---\n");
        let (msg, report) = parse_lenient(&raw).unwrap();
        assert_eq!(msg.headers.len(), MAX_HEADERS);
        assert!(
            !report.is_empty(),
            "overflow is recorded so strict callers reject"
        );
        assert!(parse_strict(&raw).is_err());
    }

    #[test]
    fn duplicate_keys_do_not_bypass_cap() {
        // Duplicate keys collapse in the map; the cap counts processed lines.
        let mut raw = String::from("---\n");
        for _ in 0..(MAX_HEADERS + 100) {
            raw.push_str("dup: v\n");
        }
        raw.push_str("---\n");
        let (msg, report) = parse_lenient(&raw).unwrap();
        assert_eq!(msg.headers.len(), 1);
        assert!(
            report
                .skipped_lines
                .iter()
                .any(|(_, l)| l.contains("exceeds"))
        );
    }

    #[test]
    fn headers_under_cap_parse_fully() {
        let (msg, report) =
            parse_lenient("---\ncommand: get\nrc: 0\nfrom: node1\n---\nbody").unwrap();
        assert_eq!(msg.headers.len(), 3);
        assert!(report.is_empty());
        assert_eq!(msg.body, "body");
    }
}

// SPDX-License-Identifier: MIT OR Apache-2.0

#[cfg(feature = "native")]
mod transport {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Read a Bus message from a Unix stream (reads until EOF).
    ///
    /// The sender must shut down their write side to signal EOF.
    pub async fn read_from_stream(
        stream: &mut tokio::net::UnixStream,
    ) -> anyhow::Result<BusMessage> {
        let mut buf = Vec::with_capacity(4096);

        // Read with a timeout (hung clients) AND a byte cap (memory DoS):
        // `take` one byte past the limit so an over-cap frame still reads
        // enough to be detected, then reject. Without the cap, a local
        // peer could force an unbounded `read_to_end` allocation.
        let mut limited = stream.take(MAX_MESSAGE_BYTES as u64 + 1);
        match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            limited.read_to_end(&mut buf),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => anyhow::bail!("Read error: {e}"),
            Err(_) => anyhow::bail!("Bus read timed out (10s)"),
        }

        if buf.is_empty() {
            anyhow::bail!("Empty Bus message (no data received)");
        }
        if buf.len() > MAX_MESSAGE_BYTES {
            anyhow::bail!("Bus message exceeds {MAX_MESSAGE_BYTES} byte limit");
        }

        let raw = String::from_utf8(buf)?;
        Ok(parse(&raw)?)
    }

    /// Write a Bus message to a Unix stream.
    pub async fn write_to_stream(
        stream: &mut tokio::net::UnixStream,
        msg: &BusMessage,
    ) -> anyhow::Result<()> {
        stream.write_all(&msg.to_bytes()).await?;
        Ok(())
    }
}

#[cfg(feature = "native")]
pub use transport::{read_from_stream, write_to_stream};

// ── Bus Address ──

/// Maximum length of a single DNS-style label.
const MAX_LABEL_LEN: usize = 63;

/// Maximum total length of a Bus address (including `@<mesh-fqdn>` suffix).
const MAX_ADDRESS_LEN: usize = 253;

/// A local Bus address, per SPEC 01 §4.1.
///
/// Canonical forms (`.bus` suffix optional on 2-/3-label forms):
/// - `<service>.<node>[.bus]` — service on a node
/// - `<sub>.<service>.<node>[.bus]` — sub-protocol/instance on a service on a node
/// - `<node>.bus` — the node itself (its broker; service implicit `noded`)
///
/// The `<sub>` slot is opaque to the broker: the broker routes by
/// `<service>.<node>`, and the destination service interprets `<sub>` to
/// demultiplex internal endpoints (e.g. `maild` treats `imap` as the IMAP
/// sub-protocol; `disp-skia` treats `editor` as a window/instance ID).
///
/// Bare `<service>` (no dot, no `.bus` suffix) is NOT a parseable address;
/// it is a local-only shorthand the caller hands to the broker registry
/// directly. See `BusTarget::parse` for the full target shape including
/// cross-mesh.
///
/// Examples:
/// ```
/// # use bus::wire::{BusAddress, BusTarget};
/// let t = BusTarget::parse("imap.maild.alpha.bus").unwrap();
/// let addr = t.local();
/// assert_eq!(addr.sub.as_deref(), Some("imap"));
/// assert_eq!(addr.service.as_deref(), Some("maild"));
/// assert_eq!(addr.node, "alpha");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BusAddress {
    /// Optional sub-protocol/instance label. Opaque to the broker;
    /// interpreted by the destination service.
    pub sub: Option<String>,
    /// Service name. `None` only for the `<node>.bus` form, which
    /// implicitly addresses the node's broker (noded).
    pub service: Option<String>,
    /// Node name. Always present.
    pub node: String,
}

/// A resolved Bus routing target, per SPEC 01 §4.
///
/// `Local` is the in-mesh form (no `@`). `CrossMesh` is the cross-mesh form
/// (`<local-bus>@<mesh-fqdn>`); routers MUST refuse this with `cross-mesh
/// routing not implemented` until federation transport exists.
///
/// The enum makes the routing distinction type-level: every router branch
/// must explicitly handle (or refuse) `CrossMesh` — a passive `mesh:
/// Option<String>` field on `BusAddress` would allow code to accidentally
/// deliver a cross-mesh address to a local service whose node name happened
/// to match.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BusTarget {
    /// In-mesh address.
    Local(BusAddress),
    /// Cross-mesh address. Reserved at the parser; refused at the router
    /// until federation transport is designed.
    CrossMesh {
        /// The mesh-local part (left of `@`).
        local: BusAddress,
        /// The destination mesh FQDN (right of `@`). Strict
        /// IDNA-canonical: lowercase ASCII, contains at least one `.`,
        /// labels 1..=63 chars from `[a-z0-9-]` with no leading/trailing
        /// hyphen, no `xn--` punycode pending homograph review.
        mesh_fqdn: String,
    },
}

impl BusTarget {
    /// Parse a Bus target string per SPEC 01 §4.
    ///
    /// Returns `None` for inputs that are not Bus addresses (bare service
    /// shorthand without `.bus` and without dots, malformed labels, more
    /// than three left-side labels, invalid FQDN on the right of `@`,
    /// etc.). Callers fall back to direct service-registry lookup when
    /// this returns `None`.
    pub fn parse(s: &str) -> Option<Self> {
        if s.is_empty() || s.len() > MAX_ADDRESS_LEN {
            return None;
        }

        // Split on `@` for cross-mesh. Exactly one `@` permitted.
        let (local_str, mesh_fqdn) = match s.split_once('@') {
            Some((l, r)) => {
                if r.contains('@') || r.is_empty() || l.is_empty() {
                    return None;
                }
                (l, Some(r))
            }
            None => (s, None),
        };

        let local = BusAddress::parse_local(local_str)?;

        match mesh_fqdn {
            None => Some(BusTarget::Local(local)),
            Some(fqdn) => {
                let normalised = validate_mesh_fqdn(fqdn)?;
                Some(BusTarget::CrossMesh {
                    local,
                    mesh_fqdn: normalised,
                })
            }
        }
    }

    /// Borrow the local component regardless of variant. Useful when a
    /// caller has already verified (or refused) the `CrossMesh` case.
    pub fn local(&self) -> &BusAddress {
        match self {
            BusTarget::Local(a) => a,
            BusTarget::CrossMesh { local, .. } => local,
        }
    }

    /// True if this is a cross-mesh target. Routers MUST check this before
    /// dispatching and refuse with `cross-mesh routing not implemented`.
    pub fn is_cross_mesh(&self) -> bool {
        matches!(self, BusTarget::CrossMesh { .. })
    }
}

impl BusAddress {
    /// Parse a *local* (no `@`) Bus address per SPEC 01 §4.1. Prefer
    /// `BusTarget::parse` which also handles the cross-mesh form.
    ///
    /// Accepts (with optional `.bus` suffix on 2-/3-label forms):
    /// - `<node>.bus` — node only (service implicit, sub absent)
    /// - `<service>.<node>` or `<service>.<node>.bus`
    /// - `<sub>.<service>.<node>` or `<sub>.<service>.<node>.bus`
    pub fn parse_local(s: &str) -> Option<Self> {
        if s.is_empty() || s.contains('@') {
            return None;
        }

        // Strip optional `.bus` suffix. Remember whether the suffix was
        // explicit, since single-label inputs require it (`alpha.bus`
        // is the node form; bare `alpha` is service shorthand and is
        // NOT a parseable address).
        let (stem, had_bus_suffix) = match s.strip_suffix(".bus") {
            Some(stripped) => (stripped, true),
            None => (s, false),
        };

        if stem.is_empty() {
            return None;
        }

        let parts: Vec<&str> = stem.split('.').collect();

        // Reject empty labels (`.foo`, `foo..bar`, `foo.`) and >3 labels.
        if parts.iter().any(|p| p.is_empty()) || parts.len() > 3 {
            return None;
        }

        // Validate each label as a DNS-style ASCII label.
        for part in &parts {
            if !is_valid_label(part) {
                return None;
            }
        }

        match parts.len() {
            1 => {
                // Single label is the node-only form `<node>.bus` and
                // requires the explicit `.bus` suffix. Bare `<service>`
                // is shorthand and must not parse as an address.
                if !had_bus_suffix {
                    return None;
                }
                Some(Self {
                    sub: None,
                    service: None,
                    node: parts[0].to_string(),
                })
            }
            2 => Some(Self {
                sub: None,
                service: Some(parts[0].to_string()),
                node: parts[1].to_string(),
            }),
            3 => Some(Self {
                sub: Some(parts[0].to_string()),
                service: Some(parts[1].to_string()),
                node: parts[2].to_string(),
            }),
            _ => None,
        }
    }

    /// Check if this address targets a specific node.
    pub fn is_for_node(&self, node_name: &str) -> bool {
        self.node == node_name
    }

    /// Resolve the service name for routing. `None` indicates the node's
    /// broker (the `<node>.bus` form); callers typically map this to
    /// `"noded"`.
    pub fn service_name(&self) -> Option<&str> {
        self.service.as_deref()
    }
}

impl fmt::Display for BusAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(sub) = &self.sub {
            write!(f, "{sub}.")?;
        }
        if let Some(service) = &self.service {
            write!(f, "{service}.")?;
        }
        write!(f, "{}.bus", self.node)
    }
}

impl fmt::Display for BusTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BusTarget::Local(a) => write!(f, "{a}"),
            BusTarget::CrossMesh { local, mesh_fqdn } => {
                write!(f, "{local}@{mesh_fqdn}")
            }
        }
    }
}

/// Validate a DNS-style label per SPEC 01 §4.1: 1..=63 ASCII characters
/// from `[a-z0-9-]`, not starting or ending with `-`.
///
/// This is the fleet-wide label grammar authority; inventory and routing
/// validators must use it rather than maintaining a parallel grammar.
pub fn is_valid_label(label: &str) -> bool {
    let bytes = label.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_LABEL_LEN {
        return false;
    }
    if bytes[0] == b'-' || *bytes.last().unwrap() == b'-' {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// Validate a mesh FQDN (right-hand side of `@`) per SPEC 01 §4.2.
/// Returns the normalised form (currently identical to input on success)
/// or `None` if invalid.
///
/// Rules: total ≤ 253 chars, contains at least one `.`, each label
/// 1..=63 chars from `[a-z0-9-]` with no leading/trailing hyphen, no
/// trailing dot, no `xn--` punycode (pending homograph review).
fn validate_mesh_fqdn(fqdn: &str) -> Option<String> {
    if fqdn.is_empty() || fqdn.len() > MAX_ADDRESS_LEN {
        return None;
    }
    if !fqdn.contains('.') || fqdn.ends_with('.') {
        return None;
    }
    for label in fqdn.split('.') {
        if !is_valid_label(label) {
            return None;
        }
        if label.starts_with("xn--") {
            return None;
        }
    }
    Some(fqdn.to_string())
}

// ── Validation ──

/// Known Bus header fields.
pub const KNOWN_HEADERS: &[&str] = &[
    // Core protocol
    "bus",
    "type",
    "id",
    "from",
    "to",
    "command",
    "args",
    "json",
    "reply-to",
    "ttl",
    "error",
    "timestamp",
    "rc",
    // Display protocol — window
    "parent",
    "title",
    "width",
    "height",
    "position",
    "decorations",
    "layer",
    "sticky",
    // Display protocol — layout
    "layout",
    "gap",
    "padding",
    "align",
    "scrollable",
    "overflow",
    // Display protocol — style
    "background",
    "text_color",
    "border_color",
    "border_width",
    "border_radius",
    "font_size",
    "opacity",
    // Display protocol — targeting
    "target",
    "source",
    "name",
    // Display protocol — permissions (federated)
    "source_peer",
    "permissions",
];

/// Valid message types.
pub const VALID_TYPES: &[&str] = &["request", "response", "event", "stream"];

/// Validate a Bus message for protocol conformance.
///
/// Returns a list of warnings (not errors — Bus is permissive).
/// An empty Vec means the message is fully conformant.
pub fn validate(msg: &BusMessage) -> Vec<String> {
    let mut warnings = Vec::new();

    // Empty messages are always valid
    if msg.is_empty_message() {
        return warnings;
    }

    // Check for unknown headers
    for key in msg.headers.keys() {
        if !KNOWN_HEADERS.contains(&key.as_str()) {
            warnings.push(format!("unknown header: {key}"));
        }
    }

    // Validate type field
    if let Some(msg_type) = msg.get("type")
        && !VALID_TYPES.contains(&msg_type)
    {
        warnings.push(format!("invalid type: {msg_type}"));
    }

    // Validate args is valid JSON
    if let Some(args) = msg.get("args")
        && serde_json::from_str::<serde_json::Value>(args).is_err()
    {
        warnings.push("args is not valid JSON".to_string());
    }

    // Validate json payload is valid JSON
    if let Some(json) = msg.get("json")
        && serde_json::from_str::<serde_json::Value>(json).is_err()
    {
        warnings.push("json payload is not valid JSON".to_string());
    }

    // Validate rc is numeric
    if let Some(rc) = msg.get("rc")
        && rc.parse::<u8>().is_err()
    {
        warnings.push(format!("rc is not a valid integer: {rc}"));
    }

    // Validate ttl is numeric
    if let Some(ttl) = msg.get("ttl")
        && ttl.parse::<u32>().is_err()
    {
        warnings.push(format!("ttl is not a valid integer: {ttl}"));
    }

    warnings
}
