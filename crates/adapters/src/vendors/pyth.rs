//! `pyth`: Hermes (live prices) and Benchmarks (historical). Owner: `market-trading` (T1.M1).
//! Since 2026-08-26 Pyth docs require `Authorization: Bearer $PYTH_API_KEY` on Hermes requests;
//! Benchmarks also answers 401 without a key (its header is undocumented ⚠, we send the same).
//!
//! Pyth feeds are keyed by symbol, so only native coins are mapped (ETH, SOL, BNB, AVAX, POL);
//! ERC-20/SPL assets return `Unsupported` and routing moves on. Feed ids are looked up once via
//! Hermes `/v2/price_feeds` and cached.

// Shared helpers compiled into each vendor module so every feature builds alone.
#[allow(clippy::duplicate_mod)]
#[path = "market_util.rs"]
mod util;

use crate::http::HttpClient;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ems_config::{Loaded, Redacted, VendorStatus};
use ems_domain::{AssetId, AssetRef, Price};
use ems_ports::{PortHandle, PortResult, PriceFeed, PriceHistory, ProviderError, Registration};
use rust_decimal::Decimal;
use serde_json::Value;
use std::{collections::HashMap, sync::Arc, sync::Mutex};

pub const ID: &str = "pyth";
const HERMES: &str = "https://hermes.pyth.network";
const BENCHMARKS: &str = "https://benchmarks.pyth.network";

pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status(ID) != VendorStatus::Active {
        return;
    }
    let Some(key) = loaded.key(ID, "api_key") else {
        return;
    };
    let a = Arc::new(Pyth::new(util::http(loaded, ID), HERMES, BENCHMARKS, key));
    out.push(
        Registration::new(util::meta(loaded, ID))
            .global_port(PortHandle::Price(a.clone()))
            .global_port(PortHandle::PriceHistory(a)),
    );
}

pub struct Pyth {
    http: HttpClient,
    hermes: String,
    benchmarks: String,
    /// `Bearer <key>`.
    auth: Redacted<String>,
    feed_ids: Mutex<HashMap<String, String>>,
}

fn symbol(asset: &AssetId) -> PortResult<&'static str> {
    match asset.asset {
        AssetRef::Native { slip44 } => match slip44 {
            60 => Ok("ETH"),
            501 => Ok("SOL"),
            714 => Ok("BNB"),
            966 => Ok("POL"),
            9000 => Ok("AVAX"),
            _ => Err(ProviderError::Unsupported(format!(
                "no pyth feed mapping for {asset}"
            ))),
        },
        _ => Err(ProviderError::Unsupported(
            "pyth adapter maps native coins only".into(),
        )),
    }
}

/// `price × 10^expo` exactly.
fn scaled(price: &Value, expo: i64) -> PortResult<Decimal> {
    let raw: i64 = match price {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.as_i64(),
        _ => None,
    }
    .ok_or(ProviderError::NotFound)?;
    let d = if expo <= 0 {
        Decimal::try_from_i128_with_scale(raw.into(), (-expo) as u32)
            .map_err(|e| ProviderError::Transient(e.to_string()))?
    } else {
        Decimal::from(raw) * Decimal::from(10i64.pow(expo as u32))
    };
    if d.is_sign_positive() && !d.is_zero() {
        Ok(d)
    } else {
        Err(ProviderError::NotFound)
    }
}

impl Pyth {
    pub fn new(http: HttpClient, hermes: &str, benchmarks: &str, key: &str) -> Self {
        Self {
            http,
            hermes: hermes.trim_end_matches('/').to_owned(),
            benchmarks: benchmarks.trim_end_matches('/').to_owned(),
            auth: Redacted::new(format!("Bearer {key}")),
            feed_ids: Mutex::new(HashMap::new()),
        }
    }

    async fn feed_id(&self, sym: &str, cur: &str) -> PortResult<String> {
        let pair = format!("{sym}/{cur}");
        if let Some(id) = self.feed_ids.lock().expect("feed id cache").get(&pair) {
            return Ok(id.clone());
        }
        let url = Redacted::new(format!(
            "{}/v2/price_feeds?query={sym}&asset_type=crypto",
            self.hermes
        ));
        let v = self.get(&url, "/v2/price_feeds").await?;
        let id = v
            .as_array()
            .into_iter()
            .flatten()
            .find(|f| {
                let a = &f["attributes"];
                a["base"].as_str() == Some(sym) && a["quote_currency"].as_str() == Some(cur)
            })
            .and_then(|f| f["id"].as_str())
            .map(str::to_owned)
            .ok_or(ProviderError::NotFound)?;
        self.feed_ids
            .lock()
            .expect("feed id cache")
            .insert(pair, id.clone());
        Ok(id)
    }

    async fn get(&self, url: &Redacted<String>, label: &str) -> PortResult<Value> {
        self.http
            .get_json(url, label, &[("authorization", self.auth.expose())])
            .await
    }

    fn parse(&self, asset: &AssetId, cur: &str, v: &Value) -> PortResult<Price> {
        let p = &v["parsed"][0]["price"];
        Ok(Price {
            asset: asset.clone(),
            currency: cur.into(),
            value: scaled(
                &p["price"],
                p["expo"].as_i64().ok_or(ProviderError::NotFound)?,
            )?,
            as_of: p["publish_time"]
                .as_i64()
                .and_then(util::unix)
                .ok_or(ProviderError::NotFound)?,
            source: ID.into(),
            liquidity_usd: None,
        })
    }
}

#[async_trait]
impl PriceFeed for Pyth {
    async fn price(&self, asset: &AssetId, currency: &str) -> PortResult<Price> {
        let cur = currency.to_uppercase();
        let id = self.feed_id(symbol(asset)?, &cur).await?;
        let url = Redacted::new(format!(
            "{}/v2/updates/price/latest?ids[]={id}&parsed=true",
            self.hermes
        ));
        let v = self.get(&url, "/v2/updates/price/latest").await?;
        self.parse(asset, &cur, &v)
    }
}

#[async_trait]
impl PriceHistory for Pyth {
    async fn price_at(
        &self,
        asset: &AssetId,
        currency: &str,
        at: DateTime<Utc>,
    ) -> PortResult<Price> {
        let cur = currency.to_uppercase();
        let id = self.feed_id(symbol(asset)?, &cur).await?;
        let url = Redacted::new(format!(
            "{}/v1/updates/price/{}?ids={id}&parsed=true",
            self.benchmarks,
            at.timestamp()
        ));
        let v = self.get(&url, "/v1/updates/price").await?;
        self.parse(asset, &cur, &v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::DEFAULT_TIMEOUT;
    use ems_testkit::wiremock::{
        matchers::{header, method, path, query_param},
        Mock, MockServer, ResponseTemplate,
    };

    #[tokio::test]
    async fn looks_up_feed_once_then_prices() {
        let server = MockServer::start().await;
        let fx = |c| ems_testkit::vendor_fixture(env!("CARGO_MANIFEST_DIR"), ID, c);
        Mock::given(method("GET"))
            .and(path("/v2/price_feeds"))
            .and(query_param("query", "ETH"))
            .and(header("authorization", "Bearer pyth-key-123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fx("price_feeds_eth")))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v2/updates/price/latest"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fx("latest_eth")))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/updates/price/1700000000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fx("latest_eth")))
            .mount(&server)
            .await;
        let p = Pyth::new(
            HttpClient::new(ID, DEFAULT_TIMEOUT),
            &server.uri(),
            &server.uri(),
            "pyth-key-123",
        );
        let eth: AssetId = "eip155:8453/slip44:60".parse().unwrap();
        for _ in 0..2 {
            let price = p.price(&eth, "usd").await.unwrap();
            assert_eq!(price.value, Decimal::new(261_234_000_000, 8));
            assert_eq!(price.as_of.timestamp(), 1_790_000_000);
        }
        let at = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        assert!(p.price_at(&eth, "usd", at).await.is_ok());
    }

    #[test]
    fn scaling_is_exact() {
        assert_eq!(
            scaled(&"123456".into(), -3).unwrap(),
            Decimal::new(123456, 3)
        );
        assert_eq!(scaled(&"5".into(), 2).unwrap(), Decimal::from(500));
        assert_eq!(scaled(&"0".into(), -8), Err(ProviderError::NotFound));
    }
}
