//! `coingecko` (Demo plan). Owner: `market-trading` (T1.M1).
//!
//! Key goes in the `x-cg-demo-api-key` header. Tokens use the on-chain endpoints (CAIP-19 →
//! GeckoTerminal network + contract), native coins use `/simple/price` with the coin id.
//! Ports: `PriceFeed`, `PriceHistory` (365 days on Demo), `TokenMetadata`.

use super::util;

use crate::http::HttpClient;
use async_trait::async_trait;
use bdm_config::{Loaded, Redacted, VendorStatus};
use bdm_domain::{AssetId, AssetRef, ChainId, Price};
use bdm_ports::{
    PortHandle, PortResult, PriceFeed, PriceHistory, ProviderError, Registration, TokenInfo,
    TokenMetadata,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use std::sync::Arc;

pub const ID: &str = "coingecko";
const BASE: &str = "https://api.coingecko.com/api/v3";
const KEY_HEADER: &str = "x-cg-demo-api-key";

pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status(ID) != VendorStatus::Active {
        return;
    }
    let Some(key) = loaded.key(ID, "api_key") else {
        return;
    };
    let a = Arc::new(CoinGecko::new(util::http(loaded, ID), BASE, key));
    out.push(
        Registration::new(loaded.vendor_meta(ID))
            .global_port(PortHandle::Price(a.clone()))
            .global_port(PortHandle::PriceHistory(a.clone()))
            .global_port(PortHandle::TokenMetadata(a)),
    );
}

pub struct CoinGecko {
    http: HttpClient,
    base: String,
    key: Redacted<String>,
}

/// Asset platform id used by `/coins/{platform}/contract/...`.
fn platform(chain: &ChainId) -> Option<&'static str> {
    if util::is_solana_mainnet(chain) {
        return Some("solana");
    }
    Some(match chain.evm_chain_id()? {
        1 => "ethereum",
        8453 => "base",
        42161 => "arbitrum-one",
        10 => "optimistic-ethereum",
        137 => "polygon-pos",
        43114 => "avalanche",
        56 => "binance-smart-chain",
        _ => return None,
    })
}

fn unsupported(asset: &AssetId) -> ProviderError {
    ProviderError::Unsupported(format!("coingecko does not cover {asset}"))
}

impl CoinGecko {
    pub fn new(http: HttpClient, base: &str, key: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            key: Redacted::new(key.to_owned()),
        }
    }

    async fn get(&self, path_and_query: &str, label: &str) -> PortResult<Value> {
        let url = Redacted::new(format!("{}{path_and_query}", self.base));
        self.http
            .get_json(&url, label, &[(KEY_HEADER, self.key.expose())])
            .await
    }

    async fn native_price(&self, asset: &AssetId, id: &str, cur: &str) -> PortResult<Price> {
        let v = self
            .get(
                &format!("/simple/price?ids={id}&vs_currencies={cur}&include_last_updated_at=true"),
                "/simple/price",
            )
            .await?;
        let row = v.get(id).ok_or(ProviderError::NotFound)?;
        Ok(Price {
            asset: asset.clone(),
            currency: cur.to_uppercase(),
            value: util::price(row.get(cur).unwrap_or(&Value::Null))?,
            as_of: row
                .get("last_updated_at")
                .and_then(Value::as_i64)
                .and_then(util::unix)
                .unwrap_or_else(Utc::now),
            source: ID.into(),
            liquidity_usd: None,
        })
    }

    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn token_price(&self, asset: &AssetId, net: &str, addr: &str) -> PortResult<Price> {
        let v = self
            .get(
                &format!(
                    "/onchain/simple/networks/{net}/token_price/{addr}?include_total_reserve_in_usd=true"
                ),
                "/onchain/simple/token_price",
            )
            .await?;
        util::gecko_token_price(asset, ID, &v, addr)
    }
}

#[async_trait]
impl PriceFeed for CoinGecko {
    async fn price(&self, asset: &AssetId, currency: &str) -> PortResult<Price> {
        let cur = currency.to_lowercase();
        match &asset.asset {
            AssetRef::Native { slip44 } => {
                let id = util::coingecko_native_id(*slip44).ok_or_else(|| unsupported(asset))?;
                self.native_price(asset, id, &cur).await
            }
            _ => {
                if cur != "usd" {
                    return Err(ProviderError::Unsupported(
                        "coingecko on-chain prices are USD only".into(),
                    ));
                }
                let net = util::gecko_network(&asset.chain).ok_or_else(|| unsupported(asset))?;
                let addr = util::token_address(asset).ok_or_else(|| unsupported(asset))?;
                self.token_price(asset, net, &addr).await
            }
        }
    }
}

#[async_trait]
impl PriceHistory for CoinGecko {
    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn price_at(
        &self,
        asset: &AssetId,
        currency: &str,
        at: DateTime<Utc>,
    ) -> PortResult<Price> {
        let cur = currency.to_lowercase();
        let path = match &asset.asset {
            AssetRef::Native { slip44 } => {
                format!(
                    "/coins/{}",
                    util::coingecko_native_id(*slip44).ok_or_else(|| unsupported(asset))?
                )
            }
            _ => format!(
                "/coins/{}/contract/{}",
                platform(&asset.chain).ok_or_else(|| unsupported(asset))?,
                util::token_address(asset).ok_or_else(|| unsupported(asset))?
            ),
        };
        let (from, to) = (
            (at - Duration::hours(1)).timestamp(),
            (at + Duration::hours(1)).timestamp(),
        );
        let v = self
            .get(
                &format!("{path}/market_chart/range?vs_currency={cur}&from={from}&to={to}"),
                "/market_chart/range",
            )
            .await?;
        closest_point(v["prices"].as_array(), at)
            .map(|(ts, value)| Price {
                asset: asset.clone(),
                currency: currency.to_uppercase(),
                value,
                as_of: ts,
                source: ID.into(),
                liquidity_usd: None,
            })
            .ok_or(ProviderError::NotFound)
    }
}

/// `[[ms, price], …]` → the positive point closest to `at`.
fn closest_point(
    points: Option<&Vec<Value>>,
    at: DateTime<Utc>,
) -> Option<(DateTime<Utc>, rust_decimal::Decimal)> {
    points?
        .iter()
        .filter_map(|p| {
            let ts = DateTime::from_timestamp_millis(p.get(0)?.as_i64()?)?;
            Some((ts, util::price(p.get(1)?).ok()?))
        })
        .min_by_key(|(ts, _)| (*ts - at).num_seconds().abs())
}

#[async_trait]
impl TokenMetadata for CoinGecko {
    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn metadata(&self, asset: &AssetId) -> PortResult<TokenInfo> {
        let net = util::gecko_network(&asset.chain).ok_or_else(|| unsupported(asset))?;
        let addr = util::token_address(asset).ok_or_else(|| unsupported(asset))?;
        let v = self
            .get(
                &format!("/onchain/networks/{net}/tokens/{addr}"),
                "/onchain/tokens",
            )
            .await?;
        util::gecko_token_info(asset, ID, &v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::DEFAULT_TIMEOUT;
    use bdm_testkit::wiremock::{
        matchers::{header, method, path, query_param},
        Mock, MockServer, ResponseTemplate,
    };
    use rust_decimal::Decimal;
    use serde_json::json;

    fn fixture(case: &str) -> Value {
        bdm_testkit::vendor_fixture(env!("CARGO_MANIFEST_DIR"), ID, case)
    }

    fn cg(server: &MockServer) -> CoinGecko {
        CoinGecko::new(
            HttpClient::new(ID, DEFAULT_TIMEOUT),
            &server.uri(),
            "CG-demo-key-123",
        )
    }

    const USDC_BASE: &str = "eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";

    #[tokio::test]
    async fn onchain_token_price_with_liquidity() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/onchain/simple/networks/base/token_price/0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
            ))
            .and(header(KEY_HEADER, "CG-demo-key-123"))
            .and(query_param("include_total_reserve_in_usd", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture("onchain_token_price")))
            .mount(&server)
            .await;
        let p = cg(&server)
            .price(&USDC_BASE.parse().unwrap(), "USD")
            .await
            .unwrap();
        assert_eq!(p.value, Decimal::new(9998, 4));
        assert_eq!(p.liquidity_usd, Some(Decimal::new(123_456_789, 1)));
        assert_eq!(p.source, "coingecko");
    }

    #[tokio::test]
    async fn native_price_and_missing_price_is_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/simple/price"))
            .and(query_param("ids", "ethereum"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture("simple_price")))
            .mount(&server)
            .await;
        let eth: AssetId = "eip155:1/slip44:60".parse().unwrap();
        let p = cg(&server).price(&eth, "USD").await.unwrap();
        assert_eq!(p.value, Decimal::new(261234, 2));
        assert_eq!(p.as_of.timestamp(), 1_790_000_000);

        let empty = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&empty)
            .await;
        assert_eq!(
            cg(&empty).price(&eth, "USD").await,
            Err(ProviderError::NotFound)
        );
        let rh: AssetId = "eip155:4663/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
            .parse()
            .unwrap();
        assert!(matches!(
            cg(&empty).price(&rh, "USD").await,
            Err(ProviderError::Unsupported(_))
        ));
    }

    #[tokio::test]
    async fn history_picks_closest_point() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/coins/ethereum/market_chart/range"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture("market_chart_range")))
            .mount(&server)
            .await;
        let eth: AssetId = "eip155:1/slip44:60".parse().unwrap();
        let at = DateTime::from_timestamp(1_790_000_290, 0).unwrap();
        let p = cg(&server).price_at(&eth, "usd", at).await.unwrap();
        assert_eq!(p.as_of.timestamp(), 1_790_000_300);
        assert_eq!(p.value, Decimal::new(261010, 2));
        assert_eq!(p.currency, "USD");
    }

    #[tokio::test]
    async fn token_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/onchain/networks/base/tokens/0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture("onchain_token")))
            .mount(&server)
            .await;
        let m = cg(&server)
            .metadata(&USDC_BASE.parse().unwrap())
            .await
            .unwrap();
        assert_eq!((m.decimals, m.symbol.as_deref()), (6, Some("USDC")));
    }

    #[tokio::test]
    async fn key_never_leaks_in_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad key CG-demo-key-123"))
            .mount(&server)
            .await;
        let c = CoinGecko::new(
            HttpClient::new(ID, DEFAULT_TIMEOUT).with_secrets(["CG-demo-key-123"]),
            &server.uri(),
            "CG-demo-key-123",
        );
        let err = c
            .price(&"eip155:1/slip44:60".parse().unwrap(), "USD")
            .await
            .unwrap_err();
        assert!(!format!("{err:?}").contains("CG-demo-key-123"), "{err:?}");
    }
}
