//! `uniswap_api` Trading API (key via `x-api-key`; FAQ says free, 6 req/s per key).
//! Owner: `market-trading` (T1.M3). Disabled by default until the `/v1/swap` request/response
//! shape is checked against the live spec. Supports Robinhood Chain (4663).
//!
//! `quote` → `POST /v1/quote` (CLASSIC routing); `build` → `/v1/check_approval` (approval to
//! Permit2, returned ready-made by the API) + `POST /v1/swap`. Quotes that need a Permit2
//! signature (`permitData`) are not buildable here yet and return `Unsupported`.

use super::util;

use crate::http::HttpClient;
use async_trait::async_trait;
use bdm_config::{Loaded, Redacted, VendorStatus};
use bdm_domain::{AccountAddress, AssetRef, SwapQuote};
use bdm_ports::{PortHandle, PortResult, ProviderError, Registration, SwapQuoter, SwapRequest};
use reqwest::Method;
use serde_json::{json, Value};
use std::sync::Arc;

pub const ID: &str = "uniswap_api";
const BASE: &str = "https://trade-api.gateway.uniswap.org/v1";
const CHAINS: &[u64] = &[1, 10, 56, 137, 8453, 42161, 43114, 4663];

pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status(ID) != VendorStatus::Active {
        return;
    }
    let Some(key) = loaded.key(ID, "api_key") else {
        return;
    };
    let a = Arc::new(UniswapApi::new(util::http(loaded, ID), BASE, key));
    out.push(Registration::new(loaded.vendor_meta(ID)).global_port(PortHandle::SwapQuote(a)));
}

pub struct UniswapApi {
    http: HttpClient,
    base: String,
    key: Redacted<String>,
}

/// Uniswap uses the zero address for the native coin.
fn token(asset: &bdm_domain::AssetId) -> PortResult<String> {
    match &asset.asset {
        AssetRef::Native { .. } => Ok(util::ZERO_ADDRESS.to_owned()),
        _ => util::evm_token_or_native(asset),
    }
}

impl UniswapApi {
    pub fn new(http: HttpClient, base: &str, key: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            key: Redacted::new(key.to_owned()),
        }
    }

    async fn post(&self, endpoint: &str, body: &Value) -> PortResult<Value> {
        let url = Redacted::new(format!("{}/{endpoint}", self.base));
        self.http
            .request(
                Method::POST,
                &url,
                &format!("/v1/{endpoint}"),
                &[("x-api-key", self.key.expose())],
                Some(body),
            )
            .await
    }

    /// `(chain id, raw quote response)`.
    async fn raw_quote(&self, req: &SwapRequest) -> PortResult<(u64, Value)> {
        let chain = util::evm_chain_id(&req.chain)?;
        if !CHAINS.contains(&chain) {
            return Err(ProviderError::Unsupported(format!(
                "uniswap api does not cover {}",
                req.chain
            )));
        }
        let swapper = match req.taker {
            Some(AccountAddress::Evm(a)) => a.to_checksum(None),
            _ => util::ZERO_ADDRESS.to_owned(),
        };
        let body = json!({
            "type": "EXACT_INPUT",
            "amount": req.sell_amount.raw.to_string(),
            "tokenInChainId": chain,
            "tokenOutChainId": chain,
            "tokenIn": token(&req.sell_asset)?,
            "tokenOut": token(&req.buy_asset)?,
            "swapper": swapper,
            // Percent as a JSON number, parsed from its exact decimal text.
            "slippageTolerance": serde_json::from_str::<Value>(&util::slippage_percent(req.slippage_bps)).ok(),
            "routingPreference": "CLASSIC",
        });
        Ok((chain, self.post("quote", &body).await?))
    }

    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    fn to_quote(req: &SwapRequest, v: &Value) -> PortResult<SwapQuote> {
        let q = &v["quote"];
        let buy = util::u256_field(&q["output"]["amount"], "quote.output.amount")?;
        let mut out = util::swap_quote(req, ID, buy, None, None);
        out.price_impact_bps = util::percent_to_bps(&q["priceImpact"]);
        Ok(out)
    }
}

#[async_trait]
impl SwapQuoter for UniswapApi {
    async fn quote(&self, req: &SwapRequest) -> PortResult<SwapQuote> {
        let (_, v) = self.raw_quote(req).await?;
        Self::to_quote(req, &v)
    }

    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn build(&self, req: &SwapRequest) -> PortResult<SwapQuote> {
        let taker = util::evm_taker(req)?;
        let (chain, v) = self.raw_quote(req).await?;
        if !v["permitData"].is_null() {
            return Err(ProviderError::Unsupported(
                "uniswap quote needs a Permit2 signature; not supported yet".into(),
            ));
        }
        let mut q = Self::to_quote(req, &v)?;
        if !req.sell_asset.is_native() {
            let approval = self
                .post(
                    "check_approval",
                    &json!({
                        "walletAddress": taker,
                        "token": token(&req.sell_asset)?,
                        "amount": req.sell_amount.raw.to_string(),
                        "chainId": chain,
                    }),
                )
                .await?;
            if approval["approval"].is_object() {
                q.required_approvals = vec![util::evm_tx(chain, &approval["approval"])?];
            }
        }
        let swap = self.post("swap", &json!({ "quote": v["quote"] })).await?;
        q.tx = Some(util::evm_tx(chain, &swap["swap"])?);
        Ok(q)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::DEFAULT_TIMEOUT;
    use bdm_domain::{Amount, ChainId};
    use bdm_testkit::wiremock::{
        matchers::{header, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    #[tokio::test]
    async fn quote_with_price_impact() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/quote"))
            .and(header("x-api-key", "uni-key"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(bdm_testkit::vendor_fixture(
                    env!("CARGO_MANIFEST_DIR"),
                    ID,
                    "quote",
                )),
            )
            .mount(&server)
            .await;
        let u = UniswapApi::new(
            HttpClient::new(ID, DEFAULT_TIMEOUT),
            &server.uri(),
            "uni-key",
        );
        let req = SwapRequest {
            chain: ChainId::evm(8453),
            sell_asset: "eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
                .parse()
                .unwrap(),
            buy_asset: "eip155:8453/slip44:60".parse().unwrap(),
            sell_amount: Amount::from_u128(1_000_000_000, 6),
            slippage_bps: 50,
            taker: None,
        };
        let q = u.quote(&req).await.unwrap();
        assert_eq!(q.price_impact_bps, Some(12));
        assert_eq!(q.buy_amount.raw.to_string(), "382100000000000000");
        assert!(matches!(
            u.build(&req).await,
            Err(ProviderError::Invalid(_))
        ));
    }
}
