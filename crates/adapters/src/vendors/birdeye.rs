//! `birdeye` (key, 30K CU/month, 1 req/s).
//! Ports: `PriceFeed` (USD, with liquidity), `PriceHistory` (1-minute candles). Headers:
//! `X-API-KEY` and `x-chain`. Native SOL is priced through the wrapped-SOL mint.

use super::util;
use bdm_protocols::solana::spl::WRAPPED_SOL_MINT;

use crate::http::HttpClient;
use async_trait::async_trait;
use bdm_config::{Loaded, Redacted, VendorStatus};
use bdm_domain::{AssetId, AssetRef, Price};
use bdm_ports::{PortHandle, PortResult, PriceFeed, PriceHistory, ProviderError, Registration};
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use std::sync::Arc;

pub const ID: &str = "birdeye";
const BASE: &str = "https://public-api.birdeye.so";

pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status(ID) != VendorStatus::Active {
        return;
    }
    let Some(key) = loaded.key(ID, "api_key") else {
        return;
    };
    let a = Arc::new(Birdeye::new(util::http(loaded, ID), BASE, key));
    out.push(
        Registration::new(loaded.vendor_meta(ID))
            .global_port(PortHandle::Price(a.clone()))
            .global_port(PortHandle::PriceHistory(a)),
    );
}

pub struct Birdeye {
    http: HttpClient,
    base: String,
    key: Redacted<String>,
}

fn target(asset: &AssetId, currency: &str) -> PortResult<(&'static str, String)> {
    let unsupported = || ProviderError::Unsupported(format!("birdeye does not cover {asset}"));
    if !currency.eq_ignore_ascii_case("usd") {
        return Err(ProviderError::Unsupported("birdeye is USD only".into()));
    }
    let chain = util::dex_chain_slug(&asset.chain).ok_or_else(unsupported)?;
    let addr = match (&asset.asset, chain) {
        (AssetRef::Native { slip44: 501 }, "solana") => WRAPPED_SOL_MINT.to_owned(),
        _ => util::token_address(asset).ok_or_else(unsupported)?,
    };
    Ok((chain, addr))
}

impl Birdeye {
    pub fn new(http: HttpClient, base: &str, key: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            key: Redacted::new(key.to_owned()),
        }
    }

    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn get(&self, chain: &str, path: &str, label: &str) -> PortResult<Value> {
        let url = Redacted::new(format!("{}{path}", self.base));
        let v = self
            .http
            .get_json(
                &url,
                label,
                &[("X-API-KEY", self.key.expose()), ("x-chain", chain)],
            )
            .await?;
        if v["success"].as_bool() != Some(true) {
            return Err(ProviderError::NotFound);
        }
        Ok(v)
    }
}

#[async_trait]
impl PriceFeed for Birdeye {
    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn price(&self, asset: &AssetId, currency: &str) -> PortResult<Price> {
        let (chain, addr) = target(asset, currency)?;
        let v = self
            .get(
                chain,
                &format!("/defi/price?address={addr}&include_liquidity=true"),
                "/defi/price",
            )
            .await?;
        let d = &v["data"];
        Ok(Price {
            asset: asset.clone(),
            currency: "USD".into(),
            value: util::price(&d["value"])?,
            as_of: d["updateUnixTime"]
                .as_i64()
                .and_then(util::unix)
                .unwrap_or_else(Utc::now),
            source: ID.into(),
            liquidity_usd: util::dec(&d["liquidity"]),
        })
    }
}

#[async_trait]
impl PriceHistory for Birdeye {
    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn price_at(
        &self,
        asset: &AssetId,
        currency: &str,
        at: DateTime<Utc>,
    ) -> PortResult<Price> {
        let (chain, addr) = target(asset, currency)?;
        let (from, to) = (
            (at - Duration::minutes(5)).timestamp(),
            (at + Duration::minutes(5)).timestamp(),
        );
        let v = self
            .get(
                chain,
                &format!("/defi/history_price?address={addr}&address_type=token&type=1m&time_from={from}&time_to={to}"),
                "/defi/history_price",
            )
            .await?;
        v["data"]["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|i| {
                Some((
                    util::unix(i["unixTime"].as_i64()?)?,
                    util::price(&i["value"]).ok()?,
                ))
            })
            .min_by_key(|(ts, _)| (*ts - at).num_seconds().abs())
            .map(|(ts, value)| Price {
                asset: asset.clone(),
                currency: "USD".into(),
                value,
                as_of: ts,
                source: ID.into(),
                liquidity_usd: None,
            })
            .ok_or(ProviderError::NotFound)
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

    #[tokio::test]
    async fn native_sol_price_via_wsol() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/defi/price"))
            .and(query_param("address", WRAPPED_SOL_MINT))
            .and(header("x-chain", "solana"))
            .and(header("X-API-KEY", "be-key-123456"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(bdm_testkit::vendor_fixture(
                    env!("CARGO_MANIFEST_DIR"),
                    ID,
                    "defi_price",
                )),
            )
            .mount(&server)
            .await;
        let b = Birdeye::new(
            HttpClient::new(ID, DEFAULT_TIMEOUT),
            &server.uri(),
            "be-key-123456",
        );
        let sol: AssetId = format!("{}/slip44:501", util::SOLANA_MAINNET)
            .parse()
            .unwrap();
        let p = b.price(&sol, "USD").await.unwrap();
        assert_eq!(p.value, Decimal::new(14_512, 2));
        assert_eq!(p.as_of.timestamp(), 1_790_000_000);
        assert!(p.liquidity_usd.is_some());
    }
}
