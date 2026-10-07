// SPDX-License-Identifier: MIT OR Apache-2.0
//! The shared `app.describe` discovery contract, `application.describe.v1`.
//!
//! One read-only verb serves a bounded product object plus a common identity
//! envelope. This module owns the envelope checks and the completion helpers;
//! each application owns its product object, its verb inventory and its actual
//! service registration. Nothing here opens a connection, reads settings or
//! mutates product state. The contract is registered in
//! `docs/spec/bus/verbs.conf.mix` under the single owner `application`; the
//! spec and machine-checkable fixtures live in `docs/spec/application/`.
//!
//! The canonical request is `{}`; empty and whitespace-only bodies are also
//! accepted for existing callers, and so is any body that parses as an empty
//! object — whitespace inside or around the braces is valid JSON. Everything
//! else is refused before dispatch: malformed JSON, non-object JSON and
//! nonempty objects are host-level argument errors, mapped by the caller to
//! its own rc and refusal body. [`validate_request`] bounds the body to
//! [`MAX_REQUEST_BYTES`] before parsing.
//!
//! [`complete`] adds `describe_contract`, `version`, `pid`, `service` and
//! `app_id` to a product object whose verb inventory already lists
//! `app.describe`. A reserved field that already exists must agree with the
//! supplied identity; a contradiction is an error and the value is left
//! untouched. The completed value is validated as a whole before any update
//! is applied, so a contradiction can never leave a half-completed response.
//! Product fields, nested values and verb representations/order are preserved;
//! a `Vec<String>` inventory is never rewritten into descriptors.
//!
//! [`validate`] accepts a v1 response: the exact [`CONTRACT`] marker, the
//! required identity fields, a bounded verb inventory that includes
//! `app.describe` (a descriptor for it must claim `read_only: true`), and
//! optional `settings`/`settings_cache` evidence that is an object or null.
//! Unknown root fields and descriptor extension fields survive. [`validate`]
//! re-encodes the value compactly and bounds that to [`MAX_RESPONSE_BYTES`] —
//! a bound on the in-memory object, never the raw wire body, which a
//! whitespace-heavy body can beat. [`parse_validate`] is the raw-body entry
//! point: it bounds the raw body to [`MAX_RESPONSE_BYTES`] before parsing.
//! Safety is never inferred from a name, the spelling "get", registry
//! membership or the verb's own read-only status: absent or null `read_only`
//! means unknown.
//!
//! [`read_legacy`] reads the current product objects of unmigrated
//! applications: a verbs array plus whatever partial identity they carry.
//! Missing fields stay missing, never inferred or fabricated, and an object
//! carrying a `describe_contract` key is refused — a migrated or foreign
//! marker-bearing object must go through [`validate`], never be silently read
//! as legacy. A bare HELP array is not a describe object, and shell.info's
//! bare verb suffixes are not converted into v1 names; BusViewer keeps its
//! own permissive HELP parser for those sources.

use serde_json::{Map, Value};
use std::collections::HashSet;
use std::fmt;

#[cfg(feature = "settings-native")]
mod native;
#[cfg(feature = "settings-native")]
pub use native::complete_native;

pub const CONTRACT: &str = "application.describe.v1";
pub const VERB: &str = "app.describe";

/// Encoded request bound, applied before parsing the command body.
pub const MAX_REQUEST_BYTES: usize = 4 * 1024;
/// Encoded response bound. [`parse_validate`] applies it to the raw body
/// before parsing; [`validate`] applies it to the compact re-encoding of an
/// already-parsed value. A v1 response that would exceed it is refused,
/// never silently truncated.
pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;
pub const MAX_VERBS: usize = 512;
pub const MAX_VERB_NAME_BYTES: usize = 128;
pub const MAX_IDENTITY_BYTES: usize = 256;
pub const MAX_DESCRIPTOR_BYTES: usize = 16 * 1024;

/// Stable [`Violation`] codes. The caller maps these to its existing rc and
/// refusal body; the contract never renames an application's error codes.
pub mod code {
    pub const MALFORMED_JSON: &str = "malformed_json";
    pub const OVERSIZE_REQUEST: &str = "oversize_request";
    pub const OVERSIZE_RESPONSE: &str = "oversize_response";
    pub const INVALID_REQUEST: &str = "invalid_request";
    pub const INVALID_ROOT: &str = "invalid_root";
    pub const INVALID_VERBS: &str = "invalid_verbs";
    pub const INVALID_VERB: &str = "invalid_verb";
    pub const DUPLICATE_VERB: &str = "duplicate_verb";
    pub const TOO_MANY_VERBS: &str = "too_many_verbs";
    pub const MISSING_MARKER: &str = "missing_marker";
    pub const UNKNOWN_MARKER: &str = "unknown_marker";
    pub const NOT_LEGACY: &str = "not_legacy";
    pub const MISSING_FIELD: &str = "missing_field";
    pub const INVALID_IDENTITY: &str = "invalid_identity";
    pub const APP_DESCRIBE_MISSING: &str = "app_describe_missing";
    pub const APP_DESCRIBE_MUTABLE: &str = "app_describe_mutable";
    pub const INVALID_EVIDENCE: &str = "invalid_evidence";
    pub const RESERVED_CONFLICT: &str = "reserved_conflict";
}

/// One bounded contract refusal: a field path, a stable code and a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    pub path: String,
    pub code: &'static str,
    pub message: String,
}
impl Violation {
    fn new(path: impl AsRef<str>, code: &'static str, message: impl Into<String>) -> Self {
        let mut path = path.as_ref().to_owned();
        const PATH_BOUND: usize = 256;
        if path.len() > PATH_BOUND {
            let mut end = PATH_BOUND;
            while !path.is_char_boundary(end) {
                end -= 1;
            }
            path.truncate(end);
        }
        Self {
            path,
            code,
            message: message.into(),
        }
    }
}
impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(f, "{}: {}", self.code, self.message)
        } else {
            write!(f, "{} at {}: {}", self.code, self.path, self.message)
        }
    }
}
impl std::error::Error for Violation {}

/// Check a command body before dispatch. Empty and whitespace-only bodies are
/// the canonical no-argument request; every other nonempty object, non-object
/// JSON value and malformed body is refused.
pub fn validate_request(body: &str) -> Result<(), Violation> {
    if body.len() > MAX_REQUEST_BYTES {
        return Err(Violation::new(
            "",
            code::OVERSIZE_REQUEST,
            format!("request exceeds {MAX_REQUEST_BYTES} bytes"),
        ));
    }
    if body.trim().is_empty() {
        return Ok(());
    }
    let value: Value = serde_json::from_str(body)
        .map_err(|error| Violation::new("", code::MALFORMED_JSON, error.to_string()))?;
    let object = value.as_object().ok_or_else(|| {
        Violation::new(
            "",
            code::INVALID_REQUEST,
            "app.describe arguments must be a JSON object",
        )
    })?;
    if !object.is_empty() {
        return Err(Violation::new(
            "",
            code::INVALID_REQUEST,
            "app.describe takes no arguments",
        ));
    }
    Ok(())
}

/// The responding owner's actual identity, sampled by the frontend. `app_id`
/// is the Wayland application id for GUI clients; headless owners pass `None`
/// (written as explicit null). `pid` must be the process that serves the
/// reply, never an inferred or neighbouring process.
#[derive(Clone, Copy)]
pub struct Identity<'a> {
    pub app_id: Option<&'a str>,
    pub version: &'a str,
    pub pid: u32,
    pub service: &'a str,
}

/// Add the common v1 envelope to a product object. Reserved fields that
/// already exist must agree with the identity; the update is applied only
/// after the completed value validates as a whole, so a refusal never leaves
/// a partially completed response.
pub fn complete(value: &mut Value, identity: Identity<'_>) -> Result<(), Violation> {
    let object = value.as_object().ok_or_else(|| {
        Violation::new(
            "",
            code::INVALID_ROOT,
            "describe value must be a JSON object",
        )
    })?;
    reserved_agree(object, &identity)?;
    let mut completed = value.clone();
    let map = completed.as_object_mut().expect("cloned object");
    if !map.contains_key("describe_contract") {
        map.insert("describe_contract".into(), Value::String(CONTRACT.into()));
    }
    if !map.contains_key("version") {
        map.insert("version".into(), Value::String(identity.version.into()));
    }
    if !map.contains_key("pid") {
        map.insert("pid".into(), Value::from(identity.pid));
    }
    if !map.contains_key("service") {
        map.insert("service".into(), Value::String(identity.service.into()));
    }
    if !map.contains_key("app_id") {
        map.insert(
            "app_id".into(),
            identity
                .app_id
                .map_or(Value::Null, |app_id| Value::String(app_id.into())),
        );
    }
    validate(&completed)?;
    *value = completed;
    Ok(())
}

fn reserved_agree(object: &Map<String, Value>, identity: &Identity<'_>) -> Result<(), Violation> {
    if let Some(existing) = object.get("describe_contract")
        && existing.as_str() != Some(CONTRACT)
    {
        return Err(Violation::new(
            "describe_contract",
            code::RESERVED_CONFLICT,
            format!("existing marker contradicts {CONTRACT}"),
        ));
    }
    for key in ["version", "service"] {
        let supplied = if key == "version" {
            identity.version
        } else {
            identity.service
        };
        if let Some(existing) = object.get(key)
            && existing.as_str() != Some(supplied)
        {
            return Err(Violation::new(
                key,
                code::RESERVED_CONFLICT,
                "reserved field contradicts the responding identity",
            ));
        }
    }
    if let Some(existing) = object.get("pid")
        && existing.as_u64() != Some(u64::from(identity.pid))
    {
        return Err(Violation::new(
            "pid",
            code::RESERVED_CONFLICT,
            "reserved pid contradicts the responding identity",
        ));
    }
    if let Some(existing) = object.get("app_id") {
        let agrees = match identity.app_id {
            Some(app_id) => existing.as_str() == Some(app_id),
            None => existing.is_null(),
        };
        if !agrees {
            return Err(Violation::new(
                "app_id",
                code::RESERVED_CONFLICT,
                "reserved app_id contradicts the responding identity",
            ));
        }
    }
    Ok(())
}

/// Validate an already-parsed v1 response. The v1 marker and the required
/// common fields must be present; an absent or unknown marker cannot pass as
/// v1. Unknown root fields are allowed and preserved.
///
/// The size check here bounds the compact re-encoding of the value, not the
/// raw wire body it may have been parsed from: a pretty-printed or
/// whitespace-heavy body can exceed [`MAX_RESPONSE_BYTES`] while its compact
/// re-encoding does not. Callers parsing raw bodies use [`parse_validate`],
/// which applies the bound before parsing.
pub fn validate(value: &Value) -> Result<Description<'_>, Violation> {
    let object = value.as_object().ok_or_else(|| {
        Violation::new("", code::INVALID_ROOT, "v1 describe must be a JSON object")
    })?;
    let encoded = serde_json::to_string(value)
        .map_err(|error| Violation::new("", code::INVALID_ROOT, error.to_string()))?;
    if encoded.len() > MAX_RESPONSE_BYTES {
        return Err(Violation::new(
            "",
            code::OVERSIZE_RESPONSE,
            format!("encoded response exceeds {MAX_RESPONSE_BYTES} bytes"),
        ));
    }
    match object.get("describe_contract") {
        None => {
            return Err(Violation::new(
                "describe_contract",
                code::MISSING_MARKER,
                format!("absent marker cannot pass as {CONTRACT}"),
            ));
        }
        Some(marker) if marker.as_str() != Some(CONTRACT) => {
            return Err(Violation::new(
                "describe_contract",
                code::UNKNOWN_MARKER,
                format!("expected the exact {CONTRACT} marker"),
            ));
        }
        Some(_) => {}
    }
    required_nonempty_bounded(object, "version")?;
    let pid = object
        .get("pid")
        .and_then(Value::as_u64)
        .filter(|pid| (1..=u64::from(u32::MAX)).contains(pid))
        .ok_or_else(|| {
            Violation::new("pid", code::INVALID_IDENTITY, "pid must be a positive u32")
        })?;
    required_nonempty_bounded(object, "service")?;
    match object.get("app_id") {
        None => {
            return Err(Violation::new(
                "app_id",
                code::MISSING_FIELD,
                "app_id is required; use null without a Wayland client identity",
            ));
        }
        Some(Value::Null) => {}
        Some(Value::String(app_id)) => {
            if app_id.is_empty() {
                return Err(Violation::new(
                    "app_id",
                    code::INVALID_IDENTITY,
                    "app_id must be nonempty",
                ));
            }
            if app_id.len() > MAX_IDENTITY_BYTES {
                return Err(Violation::new(
                    "app_id",
                    code::INVALID_IDENTITY,
                    format!("app_id exceeds {MAX_IDENTITY_BYTES} bytes"),
                ));
            }
        }
        Some(_) => {
            return Err(Violation::new(
                "app_id",
                code::INVALID_IDENTITY,
                "app_id must be a nonempty string or null",
            ));
        }
    }
    let verbs = object.get("verbs").ok_or_else(|| {
        Violation::new("verbs", code::MISSING_FIELD, "verb inventory is required")
    })?;
    let entries = verbs
        .as_array()
        .ok_or_else(|| Violation::new("verbs", code::INVALID_VERBS, "verbs must be an array"))?;
    if entries.len() > MAX_VERBS {
        return Err(Violation::new(
            "verbs",
            code::TOO_MANY_VERBS,
            format!("more than {MAX_VERBS} verbs"),
        ));
    }
    let mut seen = HashSet::with_capacity(entries.len());
    let mut app_describe = false;
    for (index, entry) in entries.iter().enumerate() {
        let path = format!("verbs[{index}]");
        let name = match entry {
            Value::String(name) => name.as_str(),
            Value::Object(_) => entry.get("name").and_then(Value::as_str).ok_or_else(|| {
                Violation::new(
                    format!("{path}.name"),
                    code::INVALID_VERB,
                    "descriptor needs a string name",
                )
            })?,
            _ => {
                return Err(Violation::new(
                    path,
                    code::INVALID_VERB,
                    "verb must be a name string or a descriptor object",
                ));
            }
        };
        if name.is_empty() {
            return Err(Violation::new(
                path,
                code::INVALID_VERB,
                "verb name must be nonempty",
            ));
        }
        if name.len() > MAX_VERB_NAME_BYTES {
            return Err(Violation::new(
                path,
                code::INVALID_VERB,
                format!("verb name exceeds {MAX_VERB_NAME_BYTES} bytes"),
            ));
        }
        if let Value::Object(descriptor) = entry {
            if let Some(read_only) = descriptor.get("read_only")
                && !read_only.is_boolean()
                && !read_only.is_null()
            {
                return Err(Violation::new(
                    format!("{path}.read_only"),
                    code::INVALID_VERB,
                    "read_only must be boolean or null",
                ));
            }
            if let Some(description) = descriptor.get("description")
                && !description.is_string()
            {
                return Err(Violation::new(
                    format!("{path}.description"),
                    code::INVALID_VERB,
                    "description must be a string",
                ));
            }
            let bytes = serde_json::to_vec(entry).map_err(|error| {
                Violation::new(path.clone(), code::INVALID_VERB, error.to_string())
            })?;
            if bytes.len() > MAX_DESCRIPTOR_BYTES {
                return Err(Violation::new(
                    path,
                    code::INVALID_VERB,
                    format!("descriptor exceeds {MAX_DESCRIPTOR_BYTES} bytes"),
                ));
            }
            if name == VERB && descriptor.get("read_only") != Some(&Value::Bool(true)) {
                return Err(Violation::new(
                    path,
                    code::APP_DESCRIBE_MUTABLE,
                    "app.describe descriptor must mark read_only true",
                ));
            }
        }
        if !seen.insert(name) {
            return Err(Violation::new(
                path,
                code::DUPLICATE_VERB,
                format!("duplicate verb {name}"),
            ));
        }
        app_describe |= name == VERB;
    }
    if !app_describe {
        return Err(Violation::new(
            "verbs",
            code::APP_DESCRIBE_MISSING,
            "verb inventory must include app.describe",
        ));
    }
    for key in ["settings", "settings_cache"] {
        if let Some(evidence) = object.get(key)
            && !evidence.is_object()
            && !evidence.is_null()
        {
            return Err(Violation::new(
                key,
                code::INVALID_EVIDENCE,
                "evidence must be an object or null",
            ));
        }
    }
    Ok(Description {
        value,
        pid: pid as u32,
    })
}

/// Parse and validate a raw v1 response body in one call. The raw byte
/// length is bounded to [`MAX_RESPONSE_BYTES`] before parsing, so an
/// oversized body is refused ([`code::OVERSIZE_RESPONSE`]) without being
/// parsed, and malformed JSON is refused with [`code::MALFORMED_JSON`].
/// This is the entry point for callers that read responses off the wire:
/// [`validate`] alone cannot see the raw body its value came from. The
/// returned description owns its value.
pub fn parse_validate(raw: &str) -> Result<OwnedDescription, Violation> {
    if raw.len() > MAX_RESPONSE_BYTES {
        return Err(Violation::new(
            "",
            code::OVERSIZE_RESPONSE,
            format!("raw response exceeds {MAX_RESPONSE_BYTES} bytes"),
        ));
    }
    let value: Value = serde_json::from_str(raw)
        .map_err(|error| Violation::new("", code::MALFORMED_JSON, error.to_string()))?;
    let pid = validate(&value)?.pid();
    Ok(OwnedDescription {
        value,
        pid,
    })
}

fn required_nonempty_bounded<'a>(
    object: &'a Map<String, Value>,
    key: &'static str,
) -> Result<&'a str, Violation> {
    let value = object
        .get(key)
        .ok_or_else(|| Violation::new(key, code::MISSING_FIELD, format!("{key} is required")))?;
    let text = value.as_str().ok_or_else(|| {
        Violation::new(
            key,
            code::INVALID_IDENTITY,
            format!("{key} must be a string"),
        )
    })?;
    if text.is_empty() {
        return Err(Violation::new(
            key,
            code::INVALID_IDENTITY,
            format!("{key} must be nonempty"),
        ));
    }
    if text.len() > MAX_IDENTITY_BYTES {
        return Err(Violation::new(
            key,
            code::INVALID_IDENTITY,
            format!("{key} exceeds {MAX_IDENTITY_BYTES} bytes"),
        ));
    }
    Ok(text)
}

/// A validated v1 response. Borrows the object; it never clones a second
/// source of truth.
#[derive(Clone, Copy, Debug)]
pub struct Description<'a> {
    value: &'a Value,
    pid: u32,
}
impl<'a> Description<'a> {
    /// The whole response object, unknown root fields included.
    pub fn value(&self) -> &'a Value {
        self.value
    }
    pub fn version(&self) -> &'a str {
        self.value
            .get("version")
            .and_then(Value::as_str)
            .expect("validated version")
    }
    pub fn pid(&self) -> u32 {
        self.pid
    }
    pub fn service(&self) -> &'a str {
        self.value
            .get("service")
            .and_then(Value::as_str)
            .expect("validated service")
    }
    /// `None` when `app_id` is explicit null; the field itself is required.
    pub fn app_id(&self) -> Option<&'a str> {
        self.value.get("app_id").and_then(Value::as_str)
    }
    /// The evidence object, or `None` when the field is absent or null.
    pub fn settings(&self) -> Option<&'a Value> {
        self.value.get("settings").filter(|value| value.is_object())
    }
    pub fn settings_cache(&self) -> Option<&'a Value> {
        self.value
            .get("settings_cache")
            .filter(|value| value.is_object())
    }
    pub fn verbs(&self) -> VerbIter<'a> {
        VerbIter {
            entries: self
                .value
                .get("verbs")
                .and_then(Value::as_array)
                .expect("validated verbs")
                .iter(),
        }
    }
}

/// A v1 response parsed and validated from a raw body by
/// [`parse_validate`]. Owns its value; [`OwnedDescription::view`] gives the
/// borrowed [`Description`] form of the same object.
#[derive(Clone, Debug)]
pub struct OwnedDescription {
    value: Value,
    pid: u32,
}
impl OwnedDescription {
    /// The whole response object, unknown root fields included.
    pub fn value(&self) -> &Value {
        &self.value
    }
    /// The borrowed [`Description`] view of the same object.
    pub fn view(&self) -> Description<'_> {
        Description {
            value: &self.value,
            pid: self.pid,
        }
    }
    pub fn version(&self) -> &str {
        self.view().version()
    }
    pub fn pid(&self) -> u32 {
        self.pid
    }
    pub fn service(&self) -> &str {
        self.view().service()
    }
    /// `None` when `app_id` is explicit null; the field itself is required.
    pub fn app_id(&self) -> Option<&str> {
        self.view().app_id()
    }
    /// The evidence object, or `None` when the field is absent or null.
    pub fn settings(&self) -> Option<&Value> {
        self.view().settings()
    }
    pub fn settings_cache(&self) -> Option<&Value> {
        self.view().settings_cache()
    }
    pub fn verbs(&self) -> VerbIter<'_> {
        self.view().verbs()
    }
}

/// One inventory entry. `read_only` is `None` for unknown safety: absent,
/// null, or a plain string entry. It is never inferred from a name.
#[derive(Clone, Copy)]
pub struct Verb<'a> {
    pub name: &'a str,
    pub read_only: Option<bool>,
    pub description: Option<&'a str>,
    /// Arbitrary bounded arguments, when the descriptor carries them.
    pub args: Option<&'a Value>,
    /// The whole entry: the name string, or the descriptor object with its
    /// extension fields.
    pub value: &'a Value,
}

#[derive(Clone)]
pub struct VerbIter<'a> {
    entries: std::slice::Iter<'a, Value>,
}
impl<'a> Iterator for VerbIter<'a> {
    type Item = Verb<'a>;
    fn next(&mut self) -> Option<Verb<'a>> {
        self.entries.next().map(|entry| match entry {
            Value::String(name) => Verb {
                name,
                read_only: None,
                description: None,
                args: None,
                value: entry,
            },
            Value::Object(_) => Verb {
                name: entry
                    .get("name")
                    .and_then(Value::as_str)
                    .expect("validated verb name"),
                read_only: entry.get("read_only").and_then(Value::as_bool),
                description: entry.get("description").and_then(Value::as_str),
                args: entry.get("args"),
                value: entry,
            },
            _ => unreachable!("validated verbs array"),
        })
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.entries.size_hint()
    }
}
impl ExactSizeIterator for VerbIter<'_> {}

/// A permissive read of a product object served before the common envelope:
/// a verbs array plus whatever partial identity it carries. Missing fields
/// stay missing; nothing is inferred or fabricated. Legacy discovery keeps
/// its duplicate-name compatibility and is not held to the v1 limits.
#[derive(Clone, Copy, Debug)]
pub struct LegacyDescription<'a> {
    value: &'a Value,
}
impl<'a> LegacyDescription<'a> {
    pub fn value(&self) -> &'a Value {
        self.value
    }
    pub fn version(&self) -> Option<&'a str> {
        self.value.get("version").and_then(Value::as_str)
    }
    pub fn pid(&self) -> Option<u32> {
        self.value
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
    }
    pub fn service(&self) -> Option<&'a str> {
        self.value.get("service").and_then(Value::as_str)
    }
    pub fn app_id(&self) -> Option<&'a str> {
        self.value.get("app_id").and_then(Value::as_str)
    }
    pub fn verbs(&self) -> VerbIter<'a> {
        VerbIter {
            entries: self
                .value
                .get("verbs")
                .and_then(Value::as_array)
                .map_or(&[][..], |entries| entries.as_slice())
                .iter(),
        }
    }
}

/// Read a legacy product object. The root must be an object: a bare HELP
/// array is not a describe object, and shell.info's bare suffixes are read as
/// they are, never converted into v1 names. An object carrying a
/// `describe_contract` key of any value is refused: a migrated or foreign
/// marker-bearing object is not legacy and must go through [`validate`].
pub fn read_legacy(value: &Value) -> Result<LegacyDescription<'_>, Violation> {
    let object = value.as_object().ok_or_else(|| {
        Violation::new(
            "",
            code::INVALID_ROOT,
            "a legacy describe must be a JSON object, not a bare HELP array",
        )
    })?;
    if object.contains_key("describe_contract") {
        return Err(Violation::new(
            "describe_contract",
            code::NOT_LEGACY,
            "an object carrying describe_contract is not a legacy describe; use validate",
        ));
    }
    if let Some(verbs) = object.get("verbs") {
        let entries = verbs.as_array().ok_or_else(|| {
            Violation::new("verbs", code::INVALID_VERBS, "verbs must be an array")
        })?;
        for (index, entry) in entries.iter().enumerate() {
            let path = format!("verbs[{index}]");
            match entry {
                Value::String(name) if !name.is_empty() => {}
                Value::Object(_) => {
                    let name = entry.get("name").and_then(Value::as_str).ok_or_else(|| {
                        Violation::new(
                            format!("{path}.name"),
                            code::INVALID_VERB,
                            "descriptor needs a string name",
                        )
                    })?;
                    if name.is_empty() {
                        return Err(Violation::new(
                            path,
                            code::INVALID_VERB,
                            "verb name must be nonempty",
                        ));
                    }
                }
                _ => {
                    return Err(Violation::new(
                        path,
                        code::INVALID_VERB,
                        "verb must be a name string or a descriptor object",
                    ));
                }
            }
        }
    }
    Ok(LegacyDescription { value })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    macro_rules! fixtures {
        ($($name:literal),* $(,)?) => {
            fn fixture(name: &str) -> Value {
                let text = match name {
                    $($name => include_str!(concat!("../../../docs/spec/application/fixtures/",$name)),)*
                    _ => panic!("unknown fixture: {name}"),
                };
                serde_json::from_str(text).expect("valid fixture JSON")
            }
        };
    }
    fixtures!(
        "v1-refusal-unknown-marker.json", "v1-refusal-bad-identity.json",
        "ced-legacy-describe.json", "v1-refusal-missing-marker.json",
        "v1-refusal-missing-app-describe.json", "v1-minimal-strings.json",
        "ced-legacy-describe-gui.json", "v1-descriptors.json", "dopus-legacy-describe.json",
        "v1-refusal-app-describe-mutable.json", "scene-editor-legacy-describe.json",
        "busviewer-legacy-describe.json", "v1-refusal-pid-overflow.json",
        "v1-refusal-bad-evidence.json", "v1-evidence.json",
        "v1-refusal-duplicate-verbs.json", "shell-info-legacy.json",
    );

    #[test]
    fn request_accepts_only_the_canonical_no_argument_forms() {
        // Whitespace-padded empty objects parse as the same request; the
        // gate fixtures pin the identical strings.
        for body in ["", "   ", "\n\t", "{}", " {} ", "\n{}\n", "{ }"] {
            assert!(validate_request(body).is_ok(), "{body:?}");
        }
        for (body, expected) in [
            ("[]", code::INVALID_REQUEST),
            ("null", code::INVALID_REQUEST),
            ("\"describe\"", code::INVALID_REQUEST),
            ("42", code::INVALID_REQUEST),
            ("{\"extra\":true}", code::INVALID_REQUEST),
            ("{not json", code::MALFORMED_JSON),
        ] {
            assert_eq!(validate_request(body).unwrap_err().code, expected, "{body}");
        }
        assert!(validate_request(&" ".repeat(MAX_REQUEST_BYTES)).is_ok());
        assert_eq!(
            validate_request(&" ".repeat(MAX_REQUEST_BYTES + 1))
                .unwrap_err()
                .code,
            code::OVERSIZE_REQUEST
        );
    }

    #[test]
    fn complete_adds_the_envelope_and_preserves_the_product_object() {
        let mut value = json!({
            "schema": "scene-editor.v1",
            "version": "0.1.1",
            "transport": "native",
            "verbs": ["scene-editor.ping", "scene-editor.info", "scene-editor.show",
                "scene-editor.action", "scene-editor.quit", "app.describe"],
            "views": ["gallery", "installed", "arrange"]
        });
        complete(
            &mut value,
            Identity {
                app_id: Some("dev.mixos.scene-editor"),
                version: "0.1.1",
                pid: 4242,
                service: "scene-editor",
            },
        )
        .unwrap();
        let description = validate(&value).unwrap();
        assert_eq!(description.version(), "0.1.1");
        assert_eq!(description.pid(), 4242);
        assert_eq!(description.service(), "scene-editor");
        assert_eq!(description.app_id(), Some("dev.mixos.scene-editor"));
        assert_eq!(value["schema"], "scene-editor.v1");
        assert_eq!(value["transport"], "native");
        assert_eq!(value["views"], json!(["gallery", "installed", "arrange"]));
        // String inventories stay strings; complete never rewrites them.
        assert_eq!(value["verbs"][0], "scene-editor.ping");
        let names: Vec<&str> = description.verbs().map(|verb| verb.name).collect();
        assert_eq!(names.iter().filter(|name| **name == VERB).count(), 1);
    }

    #[test]
    fn complete_is_idempotent_when_reserved_fields_already_agree() {
        let mut strings = fixture("v1-minimal-strings.json");
        let snapshot = strings.clone();
        complete(
            &mut strings,
            Identity {
                app_id: None,
                version: "0.4.4",
                pid: 12345,
                service: "dopus",
            },
        )
        .unwrap();
        assert_eq!(strings, snapshot, "nothing was missing");

        let mut mixed = fixture("v1-evidence.json");
        complete(
            &mut mixed,
            Identity {
                app_id: Some("dev.mixos.ced"),
                version: "0.1.8",
                pid: 8,
                service: "ced",
            },
        )
        .unwrap();
        assert!(mixed["verbs"][0].is_string());
        assert!(mixed["verbs"][1].is_object(), "mixed arrays stay mixed");
    }

    #[test]
    fn complete_refuses_contradictions_without_mutation() {
        let mut value = fixture("v1-minimal-strings.json");
        let snapshot = value.clone();
        for identity in [
            Identity {
                app_id: Some("dev.mixos.other"),
                version: "0.4.4",
                pid: 12345,
                service: "dopus",
            },
            Identity {
                app_id: None,
                version: "0.4.4",
                pid: 1,
                service: "dopus",
            },
            Identity {
                app_id: None,
                version: "0.4.4",
                pid: 12345,
                service: "other",
            },
            Identity {
                app_id: None,
                version: "other",
                pid: 12345,
                service: "dopus",
            },
        ] {
            let violation = complete(&mut value, identity).unwrap_err();
            assert_eq!(violation.code, code::RESERVED_CONFLICT);
            assert_eq!(value, snapshot, "contradiction must not mutate");
        }
        // An existing foreign marker is a contradiction too.
        let mut marked = json!({"describe_contract": "ctk-app-control.v0",
            "version": "1.0.0", "verbs": ["app.describe"]});
        let violation = complete(
            &mut marked,
            Identity {
                app_id: None,
                version: "1.0.0",
                pid: 7,
                service: "x",
            },
        )
        .unwrap_err();
        assert_eq!(violation.code, code::RESERVED_CONFLICT);
        assert_eq!(marked["describe_contract"], "ctk-app-control.v0");
    }

    #[test]
    fn complete_refuses_when_the_product_cannot_validate_as_v1() {
        let identity = Identity {
            app_id: None,
            version: "1.0.0",
            pid: 7,
            service: "x",
        };
        let mut missing = json!({"version": "1.0.0", "verbs": ["x.ping"]});
        assert_eq!(
            complete(&mut missing, identity).unwrap_err().code,
            code::APP_DESCRIBE_MISSING
        );
        let mut mutable = json!({"version": "1.0.0",
            "verbs": ["x.ping", {"name": "app.describe", "read_only": false}]});
        assert_eq!(
            complete(&mut mutable, identity).unwrap_err().code,
            code::APP_DESCRIBE_MUTABLE
        );
        let mut zero_pid = json!({"version": "1.0.0", "verbs": ["app.describe"]});
        assert_eq!(
            complete(&mut zero_pid, Identity { pid: 0, ..identity })
                .unwrap_err()
                .code,
            code::INVALID_IDENTITY
        );
        let mut blank_version = json!({"version": "1.0.0", "verbs": ["app.describe"]});
        assert_eq!(
            complete(
                &mut blank_version,
                Identity {
                    version: "",
                    ..identity
                }
            )
            .unwrap_err()
            .code,
            code::INVALID_IDENTITY
        );
        let mut duplicate = json!({"version": "1.0.0", "verbs": ["app.describe", "app.describe"]});
        assert_eq!(
            complete(&mut duplicate, identity).unwrap_err().code,
            code::DUPLICATE_VERB
        );
    }

    #[test]
    fn validate_accepts_the_validator_fixtures() {
        let minimal_fixture = fixture("v1-minimal-strings.json");
        let minimal = validate(&minimal_fixture).unwrap();
        assert_eq!(minimal.app_id(), None, "explicit null app_id");
        assert_eq!(minimal.settings(), None, "null evidence stays absent");
        assert_eq!(minimal.value()["schema"], "dopus.v1");

        let descriptors_fixture = fixture("v1-descriptors.json");
        let descriptors = validate(&descriptors_fixture).unwrap();
        let describe = descriptors
            .verbs()
            .find(|verb| verb.name == VERB)
            .expect("app.describe listed");
        assert_eq!(describe.read_only, Some(true));
        let select = descriptors
            .verbs()
            .find(|verb| verb.name == "busviewer.select")
            .unwrap();
        assert_eq!(select.read_only, Some(false));
        assert_eq!(
            select.args,
            Some(&json!({"service": "string", "verb": "string"}))
        );
        assert_eq!(select.value["extension"], json!({"kept": true}));

        // Evidence with current != applied stays a valid transient state:
        // the validator checks object-ness only, never convergence.
        validate(&fixture("v1-evidence.json")).unwrap();
    }

    #[test]
    fn validate_refuses_the_refusal_fixtures() {
        for (name, expected) in [
            ("v1-refusal-duplicate-verbs.json", code::DUPLICATE_VERB),
            ("v1-refusal-missing-marker.json", code::MISSING_MARKER),
            ("v1-refusal-unknown-marker.json", code::UNKNOWN_MARKER),
            (
                "v1-refusal-app-describe-mutable.json",
                code::APP_DESCRIBE_MUTABLE,
            ),
            (
                "v1-refusal-missing-app-describe.json",
                code::APP_DESCRIBE_MISSING,
            ),
            ("v1-refusal-bad-identity.json", code::INVALID_IDENTITY),
            ("v1-refusal-pid-overflow.json", code::INVALID_IDENTITY),
            ("v1-refusal-bad-evidence.json", code::INVALID_EVIDENCE),
        ] {
            let violation = validate(&fixture(name)).unwrap_err();
            assert_eq!(violation.code, expected, "{name}");
        }
    }

    #[test]
    fn validate_enforces_the_bounds_and_shapes() {
        let base = |verbs: Vec<Value>| {
            json!({"describe_contract": CONTRACT, "version": "v", "pid": 1,
                "service": "s", "app_id": null, "verbs": verbs})
        };
        let too_many: Vec<Value> = (0..=MAX_VERBS)
            .map(|index| Value::String(format!("v{index}")))
            .collect();
        assert_eq!(
            validate(&base(too_many)).unwrap_err().code,
            code::TOO_MANY_VERBS
        );
        assert_eq!(
            validate(&base(vec![
                Value::String("x".repeat(MAX_VERB_NAME_BYTES + 1)),
                Value::String(VERB.into()),
            ]))
            .unwrap_err()
            .code,
            code::INVALID_VERB
        );
        assert_eq!(
            validate(&base(vec![
                Value::String(String::new()),
                Value::String(VERB.into())
            ]))
            .unwrap_err()
            .code,
            code::INVALID_VERB
        );
        assert_eq!(
            validate(&base(vec![
                json!({"name": "big.args", "args": {"blob": "x".repeat(MAX_DESCRIPTOR_BYTES)}}),
                Value::String(VERB.into()),
            ]))
            .unwrap_err()
            .code,
            code::INVALID_VERB
        );
        // Encoded bound is checked before anything else.
        assert_eq!(
            validate(&json!({"describe_contract": CONTRACT,
                "blob": "x".repeat(MAX_RESPONSE_BYTES)}))
            .unwrap_err()
            .code,
            code::OVERSIZE_RESPONSE
        );
        // Required fields, one at a time.
        for missing in [
            json!({"describe_contract": CONTRACT, "version": "v", "service": "s",
                "app_id": null, "verbs": [VERB]}),
            json!({"describe_contract": CONTRACT, "version": "v", "pid": 1,
                "app_id": null, "verbs": [VERB]}),
            json!({"describe_contract": CONTRACT, "version": "v", "pid": 1,
                "service": "s", "verbs": [VERB]}),
            json!({"describe_contract": CONTRACT, "version": "v", "pid": 1,
                "service": "s", "app_id": null}),
        ] {
            assert_eq!(
                validate(&missing).unwrap_err().code,
                code::MISSING_FIELD,
                "{missing}"
            );
        }
        // Identity representations that are not a positive u32.
        for pid in [json!(0), json!(1.5), json!("1"), json!(u32::MAX as u64 + 1)] {
            assert_eq!(
                validate(&json!({"describe_contract": CONTRACT, "version": "v",
                    "pid": pid, "service": "s", "app_id": null, "verbs": [VERB]}))
                .unwrap_err()
                .code,
                code::INVALID_IDENTITY
            );
        }
        // The full positive u32 range is accepted.
        assert!(
            validate(&json!({"describe_contract": CONTRACT, "version": "v",
            "pid": u32::MAX, "service": "s", "app_id": null, "verbs": [VERB]}))
            .is_ok()
        );
    }

    #[test]
    fn parse_validate_bounds_the_raw_body_before_parsing() {
        let compact = json!({"describe_contract": CONTRACT, "version": "v", "pid": 1,
            "service": "s", "app_id": null, "verbs": [VERB]});
        validate(&compact).unwrap();
        // Pretty-printed and padded past the bound, the raw body parses to
        // the same compact value: only parse_validate can see the raw bytes.
        let raw = serde_json::to_string_pretty(&compact).unwrap();
        assert!(raw.len() < MAX_RESPONSE_BYTES);
        let padded = format!("{raw}{}", " ".repeat(MAX_RESPONSE_BYTES));
        assert_eq!(
            parse_validate(&padded).unwrap_err().code,
            code::OVERSIZE_RESPONSE
        );
        assert_eq!(
            parse_validate("{not json").unwrap_err().code,
            code::MALFORMED_JSON
        );
        let parsed = parse_validate(&raw).unwrap();
        assert_eq!(parsed.pid(), 1);
        assert_eq!(parsed.service(), "s");
        assert_eq!(parsed.version(), "v");
        assert_eq!(parsed.verbs().count(), 1);
        assert_eq!(parsed.view().pid(), 1);
        assert_eq!(parsed.value()["verbs"][0], VERB);
    }

    #[test]
    fn unknown_safety_stays_unknown() {
        let value = json!({"describe_contract": CONTRACT, "version": "v", "pid": 1,
            "service": "s", "app_id": null,
            "verbs": [{"name": "a.ping"}, {"name": "a.state", "read_only": null}, "app.describe"]});
        let verbs: Vec<Verb<'_>> = validate(&value).unwrap().verbs().collect();
        assert_eq!(verbs[0].read_only, None, "absent read_only is unknown");
        assert_eq!(verbs[1].read_only, None, "null read_only is unknown");
        assert_eq!(verbs[2].read_only, None, "string entries claim nothing");
    }

    #[test]
    fn read_legacy_reads_product_objects_and_keeps_missing_fields_missing() {
        let ced_fixture = fixture("ced-legacy-describe.json");
        let ced = read_legacy(&ced_fixture).unwrap();
        assert_eq!(
            ced.value()["contract"],
            "ctk-app-control.v0",
            "the ctk contract field is not the v1 marker and survives"
        );
        assert_eq!(ced.version(), Some("0.1.8"));
        assert_eq!(ced.pid(), None);
        assert_eq!(ced.service(), None);
        assert_eq!(ced.app_id(), None);
        assert!(ced.verbs().any(|verb| verb.name == VERB));

        let gui_fixture = fixture("ced-legacy-describe-gui.json");
        let gui = read_legacy(&gui_fixture).unwrap();
        assert_eq!(gui.pid(), None);
        assert!(gui.value()["settings"].is_object());
        assert!(gui.value()["settings_cache"].is_object());

        let editor_fixture = fixture("scene-editor-legacy-describe.json");
        let editor = read_legacy(&editor_fixture).unwrap();
        assert_eq!(editor.app_id(), Some("dev.mixos.scene-editor"));
        assert_eq!(editor.version(), Some("0.1.1"));
        assert_eq!(editor.pid(), None);

        let viewer_fixture = fixture("busviewer-legacy-describe.json");
        let viewer = read_legacy(&viewer_fixture).unwrap();
        let select = viewer
            .verbs()
            .find(|verb| verb.name == "busviewer.select")
            .unwrap();
        assert_eq!(select.read_only, Some(false));
        assert_eq!(
            select.args,
            Some(&json!({"service": "string", "verb": "string"}))
        );
        assert!(
            !viewer.verbs().any(|verb| verb.name == VERB),
            "not yet advertised"
        );

        let dopus_fixture = fixture("dopus-legacy-describe.json");
        let dopus = read_legacy(&dopus_fixture).unwrap();
        assert_eq!(dopus.version(), Some("0.4.4"));
        assert_eq!(dopus.pid(), None);

        // shell.info is a documented non-app.describe object: bare suffixes,
        // never converted into v1 names.
        let shell_fixture = fixture("shell-info-legacy.json");
        let shell = read_legacy(&shell_fixture).unwrap();
        assert_eq!(shell.service(), Some("shell"));
        assert_eq!(shell.value()["contract"], "shell.v1");
        let names: Vec<&str> = shell.verbs().map(|verb| verb.name).collect();
        assert!(names.contains(&"props.get"));
        assert!(!names.contains(&"shell.props.get"));
        assert_eq!(
            validate(&fixture("shell-info-legacy.json"))
                .unwrap_err()
                .code,
            code::MISSING_MARKER
        );

        // A bare HELP array is not a describe object.
        assert_eq!(
            read_legacy(&json!(["ced.ping", "ced.info"]))
                .unwrap_err()
                .code,
            code::INVALID_ROOT
        );
        // Legacy discovery keeps its duplicate compatibility.
        assert!(read_legacy(&json!({"title": "old", "verbs": ["ping", "ping"]})).is_ok());
        // A migrated v1 object or a foreign marker is not legacy: validate
        // owns every marker-carrying object, never a silent demotion.
        assert_eq!(
            read_legacy(&fixture("v1-minimal-strings.json"))
                .unwrap_err()
                .code,
            code::NOT_LEGACY
        );
        assert_eq!(
            read_legacy(&fixture("v1-refusal-unknown-marker.json"))
                .unwrap_err()
                .code,
            code::NOT_LEGACY
        );
    }

    #[test]
    fn violations_carry_a_bounded_path() {
        let violation = Violation::new("verbs[3].name", code::DUPLICATE_VERB, "dup");
        assert_eq!(
            violation.to_string(),
            "duplicate_verb at verbs[3].name: dup"
        );
        let violation = Violation::new("x".repeat(1000), code::INVALID_VERB, "m");
        assert_eq!(violation.path.len(), 256);
    }
}
