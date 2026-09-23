//! `geckoterminal` public API (keyless, ~10–30 req/min ⚠). Owner: `market-trading` (T1.M1).
//! Ports: `PriceFeed` (USD, tokens only, with pool reserve as liquidity), `TokenMetadata`.

use super::market_util as util;

use crate::http::HttpClient;
use async_trait::async_trait;
use chrono::Utc;
use ems_config::{Loaded, Redacted, VendorStatus};
use ems_domain::{AssetId, ChainId, Price};
use ems_ports::{
    PortHandle, PortResult, PriceFeed, ProviderError, Registration, TokenInfo, TokenMetadata,
};
use serde_json::Value;
use std::sync::Arc;

pub const ID: &str = "geckoterminal";
const BASE: &str = "https://api.geckoterminal.com/api/v2";
const ACCEPT: (&str, &str) = ("accept", "application/json;version=20230302");

pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status(ID) != VendorStatus::Active {
        return;
    }
    let a = Arc::new(GeckoTerminal::new(util::http(loaded, ID), BASE));
    out.push(
        Registration::new(util::meta(loaded, ID))
            .global_port(PortHandle::Price(a.clone()))
            .global_port(PortHandle::TokenMetadata(a)),
    );
}

pub struct GeckoTerminal {
    http: HttpClient,
    base: String,
}

fn network(chain: &ChainId) -> Option<&'static str> {
    if util::is_solana_mainnet(chain) {
        return Some("solana");
    }
    Some(match chain.evm_chain_id()? {
        1 => "eth",
        8453 => "base",
        42161 => "arbitrum",
        10 => "optimism",
        137 => "polygon_pos",
        43114 => "avax",
        56 => "bsc",
        _ => return None,
    })
}

fn target(asset: &AssetId) -> PortResult<(&'static str, String)> {
    let unsupported =
        || ProviderError::Unsupported(format!("geckoterminal does not cover {asset}"));
    Ok((
        network(&asset.chain).ok_or_else(unsupported)?,
        util::token_address(asset).ok_or_else(unsupported)?,
    ))
}

fn get_ci<'a>(obj: &'a Value, key: &str) -> Option<&'a Value> {
    obj.as_object()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
}

impl GeckoTerminal {
    pub fn new(http: HttpClient, base: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
        }
    }

    async fn get(&self, path: &str, label: &str) -> PortResult<Value> {
        let url = Redacted::new(format!("{}{path}", self.base));
        self.http.get_json(&url, label, &[ACCEPT]).await
    }
}

#[async_trait]
impl PriceFeed for GeckoTerminal {
    async fn price(&self, asset: &AssetId, currency: &str) -> PortResult<Price> {
        if !currency.eq_ignore_ascii_case("usd") {
            return Err(ProviderError::Unsupported(
                "geckoterminal is USD only".into(),
            ));
        }
        let (net, addr) = target(asset)?;
        let v = self
            .get(
                &format!(
                    "/simple/networks/{net}/token_price/{addr}?include_total_reserve_in_usd=true"
                ),
                "/simple/token_price",
            )
            .await?;
        let attrs = &v["data"]["attributes"];
        Ok(Price {
            asset: asset.clone(),
            currency: "USD".into(),
            value: util::price(get_ci(&attrs["token_prices"], &addr).unwrap_or(&Value::Null))?,
            as_of: Utc::now(),
            source: ID.into(),
            liquidity_usd: get_ci(&attrs["total_reserve_in_usd"], &addr).and_then(util::dec),
        })
    }
}

#[async_trait]
impl TokenMetadata for GeckoTerminal {
    async fn metadata(&self, asset: &AssetId) -> PortResult<TokenInfo> {
        let (net, addr) = target(asset)?;
        let v = self
            .get(&format!("/networks/{net}/tokens/{addr}"), "/tokens")
            .await?;
        let a = &v["data"]["attributes"];
        let s = |k: &str| a.get(k).and_then(Value::as_str).map(str::to_owned);
        Ok(TokenInfo {
            asset: asset.clone(),
            decimals: a
                .get("decimals")
                .and_then(Value::as_u64)
                .and_then(|d| u8::try_from(d).ok())
                .ok_or(ProviderError::NotFound)?,
            symbol: s("symbol"),
            name: s("name"),
            logo_url: s("image_url").filter(|u| u.starts_with("https://")),
            verified: None,
            source: ID.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::DEFAULT_TIMEOUT;
    use ems_testkit::wiremock::{
        matchers::{header, method, path},
        Mock, MockServer, ResponseTemplate,
    };
    use rust_decimal::Decimal;

    #[tokio::test]
    async fn solana_token_price() {
        let server = MockServer::start().await;
        let mint = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
        Mock::given(method("GET"))
            .and(path(format!("/simple/networks/solana/token_price/{mint}")))
            .and(header("accept", ACCEPT.1))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(ems_testkit::vendor_fixture(
                    env!("CARGO_MANIFEST_DIR"),
                    ID,
                    "token_price_solana",
                )),
            )
            .mount(&server)
            .await;
        let g = GeckoTerminal::new(HttpClient::new(ID, DEFAULT_TIMEOUT), &server.uri());
        let asset: AssetId = format!("{}/token:{mint}", util::SOLANA_MAINNET)
            .parse()
            .unwrap();
        let p = g.price(&asset, "usd").await.unwrap();
        assert_eq!(p.value, Decimal::new(10_002, 4));
        assert_eq!(p.liquidity_usd, Some(Decimal::new(55_000_000, 0)));
        let eth: AssetId = "eip155:1/slip44:60".parse().unwrap();
        assert!(matches!(
            g.price(&eth, "usd").await,
            Err(ProviderError::Unsupported(_))
        ));
    }
}
