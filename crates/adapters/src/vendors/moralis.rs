//! `moralis` vendor adapter. Owner: `evm` (T1.E4).
//!
//! Web3 Data API v2.2 (REST, `X-API-Key` header):
//! - `token_balances`: `GET /wallets/{address}/tokens`
//!   (https://docs.moralis.com/web3-data-api/evm/reference/get-wallet-token-balances-price)
//! - `transfer_history`: `GET /{address}/erc20/transfers` (ERC-20 only; native history is left
//!   to the next vendor) (https://docs.moralis.com/web3-data-api/evm/reference/get-wallet-token-transfers)
//!
//! No `QuotaReporter`: no credit-free usage endpoint is confirmed.

use crate::http::{HttpClient, DEFAULT_TIMEOUT};
use alloy_primitives::{Address, U256};
use async_trait::async_trait;
use bdm_config::{ChainEntry, Loaded, Redacted, VendorStatus};
use bdm_domain::{AccountAddress, Amount, AssetId, AssetRef, BlockRef, Transfer, TransferKind};
use bdm_ports::{
    Direction, Page, PortHandle, PortResult, ProviderError, Registration, TokenBalance,
    TokenBalances, TransferHistory, TransferQuery, VendorMeta,
};
use serde_json::Value;
use std::sync::Arc;

const VENDOR: &str = "moralis";
/// Source: https://docs.moralis.com/web3-data-api/evm/reference (base URL).
const BASE_URL: &str = "https://deep-index.moralis.io/api/v2.2";
/// Chains Moralis supports among ours (https://docs.moralis.com/supported-chains). Robinhood
/// Chain (4663) is not listed.
const CHAINS: [u64; 7] = [1, 8453, 42161, 10, 137, 43114, 56];
const MAX_PAGE: u32 = 100;

/// Push this vendor's registration if it is active (`loaded.vendor_status("moralis")`).
pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status(VENDOR) != VendorStatus::Active {
        return;
    }
    let (Some(entry), Some(key)) = (
        loaded.registry.vendors.get(VENDOR),
        loaded.key(VENDOR, "api_key"),
    ) else {
        return;
    };
    let http = HttpClient::new(VENDOR, DEFAULT_TIMEOUT).with_secrets(loaded.secret_values());
    let mut reg = Registration::new(VendorMeta {
        id: VENDOR.into(),
        display_name: entry.display_name.clone(),
        requires_key: entry.requires_key,
        signup_url: entry.signup_url.clone(),
        rpc_features: Default::default(),
    });
    for chain in loaded.registry.chains.enabled() {
        if !chain
            .id
            .evm_chain_id()
            .is_some_and(|id| CHAINS.contains(&id))
        {
            continue;
        }
        let api = Arc::new(Moralis::new(
            http.clone(),
            BASE_URL.into(),
            Redacted::new(key.to_owned()),
            chain.clone(),
        ));
        reg = reg
            .chain_port(chain.id.clone(), PortHandle::TokenBalances(api.clone()))
            .chain_port(chain.id.clone(), PortHandle::TransferHistory(api));
    }
    if !reg.ports.is_empty() {
        out.push(reg);
    }
}

struct Moralis {
    http: HttpClient,
    base: String,
    key: Redacted<String>,
    chain: ChainEntry,
}

impl Moralis {
    fn new(http: HttpClient, base: String, key: Redacted<String>, chain: ChainEntry) -> Self {
        Self {
            http,
            base,
            key,
            chain,
        }
    }

    /// `GET {base}{path}?chain=0x…&{query}`; `label` is the metering / cost-table key.
    async fn get(&self, path: &str, query: &[(&str, String)], label: &str) -> PortResult<Value> {
        let chain = format!("0x{:x}", self.chain.id.evm_chain_id().unwrap_or(0));
        let qs: String = std::iter::once(("chain", chain))
            .chain(query.iter().map(|(k, v)| (*k, v.clone())))
            .map(|(k, v)| format!("{k}={}", encode(&v)))
            .collect::<Vec<_>>()
            .join("&");
        let url = Redacted::new(format!("{}{path}?{qs}", self.base));
        self.http
            .get_json(&url, label, &[("X-API-Key", self.key.expose())])
            .await
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
}

/// Percent-encode a query value (addresses, numbers and opaque cursors).
fn encode(v: &str) -> String {
    v.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn evm(owner: &AccountAddress) -> PortResult<Address> {
    match owner {
        AccountAddress::Evm(a) => Ok(*a),
        AccountAddress::Solana(_) => Err(ProviderError::Invalid("expected an EVM address".into())),
    }
}

fn dec_u256(v: &Value) -> Option<U256> {
    U256::from_str_radix(v.as_str()?, 10).ok()
}

/// Decimals arrive as a number (balances) or a string (transfers).
fn decimals(v: &Value) -> Option<u8> {
    v.as_u64()
        .or_else(|| v.as_str()?.parse().ok())
        .and_then(|d| u8::try_from(d).ok())
}

fn results(v: &Value) -> impl Iterator<Item = &Value> {
    v["result"].as_array().into_iter().flatten()
}

#[async_trait]
impl TokenBalances for Moralis {
    /// Native + ERC-20 balances (first page). Without an explicit asset list, rows Moralis
    /// flags as `possible_spam` are dropped.
    async fn balances(
        &self,
        owner: &AccountAddress,
        assets: Option<&[AssetId]>,
    ) -> PortResult<Vec<TokenBalance>> {
        let who = evm(owner)?;
        let v = self
            .get(&format!("/wallets/{who:#x}/tokens"), &[], "wallet_tokens")
            .await?;
        let out = results(&v)
            .filter(|r| assets.is_some() || r["possible_spam"].as_bool() != Some(true))
            .filter_map(|r| {
                let native = r["native_token"].as_bool() == Some(true);
                let asset = if native {
                    self.native()
                } else {
                    self.erc20(r["token_address"].as_str()?.parse().ok()?)
                };
                Some(TokenBalance {
                    asset,
                    amount: Amount::new(dec_u256(&r["balance"])?, decimals(&r["decimals"])?),
                    symbol: r["symbol"].as_str().map(str::to_owned),
                    token_account: None,
                })
            })
            .filter(|b| assets.is_none_or(|a| a.contains(&b.asset)))
            .collect();
        Ok(out)
    }
}

#[async_trait]
impl TransferHistory for Moralis {
    /// ERC-20 transfers, newest first; direction and asset filters are applied locally, so a
    /// page may hold fewer than `limit` rows. Native-only queries are `Unsupported`.
    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn transfers(&self, q: &TransferQuery) -> PortResult<Page<Transfer>> {
        let who = evm(&q.owner)?;
        if q.assets
            .as_ref()
            .is_some_and(|a| a.iter().all(AssetId::is_native))
        {
            return Err(ProviderError::Unsupported(
                "moralis adapter serves ERC-20 transfers only".into(),
            ));
        }
        let mut query = vec![
            ("limit", q.limit.clamp(1, MAX_PAGE).to_string()),
            ("order", "DESC".to_owned()),
        ];
        if let Some(b) = q.from_block {
            query.push(("from_block", b.to_string()));
        }
        if let Some(b) = q.to_block {
            query.push(("to_block", b.to_string()));
        }
        if let Some(c) = &q.cursor {
            query.push(("cursor", c.clone()));
        }
        let v = self
            .get(
                &format!("/{who:#x}/erc20/transfers"),
                &query,
                "erc20_transfers",
            )
            .await?;
        let me = AccountAddress::Evm(who);
        let items = results(&v)
            .filter(|r| q.assets.is_some() || r["possible_spam"].as_bool() != Some(true))
            .filter_map(|r| self.parse(r))
            .filter(|t| match q.direction {
                Direction::In => t.to == me,
                Direction::Out => t.from == Some(me),
                Direction::Both => true,
            })
            .filter(|t| q.assets.as_ref().is_none_or(|a| a.contains(&t.asset)))
            .collect();
        Ok(Page {
            items,
            next_cursor: v["cursor"]
                .as_str()
                .filter(|c| !c.is_empty())
                .map(str::to_owned),
        })
    }
}

impl Moralis {
    fn parse(&self, r: &Value) -> Option<Transfer> {
        let addr = |k: &str| r[k].as_str()?.parse().ok().map(AccountAddress::Evm);
        Some(Transfer {
            chain: self.chain.id.clone(),
            tx_hash: r["transaction_hash"].as_str()?.to_owned(),
            log_index: r["log_index"].as_u64(),
            kind: TransferKind::Token,
            asset: self.erc20(r["token_address"].as_str()?.parse().ok()?),
            from: addr("from_address"),
            to: addr("to_address")?,
            amount: Amount::new(dec_u256(&r["value"])?, decimals(&r["token_decimals"])?),
            block: r["block_number"]
                .as_str()
                .and_then(|n| n.parse().ok())
                .map(|number| BlockRef {
                    number,
                    hash: r["block_hash"].as_str().map(str::to_owned),
                    timestamp: None,
                }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bdm_config::Registry;
    use bdm_testkit::{
        vendor_fixture,
        wiremock::{
            matchers::{header, method, path, query_param},
            Mock, MockServer, ResponseTemplate,
        },
    };

    const OWNER: &str = "0xd8da6bf26964af9d7eed9e03e53415d37aa96045";

    fn api(server: &MockServer, alias: &str) -> Moralis {
        let chain = Registry::builtin()
            .unwrap()
            .chains
            .resolve(alias)
            .unwrap()
            .clone();
        Moralis::new(
            HttpClient::new(VENDOR, DEFAULT_TIMEOUT),
            server.uri(),
            Redacted::new("test-moralis-key".into()),
            chain,
        )
    }

    fn fixture(case: &str) -> Value {
        vendor_fixture(env!("CARGO_MANIFEST_DIR"), VENDOR, case)["response"].clone()
    }

    #[tokio::test]
    async fn wallet_tokens_with_key_header_and_spam_filter() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/wallets/{OWNER}/tokens")))
            .and(query_param("chain", "0x2105"))
            .and(header("X-API-Key", "test-moralis-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture("wallet_tokens")))
            .mount(&server)
            .await;
        let owner: AccountAddress = OWNER.parse().unwrap();
        let b = api(&server, "base").balances(&owner, None).await.unwrap();
        assert_eq!(b.len(), 2, "spam row dropped");
        assert!(b[0].asset.is_native());
        assert_eq!(b[0].amount.format_units(), "0.25");
        assert_eq!(
            (b[1].symbol.as_deref(), b[1].amount.format_units().as_str()),
            (Some("USDC"), "1000.5")
        );
    }

    #[tokio::test]
    async fn erc20_transfers_direction_filter_and_cursor() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/{OWNER}/erc20/transfers")))
            .and(query_param("chain", "0x38"))
            .and(query_param("cursor", "abc+/="))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture("erc20_transfers")))
            .mount(&server)
            .await;
        let q = TransferQuery {
            owner: OWNER.parse().unwrap(),
            direction: Direction::In,
            assets: None,
            from_block: None,
            to_block: None,
            cursor: Some("abc+/=".into()),
            limit: 100,
        };
        let page = api(&server, "bsc").transfers(&q).await.unwrap();
        // incoming only; the outgoing row and the spam row are filtered out.
        assert_eq!(page.items.len(), 1);
        let t = &page.items[0];
        // BSC stablecoin: 18 decimals.
        assert_eq!(
            (t.amount.decimals, t.amount.format_units().as_str()),
            (18, "42")
        );
        assert_eq!(t.log_index, Some(12));
        assert_eq!(t.block.as_ref().unwrap().number, 41_000_000);
        assert_eq!(page.next_cursor.as_deref(), Some("next-cursor"));

        let native_only = TransferQuery {
            assets: Some(vec!["eip155:56/slip44:714".parse().unwrap()]),
            ..q
        };
        assert!(matches!(
            api(&server, "bsc").transfers(&native_only).await,
            Err(ProviderError::Unsupported(_))
        ));
    }

    #[tokio::test]
    async fn http_errors_map_to_routing_taxonomy() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "3"))
            .mount(&server)
            .await;
        let owner: AccountAddress = OWNER.parse().unwrap();
        assert!(matches!(
            api(&server, "ethereum").balances(&owner, None).await,
            Err(ProviderError::RateLimited { .. })
        ));
    }

    #[test]
    fn registers_supported_chains_only() {
        let loaded = bdm_config::ConfigLoader::new(
            bdm_config::ConfigDir::new("/nonexistent"),
            bdm_config::EnvSource::from_pairs([("MORALIS_API_KEY", "mor_key_123456")]),
        )
        .unwrap()
        .load_texts("", "")
        .unwrap();
        let mut out = Vec::new();
        register(&loaded, &mut out);
        let chains: std::collections::BTreeSet<String> = out[0]
            .ports
            .iter()
            .map(|(c, _)| c.as_ref().unwrap().to_string())
            .collect();
        assert_eq!(chains.len(), 7);
        assert!(!chains.contains("eip155:4663"));
    }
}
