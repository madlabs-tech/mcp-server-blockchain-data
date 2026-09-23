//! `mev_blocker` vendor adapter: MEV Blocker private relay (`private_relay`), Ethereum
//! mainnet only; other chains get no private relay. Owner: `evm` (T1.E3).

use crate::{
    chain_rpc::EvmRpcClient,
    http::{HttpClient, DEFAULT_TIMEOUT},
};
use alloy_primitives::{hex, keccak256};
use async_trait::async_trait;
use bdm_config::{Loaded, Redacted, VendorStatus};
use bdm_domain::ChainId;
use bdm_ports::{
    BroadcastReceipt, Broadcaster, PortHandle, PortResult, ProviderError, Registration, VendorMeta,
};
use std::sync::Arc;

const VENDOR: &str = "mev_blocker";
/// Keyless; accepts plain `eth_sendRawTransaction`.
/// Source: https://docs.mevblocker.io/reference/api/transaction-endpoints
const URL: &str = "https://rpc.mevblocker.io";

/// Push this vendor's registration if it is active (`loaded.vendor_status("mev_blocker")`).
pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    let mainnet = ChainId::evm(1);
    if loaded.vendor_status(VENDOR) != VendorStatus::Active
        || !loaded.registry.chains.enabled().any(|c| c.id == mainnet)
    {
        return;
    }
    let Some(entry) = loaded.registry.vendors.get(VENDOR) else {
        return;
    };
    let meta = VendorMeta {
        id: VENDOR.into(),
        display_name: entry.display_name.clone(),
        requires_key: false,
        signup_url: entry.signup_url.clone(),
        rpc_features: Default::default(),
    };
    let relay = Relay::new(URL.into());
    out.push(
        Registration::new(meta).chain_port(mainnet, PortHandle::PrivateRelay(Arc::new(relay))),
    );
}

struct Relay(EvmRpcClient);

impl Relay {
    fn new(url: String) -> Self {
        Self(EvmRpcClient::new(
            HttpClient::new(VENDOR, DEFAULT_TIMEOUT),
            1,
            Redacted::new(url),
        ))
    }
}

#[async_trait]
impl Broadcaster for Relay {
    /// The hash is computed locally (keccak of the signed envelope), so a resend of the same
    /// transaction ("already known") is a success with the same hash.
    async fn send_raw(&self, signed: &str) -> PortResult<BroadcastReceipt> {
        let bytes = hex::decode(signed)
            .map_err(|_| ProviderError::Invalid("signed transaction is not hex".into()))?;
        let receipt = BroadcastReceipt {
            tx_hash: format!("{:#x}", keccak256(&bytes)),
            private: true,
        };
        match self.0.send_raw(signed).await {
            Ok(_) => Ok(receipt),
            Err(ProviderError::Invalid(m)) if m.to_lowercase().contains("already known") => {
                Ok(receipt)
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bdm_testkit::FakeJsonRpc;
    use serde_json::json;

    /// keccak256 of the empty byte string.
    const EMPTY_HASH: &str = "0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470";

    #[tokio::test]
    async fn private_send_with_local_hash_and_idempotent_resend() {
        let server = FakeJsonRpc::start().await;
        server.on("eth_sendRawTransaction", json!("0xrelay"));
        let relay = Relay::new(server.url());
        let r = relay.send_raw("0x").await.unwrap();
        assert_eq!((r.tx_hash.as_str(), r.private), (EMPTY_HASH, true));

        server.on_fn("eth_sendRawTransaction", |_| {
            Err((-32000, "already known".into()))
        });
        assert_eq!(relay.send_raw("0x").await.unwrap().tx_hash, EMPTY_HASH);
        assert!(matches!(
            relay.send_raw("zz").await,
            Err(ProviderError::Invalid(_))
        ));
    }

    #[test]
    fn ethereum_mainnet_only() {
        let loaded = bdm_config::ConfigLoader::new(
            bdm_config::ConfigDir::new("/nonexistent"),
            bdm_config::EnvSource::from_pairs(std::iter::empty::<(&str, &str)>()),
        )
        .unwrap()
        .load_texts("", "")
        .unwrap();
        let mut out = Vec::new();
        register(&loaded, &mut out);
        let ports = &out[0].ports;
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].0, Some(ChainId::evm(1)));
        assert_eq!(ports[0].1.capability(), bdm_ports::Capability::PrivateRelay);
    }
}
