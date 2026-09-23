//! `quicknode_sol` vendor adapter. Owner: `solana` (T1.S4).
//!
//! `qn_estimatePriorityFees` → `fee_estimate`, registered under the vendor id `quicknode` (the
//! id used in routing orders; chain RPC for QuickNode comes from `factory::base_registrations`).
//!
//! The method needs the "Solana Priority Fee API" add-on. Registration is synchronous (no
//! network at boot), so the port probes on first use: if the endpoint answers "method not
//! found", it remembers that and returns `Unsupported` from then on without calling again, so
//! the router skips straight to the next fee source, as if it were never registered.
//!
//! Source: <https://www.quicknode.com/docs/solana/qn_estimatePriorityFees>.

use crate::{
    http::{HttpClient, DEFAULT_TIMEOUT},
    jsonrpc::JsonRpcClient,
};
use async_trait::async_trait;
use bdm_config::{ChainEntry, Loaded, VendorStatus};
use bdm_domain::{FeeEstimate, FeeSpeed};
use bdm_ports::{FeeOracle, PortHandle, PortResult, ProviderError, Registration, VendorMeta};
use bdm_protocols::solana::{fees, SOLANA_MAINNET};
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

const VENDOR: &str = "quicknode";

/// Push the QuickNode Solana fee oracle when the QuickNode endpoint is configured.
pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status(VENDOR) != VendorStatus::Active {
        return;
    }
    let Some(chain) = loaded
        .registry
        .chains
        .enabled()
        .find(|c| c.id.to_string() == SOLANA_MAINNET)
    else {
        return;
    };
    let Some(url) = loaded.rpc_url(VENDOR, &chain.id) else {
        return;
    };
    let e = loaded.registry.vendors.get(VENDOR);
    let meta = VendorMeta {
        id: VENDOR.into(),
        display_name: e.map_or_else(|| "QuickNode".into(), |e| e.display_name.clone()),
        requires_key: true,
        signup_url: e.and_then(|e| e.signup_url.clone()),
        rpc_features: e.map(|e| e.rpc_features.clone()).unwrap_or_default(),
    };
    let http = HttpClient::new(VENDOR, DEFAULT_TIMEOUT)
        .with_secrets(loaded.secret_values().into_iter().map(str::to_owned));
    let fees = Arc::new(QuickNodeFees::new(
        JsonRpcClient::new(http, url),
        chain.clone(),
    ));
    out.push(Registration::new(meta).chain_port(chain.id.clone(), PortHandle::FeeEstimate(fees)));
}

pub struct QuickNodeFees {
    rpc: JsonRpcClient,
    chain: ChainEntry,
    no_addon: AtomicBool,
}

impl QuickNodeFees {
    pub fn new(rpc: JsonRpcClient, chain: ChainEntry) -> Self {
        Self {
            rpc,
            chain,
            no_addon: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl FeeOracle for QuickNodeFees {
    /// Slow / Standard / Fast = `per_compute_unit.low / medium / high` (micro-lamports per CU).
    async fn fee_estimate(&self) -> PortResult<FeeEstimate> {
        let addon_missing =
            || ProviderError::Unsupported("QuickNode Priority Fee API add-on not enabled".into());
        if self.no_addon.load(Ordering::Relaxed) {
            return Err(addon_missing());
        }
        let r = match self
            .rpc
            .request(
                "qn_estimatePriorityFees",
                json!({"last_n_blocks": 100, "api_version": 2}),
            )
            .await
        {
            Err(ProviderError::Unsupported(_)) => {
                self.no_addon.store(true, Ordering::Relaxed);
                return Err(addon_missing());
            }
            other => other?,
        };
        let pcu = &r["per_compute_unit"];
        let tiers = [
            (FeeSpeed::Slow, "low"),
            (FeeSpeed::Standard, "medium"),
            (FeeSpeed::Fast, "high"),
        ]
        .into_iter()
        .map(|(s, k)| {
            pcu[k]
                .as_u64()
                .map(|p| fees::tier(s, p))
                .ok_or_else(|| ProviderError::Transient(format!("per_compute_unit.{k} missing")))
        })
        .collect::<PortResult<Vec<_>>>()?;
        Ok(FeeEstimate {
            chain: self.chain.id.clone(),
            tiers,
            l1_data_fee: None,
            tip: Some(fees::suggested_tip(&self.chain)),
            as_of: chrono::Utc::now(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bdm_config::{Redacted, Registry};
    use bdm_testkit::FakeJsonRpc;
    use std::time::Duration;

    fn fees_for(server: &FakeJsonRpc) -> QuickNodeFees {
        let chain = Registry::builtin()
            .unwrap()
            .chains
            .resolve("solana")
            .unwrap()
            .clone();
        QuickNodeFees::new(
            JsonRpcClient::new(
                HttpClient::new(VENDOR, Duration::from_secs(2)),
                Redacted::new(server.url()),
            ),
            chain,
        )
    }

    #[tokio::test]
    async fn per_compute_unit_levels() {
        let server = FakeJsonRpc::start().await;
        // Example response from the qn_estimatePriorityFees docs.
        server.on(
            "qn_estimatePriorityFees",
            json!({"context": {"slot": 335501774u64},
                   "per_compute_unit": {"extreme": 1432990, "high": 615532, "medium": 89430, "low": 36050},
                   "per_transaction": {"extreme": 309996858926u64, "high": 78616978104u64,
                                       "medium": 19888064000u64, "low": 4999945143u64},
                   "recommended": 877892}),
        );
        let e = fees_for(&server).fee_estimate().await.unwrap();
        let p: Vec<_> = e
            .tiers
            .iter()
            .map(|t| t.compute_unit_price_micro_lamports.unwrap())
            .collect();
        assert_eq!(p, [36050, 89430, 615532]);
    }

    #[tokio::test]
    async fn missing_addon_is_remembered() {
        let server = FakeJsonRpc::start().await; // unknown method → -32601
        let f = fees_for(&server);
        for _ in 0..3 {
            assert!(matches!(
                f.fee_estimate().await,
                Err(ProviderError::Unsupported(_))
            ));
        }
        assert_eq!(server.calls().len(), 1, "probe once, then stay quiet");
    }
}
