// SPDX-License-Identifier: MIT OR Apache-2.0

//! Automatic discovery around the explicit native ABP connection APIs.
//! The data/config crate remains independent of the client and interpreter.

use crate::native_client::{BrokerAccount, NodedClient, UnixConnectOptions};

pub fn resolve_noded_url() -> String {
    crate::noded_url()
}

/// Configuration errors propagate for authenticated ingress. Even with no
/// node file, the endpoint stays inside the caller's owned runtime directory.
pub fn unix_connect_options(account: BrokerAccount) -> anyhow::Result<UnixConnectOptions> {
    let mut options = UnixConnectOptions::new(account);
    options.configured_endpoint = Some(match config::node::load_node_config()? {
        Some(config) => config.noded.broker_unix_endpoint(),
        None => config::path(config::Dir::Run).join("noded/bus.sock"),
    });
    Ok(options)
}

pub async fn connect_default(service: &str) -> anyhow::Result<NodedClient> {
    NodedClient::connect(service, &resolve_noded_url()).await
}

pub async fn connect_anonymous_default() -> anyhow::Result<NodedClient> {
    NodedClient::connect_anonymous(&resolve_noded_url()).await
}

pub async fn connect_default_with_provenance(
    service: &str,
    provenance: Option<crate::RegisterProvenance>,
) -> anyhow::Result<NodedClient> {
    NodedClient::connect_with_provenance(service, &resolve_noded_url(), provenance).await
}
