//! EVM private relays (`private_relay`), Ethereum mainnet only; other chains get no private
//! relay. Keyless; both accept plain `eth_sendRawTransaction`.
//! - `flashbots`: Flashbots Protect. Source: https://docs.flashbots.net/flashbots-protect/quick-start
//! - `mev_blocker`: MEV Blocker. Source: https://docs.mevblocker.io/reference/api/transaction-endpoints

use crate::{
    chain_rpc::EvmRpcClient,
    http::{HttpClient, DEFAULT_TIMEOUT},
};
use alloy_primitives::{hex, keccak256};
use async_trait::async_trait;
use bdm_config::{Loaded, Redacted, VendorStatus};
use bdm_domain::ChainId;
use bdm_ports::{
    BroadcastReceipt, Broadcaster, PortHandle, PortResult, ProviderError, Registration,
};
use std::sync::Arc;

#[cfg(feature = "flashbots")]
pub const FLASHBOTS: (&str, &str) = ("flashbots", "https://rpc.flashbots.net/fast");
#[cfg(feature = "mev_blocker")]
pub const MEV_BLOCKER: (&str, &str) = ("mev_blocker", "https://rpc.mevblocker.io");

/// Push `(vendor, url)`'s registration if it is active (`loaded.vendor_status(vendor)`).
pub fn register(loaded: &Loaded, out: &mut Vec<Registration>, (vendor, url): (&str, &str)) {
    let mainnet = ChainId::evm(1);
    if loaded.vendor_status(vendor) != VendorStatus::Active
        || !loaded.registry.chains.enabled().any(|c| c.id == mainnet)
    {
        return;
    }
    let relay = Arc::new(Relay::new(vendor, url.into()));
    out.push(
        Registration::new(loaded.vendor_meta(vendor))
            .chain_port(mainnet, PortHandle::PrivateRelay(relay)),
    );
}

struct Relay(EvmRpcClient);

impl Relay {
    fn new(vendor: &str, url: String) -> Self {
        Self(EvmRpcClient::new(
            HttpClient::new(vendor, DEFAULT_TIMEOUT),
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
        let relay = Relay::new("test", server.url());
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
        let mut relays = Vec::new();
        #[cfg(feature = "flashbots")]
        relays.push(FLASHBOTS);
        #[cfg(feature = "mev_blocker")]
        relays.push(MEV_BLOCKER);
        for relay in relays {
            let mut out = Vec::new();
            register(&loaded, &mut out, relay);
            assert_eq!(out[0].vendor.id, relay.0);
            let ports = &out[0].ports;
            assert_eq!(ports.len(), 1);
            assert_eq!(ports[0].0, Some(ChainId::evm(1)));
            assert_eq!(ports[0].1.capability(), bdm_ports::Capability::PrivateRelay);
        }
    }
}
