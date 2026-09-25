//! Solana readers.
//!
//! ## Commitment and finality (Alpenglow-ready)
//! The settled commitment comes from `registry/chains.toml` (`finality.default`, overridable
//! per operator via `chain_overrides`), never a hard-coded 32 slots:
//! - `"finalized"` (default): `finalized` → [`Finality::Finalized`], `confirmed` →
//!   [`Finality::Confirmed`], `processed` → [`Finality::Pending`].
//! - `"confirmed"`: operators who accept optimistic confirmation as final (e.g. after
//!   Alpenglow, where confirmed ≈ finalized in ~150 ms) get `confirmed` → `Finalized`.
//!
//! Confidential transfers override everything with [`Finality::Unverifiable`] (see [`tx`]).

pub mod fees;
pub mod rpc_vendor;
pub mod spl;
pub mod tx;

use bdm_config::{ChainEntry, Loaded};
use bdm_domain::{AssetId, AssetRef, Finality, SolanaPubkey};

/// CAIP-2 id of Solana mainnet-beta (genesis-hash prefix). Mainnet-only vendor APIs (Jito,
/// Helius Sender, Jupiter) register for this chain only.
/// <https://namespaces.chainagnostic.org/solana/caip2>
pub const SOLANA_MAINNET: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";

/// The Solana mainnet entry, if that chain is enabled.
pub fn enabled_mainnet(loaded: &Loaded) -> Option<&ChainEntry> {
    loaded
        .registry
        .chains
        .enabled()
        .find(|c| c.id.to_string() == SOLANA_MAINNET)
}

/// Commitment level the chain policy treats as settled: `"finalized"` or `"confirmed"`.
pub fn settled_commitment(chain: &ChainEntry) -> &'static str {
    match chain.finality.default.as_deref() {
        Some("confirmed") => "confirmed",
        _ => "finalized",
    }
}

/// Map an RPC `confirmationStatus` (+ `confirmations`, `null` once rooted) to [`Finality`].
pub fn finality_from_status(
    chain: &ChainEntry,
    status: Option<&str>,
    confirmations: Option<u64>,
) -> Finality {
    match status {
        Some("finalized") => Finality::Finalized,
        Some("confirmed") if settled_commitment(chain) == "confirmed" => Finality::Finalized,
        Some("confirmed") => Finality::Confirmed {
            confirmations: confirmations.unwrap_or(0),
        },
        _ => Finality::Pending,
    }
}

pub(crate) fn native_asset(chain: &ChainEntry) -> AssetId {
    AssetId::native(chain.id.clone(), chain.native.slip44)
}

pub(crate) fn token_asset(chain: &ChainEntry, mint: SolanaPubkey) -> AssetId {
    AssetId {
        chain: chain.id.clone(),
        asset: AssetRef::SplToken(mint),
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use async_trait::async_trait;
    use bdm_config::{ChainEntry, Registry};
    use bdm_ports::{PortResult, ProviderError, SolanaRpc};
    use serde_json::Value;
    use std::sync::Mutex;

    type Handler = Box<dyn Fn(&str, &Value) -> Option<Value> + Send + Sync>;

    /// In-process `SolanaRpc`: a function of (method, params). `None` = method not found.
    pub struct FnRpc {
        handler: Handler,
        pub calls: Mutex<Vec<(String, Value)>>,
    }

    impl FnRpc {
        pub fn new(f: impl Fn(&str, &Value) -> Option<Value> + Send + Sync + 'static) -> Self {
            Self {
                handler: Box::new(f),
                calls: Mutex::new(Vec::new()),
            }
        }
        pub fn methods(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .map(|c| c.0.clone())
                .collect()
        }
    }

    #[async_trait]
    impl SolanaRpc for FnRpc {
        async fn request(&self, method: &str, params: Value) -> PortResult<Value> {
            self.calls
                .lock()
                .unwrap()
                .push((method.to_owned(), params.clone()));
            (self.handler)(method, &params)
                .ok_or_else(|| ProviderError::Unsupported(format!("no fake for {method}")))
        }
    }

    pub fn mainnet() -> ChainEntry {
        Registry::builtin()
            .unwrap()
            .chains
            .resolve("solana")
            .unwrap()
            .clone()
    }

    pub fn fixture(name: &str) -> Value {
        let path = format!("{}/fixtures/solana/{name}.json", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(&path).expect(&path)).expect(&path)
    }
}

#[cfg(test)]
mod tests {
    use super::{testutil::mainnet, *};

    #[test]
    fn finality_follows_chain_policy() {
        let mut chain = mainnet();
        assert_eq!(settled_commitment(&chain), "finalized");
        assert_eq!(
            finality_from_status(&chain, Some("confirmed"), Some(3)),
            Finality::Confirmed { confirmations: 3 }
        );
        assert_eq!(
            finality_from_status(&chain, Some("finalized"), None),
            Finality::Finalized
        );
        assert_eq!(
            finality_from_status(&chain, Some("processed"), Some(0)),
            Finality::Pending
        );
        // Alpenglow switch: operator declares `confirmed` as settled.
        chain.finality.default = Some("confirmed".into());
        assert_eq!(
            finality_from_status(&chain, Some("confirmed"), Some(1)),
            Finality::Finalized
        );
    }
}
