//! `defillama` coins API (`coins.llama.fi`, keyless, non-Pro endpoints only).
//! Owner: `market-trading` (T1.M1). Ports: `PriceFeed`, `PriceHistory` (USD).

use super::market_util as util;

use crate::http::HttpClient;
use async_trait::async_trait;
use bdm_config::{Loaded, Redacted, VendorStatus};
use bdm_domain::{AssetId, AssetRef, Price};
use bdm_ports::{PortHandle, PortResult, PriceFeed, PriceHistory, ProviderError, Registration};
use chrono::{DateTime, Utc};
use std::sync::Arc;

pub const ID: &str = "defillama";
const BASE: &str = "https://coins.llama.fi";

pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status(ID) != VendorStatus::Active {
        return;
    }
    let a = Arc::new(DefiLlama::new(util::http(loaded, ID), BASE));
    out.push(
        Registration::new(util::meta(loaded, ID))
            .global_port(PortHandle::Price(a.clone()))
            .global_port(PortHandle::PriceHistory(a)),
    );
}

pub struct DefiLlama {
    http: HttpClient,
    base: String,
}

/// DefiLlama coin key: `{chain}:{address}` for tokens, `coingecko:{id}` for native coins.
fn coin_key(asset: &AssetId) -> PortResult<String> {
    let unsupported = || ProviderError::Unsupported(format!("defillama does not cover {asset}"));
    if let AssetRef::Native { slip44 } = asset.asset {
        let id = match slip44 {
            60 => "ethereum",
            501 => "solana",
            714 => "binancecoin",
            966 => "polygon-ecosystem-token",
            9000 => "avalanche-2",
            _ => return Err(unsupported()),
        };
        return Ok(format!("coingecko:{id}"));
    }
    let chain = if util::is_solana_mainnet(&asset.chain) {
        "solana"
    } else {
        match asset.chain.evm_chain_id().ok_or_else(unsupported)? {
            1 => "ethereum",
            8453 => "base",
            42161 => "arbitrum",
            10 => "optimism",
            137 => "polygon",
            43114 => "avax",
            56 => "bsc",
            _ => return Err(unsupported()),
        }
    };
    Ok(format!(
        "{chain}:{}",
        util::token_address(asset).ok_or_else(unsupported)?
    ))
}

impl DefiLlama {
    pub fn new(http: HttpClient, base: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
        }
    }

    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn fetch(
        &self,
        asset: &AssetId,
        currency: &str,
        path: &str,
        label: &str,
    ) -> PortResult<Price> {
        if !currency.eq_ignore_ascii_case("usd") {
            return Err(ProviderError::Unsupported("defillama is USD only".into()));
        }
        let key = coin_key(asset)?;
        let url = Redacted::new(format!("{}{path}/{key}", self.base));
        let v = self.http.get_json(&url, label, &[]).await?;
        let row = v["coins"]
            .as_object()
            .and_then(|m| m.iter().find(|(k, _)| k.eq_ignore_ascii_case(&key)))
            .map(|(_, v)| v)
            .ok_or(ProviderError::NotFound)?;
        Ok(Price {
            asset: asset.clone(),
            currency: "USD".into(),
            value: util::price(&row["price"])?,
            as_of: row["timestamp"]
                .as_i64()
                .and_then(util::unix)
                .ok_or(ProviderError::NotFound)?,
            source: ID.into(),
            liquidity_usd: None,
        })
    }
}

#[async_trait]
impl PriceFeed for DefiLlama {
    async fn price(&self, asset: &AssetId, currency: &str) -> PortResult<Price> {
        self.fetch(asset, currency, "/prices/current", "/prices/current")
            .await
    }
}

#[async_trait]
impl PriceHistory for DefiLlama {
    async fn price_at(
        &self,
        asset: &AssetId,
        currency: &str,
        at: DateTime<Utc>,
    ) -> PortResult<Price> {
        let path = format!("/prices/historical/{}", at.timestamp());
        self.fetch(asset, currency, &path, "/prices/historical")
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::DEFAULT_TIMEOUT;
    use bdm_testkit::wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };
    use rust_decimal::Decimal;

    #[tokio::test]
    async fn current_and_historical() {
        let server = MockServer::start().await;
        let fx = |c| bdm_testkit::vendor_fixture(env!("CARGO_MANIFEST_DIR"), ID, c);
        Mock::given(method("GET"))
            .and(path(
                "/prices/current/ethereum:0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(fx("prices_current")))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/prices/historical/1700000000/coingecko:ethereum"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fx("prices_historical")))
            .mount(&server)
            .await;
        let d = DefiLlama::new(HttpClient::new(ID, DEFAULT_TIMEOUT), &server.uri());
        let usdc: AssetId = "eip155:1/erc20:0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
            .parse()
            .unwrap();
        let p = d.price(&usdc, "USD").await.unwrap();
        assert_eq!(p.value, Decimal::new(9_998, 4));
        assert_eq!(p.as_of.timestamp(), 1_790_000_000);
        let eth: AssetId = "eip155:1/slip44:60".parse().unwrap();
        let h = d
            .price_at(
                &eth,
                "usd",
                DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(h.value, Decimal::new(206_345, 2));
        assert_eq!(h.as_of.timestamp(), 1_699_999_950);
        // A coin missing from the response is unknown, not zero.
        let wbtc: AssetId = "eip155:1/erc20:0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
            .parse()
            .unwrap();
        assert!(d.price(&wbtc, "USD").await.is_err());
    }
}
