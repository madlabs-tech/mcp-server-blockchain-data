//! Chainalysis on-chain sanctions oracle as the `chainalysis_oracle` vendor (`sanctions` port,
//! EVM only). Owner: `payments-stablecoin` (T1.P2).
//!
//! Addresses from <https://go.chainalysis.com/chainalysis-oracle-docs.html> (verified
//! 2026-09-23). Base uses a different address; Robinhood Chain and Solana are not listed, so the
//! port answers `Unsupported` there and routing falls through to the next sanctions vendor.

use crate::issuer::eth_call_bool;
use alloy_primitives::{address, Address};
use alloy_sol_types::{sol, SolCall};
use async_trait::async_trait;
use bdm_config::{Loaded, VendorStatus};
use bdm_domain::{AccountAddress, AccountId, ChainId};
use bdm_ports::{
    EvmRpc, PortHandle, PortResult, ProviderError, Registration, SanctionsScreener, ScreenResult,
    VendorMeta,
};
use bdm_routing::{RoutedEvmRpc, Router};
use chrono::Utc;
use std::{collections::HashMap, sync::Arc};

pub const VENDOR: &str = "chainalysis_oracle";
pub const SOURCE_URL: &str = "https://go.chainalysis.com/chainalysis-oracle-docs.html";

const ORACLE: Address = address!("0x40C57923924B5c5c5455c48D93317139ADDaC8fb");
const ORACLE_BASE: Address = address!("0x3A91A31cB3dC49b4db9Ce721F50a9D076c8D739B");

sol! {
    function isSanctioned(address addr) external view returns (bool);
}

/// Oracle contract per EIP-155 chain id (only chains Chainalysis lists).
pub fn oracle_address(chain_id: u64) -> Option<Address> {
    match chain_id {
        8453 => Some(ORACLE_BASE),
        1 | 10 | 56 | 137 | 42161 | 43114 => Some(ORACLE),
        _ => None,
    }
}

/// `isSanctioned(addr)` on one chain, read at `latest`.
pub async fn is_sanctioned(rpc: &dyn EvmRpc, oracle: Address, who: Address) -> PortResult<bool> {
    let data = isSanctionedCall { addr: who }.abi_encode();
    eth_call_bool(rpc, oracle, &data, "latest").await
}

struct ChainalysisOracle {
    rpcs: HashMap<ChainId, (Arc<dyn EvmRpc>, Address)>,
}

#[async_trait]
impl SanctionsScreener for ChainalysisOracle {
    async fn screen(&self, account: &AccountId) -> PortResult<ScreenResult> {
        let (Some((rpc, oracle)), AccountAddress::Evm(who)) =
            (self.rpcs.get(&account.chain), account.address)
        else {
            return Err(ProviderError::Unsupported(format!(
                "Chainalysis oracle is not deployed on {}",
                account.chain
            )));
        };
        let sanctioned = is_sanctioned(rpc.as_ref(), *oracle, who).await?;
        Ok(ScreenResult {
            sanctioned,
            source: VENDOR.into(),
            detail: Some(format!("isSanctioned on {oracle} ({})", account.chain)),
            as_of: Utc::now(),
        })
    }
}

pub fn registrations(loaded: &Loaded, router: &Arc<Router>) -> Vec<Registration> {
    if loaded.vendor_status(VENDOR) != VendorStatus::Active {
        return Vec::new();
    }
    let rpcs: HashMap<ChainId, (Arc<dyn EvmRpc>, Address)> = loaded
        .registry
        .chains
        .enabled()
        .filter_map(|c| {
            let oracle = oracle_address(c.id.evm_chain_id()?)?;
            let rpc = RoutedEvmRpc::new(router.clone(), c.id.clone())?;
            Some((c.id.clone(), (Arc::new(rpc) as Arc<dyn EvmRpc>, oracle)))
        })
        .collect();
    if rpcs.is_empty() {
        return Vec::new();
    }
    let entry = loaded.registry.vendors.get(VENDOR);
    let meta = VendorMeta {
        id: VENDOR.into(),
        display_name: entry.map_or_else(|| VENDOR.into(), |e| e.display_name.clone()),
        requires_key: false,
        signup_url: None,
        rpc_features: Default::default(),
    };
    vec![Registration::new(meta)
        .global_port(PortHandle::Sanctions(Arc::new(ChainalysisOracle { rpcs })))]
}

#[cfg(test)]
mod tests {
    use super::*;
    use bdm_testkit::mocks::MockEvmRpc;
    use serde_json::json;

    fn word(b: bool) -> serde_json::Value {
        json!(format!("0x{:064x}", b as u8))
    }

    #[tokio::test]
    async fn screens_via_oracle_and_rejects_unlisted_chains() {
        let mock = Arc::new(MockEvmRpc {
            chain_id: 8453,
            ..Default::default()
        });
        mock.script.push_ok(word(true)).push_ok(word(false));
        let base = ChainId::evm(8453);
        let oracle = ChainalysisOracle {
            rpcs: HashMap::from([(base.clone(), (mock.clone() as Arc<dyn EvmRpc>, ORACLE_BASE))]),
        };
        let acct: AccountId = "eip155:8453:0xd8da6bf26964af9d7eed9e03e53415d37aa96045"
            .parse()
            .unwrap();
        assert!(oracle.screen(&acct).await.unwrap().sanctioned);
        let r = oracle.screen(&acct).await.unwrap();
        assert!(!r.sanctioned);
        assert_eq!(r.source, VENDOR);

        let rh: AccountId = "eip155:4663:0xd8da6bf26964af9d7eed9e03e53415d37aa96045"
            .parse()
            .unwrap();
        assert!(matches!(
            oracle.screen(&rh).await,
            Err(ProviderError::Unsupported(_))
        ));
    }

    #[test]
    fn base_uses_its_own_oracle() {
        assert_eq!(oracle_address(8453), Some(ORACLE_BASE));
        assert_eq!(oracle_address(1), Some(ORACLE));
        assert_eq!(oracle_address(4663), None);
        assert_eq!(
            ORACLE_BASE.to_checksum(None),
            "0x3A91A31cB3dC49b4db9Ce721F50a9D076c8D739B"
        );
    }
}
