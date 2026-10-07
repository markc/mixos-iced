// SPDX-License-Identifier: MIT OR Apache-2.0
//! Renderer-independent protocol model. Adapted from Cosmix BusViewer
//! 70e6c9233a02577099e9c14500aebf44856530e1 src/desktop/apps/busviewer/src/bus.rs.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const APP_ID: &str = "dev.mixos.busviewer";
pub const BODY_LIMIT: usize = 65_536;
pub const REPLY_LIMIT: usize = 1_000_000;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Verb {
    pub name: String,
    pub args: String,
    pub description: String,
    pub read_only: Option<bool>,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Snapshot {
    pub services: BTreeMap<String, Result<Vec<Verb>, String>>,
    pub peers: Vec<String>,
    pub peer_error: Option<String>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub service: String,
    pub verb: String,
}
impl Snapshot {
    pub fn verb(&self, selection: &Selection) -> Option<&Verb> {
        self.services
            .get(&selection.service)?
            .as_ref()
            .ok()?
            .iter()
            .find(|v| v.name == selection.verb)
    }
    pub fn failures(&self) -> usize {
        self.services
            .values()
            .filter(|value| value.is_err())
            .count()
    }
}
pub fn parse_verbs(value: &Value) -> Result<Vec<Verb>, String> {
    let entries = value
        .as_array()
        .or_else(|| value.get("verbs")?.as_array())
        .ok_or("Expected a HELP array or app.describe verbs array")?;
    let mut verbs = Vec::new();
    for entry in entries {
        let name = entry
            .as_str()
            .or_else(|| entry.get("name")?.as_str())
            .filter(|name| !name.is_empty())
            .ok_or("Verb has no name")?;
        verbs.push(Verb {
            name: name.into(),
            args: entry
                .get("args")
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| v.to_string())
                })
                .unwrap_or_default(),
            description: entry
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            read_only: entry.get("read_only").and_then(Value::as_bool),
        });
    }
    verbs.sort_by(|a, b| a.name.cmp(&b.name));
    verbs.dedup_by(|a, b| a.name == b.name);
    Ok(verbs)
}
pub fn services(value: &Value) -> Result<Vec<String>, String> {
    let mut names: Vec<String> = value
        .as_array()
        .ok_or("noded.list is not an array")?
        .iter()
        .filter_map(|v| v.get("name")?.as_str())
        .map(str::to_owned)
        .collect();
    names.push("noded".into());
    names.sort();
    names.dedup();
    Ok(names)
}
pub fn peers(value: &Value) -> Vec<String> {
    let local = value.get("node").and_then(Value::as_str);
    let mut names: Vec<String> = value
        .pointer("/authority/routing_view")
        .or_else(|| value.get("peers"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.get("name")?.as_str())
        .filter(|name| Some(*name) != local)
        .map(str::to_owned)
        .collect();
    names.sort();
    names.dedup();
    names
}
pub fn validate_body(body: &str) -> Result<(), String> {
    if body.len() > BODY_LIMIT {
        return Err(format!("JSON body exceeds {BODY_LIMIT} bytes"));
    }
    if !body.trim().is_empty() {
        serde_json::from_str::<Value>(body).map_err(|e| e.to_string())?;
    }
    Ok(())
}
pub fn pretty(body: &str) -> String {
    if body.len() > REPLY_LIMIT {
        return bounded(body);
    }
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| body.to_owned())
}
pub fn bounded(body: &str) -> String {
    if body.len() <= REPLY_LIMIT {
        return body.into();
    }
    let mut end = REPLY_LIMIT;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n{}", &body[..end], crate::strings::label("truncated"))
}
pub fn describe() -> Value {
    json!({"schema":"busviewer.v1","app_id":APP_ID,"verbs":[
        {"name":"busviewer.ping","description":"Probe BusViewer","read_only":true},
        {"name":"busviewer.info","description":"Inspect discovery, selection, reply and UI state","read_only":true},
        {"name":"busviewer.show","description":"Restore and focus the existing window","read_only":false},
        {"name":"busviewer.refresh","description":"Refetch services and descriptions","read_only":true},
        {"name":"busviewer.select","args":{"service":"string","verb":"string"},"description":"Select an advertised verb","read_only":false},
        {"name":"busviewer.call","args":{"service":"optional string","verb":"optional string","body":"optional JSON text"},"description":"Call exactly once; target's safety is unchanged","read_only":false},
        {"name":"busviewer.quit","description":"Close when idle","read_only":false}
    ]})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn descriptions_preserve_legacy_unknown_safety_and_sort() {
        let verbs = parse_verbs(&json!({"verbs":["quit",{"name":"ping","read_only":true},"ping"]}))
            .unwrap();
        assert_eq!(verbs.len(), 2);
        assert_eq!(verbs[0].name, "ping");
        assert_eq!(verbs[0].read_only, Some(true));
        assert_eq!(verbs[1].read_only, None);
        assert!(parse_verbs(&json!({"title":"legacy"})).is_err());
    }
    #[test]
    fn authority_peers_override_stale_roster_and_exclude_self() {
        assert_eq!(
            peers(
                &json!({"node":"alpha","authority":{"routing_view":[{"name":"alpha"},{"name":"beta"}]},"peers":[{"name":"stale"}]})
            ),
            vec!["beta"]
        );
        assert_eq!(
            peers(&json!({"peers":[{"name":"beta"},{"name":"beta"}]})),
            vec!["beta"]
        );
    }
    #[test]
    fn body_and_reply_bounds_preserve_utf8_and_plain_errors() {
        assert!(validate_body("").is_ok());
        assert!(validate_body("42").is_ok());
        assert!(validate_body("{").is_err());
        assert!(validate_body(&" ".repeat(BODY_LIMIT + 1)).is_err());
        assert_eq!(pretty("permission denied"), "permission denied");
        assert!(bounded(&"é".repeat(REPLY_LIMIT)).ends_with(&crate::strings::label("truncated")));
    }
}
