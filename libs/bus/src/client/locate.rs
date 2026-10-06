// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where the broker is.
//!
//! Resolution order: `MIXOS_NODED_URL`; the node configuration file
//! (`MIXOS_NODE_CONFIG`, else `$MIXOS_ETC/node.conf.mix` when `MIXOS_ETC`
//! is set, else `~/.config/mixos/node.conf.mix` then
//! `/etc/mixos/node.conf.mix`); and finally the loopback broker. The
//! fallback is loopback, never another node's address: an unconfigured node
//! must fail to reach its own broker loudly rather than dial someone else's.

use std::path::PathBuf;

use strict::Value;

/// The broker URL when nothing configures one.
pub const DEFAULT_NODED_URL: &str = "ws://127.0.0.1:4200/ws";

const URL_VAR: &str = "MIXOS_NODED_URL";
const CONFIG_VAR: &str = "MIXOS_NODE_CONFIG";
const CONFIG_FILE: &str = "node.conf.mix";
const SYSTEM_ETC: &str = "/etc/mixos";
const DEFAULT_PORT: u16 = 4200;

/// The broker WebSocket URL for this node.
pub fn noded_url() -> String {
    if let Some(url) = std::env::var(URL_VAR)
        .ok()
        .filter(|url| !url.trim().is_empty())
    {
        return url;
    }
    let Some(path) = node_config_path() else {
        return DEFAULT_NODED_URL.to_string();
    };
    let source = match std::fs::read_to_string(&path) {
        Ok(source) => source,
        Err(e) => {
            tracing::warn!(
                source = %path.display(),
                error = %e,
                "failed to read node config; using {DEFAULT_NODED_URL}"
            );
            return DEFAULT_NODED_URL.to_string();
        }
    };
    match strict::parse(&source) {
        Ok(config) => match url_from_node_value(&config) {
            Some(url) => {
                tracing::debug!(source = %path.display(), url = %url, "broker URL from node config");
                url
            }
            None => {
                tracing::warn!(
                    source = %path.display(),
                    "node config has no readable wg_ip/noded.port; using {DEFAULT_NODED_URL}"
                );
                DEFAULT_NODED_URL.to_string()
            }
        },
        Err(e) => {
            tracing::warn!(
                source = %path.display(),
                error = %e,
                "node config is not valid strict data; using {DEFAULT_NODED_URL}"
            );
            DEFAULT_NODED_URL.to_string()
        }
    }
}

/// The node configuration file to read, or `None` when no candidate exists.
/// `MIXOS_NODE_CONFIG` names it outright (whether or not it exists, so a
/// misconfiguration is reported rather than silently skipped).
pub fn node_config_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(CONFIG_VAR).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let environment = config::Environment::current();
    let etc = environment
        .etc
        .or_else(|| environment.root.map(|root| root.join("etc")));
    if let Some(etc) = etc {
        return Some(etc.join(CONFIG_FILE));
    }
    candidate_paths(
        None,
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
    .into_iter()
    .find(|path| path.is_file())
}

/// The search order, the user directory ahead of the system one. When
/// `MIXOS_ETC` is set it is the only candidate: the caller asked for an
/// isolated tree (a test, a chroot, an alternate install) and must not fall
/// through to the host's `/etc/mixos`.
fn candidate_paths(
    etc: Option<PathBuf>,
    xdg_config_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Vec<PathBuf> {
    if let Some(etc) = etc {
        return vec![etc.join(CONFIG_FILE)];
    }
    let mut dirs = Vec::new();
    if let Some(dir) = xdg_config_home
        .filter(|d| d.is_absolute())
        .or_else(|| home.map(|h| h.join(".config")))
    {
        dirs.push(dir.join("mixos"));
    }
    dirs.push(PathBuf::from(SYSTEM_ETC));
    dirs.into_iter().map(|dir| dir.join(CONFIG_FILE)).collect()
}

/// The broker URL a node configuration describes: `ws://{wg_ip}:{port}/ws`,
/// with loopback for a missing or empty `wg_ip` and 4200 for a missing
/// `noded.port`. `None` when the text is not strict data or has neither
/// key in a readable form.
pub fn url_from_node_config(source: &str) -> Option<String> {
    url_from_node_value(&strict::parse(source).ok()?)
}

/// [`url_from_node_config`] on an already parsed document.
fn url_from_node_value(config: &Value) -> Option<String> {
    let wg_ip = config.get("wg_ip").and_then(Value::as_str);
    let port = config
        .get("noded")
        .and_then(|noded| noded.get("port"))
        .and_then(port_of);
    if wg_ip.is_none() && port.is_none() {
        return None;
    }
    let host = wg_ip.filter(|ip| !ip.is_empty()).unwrap_or("127.0.0.1");
    Some(format!("ws://{host}:{}/ws", port.unwrap_or(DEFAULT_PORT)))
}

/// A port: a whole number in range, or a string holding one.
fn port_of(value: &Value) -> Option<u16> {
    match value {
        Value::Number(n) if n.fract() == 0.0 && (0.0..=f64::from(u16::MAX)).contains(n) => {
            Some(*n as u16)
        }
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_example_shape() {
        let source = r#"
-- A node's identity and services.
node: "alpha"
wg_ip: "192.0.2.10"
mesh: "example.com"

noded: {
  port: 4201,
  admission: "off",
}

observe: {
  allowed_services: [
    "tower-*",
    "log-*",
  ]
}
"#;
        assert_eq!(
            url_from_node_config(source).as_deref(),
            Some("ws://192.0.2.10:4201/ws")
        );
    }

    #[test]
    fn reads_the_quoted_key_shape_with_commas() {
        let source = r#"{
  "maild": { "port": 25, "enabled": true },
  "node": "beta",
  "noded": {
    "admission": "enforce",
    "port": 4200
  },
  "wg_ip": "192.0.2.9"
}"#;
        assert_eq!(
            url_from_node_config(source).as_deref(),
            Some("ws://192.0.2.9:4200/ws")
        );
    }

    #[test]
    fn empty_wg_ip_falls_back_to_loopback_and_missing_port_to_default() {
        assert_eq!(
            url_from_node_config("wg_ip: \"\"\nnoded: { port: 4300 }\n").as_deref(),
            Some("ws://127.0.0.1:4300/ws")
        );
        assert_eq!(
            url_from_node_config("node: \"solo\"\nwg_ip: \"10.0.0.2\"\n").as_deref(),
            Some("ws://10.0.0.2:4200/ws")
        );
        assert_eq!(url_from_node_config("node: \"solo\"\n"), None);
        assert_eq!(url_from_node_config(""), None);
    }

    #[test]
    fn nested_port_keys_do_not_leak_into_noded() {
        // `port` inside maild, and a `wg_ip` inside a nested map, are not the
        // top-level keys.
        let source = "maild: { port: 25, wg_ip: \"9.9.9.9\" }\nnoded: { port: 4444 }\nwg_ip: \"192.0.2.1\"\n";
        assert_eq!(
            url_from_node_config(source).as_deref(),
            Some("ws://192.0.2.1:4444/ws")
        );
        // A list between keys does not disturb the top level.
        let source = "names: [\"a\", \"b\"]\nnoded: { port: 4445 }\n";
        assert_eq!(
            url_from_node_config(source).as_deref(),
            Some("ws://127.0.0.1:4445/ws")
        );
    }

    #[test]
    fn comments_and_strings_with_dashes_are_handled() {
        let source = "wg_ip: \"10.0.0.1\" -- the mesh address\n# hash comment\nnoded: { port: 4200, note: \"a--b\" }\n";
        assert_eq!(
            url_from_node_config(source).as_deref(),
            Some("ws://10.0.0.1:4200/ws")
        );
    }

    #[test]
    fn ports_read_as_whole_numbers_in_range_or_numeric_strings() {
        assert_eq!(
            url_from_node_config("noded: { port: \"4301\" }\n").as_deref(),
            Some("ws://127.0.0.1:4301/ws")
        );
        // An unreadable port is missing: the default applies when wg_ip is
        // there, and nothing is readable when it is not.
        assert_eq!(
            url_from_node_config("wg_ip: \"10.0.0.3\"\nnoded: { port: 70000 }\n").as_deref(),
            Some("ws://10.0.0.3:4200/ws")
        );
        assert_eq!(url_from_node_config("noded: { port: 42.5 }\n"), None);
        assert_eq!(url_from_node_config("noded: { port: true }\n"), None);
    }

    #[test]
    fn a_document_that_is_not_strict_data_reads_as_nothing() {
        // Executable constructs and broken syntax are refused, not scanned.
        assert_eq!(url_from_node_config("wg_ip: $addr\n"), None);
        assert_eq!(
            url_from_node_config("wg_ip: \"10.0.0.4\"\nnoded: {\n"),
            None
        );
        // Entries inside braces need commas; a newline alone is a refusal.
        assert_eq!(
            url_from_node_config(
                "wg_ip: \"10.0.0.5\"\nnoded: {\n  port: 4200\n  admission: \"off\"\n}\n"
            ),
            None
        );
    }

    #[test]
    fn candidate_order_is_user_then_system_unless_etc_is_set() {
        let paths = candidate_paths(None, None, Some(PathBuf::from("/home/user")));
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/home/user/.config/mixos/node.conf.mix"),
                PathBuf::from("/etc/mixos/node.conf.mix"),
            ]
        );
        let paths = candidate_paths(
            None,
            Some(PathBuf::from("/home/user/cfg")),
            Some(PathBuf::from("/home/user")),
        );
        assert_eq!(
            paths[0],
            PathBuf::from("/home/user/cfg/mixos/node.conf.mix")
        );
        // MIXOS_ETC suppresses the system fallback.
        let paths = candidate_paths(Some(PathBuf::from("/sandbox/etc/mixos")), None, None);
        assert_eq!(
            paths,
            vec![PathBuf::from("/sandbox/etc/mixos/node.conf.mix")]
        );
        // No home at all still finds the system file.
        assert_eq!(
            candidate_paths(None, None, None),
            vec![PathBuf::from("/etc/mixos/node.conf.mix")]
        );
    }
}
