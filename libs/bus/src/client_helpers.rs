// SPDX-License-Identifier: MIT OR Apache-2.0

//! Automatic discovery around the explicit native ABP connection APIs.
//! The data/config crate remains independent of the client and interpreter.

use crate::native_client::{BrokerAccount, NodedClient, UnixConnectOptions};

pub fn resolve_noded_url() -> String {
    crate::noded_url()
}

/// Resolve an explicitly configured broker identity through the system account
/// database. Socket ownership and the caller's identity are never trust roots.
pub fn broker_account_named(account_name: &str) -> anyhow::Result<BrokerAccount> {
    let name = std::ffi::CString::new(account_name)?;
    anyhow::ensure!(!account_name.is_empty(), "empty broker account");
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0u8; 65536];
    // SAFETY: libc writes only supplied storage; UID/GID are copied before
    // dropping the scratch buffer, with no static account storage retained.
    let rc = unsafe {
        libc::getpwnam_r(name.as_ptr(), entry.as_mut_ptr(), buffer.as_mut_ptr().cast(), buffer.len(), &mut result)
    };
    anyhow::ensure!(rc == 0 && !result.is_null(), "configured broker account unavailable");
    // SAFETY: successful getpwnam_r returned a populated entry above.
    let entry = unsafe { entry.assume_init() };
    Ok(BrokerAccount { uid: entry.pw_uid, gid: entry.pw_gid })
}

/// The desktop profile explicitly opts into verified local ABP ingress. Other
/// callers retain the existing TCP behaviour when no profile is configured.
pub fn local_supervised_options(service: &str, url: &str) -> anyhow::Result<crate::SupervisedConnectOptions> {
    let options = crate::SupervisedClient::connect_options(service, url);
    match std::env::var("MIXOS_BROKER_ACCOUNT") {
        Ok(name) => {
            let mut unix = unix_connect_options(broker_account_named(&name)?)?;
            unix.require_native_session = true;
            Ok(options.with_unix(unix))
        }
        Err(std::env::VarError::NotPresent) => Ok(options),
        Err(error) => Err(error.into()),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_identity_requires_a_valid_named_system_account() {
        assert!(broker_account_named("").is_err());
        assert!(broker_account_named("broker\0account").is_err());
        assert!(broker_account_named("mixos-no-such-broker-account-7e95015e").is_err());
        let root = broker_account_named("root").expect("system root account");
        assert_eq!(root.uid, 0);
    }
}
