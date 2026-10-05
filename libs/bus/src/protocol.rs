// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::{Deserialize, Serialize};

/// One entry in a SPEC 02 HELP reply. Metadata describes capabilities; it
/// does not grant permission to invoke them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerbDescriptor {
    pub name: String,
    pub args: Vec<String>,
    pub description: String,
    #[serde(default)]
    pub read_only: bool,
}

impl VerbDescriptor {
    pub fn new(name: &str, args: &[&str], description: &str, read_only: bool) -> Self {
        Self {
            name: name.into(),
            args: args.iter().map(|arg| (*arg).into()).collect(),
            description: description.into(),
            read_only,
        }
    }
}

#[cfg(test)]
mod verb_descriptor_tests {
    use super::VerbDescriptor;

    #[test]
    fn help_shape_is_additive_to_mix_descriptors() {
        let old = serde_json::json!({"name":"status", "args":[], "description":"Read status"});
        let descriptor: VerbDescriptor = serde_json::from_value(old).unwrap();
        assert!(!descriptor.read_only);
        assert_eq!(
            serde_json::to_value(VerbDescriptor::new("status", &[], "Read status", true)).unwrap(),
            serde_json::json!({"name":"status", "args":[], "description":"Read status", "read_only":true})
        );
    }
}

// ── RC codes (ARexx convention) ──

use crate::{RC_ERROR, RC_FAILURE, RC_SUCCESS, RC_WARNING};

// ── Wire format ──

#[derive(Debug, Deserialize)]
pub struct PortRequest {
    pub command: String,
    #[serde(default = "default_args")]
    pub args: serde_json::Value,
}

fn default_args() -> serde_json::Value {
    serde_json::Value::Null
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PortResponse {
    pub rc: u8,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl PortResponse {
    pub fn success(data: serde_json::Value) -> Self {
        Self {
            rc: RC_SUCCESS,
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn ok() -> Self {
        Self {
            rc: RC_SUCCESS,
            ok: true,
            data: None,
            error: None,
        }
    }

    pub fn warning(msg: &str) -> Self {
        Self {
            rc: RC_WARNING,
            ok: true,
            data: None,
            error: Some(msg.to_string()),
        }
    }

    pub fn error(msg: &str) -> Self {
        Self {
            rc: RC_ERROR,
            ok: false,
            data: None,
            error: Some(msg.to_string()),
        }
    }

    pub fn failure(msg: &str) -> Self {
        Self {
            rc: RC_FAILURE,
            ok: false,
            data: None,
            error: Some(msg.to_string()),
        }
    }
}

// ── Script info (for macro menus) ──

/// Metadata for a script that appears in an app's Scripts menu.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptInfo {
    /// Display name (derived from filename, e.g. "Add Watermark")
    pub display_name: String,
    /// Full path to the script file
    pub path: String,
}

// ── Port events (notification channel for UI updates) ──

#[derive(Debug, Clone)]
pub enum PortEvent {
    /// A command was dispatched on the port
    Command { name: String, ok: bool },
    /// App should bring its window to front
    Activate,
    /// Scripts menu was updated by daemon
    ScriptsUpdated(Vec<ScriptInfo>),
}

/// A received application status, distinct from transport failure.
#[derive(Debug, Clone)]
pub enum PortReply {
    Ok { rc: u8, value: serde_json::Value },
    AppError { rc: u8, message: String },
}
