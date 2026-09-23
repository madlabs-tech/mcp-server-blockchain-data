//! `dexscreener` (keyless, 300 req/min on token/pair endpoints). Owner: `market-trading` (T1.M1).
//! Port: `PriceFeed` (USD) from the most liquid pair where the token is the base token.
//! Note: DexScreener "boosts" are paid ads; we never rank by them.

use super::market_util as util;

use crate::http::HttpClient;
use async_trait::async_trait;
use bdm_config::{Loaded, Redacted, VendorStatus};
use bdm_domain::{AssetId, ChainId, Price};
use bdm_ports::{PortHandle, PortResult, PriceFeed, ProviderError, Registration};
use chrono::Utc;
use serde_json::Value;
use std::sync::Arc;

pub const ID: &str = "dexscreener";
const BASE: &str = "https://api.dexscreener.com";

pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status(ID) != VendorStatus::Active {
        return;
    }
    let a = Arc::new(DexScreener::new(util::http(loaded, ID), BASE));
    out.push(Registration::new(util::meta(loaded, ID)).global_port(PortHandle::Price(a)));
}

pub struct DexScreener {
    http: HttpClient,
    base: String,
}

fn chain_slug(chain: &ChainId) -> Option<&'static str> {
    if util::is_solana_mainnet(chain) {
        return Some("solana");
    }
    Some(match chain.evm_chain_id()? {
        1 => "ethereum",
        8453 => "base",
        42161 => "arbitrum",
        10 => "optimism",
        137 => "polygon",
        43114 => "avalanche",
        56 => "bsc",
        _ => return None, // Robinhood Chain coverage unverified
    })
}

impl DexScreener {
    pub fn new(http: HttpClient, base: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
        }
    }
}

#[async_trait]
impl PriceFeed for DexScreener {
    async fn price(&self, asset: &AssetId, currency: &str) -> PortResult<Price> {
        let unsupported =
            || ProviderError::Unsupported(format!("dexscreener does not cover {asset}"));
        if !currency.eq_ignore_ascii_case("usd") {
            return Err(ProviderError::Unsupported("dexscreener is USD only".into()));
        }
        let chain = chain_slug(&asset.chain).ok_or_else(unsupported)?;
        let addr = util::token_address(asset).ok_or_else(unsupported)?;
        let url = Redacted::new(format!("{}/tokens/v1/{chain}/{addr}", self.base));
        let v = self.http.get_json(&url, "/tokens/v1", &[]).await?;
        let liq = |p: &Value| util::dec(&p["liquidity"]["usd"]).unwrap_or_default();
        let best = v
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| {
                p["baseToken"]["address"]
                    .as_str()
                    .is_some_and(|a| a.eq_ignore_ascii_case(&addr))
                    && util::price(&p["priceUsd"]).is_ok()
            })
            .max_by_key(|p| liq(p))
            .ok_or(ProviderError::NotFound)?;
        Ok(Price {
            asset: asset.clone(),
            currency: "USD".into(),
            value: util::price(&best["priceUsd"])?,
            as_of: Utc::now(),
            source: ID.into(),
            liquidity_usd: util::dec(&best["liquidity"]["usd"]),
        })
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
    async fn picks_most_liquid_base_pair() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/tokens/v1/base/0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(bdm_testkit::vendor_fixture(
                    env!("CARGO_MANIFEST_DIR"),
                    ID,
                    "tokens_v1",
                )),
            )
            .mount(&server)
            .await;
        let d = DexScreener::new(HttpClient::new(ID, DEFAULT_TIMEOUT), &server.uri());
        let asset: AssetId = "eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
            .parse()
            .unwrap();
        let p = d.price(&asset, "USD").await.unwrap();
        // The quote-side pair (USDC as quote) and the thin pair are ignored.
        assert_eq!(p.value, Decimal::new(1_0001, 4));
        assert_eq!(p.liquidity_usd, Some(Decimal::new(8_500_000, 0)));
    }
}
