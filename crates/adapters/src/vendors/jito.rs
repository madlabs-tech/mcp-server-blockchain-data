//! `jito` vendor adapter. Owner: `solana` (T1.S4).
//!
//! Jito block engine `sendTransaction` with `bundleOnly=true` → `private_relay`: the tx travels
//! only as a single-tx bundle (revert protection, never the public TPU path). A bundle is only
//! considered with a tip, so a tx without a System transfer of ≥ 1000 lamports to a Jito tip
//! account is refused locally as `Unsupported` (the router moves on) instead of silently
//! never landing. Keyless: 1 req/s per IP per region.
//!
//! Source: <https://docs.jito.wtf/lowlatencytxnsend/>.

use crate::{
    http::{HttpClient, DEFAULT_TIMEOUT},
    jsonrpc::JsonRpcClient,
};
use async_trait::async_trait;
use bdm_config::{Loaded, Redacted, VendorStatus};
use bdm_ports::{
    BroadcastReceipt, Broadcaster, PortHandle, PortResult, ProviderError, Registration,
};
use bdm_protocols::solana::{fees::JITO_MIN_TIP_LAMPORTS, tx, SOLANA_MAINNET};
use serde_json::json;
use std::sync::Arc;

/// Mainnet block engine, transactions endpoint, bundle-only mode.
pub const TX_URL: &str =
    "https://mainnet.block-engine.jito.wtf/api/v1/transactions?bundleOnly=true";
/// Mainnet tip accounts (`getTipAccounts`), from the Jito docs.
pub const TIP_ACCOUNTS: [&str; 8] = [
    "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
    "HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe",
    "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
    "ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49",
    "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
    "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt",
    "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
    "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT",
];

/// Push the `jito` relay for Solana mainnet when active.
pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status("jito") != VendorStatus::Active {
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
    let meta = loaded.vendor_meta("jito");
    let rpc = JsonRpcClient::new(
        HttpClient::new("jito", DEFAULT_TIMEOUT),
        Redacted::new(TX_URL.to_owned()),
    );
    out.push(Registration::new(meta).chain_port(
        chain.id.clone(),
        PortHandle::PrivateRelay(Arc::new(Jito::new(rpc))),
    ));
}

pub struct Jito {
    rpc: JsonRpcClient,
}

impl Jito {
    pub fn new(rpc: JsonRpcClient) -> Self {
        Self { rpc }
    }
}

#[async_trait]
impl Broadcaster for Jito {
    async fn send_raw(&self, signed: &str) -> PortResult<BroadcastReceipt> {
        let sig = tx::signature_of(signed).map_err(|e| ProviderError::Invalid(e.to_string()))?;
        tx::check_tip(signed, &TIP_ACCOUNTS, JITO_MIN_TIP_LAMPORTS, false)
            .map_err(|e| ProviderError::Unsupported(format!("jito: {e}")))?;
        self.rpc
            .request("sendTransaction", json!([signed, {"encoding": "base64"}]))
            .await?;
        Ok(BroadcastReceipt {
            tx_hash: sig,
            private: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use bdm_testkit::FakeJsonRpc;
    use std::time::Duration;

    fn signed_tip(to: &str, lamports: u64) -> String {
        // Legacy tx: [payer, tip account, system]; one System transfer payer → tip.
        let mut m = vec![1u8, 0, 1, 3];
        m.extend([7u8; 32]);
        m.extend(bs58::decode(to).into_vec().unwrap());
        m.extend([0u8; 32]);
        m.extend([9u8; 32]);
        m.extend([1u8, 2, 2, 0, 1, 12, 2, 0, 0, 0]);
        m.extend(lamports.to_le_bytes());
        let mut t = vec![1u8];
        t.extend([6u8; 64]);
        t.extend(m);
        base64::engine::general_purpose::STANDARD.encode(t)
    }

    #[tokio::test]
    async fn bundle_only_send_with_tip_check() {
        let server = FakeJsonRpc::start().await;
        server.on("sendTransaction", json!("ignored-vendor-sig"));
        let rpc = JsonRpcClient::new(
            HttpClient::new("jito", Duration::from_secs(2)),
            Redacted::new(format!(
                "{}/api/v1/transactions?bundleOnly=true",
                server.url()
            )),
        );
        let j = Jito::new(rpc);
        let r = j
            .send_raw(&signed_tip(TIP_ACCOUNTS[3], 1_000))
            .await
            .unwrap();
        assert!(r.private);
        assert_eq!(r.tx_hash, bs58::encode([6u8; 64]).into_string()); // computed locally
        assert_eq!(server.calls()[0].1[1]["encoding"], "base64");
        assert!(matches!(
            j.send_raw(&signed_tip(TIP_ACCOUNTS[3], 999)).await,
            Err(ProviderError::Unsupported(_))
        ));
        assert_eq!(server.calls().len(), 1);
    }
}
