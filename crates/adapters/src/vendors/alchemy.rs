//! `alchemy` vendor adapter. Owner: `evm` (T1.E4).
//!
//! Enhanced JSON-RPC on the vendor's per-chain RPC URL (`loaded.rpc_url("alchemy", chain)`); the
//! base `evm_rpc`/`broadcast` ports come from `factory::base_registrations`.
//! - `transfer_history`: `alchemy_getAssetTransfers`
//!   (https://www.alchemy.com/docs/reference/alchemy-getassettransfers)
//! - `token_balances`: `eth_getBalance` + `alchemy_getTokenBalances` + `alchemy_getTokenMetadata`
//!   (https://www.alchemy.com/docs/reference/alchemy-gettokenbalances)
//!
//! No `QuotaReporter`: no credit-free usage endpoint is confirmed.

use super::util;

use crate::jsonrpc::{array_field, JsonRpcClient};
use alloy_primitives::{Address, U256};
use async_trait::async_trait;
use bdm_config::{ChainEntry, Loaded, VendorStatus};
use bdm_domain::{
    AccountAddress, Amount, AssetId, AssetRef, BlockRef, ChainFamily, Transfer, TransferKind,
};
use bdm_ports::{
    Direction, Page, PortHandle, PortResult, ProviderError, Registration, TokenBalance,
    TokenBalances, TransferHistory, TransferQuery,
};
use serde_json::{json, Map, Value};
use std::sync::Arc;

const VENDOR: &str = "alchemy";
/// Chains where the `internal` category is supported (Alchemy docs: Ethereum, Polygon, Base).
const INTERNAL_CHAINS: [u64; 3] = [1, 137, 8453];
/// Max `maxCount` for `alchemy_getAssetTransfers` (docs default 0x3e8).
const MAX_PAGE: u32 = 1000;

/// Push this vendor's registration if it is active (`loaded.vendor_status("alchemy")`).
pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status(VENDOR) != VendorStatus::Active {
        return;
    }
    let http = util::http(loaded, VENDOR);
    let mut reg = Registration::new(loaded.vendor_meta(VENDOR));
    for chain in loaded
        .registry
        .chains
        .enabled()
        .filter(|c| c.family == ChainFamily::Evm)
    {
        let Some(url) = loaded.rpc_url(VENDOR, &chain.id) else {
            continue;
        };
        let api = Arc::new(Alchemy {
            rpc: JsonRpcClient::new(http.clone(), url),
            chain: chain.clone(),
        });
        reg = reg
            .chain_port(chain.id.clone(), PortHandle::TokenBalances(api.clone()))
            .chain_port(chain.id.clone(), PortHandle::TransferHistory(api));
    }
    if !reg.ports.is_empty() {
        out.push(reg);
    }
}

struct Alchemy {
    rpc: JsonRpcClient,
    chain: ChainEntry,
}

impl Alchemy {
    #[cfg(test)]
    fn new(url: String, chain: ChainEntry) -> Self {
        Self {
            rpc: JsonRpcClient::new(
                crate::http::HttpClient::new(VENDOR, crate::http::DEFAULT_TIMEOUT),
                bdm_config::Redacted::new(url),
            ),
            chain,
        }
    }

    fn native(&self) -> AssetId {
        AssetId::native(self.chain.id.clone(), self.chain.native.slip44)
    }

    fn erc20(&self, a: Address) -> AssetId {
        AssetId {
            chain: self.chain.id.clone(),
            asset: AssetRef::Erc20(a),
        }
    }

    /// Enhanced APIs aren't on every Alchemy network; an "unsupported" answer must let routing
    /// move on instead of failing the whole request as invalid.
    async fn enhanced(&self, method: &str, params: Value) -> PortResult<Value> {
        self.rpc.request(method, params).await.map_err(unsupported)
    }
}

fn unsupported(e: ProviderError) -> ProviderError {
    match e {
        ProviderError::Invalid(m) if m.to_lowercase().contains("support") => {
            ProviderError::Unsupported(m)
        }
        e => e,
    }
}

fn hex_u256(v: &Value) -> Option<U256> {
    let s = v.as_str()?.trim_start_matches("0x");
    U256::from_str_radix(if s.is_empty() { "0" } else { s }, 16).ok()
}

fn hex_u64(v: &Value) -> Option<u64> {
    u64::from_str_radix(v.as_str()?.trim_start_matches("0x"), 16).ok()
}

/// Which asset classes a query wants, from its optional asset list.
struct Wanted {
    native: bool,
    /// `None` = every token.
    tokens: Option<Vec<Address>>,
}

fn wanted(assets: Option<&[AssetId]>) -> Wanted {
    match assets {
        None => Wanted {
            native: true,
            tokens: None,
        },
        Some(list) => Wanted {
            native: list.iter().any(AssetId::is_native),
            tokens: Some(
                list.iter()
                    .filter_map(|a| match a.asset {
                        AssetRef::Erc20(t) => Some(t),
                        _ => None,
                    })
                    .collect(),
            ),
        },
    }
}

#[async_trait]
impl TokenBalances for Alchemy {
    /// Native balance + ERC-20 balances (one page, ≤100 tokens), decimals/symbol from
    /// `alchemy_getTokenMetadata` in one batch. Zero balances are dropped unless asked for.
    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn balances(
        &self,
        owner: &AccountAddress,
        assets: Option<&[AssetId]>,
    ) -> PortResult<Vec<TokenBalance>> {
        let who = bdm_protocols::evm::evm_owner(owner)?;
        let want = wanted(assets);
        let mut out = Vec::new();
        if want.native {
            let v = self
                .rpc
                .request("eth_getBalance", json!([who, "latest"]))
                .await?;
            let raw = hex_u256(&v)
                .ok_or_else(|| ProviderError::Transient("malformed eth_getBalance".into()))?;
            out.push(TokenBalance {
                asset: self.native(),
                amount: Amount::new(raw, self.chain.native.decimals),
                symbol: Some(self.chain.native.symbol.clone()),
                token_account: None,
            });
        }
        let spec = match &want.tokens {
            None => json!("erc20"),
            Some(t) if t.is_empty() => return Ok(out),
            Some(t) => json!(t),
        };
        let res = self
            .enhanced("alchemy_getTokenBalances", json!([who, spec]))
            .await?;
        let held: Vec<(Address, U256)> =
            array_field(&res, "tokenBalances", "alchemy_getTokenBalances")?
                .iter()
                .filter(|b| b["error"].is_null())
                .filter_map(|b| {
                    Some((
                        b["contractAddress"].as_str()?.parse().ok()?,
                        hex_u256(&b["tokenBalance"])?,
                    ))
                })
                .filter(|(_, raw)| want.tokens.is_some() || !raw.is_zero())
                .collect();
        if held.is_empty() {
            return Ok(out);
        }
        let calls: Vec<(&str, Value)> = held
            .iter()
            .map(|(t, _)| ("alchemy_getTokenMetadata", json!([t])))
            .collect();
        let meta = self.rpc.batch(&calls).await?;
        for ((token, raw), m) in held.into_iter().zip(meta) {
            // Without decimals there is no exact amount: skip rather than guess.
            let Ok(m) = m else { continue };
            let Some(d) = m["decimals"].as_u64().and_then(|d| u8::try_from(d).ok()) else {
                continue;
            };
            out.push(TokenBalance {
                asset: self.erc20(token),
                amount: Amount::new(raw, d),
                symbol: m["symbol"].as_str().map(str::to_owned),
                token_account: None,
            });
        }
        Ok(out)
    }
}

#[async_trait]
impl TransferHistory for Alchemy {
    /// Native (`external`, plus `internal` where supported) and ERC-20 transfers, newest first.
    /// Alchemy filters by one side at a time, so `Both` runs an in- and an out-query; the cursor
    /// carries both page keys.
    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn transfers(&self, q: &TransferQuery) -> PortResult<Page<Transfer>> {
        let who = bdm_protocols::evm::evm_owner(&q.owner)?;
        let want = wanted(q.assets.as_deref());
        let mut categories = Vec::new();
        if want.native {
            categories.push("external");
            if INTERNAL_CHAINS.contains(&self.chain.id.evm_chain_id().unwrap_or(0)) {
                categories.push("internal");
            }
        }
        if want.tokens.as_ref().is_none_or(|t| !t.is_empty()) {
            categories.push("erc20");
        }
        if categories.is_empty() {
            return Ok(Page {
                items: Vec::new(),
                next_cursor: None,
            });
        }
        let mut base = Map::new();
        base.insert("category".into(), json!(categories));
        base.insert("withMetadata".into(), json!(false));
        base.insert("excludeZeroValue".into(), json!(true));
        base.insert(
            "maxCount".into(),
            json!(format!("0x{:x}", q.limit.clamp(1, MAX_PAGE))),
        );
        base.insert("order".into(), json!("desc"));
        if let Some(b) = q.from_block {
            base.insert("fromBlock".into(), json!(format!("0x{b:x}")));
        }
        if let Some(b) = q.to_block {
            base.insert("toBlock".into(), json!(format!("0x{b:x}")));
        }
        // A contract filter would drop native rows, so only use it for token-only queries.
        if let (false, Some(t)) = (want.native, &want.tokens) {
            base.insert("contractAddresses".into(), json!(t));
        }

        let cursor: Map<String, Value> = match &q.cursor {
            None => Map::new(),
            Some(c) => serde_json::from_str(c).map_err(|_| {
                ProviderError::Invalid(format!("cursor '{c}' is not from this source"))
            })?,
        };
        let sides: Vec<(&str, &str)> = match q.direction {
            Direction::In => vec![("in", "toAddress")],
            Direction::Out => vec![("out", "fromAddress")],
            Direction::Both => vec![("in", "toAddress"), ("out", "fromAddress")],
        }
        .into_iter()
        // On a follow-up page, only the sides that still have a page key continue.
        .filter(|(side, _)| q.cursor.is_none() || cursor.contains_key(*side))
        .collect();

        let mut items = Vec::new();
        let mut next = Map::new();
        for (side, field) in sides {
            let mut params = base.clone();
            params.insert(field.into(), json!(who));
            if let Some(k) = cursor.get(side) {
                params.insert("pageKey".into(), k.clone());
            }
            let res = self
                .enhanced("alchemy_getAssetTransfers", json!([params]))
                .await?;
            items.extend(
                res["transfers"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|t| self.parse(t)),
            );
            if let Some(k) = res["pageKey"].as_str() {
                next.insert(side.into(), json!(k));
            }
        }
        if let Some(assets) = &q.assets {
            items.retain(|t| assets.contains(&t.asset));
        }
        items.sort_by(|a, b| {
            let key = |t: &Transfer| (t.block.as_ref().map(|b| b.number), t.log_index);
            key(b).cmp(&key(a))
        });
        // A self-transfer shows up on both sides.
        items.dedup_by(|a, b| {
            a.tx_hash == b.tx_hash && a.log_index == b.log_index && a.asset == b.asset
        });
        Ok(Page {
            items,
            next_cursor: (!next.is_empty()).then(|| Value::Object(next).to_string()),
        })
    }
}

impl Alchemy {
    /// One `transfers[]` row. Amounts come from `rawContract` (hex integers), never the float
    /// `value` field. Rows without decimals (or a recipient) are skipped.
    fn parse(&self, t: &Value) -> Option<Transfer> {
        let kind = match t["category"].as_str()? {
            "external" => TransferKind::Native,
            "internal" => TransferKind::Internal,
            "erc20" => TransferKind::Token,
            _ => return None,
        };
        let raw = &t["rawContract"];
        let (asset, decimals) = if kind == TransferKind::Token {
            let d = hex_u64(&raw["decimal"]).and_then(|d| u8::try_from(d).ok())?;
            (self.erc20(raw["address"].as_str()?.parse().ok()?), d)
        } else {
            (self.native(), self.chain.native.decimals)
        };
        let addr = |v: &Value| v.as_str()?.parse().ok().map(AccountAddress::Evm);
        // uniqueId is `{hash}:log:{logIndex}` for token transfers, `{hash}:{category}` otherwise.
        let log_index = t["uniqueId"]
            .as_str()
            .and_then(|u| u.rsplit_once(":log:"))
            .and_then(|(_, i)| i.parse().ok());
        Some(Transfer {
            chain: self.chain.id.clone(),
            tx_hash: t["hash"].as_str()?.to_owned(),
            log_index,
            kind,
            asset,
            from: addr(&t["from"]),
            to: addr(&t["to"])?,
            amount: Amount::new(hex_u256(&raw["value"])?, decimals),
            block: hex_u64(&t["blockNum"]).map(|number| BlockRef {
                number,
                hash: None,
                timestamp: None,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bdm_config::{ConfigDir, ConfigLoader, EnvSource, Registry};
    use bdm_ports::Capability;
    use bdm_testkit::{vendor_fixture, FakeJsonRpc};

    fn chain(alias: &str) -> ChainEntry {
        Registry::builtin()
            .unwrap()
            .chains
            .resolve(alias)
            .unwrap()
            .clone()
    }

    fn fixture(case: &str) -> Value {
        vendor_fixture(env!("CARGO_MANIFEST_DIR"), VENDOR, case)["response"].clone()
    }

    fn owner() -> AccountAddress {
        "0xd8da6bf26964af9d7eed9e03e53415d37aa96045"
            .parse()
            .unwrap()
    }

    fn query(direction: Direction) -> TransferQuery {
        TransferQuery {
            owner: owner(),
            direction,
            assets: None,
            from_block: None,
            to_block: None,
            cursor: None,
            limit: 100,
        }
    }

    #[test]
    fn registers_every_evm_chain_incl_robinhood_only_with_key() {
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
        assert!(out.is_empty(), "no key → not registered");

        let loaded = load(&[("ALCHEMY_API_KEY", "alc_key_123456")]);
        register(&loaded, &mut out);
        let chains: Vec<String> = out[0]
            .ports
            .iter()
            .filter(|(_, h)| h.capability() == Capability::TransferHistory)
            .map(|(c, _)| c.as_ref().unwrap().to_string())
            .collect();
        assert_eq!(chains.len(), 8);
        assert!(chains.contains(&"eip155:4663".to_string()));
        assert_eq!(
            loaded
                .rpc_url(VENDOR, &"eip155:4663".parse().unwrap())
                .unwrap()
                .expose(),
            "https://robinhood-mainnet.g.alchemy.com/v2/alc_key_123456"
        );
    }

    #[tokio::test]
    async fn asset_transfers_both_sides_merged_with_cursor() {
        let server = FakeJsonRpc::start().await;
        let page = fixture("get_asset_transfers");
        server.on_fn("alchemy_getAssetTransfers", move |p| {
            // Incoming side has a next page; outgoing is empty.
            Ok(if p[0]["toAddress"].is_string() {
                page.clone()
            } else {
                json!({"transfers": []})
            })
        });
        let api = Alchemy::new(server.url(), chain("ethereum"));
        let page = api.transfers(&query(Direction::Both)).await.unwrap();

        assert_eq!(page.items.len(), 3, "ERC-721 row dropped");
        let usdc = &page.items[0];
        assert_eq!(usdc.kind, TransferKind::Token);
        assert_eq!(usdc.amount.format_units(), "250.5");
        assert_eq!(usdc.log_index, Some(47));
        assert_eq!(usdc.block.as_ref().unwrap().number, 0x1234567);
        assert!(page.items.iter().any(|t| t.kind == TransferKind::Internal));
        let eth = page
            .items
            .iter()
            .find(|t| t.kind == TransferKind::Native)
            .unwrap();
        assert_eq!(
            (eth.amount.decimals, eth.amount.format_units().as_str()),
            (18, "0.5")
        );
        assert_eq!(
            page.next_cursor.as_deref(),
            Some(r#"{"in":"next-page-key"}"#)
        );

        let calls = server.calls();
        assert_eq!(calls.len(), 2);
        let sent = &calls[0].1[0];
        assert_eq!(sent["category"], json!(["external", "internal", "erc20"]));
        assert!(sent.get("contractAddresses").is_none());

        // Follow-up page continues only the incoming side.
        let mut q = query(Direction::Both);
        q.cursor = page.next_cursor;
        api.transfers(&q).await.unwrap();
        let calls = server.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[2].1[0]["pageKey"], json!("next-page-key"));
    }

    #[tokio::test]
    async fn token_only_query_uses_contract_filter_and_unsupported_network_fails_over() {
        let server = FakeJsonRpc::start().await;
        server.on("alchemy_getAssetTransfers", json!({"transfers": []}));
        let api = Alchemy::new(server.url(), chain("bsc"));
        let token: AssetId = "eip155:56/erc20:0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        let mut q = query(Direction::In);
        q.assets = Some(vec![token]);
        api.transfers(&q).await.unwrap();
        let sent = &server.calls()[0].1[0];
        assert_eq!(sent["category"], json!(["erc20"]));
        assert_eq!(sent["contractAddresses"].as_array().unwrap().len(), 1);

        server.on_fn("alchemy_getAssetTransfers", |_| {
            Err((
                -32600,
                "alchemy_getAssetTransfers is not supported on this network".into(),
            ))
        });
        assert!(matches!(
            api.transfers(&q).await,
            Err(ProviderError::Unsupported(_))
        ));
    }

    #[tokio::test]
    async fn token_balances_with_metadata() {
        let server = FakeJsonRpc::start().await;
        server.on("eth_getBalance", json!("0xde0b6b3a7640000")); // 1 ETH
        server.on("alchemy_getTokenBalances", fixture("get_token_balances"));
        let meta = fixture("get_token_metadata");
        server.on_fn("alchemy_getTokenMetadata", move |p| {
            Ok(meta[p[0].as_str().unwrap().to_lowercase()].clone())
        });
        let api = Alchemy::new(server.url(), chain("ethereum"));
        let b = api.balances(&owner(), None).await.unwrap();
        // native + USDC; the zero balance and the token without decimals are dropped.
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].amount.format_units(), "1");
        assert_eq!(b[1].symbol.as_deref(), Some("USDC"));
        assert_eq!(b[1].amount.format_units(), "12.345678");
    }

    #[tokio::test]
    async fn malformed_token_balances_fail_over_and_bad_decimals_skip() {
        let server = FakeJsonRpc::start().await;
        server.on("eth_getBalance", json!("0x0"));
        server.on("alchemy_getTokenBalances", json!({"tokenBalances": "nope"}));
        let api = Alchemy::new(server.url(), chain("ethereum"));
        assert!(matches!(
            api.balances(&owner(), None).await,
            Err(ProviderError::Transient(_))
        ));

        server.on(
            "alchemy_getTokenBalances",
            json!({"tokenBalances": [{"contractAddress": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
                                       "tokenBalance": "0xbc614e"}]}),
        );
        server.on(
            "alchemy_getTokenMetadata",
            json!({"decimals": "18", "symbol": "USDC"}),
        );
        let b = api.balances(&owner(), None).await.unwrap();
        assert_eq!(b.len(), 1, "string decimals: token skipped, native kept");
        assert!(b[0].asset.is_native());
    }
}
