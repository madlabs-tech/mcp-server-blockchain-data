//! `okx_dex` aggregator API v6 (key + secret + passphrase, HMAC-SHA256). Owner: `market-trading`
//! (T1.M3). Disabled by default: free tier unverified.
//!
//! Signing: `OK-ACCESS-SIGN = base64(HMAC_SHA256(secret, timestamp + "GET" + path?query))` with
//! `OK-ACCESS-KEY`, `OK-ACCESS-TIMESTAMP` (ISO-8601 ms) and `OK-ACCESS-PASSPHRASE`.
//! `quote` → `/quote` (EVM + Solana); `build` → `/swap` + approval to the spender from
//! `/approve-transaction` (EVM only; Solana builds are not supported yet).

use super::util;

use crate::http::HttpClient;
use async_trait::async_trait;
use base64::Engine;
use bdm_config::{Loaded, Redacted, VendorStatus};
use bdm_domain::{AssetRef, SwapQuote};
use bdm_ports::{PortHandle, PortResult, ProviderError, Registration, SwapQuoter, SwapRequest};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use std::sync::Arc;

pub const ID: &str = "okx_dex";
const BASE: &str = "https://web3.okx.com";
const PREFIX: &str = "/api/v6/dex/aggregator";
const EVM_CHAINS: &[u64] = &[1, 10, 56, 137, 8453, 42161, 43114];
/// OKX chain index for Solana and its native-SOL placeholder.
const SOLANA_INDEX: &str = "501";
const SOL_NATIVE: &str = "11111111111111111111111111111111";

pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status(ID) != VendorStatus::Active {
        return;
    }
    let (Some(key), Some(secret), Some(pass)) = (
        loaded.key(ID, "api_key"),
        loaded.key(ID, "secret_key"),
        loaded.key(ID, "passphrase"),
    ) else {
        return;
    };
    let a = Arc::new(OkxDex::new(util::http(loaded, ID), BASE, key, secret, pass));
    out.push(Registration::new(loaded.vendor_meta(ID)).global_port(PortHandle::SwapQuote(a)));
}

pub struct OkxDex {
    http: HttpClient,
    base: String,
    key: Redacted<String>,
    secret: Redacted<String>,
    passphrase: Redacted<String>,
}

fn sign(secret: &str, prehash: &str) -> PortResult<String> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|e| ProviderError::Fatal(format!("hmac init: {e}")))?;
    mac.update(prehash.as_bytes());
    Ok(base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes()))
}

/// (chainIndex, sell token, buy token)
fn route(req: &SwapRequest) -> PortResult<(String, String, String)> {
    if util::is_solana_mainnet(&req.chain) {
        let t = |a: &bdm_domain::AssetId| match &a.asset {
            AssetRef::Native { .. } => Ok(SOL_NATIVE.to_owned()),
            AssetRef::SplToken(m) => Ok(m.to_string()),
            AssetRef::Erc20(_) => Err(ProviderError::Invalid("EVM asset on Solana".into())),
        };
        return Ok((SOLANA_INDEX.into(), t(&req.sell_asset)?, t(&req.buy_asset)?));
    }
    let chain = util::evm_chain_id(&req.chain)?;
    if !EVM_CHAINS.contains(&chain) {
        return Err(ProviderError::Unsupported(format!(
            "okx dex does not cover {}",
            req.chain
        )));
    }
    Ok((
        chain.to_string(),
        util::evm_token_or_native(&req.sell_asset)?,
        util::evm_token_or_native(&req.buy_asset)?,
    ))
}

impl OkxDex {
    pub fn new(http: HttpClient, base: &str, key: &str, secret: &str, passphrase: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            key: Redacted::new(key.to_owned()),
            secret: Redacted::new(secret.to_owned()),
            passphrase: Redacted::new(passphrase.to_owned()),
        }
    }

    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn get(&self, endpoint: &str, query: &str) -> PortResult<Value> {
        let path = format!("{PREFIX}/{endpoint}?{query}");
        let ts = chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string();
        let signature = sign(self.secret.expose(), &format!("{ts}GET{path}"))?;
        let url = Redacted::new(format!("{}{path}", self.base));
        let v = self
            .http
            .get_json(
                &url,
                &format!("/{endpoint}"),
                &[
                    ("OK-ACCESS-KEY", self.key.expose()),
                    ("OK-ACCESS-SIGN", &signature),
                    ("OK-ACCESS-TIMESTAMP", &ts),
                    ("OK-ACCESS-PASSPHRASE", self.passphrase.expose()),
                ],
            )
            .await?;
        if v["code"].as_str() != Some("0") {
            return Err(ProviderError::Transient(format!(
                "okx code {}: {}",
                v["code"],
                v["msg"].as_str().unwrap_or("")
            )));
        }
        v["data"].get(0).cloned().ok_or(ProviderError::NotFound)
    }

    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    fn to_quote(req: &SwapRequest, d: &Value) -> PortResult<SwapQuote> {
        let r = if d["routerResult"].is_object() {
            &d["routerResult"]
        } else {
            d
        };
        let buy = util::u256_field(&r["toTokenAmount"], "toTokenAmount")?;
        let decimals = r["toToken"]["decimal"]
            .as_str()
            .and_then(|s| s.parse().ok());
        let min = util::u256(&d["tx"]["minReceiveAmount"]);
        let mut q = util::swap_quote(req, ID, buy, decimals, min);
        q.price_impact_bps = util::percent_to_bps(&r["priceImpactPercentage"]);
        Ok(q)
    }
}

#[async_trait]
impl SwapQuoter for OkxDex {
    async fn quote(&self, req: &SwapRequest) -> PortResult<SwapQuote> {
        let (chain, from, to) = route(req)?;
        let d = self
            .get(
                "quote",
                &format!(
                    "chainIndex={chain}&amount={}&fromTokenAddress={from}&toTokenAddress={to}",
                    req.sell_amount.raw
                ),
            )
            .await?;
        Self::to_quote(req, &d)
    }

    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn build(&self, req: &SwapRequest) -> PortResult<SwapQuote> {
        if util::is_solana_mainnet(&req.chain) {
            return Err(ProviderError::Unsupported(
                "okx solana swap builds not supported yet".into(),
            ));
        }
        let taker = util::evm_taker(req)?;
        let (chain, from, to) = route(req)?;
        let slippage = rust_decimal::Decimal::new(i64::from(req.slippage_bps), 4).normalize();
        let d = self
            .get(
                "swap",
                &format!(
                    "chainIndex={chain}&amount={}&fromTokenAddress={from}&toTokenAddress={to}&slippage={slippage}&userWalletAddress={taker}",
                    req.sell_amount.raw
                ),
            )
            .await?;
        let mut q = Self::to_quote(req, &d)?;
        q.tx = Some(util::evm_tx(util::evm_chain_id(&req.chain)?, &d["tx"])?);
        if !req.sell_asset.is_native() {
            let a = self
                .get(
                    "approve-transaction",
                    &format!(
                        "chainIndex={chain}&tokenContractAddress={from}&approveAmount={}",
                        req.sell_amount.raw
                    ),
                )
                .await?;
            let spender = a["dexContractAddress"]
                .as_str()
                .ok_or_else(|| ProviderError::Transient("okx approve has no spender".into()))?;
            q.required_approvals = util::sell_approval(req, spender)?;
        }
        Ok(q)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::DEFAULT_TIMEOUT;
    use bdm_domain::{Amount, ChainId};
    use bdm_testkit::wiremock::{
        matchers::{header, header_exists, method, path, query_param},
        Mock, MockServer, ResponseTemplate,
    };

    #[test]
    fn hmac_base64() {
        // RFC 4231 test case 2: key "Jefe", data "what do ya want for nothing?"
        assert_eq!(
            sign("Jefe", "what do ya want for nothing?").unwrap(),
            "W9zBRr9gdU5qBCQmCJV1x1oAPwidJzmDnexYuWTsOEM="
        );
    }

    #[tokio::test]
    async fn solana_quote_is_signed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("{PREFIX}/quote")))
            .and(query_param("chainIndex", "501"))
            .and(query_param("fromTokenAddress", SOL_NATIVE))
            .and(header("OK-ACCESS-KEY", "okx-key"))
            .and(header_exists("OK-ACCESS-SIGN"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(bdm_testkit::vendor_fixture(
                    env!("CARGO_MANIFEST_DIR"),
                    ID,
                    "quote_solana",
                )),
            )
            .mount(&server)
            .await;
        let o = OkxDex::new(
            HttpClient::new(ID, DEFAULT_TIMEOUT),
            &server.uri(),
            "okx-key",
            "s3cret",
            "pass",
        );
        let req = SwapRequest {
            chain: util::SOLANA_MAINNET.parse::<ChainId>().unwrap(),
            sell_asset: format!("{}/slip44:501", util::SOLANA_MAINNET)
                .parse()
                .unwrap(),
            buy_asset: format!(
                "{}/token:EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                util::SOLANA_MAINNET
            )
            .parse()
            .unwrap(),
            sell_amount: Amount::from_u128(1_000_000_000, 9),
            slippage_bps: 50,
            taker: None,
        };
        let q = o.quote(&req).await.unwrap();
        assert_eq!(q.buy_amount, Amount::from_u128(145_120_000, 6));
        assert_eq!(q.price_impact_bps, Some(-3));
    }
}
