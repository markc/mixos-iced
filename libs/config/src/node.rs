// SPDX-License-Identifier: MIT OR Apache-2.0
//! Typed node and broker configuration over strict data.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeConfig {
    /// Unowned service sections survive typed read/encode round trips.
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
    pub node: String,
    pub wg_ip: String,
    pub mesh: Option<String>,
    pub noded: NodedConfig,
    pub observe: ObserveConfig,
}

impl NodeConfig {
    pub fn noded_listen(&self) -> String {
        let host = if self.wg_ip.is_empty() {
            "127.0.0.1"
        } else {
            &self.wg_ip
        };
        format!("{host}:{}", self.noded.port)
    }
    pub fn noded_url(&self) -> String {
        format!("ws://{}/ws", self.noded_listen())
    }
}

// SPDX-License-Identifier: MIT OR Apache-2.0

/// SPEC 13 §9a D2 broker-admission posture (slice 2-c-1). Per-node operator
/// policy, set in `node.conf.mix` `[noded] admission`. **Default `off`** — a
/// fresh node never starts challenging until told to (clients-before-broker,
/// §16). `observe` challenges + verdicts + logs but NEVER refuses (the §7.8 B3
/// decide-by-doing vehicle); `enforce` refuses a failed gated session (2-c-2,
/// Mark-gated, hub-last).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdmissionMode {
    #[default]
    Off,
    Observe,
    Enforce,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NodedConfig {
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
    pub port: u16,
    /// Published system broker endpoint (BUS-013), independent of client XDG.
    #[serde(deserialize_with = "absolute_unix_socket")]
    pub unix_socket: Option<std::path::PathBuf>,
    /// BROKER-019 pending grants per Term; may lower the initial cap of 32.
    pub pending_grants_per_parent: usize,
    pub mesh_config: Option<String>,
    /// SPEC 13 §9a D2 admission posture (off | observe | enforce). Default off.
    pub admission: AdmissionMode,
    /// AGENTIC-FIRST posture switch (Mark, 2026-09-14). When `true` (the
    /// default), noded lets ABP flow freely between admitted WG mesh peers:
    /// the per-message principal/native-session-locality guards are relaxed and
    /// the WG /24 + signed inventory (mesh membership) is the trust boundary.
    /// This is the CLAUDE.md "default open, opt-in hard" law — the enforcement
    /// code is retained and re-armed by setting this to `false` per node once
    /// the mesh/app layer is mature. Membership trust (WG + signed inventory)
    /// is UNAFFECTED either way; only per-message authorization relaxes.
    pub mesh_open: bool,
}

fn absolute_unix_socket<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<std::path::PathBuf>, D::Error> {
    let path = Option::<std::path::PathBuf>::deserialize(deserializer)?;
    if path.as_ref().is_some_and(|path| !path.is_absolute()) {
        return Err(serde::de::Error::custom(
            "noded.unix_socket must be absolute",
        ));
    }
    Ok(path)
}

impl Default for NodedConfig {
    fn default() -> Self {
        Self {
            extra: Default::default(),
            port: 4200,
            unix_socket: None,
            pending_grants_per_parent: 32,
            mesh_config: None,
            admission: AdmissionMode::Off,
            mesh_open: true,
        }
    }
}

impl NodedConfig {
    /// Client resolution must never derive the system endpoint from user XDG.
    pub fn unix_endpoint(&self) -> std::path::PathBuf {
        self.unix_socket
            .clone()
            .unwrap_or_else(|| "/run/mixos/noded/bus.sock".into())
    }

    /// A broker publishes its resolved endpoint through noded.ping. System
    /// units pin MIXOS_RUN; development installs retain the common path rules.
    pub fn broker_unix_endpoint(&self) -> std::path::PathBuf {
        self.unix_socket
            .clone()
            .unwrap_or_else(|| crate::path(crate::Dir::Run).join("noded/bus.sock"))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ObserveConfig {
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
    /// Anchored service-name globs admitted to `noded.observe.start`.
    /// The broker validates pattern syntax and ignores invalid entries.
    pub allowed_services: Vec<String>,
}
// SPDX-License-Identifier: MIT OR Apache-2.0

const ENV_VAR: &str = "MIXOS_NODE_CONFIG";

/// Search paths in priority order (after env var).
///
/// In user mode (uid != 0) `path(Etc)` resolves to
/// `~/.config/mixos/`, which means a system daemon user (e.g.
/// `mixos-maild`) running an unprivileged CLI would never find a
/// node config placed at `/etc/mixos/`. The doc comment at the top of
/// this file already promised user-XDG-then-system-etc; this honours
/// that promise by appending the system directory as a secondary
/// candidate whenever it differs from the primary (root mode
/// dedupes naturally). Within each directory `node.conf.mix` is the
/// sole config file.
///
/// `MIXOS_ETC` is the explicit isolation knob (tests, chroots,
/// alternate installs): when it is set we honour the caller's intent
/// and do NOT silently fall through to the host's `/etc/mixos/`.
fn search_paths() -> Vec<PathBuf> {
    let primary_dir = crate::path(crate::Dir::Etc);
    let mixos_etc_set =
        has_override(std::env::var_os("MIXOS_ETC")) || has_override(std::env::var_os("MIXOS"));
    build_search_paths(&primary_dir, mixos_etc_set)
}

fn has_override(value: Option<std::ffi::OsString>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

/// Pure helper extracted from `search_paths()` so the search order
/// can be unit-tested without touching process env vars (which would
/// poison the `OnceLock`-cached path resolver for sibling tests).
///
/// Emits one `node.conf.mix` per candidate directory, the user dir
/// ahead of the system dir.
fn build_search_paths(primary_dir: &Path, mixos_etc_set: bool) -> Vec<PathBuf> {
    let mut dirs = vec![primary_dir.to_path_buf()];
    if !mixos_etc_set {
        let system = PathBuf::from("/etc/mixos");
        if !dirs.contains(&system) {
            dirs.push(system);
        }
    }
    dirs.into_iter()
        .map(|dir| dir.join("node.conf.mix"))
        .collect()
}

/// Load node config from the given path (strict-data `.conf.mix`).
///
/// A read or parse failure is a hard error with the path in the chain.
/// C11 removed the legacy `.toml` parse + upgrade-write; a `.toml` path
/// now simply fails to parse as strict-data.
pub fn load_from(path: &Path) -> Result<NodeConfig> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let config: NodeConfig = strict::from_str(&contents)
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;
    Ok(config)
}

/// Load node config using the standard search order.
///
/// Returns `Ok(Some(config))` if found, `Ok(None)` if no file exists
/// at any search path.
pub fn load_node_config() -> Result<Option<NodeConfig>> {
    // 1. Environment variable
    if let Ok(path) = std::env::var(ENV_VAR) {
        let config = load_from(Path::new(&path)).with_context(|| format!("{ENV_VAR}={path}"))?;
        tracing::debug!(source = %path, env_var = ENV_VAR, "loaded node config");
        return Ok(Some(config));
    }

    // 2. Standard search paths
    for path in search_paths() {
        if path.exists() {
            let config = load_from(&path)?;
            tracing::debug!(source = %path.display(), "loaded node config");
            return Ok(Some(config));
        }
    }

    Ok(None)
}

/// Load node config, returning an error if not found.
pub fn require_node_config() -> Result<NodeConfig> {
    load_node_config()?.with_context(|| {
        let paths: Vec<_> = search_paths()
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        format!(
            "node config not found — set {ENV_VAR} or place node.conf.mix at: {}",
            paths.join(", ")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn broker_defaults_and_explicit_native_endpoint() {
        let config: NodeConfig = strict::from_str("node: alpha\n").unwrap();
        assert_eq!(config.noded_listen(), "127.0.0.1:4200");
        assert!(config.noded.mesh_open);
        assert_eq!(config.noded.admission, AdmissionMode::Off);
        assert_eq!(config.noded.pending_grants_per_parent, 32);
        let config: NodeConfig = strict::from_str(
            "node: alpha\nnoded: { unix_socket: '/tmp/alpha/bus.sock', admission: enforce }\n",
        )
        .unwrap();
        assert_eq!(
            config.noded.unix_endpoint(),
            PathBuf::from("/tmp/alpha/bus.sock")
        );
        assert_eq!(
            config.noded.broker_unix_endpoint(),
            config.noded.unix_endpoint()
        );
        assert_eq!(config.noded.admission, AdmissionMode::Enforce);
        assert!(strict::from_str::<NodeConfig>("noded: { unix_socket: relative }\n").is_err());
    }
    #[test]
    fn explicit_etc_isolates_and_system_path_deduplicates() {
        assert!(!has_override(None));
        assert!(!has_override(Some("".into())));
        assert!(has_override(Some("/sandbox".into())));
        assert_eq!(
            build_search_paths(
                Path::new("/home/user/.config/mixos"),
                has_override(Some("".into()))
            ),
            vec![
                PathBuf::from("/home/user/.config/mixos/node.conf.mix"),
                PathBuf::from("/etc/mixos/node.conf.mix")
            ]
        );
        assert_eq!(
            build_search_paths(Path::new("/sandbox"), true),
            vec![PathBuf::from("/sandbox/node.conf.mix")]
        );
        assert_eq!(
            build_search_paths(Path::new("/etc/mixos"), false),
            vec![PathBuf::from("/etc/mixos/node.conf.mix")]
        );
    }
    #[test]
    fn unrelated_service_fields_are_forward_compatible() {
        let config: NodeConfig = strict::from_str("node: beta\nwg_ip: '192.0.2.2'\nwebd: { port: 443 }\nobserve: { allowed_services: [tower] }\n").unwrap();
        assert_eq!(config.noded_url(), "ws://192.0.2.2:4200/ws");
        assert_eq!(config.observe.allowed_services, vec!["tower"]);
    }

    #[test]
    fn typed_node_round_trip_preserves_unowned_sections() {
        let source = "node: alpha\nwebd: { port: 443, tls: true }\nnoded: { future_option: [a, b] }\nobserve: { future_policy: { enabled: true } }\n";
        let config: NodeConfig = strict::from_str(source).unwrap();
        let encoded = strict::to_string_pretty(&config).unwrap();
        let decoded: NodeConfig = strict::from_str(&encoded).unwrap();
        assert_eq!(decoded.extra, config.extra);
        assert_eq!(decoded.noded.extra, config.noded.extra);
        assert_eq!(decoded.observe.extra, config.observe.extra);
        assert_eq!(decoded.extra["webd"]["port"].as_f64(), Some(443.0));
    }
}
