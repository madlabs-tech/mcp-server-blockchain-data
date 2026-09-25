//! `geckoterminal` public API (keyless, ~10–30 req/min ⚠).
//! Ports: `PriceFeed` (USD, tokens only, with pool reserve as liquidity), `TokenMetadata`.

use super::util;

use crate::http::HttpClient;
use async_trait::async_trait;
use bdm_config::{Loaded, Redacted, VendorStatus};
use bdm_domain::{AssetId, Price};
use bdm_ports::{
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
        Registration::new(loaded.vendor_meta(ID))
            .global_port(PortHandle::Price(a.clone()))
            .global_port(PortHandle::TokenMetadata(a)),
    );
}

pub struct GeckoTerminal {
    http: HttpClient,
    base: String,
}

fn target(asset: &AssetId) -> PortResult<(&'static str, String)> {
    let unsupported =
        || ProviderError::Unsupported(format!("geckoterminal does not cover {asset}"));
    Ok((
        util::gecko_network(&asset.chain).ok_or_else(unsupported)?,
        util::token_address(asset).ok_or_else(unsupported)?,
    ))
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
    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
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
        util::gecko_token_price(asset, ID, &v, &addr)
    }
}

#[async_trait]
impl TokenMetadata for GeckoTerminal {
    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn metadata(&self, asset: &AssetId) -> PortResult<TokenInfo> {
        let (net, addr) = target(asset)?;
        let v = self
            .get(&format!("/networks/{net}/tokens/{addr}"), "/tokens")
            .await?;
        util::gecko_token_info(asset, ID, &v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::DEFAULT_TIMEOUT;
    use bdm_testkit::wiremock::{
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
                ResponseTemplate::new(200).set_body_json(bdm_testkit::vendor_fixture(
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
