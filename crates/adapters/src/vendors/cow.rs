//! `cow` Protocol orderbook API (keyless).
//!
//! Quotes only: CoW orders are EIP-712 intents signed off-chain and settled by solvers
//! (MEV-protected, useful on L2s without private mempools). There is no transaction to build,
//! so `build` returns `Unsupported` and routing moves to the next vendor. Native sells need
//! the eth-flow contract and are not supported.

use super::util;

use crate::http::HttpClient;
use async_trait::async_trait;
use bdm_config::{Loaded, Redacted, VendorStatus};
use bdm_domain::{AccountAddress, SwapQuote};
use bdm_ports::{PortHandle, PortResult, ProviderError, Registration, SwapQuoter, SwapRequest};
use chrono::DateTime;
use serde_json::json;
use std::sync::Arc;

pub const ID: &str = "cow";
const BASE: &str = "https://api.cow.fi";

pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status(ID) != VendorStatus::Active {
        return;
    }
    let a = Arc::new(Cow::new(util::http(loaded, ID), BASE));
    out.push(Registration::new(loaded.vendor_meta(ID)).global_port(PortHandle::SwapQuote(a)));
}

pub struct Cow {
    http: HttpClient,
    base: String,
}

fn network(chain_id: u64) -> Option<&'static str> {
    Some(match chain_id {
        1 => "mainnet",
        8453 => "base",
        42161 => "arbitrum_one",
        137 => "polygon",
        43114 => "avalanche",
        56 => "bnb",
        _ => return None,
    })
}

impl Cow {
    pub fn new(http: HttpClient, base: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
        }
    }
}

#[async_trait]
impl SwapQuoter for Cow {
    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn quote(&self, req: &SwapRequest) -> PortResult<SwapQuote> {
        let net = network(util::evm_chain_id(&req.chain)?).ok_or_else(|| {
            ProviderError::Unsupported(format!("cow does not cover {}", req.chain))
        })?;
        if req.sell_asset.is_native() {
            return Err(ProviderError::Unsupported(
                "cow cannot sell the native coin (eth-flow)".into(),
            ));
        }
        let from = match req.taker {
            Some(AccountAddress::Evm(a)) => a.to_checksum(None),
            _ => util::ZERO_ADDRESS.to_owned(),
        };
        let body = json!({
            "sellToken": util::evm_token_or_native(&req.sell_asset)?,
            "buyToken": util::evm_token_or_native(&req.buy_asset)?,
            "from": from,
            "kind": "sell",
            "sellAmountBeforeFee": req.sell_amount.raw.to_string(),
            "signingScheme": "eip712",
            "priceQuality": "optimal",
        });
        let url = Redacted::new(format!("{}/{net}/api/v1/quote", self.base));
        let v = self.http.post_json(&url, "/api/v1/quote", &body).await?;
        let buy = util::u256_field(&v["quote"]["buyAmount"], "buyAmount")?;
        let mut q = util::swap_quote(req, ID, buy, None, None);
        if let Some(exp) = v["expiration"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        {
            q.expires_at = exp.to_utc();
        }
        Ok(q)
    }

    async fn build(&self, _req: &SwapRequest) -> PortResult<SwapQuote> {
        Err(ProviderError::Unsupported(
            "cow orders are EIP-712 intents signed off-chain; no transaction to build".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::DEFAULT_TIMEOUT;
    use alloy_primitives::U256;
    use bdm_domain::{Amount, ChainId};
    use bdm_testkit::wiremock::{
        matchers::{body_partial_json, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    #[tokio::test]
    async fn quote_uses_vendor_expiry_and_build_is_unsupported() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/base/api/v1/quote"))
            .and(body_partial_json(
                json!({"kind": "sell", "sellAmountBeforeFee": "1000000000"}),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(bdm_testkit::vendor_fixture(
                    env!("CARGO_MANIFEST_DIR"),
                    ID,
                    "quote",
                )),
            )
            .mount(&server)
            .await;
        let c = Cow::new(HttpClient::new(ID, DEFAULT_TIMEOUT), &server.uri());
        let req = SwapRequest {
            chain: ChainId::evm(8453),
            sell_asset: "eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
                .parse()
                .unwrap(),
            buy_asset: "eip155:8453/slip44:60".parse().unwrap(),
            sell_amount: Amount::from_u128(1_000_000_000, 6),
            slippage_bps: 100,
            taker: None,
        };
        let q = c.quote(&req).await.unwrap();
        assert_eq!(q.buy_amount.raw, U256::from(381_000_000_000_000_000u128));
        assert_eq!(
            q.min_buy_amount.raw,
            U256::from(377_190_000_000_000_000u128)
        );
        assert_eq!(q.expires_at.to_rfc3339(), "2026-09-23T12:01:00+00:00");
        assert!(matches!(
            c.build(&req).await,
            Err(ProviderError::Unsupported(_))
        ));
    }
}
