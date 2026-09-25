//! `helius` vendor adapter. Owner: `solana` (T1.S3).
//!
//! - `helius` (key): DAS `getAssetsByOwner` → `token_balances`, DAS `getAsset` →
//!   `token_metadata`, `getPriorityFeeEstimate` → `fee_estimate`, all on the key-bearing RPC URL.
//!   Chain RPC + public broadcast come from `factory::base_registrations`.
//! - `helius_sender` (keyless): Sender `sendTransaction` → `private_relay`. Sender requires a
//!   tip (≥ 0.001 SOL to a Sender tip account) and a `SetComputeUnitPrice` instruction; both
//!   are checked locally and a tx without them is refused as `Unsupported` (the router moves to
//!   the next relay) instead of being dropped by Sender.
//!
//! Free-plan methods only: the paid `getTransfersByAddress` / `getTransactionsForAddress` are
//! never called (history uses the `rpc` scanner). No `QuotaReporter`: no credit-free usage
//! endpoint is confirmed; credits are metered locally from `registry/vendors.toml`.
//!
//! Sources: <https://www.helius.dev/docs/das-api>, <https://www.helius.dev/docs/priority-fee-api>,
//! <https://www.helius.dev/docs/sending-transactions/sender>.

use super::util;

use crate::jsonrpc::{array_field, JsonRpcClient};
use async_trait::async_trait;
use bdm_config::{ChainEntry, Loaded, Redacted, VendorStatus};
use bdm_domain::{AccountAddress, Amount, AssetId, AssetRef, FeeEstimate, SolanaPubkey};
use bdm_ports::{
    BroadcastReceipt, Broadcaster, FeeOracle, PortHandle, PortResult, ProviderError, Registration,
    TokenBalance, TokenBalances, TokenInfo, TokenMetadata,
};
use bdm_protocols::solana::{fees, spl, tx};
use serde_json::{json, Value};
use std::sync::Arc;

/// Global HTTPS Sender endpoint (keyless). Regional HTTP endpoints exist for backends.
pub const SENDER_URL: &str = "https://sender.helius-rpc.com/fast";
/// Sender tip accounts (mainnet), from the Sender docs.
pub const SENDER_TIP_ACCOUNTS: [&str; 10] = [
    "4ACfpUFoaSD9bfPdeu6DBt89gB6ENTeHBXCAi87NhDEE",
    "D2L6yPZ2FmmmTKPgzaMKdhu6EWZcTpLy1Vhx8uvZe7NZ",
    "9bnz4RShgq1hAnLnZbP8kbgBg1kEmcJBYQq3gQbmnSta",
    "5VY91ws6B2hMmBFRsXkoAAdsPHBJwRfBht4DXox3xkwn",
    "2nyhqdwKcJZR2vcqCyrYsaPVdAnFoJjiksCXJ7hfEYgD",
    "2q5pghRs6arqVjRvT5gfgWfWcHWmw1ZuCzphgd5KfWGJ",
    "wyvPkWjVZz1M8fHQnMMCDTQDbkManefNNhweYk5WkcF",
    "3KCKozbAaF75qEU33jtzozcJ29yJuaLJTy2jFdzUY8bT",
    "4vieeGHPYPG2MmyPRcYjdiDmmhN3ww7hsFNap8pVN3Ey",
    "4TQLFNWK8AovT1gFvda5jfw2oJeRMKEmw7aH6MGBJ3or",
];
/// DAS page size (max 1000).
const DAS_PAGE: usize = 1000;
// ponytail: 10 pages × 1000 assets; wallets beyond that are truncated (fall back to `rpc`).
const DAS_MAX_PAGES: u32 = 10;

/// Push `helius` (DAS, priority fee) and `helius_sender` (relay) registrations when active.
pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    let Some(chain) = bdm_protocols::solana::enabled_mainnet(loaded) else {
        return;
    };
    if loaded.vendor_status("helius") == VendorStatus::Active {
        if let Some(url) = loaded.rpc_url("helius", &chain.id) {
            let http = util::http(loaded, "helius");
            let h = Arc::new(Helius::new(JsonRpcClient::new(http, url), chain.clone()));
            out.push(
                Registration::new(loaded.vendor_meta("helius"))
                    .chain_port(chain.id.clone(), PortHandle::TokenBalances(h.clone()))
                    .chain_port(chain.id.clone(), PortHandle::FeeEstimate(h.clone()))
                    .global_port(PortHandle::TokenMetadata(h)),
            );
        }
    }
    if loaded.vendor_status("helius_sender") == VendorStatus::Active {
        let http = util::http(loaded, "helius_sender");
        let s = Arc::new(HeliusSender::new(JsonRpcClient::new(
            http,
            Redacted::new(SENDER_URL.to_owned()),
        )));
        out.push(
            Registration::new(loaded.vendor_meta("helius_sender"))
                .chain_port(chain.id.clone(), PortHandle::PrivateRelay(s)),
        );
    }
}

/// Helius enhanced JSON-RPC (DAS + priority fee) on one cluster.
pub struct Helius {
    rpc: JsonRpcClient,
    chain: ChainEntry,
}

impl Helius {
    pub fn new(rpc: JsonRpcClient, chain: ChainEntry) -> Self {
        Self { rpc, chain }
    }
}

/// Fee levels may be serialized as floats (schema: `format: float`). They are rates
/// (micro-lamports per CU), not amounts; round up to whole micro-lamports.
fn fee_level(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| {
        v.as_f64()
            .filter(|f| f.is_finite() && *f >= 0.0)
            .map(|f| f.ceil() as u64)
    })
}

#[async_trait]
impl TokenBalances for Helius {
    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn balances(
        &self,
        owner: &AccountAddress,
        assets: Option<&[AssetId]>,
    ) -> PortResult<Vec<TokenBalance>> {
        let AccountAddress::Solana(owner_pk) = owner else {
            return Err(ProviderError::Invalid(format!(
                "{owner} is not a Solana address"
            )));
        };
        let wanted = |a: &AssetId| assets.is_none_or(|l| l.contains(a));
        let mut out = Vec::new();
        for page in 1..=DAS_MAX_PAGES {
            let r = self
                .rpc
                .request(
                    "getAssetsByOwner",
                    json!({"ownerAddress": owner.to_string(), "page": page, "limit": DAS_PAGE,
                           "displayOptions": {"showFungible": true, "showNativeBalance": true}}),
                )
                .await?;
            if page == 1 {
                if let Some(lamports) = r["nativeBalance"]["lamports"].as_u64() {
                    let asset = AssetId::native(self.chain.id.clone(), self.chain.native.slip44);
                    if wanted(&asset) {
                        out.push(TokenBalance {
                            asset,
                            amount: Amount::from_u128(lamports.into(), self.chain.native.decimals),
                            symbol: Some(self.chain.native.symbol.clone()),
                            token_account: None,
                        });
                    }
                }
            }
            let items = array_field(&r, "items", "getAssetsByOwner")?;
            for item in items {
                let ti = &item["token_info"];
                let (Some(balance), Some(decimals), Some(mint)) = (
                    ti["balance"].as_u64(),
                    ti["decimals"].as_u64().and_then(|d| u8::try_from(d).ok()),
                    item["id"]
                        .as_str()
                        .and_then(|s| s.parse::<SolanaPubkey>().ok()),
                ) else {
                    continue; // NFTs and non-fungibles have no token_info.balance
                };
                let asset = AssetId {
                    chain: self.chain.id.clone(),
                    asset: AssetRef::SplToken(mint),
                };
                if !wanted(&asset) {
                    continue;
                }
                let ata = ti["associated_token_address"]
                    .as_str()
                    .and_then(|s| s.parse::<SolanaPubkey>().ok())
                    .or_else(|| {
                        let program = ti["token_program"].as_str()?.parse().ok()?;
                        spl::associated_token_address(owner_pk, &mint, &program).ok()
                    });
                out.push(TokenBalance {
                    asset,
                    amount: Amount::from_u128(balance.into(), decimals),
                    symbol: ti["symbol"]
                        .as_str()
                        .or_else(|| item["content"]["metadata"]["symbol"].as_str())
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned),
                    token_account: ata.map(AccountAddress::Solana),
                });
            }
            if items.len() < DAS_PAGE {
                break;
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl FeeOracle for Helius {
    /// Global estimate (no account keys); Slow / Standard / Fast = low / medium / high.
    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn fee_estimate(&self) -> PortResult<FeeEstimate> {
        let r = self
            .rpc
            .request(
                "getPriorityFeeEstimate",
                json!([{"options": {"includeAllPriorityFeeLevels": true}}]),
            )
            .await?;
        let levels = &r["priorityFeeLevels"];
        fees::estimate_from_levels(&self.chain, "priorityFeeLevels", |k| fee_level(&levels[k]))
    }
}

#[async_trait]
impl TokenMetadata for Helius {
    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn metadata(&self, asset: &AssetId) -> PortResult<TokenInfo> {
        let AssetRef::SplToken(mint) = asset.asset else {
            return Err(ProviderError::Unsupported(
                "helius serves SPL mints only".into(),
            ));
        };
        if asset.chain != self.chain.id {
            return Err(ProviderError::Unsupported(format!(
                "helius does not serve {}",
                asset.chain
            )));
        }
        let r = self
            .rpc
            .request("getAsset", json!({"id": mint.to_string()}))
            .await?;
        if r.is_null() {
            return Err(ProviderError::NotFound);
        }
        let decimals = r["token_info"]["decimals"]
            .as_u64()
            .and_then(|d| u8::try_from(d).ok())
            .ok_or_else(|| ProviderError::Unsupported(format!("{mint} is not fungible")))?;
        let text = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
        let md = &r["content"]["metadata"];
        Ok(TokenInfo {
            asset: asset.clone(),
            decimals,
            symbol: text(&r["token_info"]["symbol"]).or_else(|| text(&md["symbol"])),
            name: text(&md["name"]),
            logo_url: text(&r["content"]["links"]["image"]),
            verified: None,
            source: "helius".into(),
        })
    }
}

/// Helius Sender relay.
pub struct HeliusSender {
    rpc: JsonRpcClient,
}

impl HeliusSender {
    pub fn new(rpc: JsonRpcClient) -> Self {
        Self { rpc }
    }
}

#[async_trait]
impl Broadcaster for HeliusSender {
    async fn send_raw(&self, signed: &str) -> PortResult<BroadcastReceipt> {
        let sig = tx::signature_of(signed).map_err(|e| ProviderError::Invalid(e.to_string()))?;
        tx::check_tip(
            signed,
            &SENDER_TIP_ACCOUNTS,
            fees::HELIUS_SENDER_MIN_TIP_LAMPORTS,
            true,
        )
        .map_err(|e| ProviderError::Unsupported(format!("helius_sender: {e}")))?;
        self.rpc
            .request(
                "sendTransaction",
                json!([signed, {"encoding": "base64", "skipPreflight": true, "maxRetries": 0}]),
            )
            .await?;
        // Sender routes to validators (SWQoS) and Jito at once: not a private mempool.
        Ok(BroadcastReceipt {
            tx_hash: sig,
            private: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{HttpClient, DEFAULT_TIMEOUT};
    use bdm_config::Registry;
    use bdm_protocols::solana::SOLANA_MAINNET;
    use bdm_testkit::FakeJsonRpc;
    use std::time::Duration;

    const OWNER: &str = "Go5EVXxV3ob4CJVaq6YiGGUKqPmEfDo32kSmbWsYevsF";
    const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

    fn chain() -> ChainEntry {
        Registry::builtin()
            .unwrap()
            .chains
            .resolve("solana")
            .unwrap()
            .clone()
    }

    fn client(server: &FakeJsonRpc, vendor: &str) -> JsonRpcClient {
        JsonRpcClient::new(
            HttpClient::new(vendor, Duration::from_secs(2)),
            Redacted::new(format!("{}/?api-key=secret-key-123456", server.url())),
        )
    }

    #[tokio::test]
    async fn das_balances_and_metadata() {
        let server = FakeJsonRpc::start().await;
        // Shape from https://www.helius.dev/docs/api-reference/das/getassetsbyowner (hand-built).
        server.on(
            "getAssetsByOwner",
            json!({"total": 2, "limit": 1000, "page": 1, "nativeBalance": {"lamports": 1_500_000_000u64},
                "items": [
                  {"id": USDC, "interface": "FungibleToken",
                   "content": {"metadata": {"name": "USD Coin", "symbol": "USDC"}},
                   "token_info": {"balance": 25_000_000u64, "decimals": 6, "symbol": "USDC",
                                  "token_program": spl::TOKEN_PROGRAM,
                                  "associated_token_address": "5MjBG96YjNJWEL687DuGroThtGN5GPd9dFgQXg8o22TV"}},
                  {"id": "4SMRzaLXsuvTi2B5cSVs4LuCUUqQhB2uByTWUvbixjfF", "interface": "V1_NFT", "content": {}}]}),
        );
        server.on(
            "getAsset",
            json!({"id": USDC, "interface": "FungibleToken",
                   "content": {"metadata": {"name": "USD Coin", "symbol": "USDC"},
                               "links": {"image": "https://example.invalid/usdc.png"}},
                   "token_info": {"decimals": 6, "symbol": "USDC"}}),
        );
        let h = Helius::new(client(&server, "helius"), chain());
        let b = h.balances(&OWNER.parse().unwrap(), None).await.unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].amount.format_units(), "1.5");
        assert_eq!(b[1].amount.format_units(), "25");
        assert_eq!(b[1].symbol.as_deref(), Some("USDC"));
        assert_eq!(
            b[1].token_account.unwrap().to_string(),
            "5MjBG96YjNJWEL687DuGroThtGN5GPd9dFgQXg8o22TV"
        );
        let params = &server.calls()[0].1;
        assert_eq!(params["displayOptions"]["showFungible"], true);

        let asset: AssetId = format!("{SOLANA_MAINNET}/token:{USDC}").parse().unwrap();
        let info = h.metadata(&asset).await.unwrap();
        assert_eq!(
            (info.decimals, info.symbol.as_deref(), info.source.as_str()),
            (6, Some("USDC"), "helius")
        );
        // Paid history methods are never used.
        assert!(!server
            .calls()
            .iter()
            .any(|(m, _)| m == "getTransfersByAddress" || m == "getTransactionsForAddress"));
    }

    #[tokio::test]
    async fn malformed_das_answers_are_errors_never_panics() {
        let server = FakeJsonRpc::start().await;
        server.on("getAssetsByOwner", json!({"total": 0, "items": {}}));
        server.on(
            "getAsset",
            json!({"id": USDC, "token_info": {"decimals": "6", "symbol": "USDC"}}),
        );
        let h = Helius::new(client(&server, "helius"), chain());
        assert!(matches!(
            h.balances(&OWNER.parse().unwrap(), None).await,
            Err(ProviderError::Transient(_))
        ));
        let asset: AssetId = format!("{SOLANA_MAINNET}/token:{USDC}").parse().unwrap();
        assert!(matches!(
            h.metadata(&asset).await,
            Err(ProviderError::Unsupported(_))
        ));
    }

    #[tokio::test]
    async fn priority_fee_levels_to_tiers() {
        let server = FakeJsonRpc::start().await;
        // Example response from the getPriorityFeeEstimate API reference.
        server.on(
            "getPriorityFeeEstimate",
            json!({"priorityFeeLevels": {"min": 0, "low": 2, "medium": 10082, "high": 100000.0,
                                         "veryHigh": 1000000, "unsafeMax": 50000000}}),
        );
        let e = Helius::new(client(&server, "helius"), chain())
            .fee_estimate()
            .await
            .unwrap();
        let p: Vec<_> = e
            .tiers
            .iter()
            .map(|t| t.compute_unit_price_micro_lamports.unwrap())
            .collect();
        assert_eq!(p, [2, 10082, 100000]);
        assert!(e.tip.is_some());
    }

    /// Signed legacy tx: [payer, tip account, system, compute budget]; optional price ix.
    fn signed_tx(tip_to: &str, tip: u64, with_price: bool) -> String {
        use base64::Engine;
        let mut m = vec![1u8, 0, 2, 4];
        m.extend([7u8; 32]);
        m.extend(bs58::decode(tip_to).into_vec().unwrap());
        m.extend([0u8; 32]);
        m.extend(bs58::decode(tx::COMPUTE_BUDGET_PROGRAM).into_vec().unwrap());
        m.extend([9u8; 32]);
        m.push(if with_price { 2 } else { 1 });
        if with_price {
            m.extend([3u8, 0, 9, 3]);
            m.extend(100_000u64.to_le_bytes());
        }
        m.extend([2u8, 2, 0, 1, 12, 2, 0, 0, 0]);
        m.extend(tip.to_le_bytes());
        let mut t = vec![1u8];
        t.extend([5u8; 64]);
        t.extend(m);
        base64::engine::general_purpose::STANDARD.encode(t)
    }

    #[tokio::test]
    async fn sender_requires_tip_and_compute_price() {
        let server = FakeJsonRpc::start().await;
        server.on_fn("sendTransaction", |p| {
            assert_eq!(p[1]["skipPreflight"], true);
            assert_eq!(p[1]["maxRetries"], 0);
            Ok(json!(bs58::encode([5u8; 64]).into_string()))
        });
        let s = HeliusSender::new(client(&server, "helius_sender"));
        let good = signed_tx(SENDER_TIP_ACCOUNTS[0], 1_000_000, true);
        let r = s.send_raw(&good).await.unwrap();
        assert_eq!(r.tx_hash, bs58::encode([5u8; 64]).into_string());
        for bad in [
            signed_tx(SENDER_TIP_ACCOUNTS[0], 999_999, true),
            signed_tx(SENDER_TIP_ACCOUNTS[0], 1_000_000, false),
            signed_tx(
                "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
                1_000_000,
                true,
            ),
        ] {
            assert!(matches!(
                s.send_raw(&bad).await,
                Err(ProviderError::Unsupported(_))
            ));
        }
        assert!(matches!(
            s.send_raw("garbage").await,
            Err(ProviderError::Invalid(_))
        ));
        assert_eq!(server.calls().len(), 1, "invalid txs must not be sent");
    }

    #[test]
    fn registers_only_when_active() {
        use bdm_config::{ConfigDir, ConfigLoader, EnvSource};
        let load = |env: &[(&str, &str)]| {
            ConfigLoader::new(
                ConfigDir::new("/nonexistent"),
                EnvSource::from_pairs(env.iter().copied()),
            )
            .unwrap()
            .load_texts("", "")
            .unwrap()
        };
        let mut out = Vec::new();
        register(&load(&[]), &mut out);
        let ids: Vec<_> = out.iter().map(|r| r.vendor.id.as_str()).collect();
        assert_eq!(ids, ["helius_sender"]); // keyless relay only
        out.clear();
        register(&load(&[("HELIUS_API_KEY", "hel_key_123456")]), &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].ports.len(), 3);
    }

    #[tokio::test]
    #[ignore = "live: needs HELIUS_API_KEY"]
    async fn live_priority_fee() {
        let key = std::env::var("HELIUS_API_KEY").unwrap();
        let rpc = JsonRpcClient::new(
            HttpClient::new("helius", DEFAULT_TIMEOUT),
            Redacted::new(format!("https://mainnet.helius-rpc.com/?api-key={key}")),
        );
        let e = Helius::new(rpc, chain()).fee_estimate().await.unwrap();
        assert_eq!(e.tiers.len(), 3);
    }
}
