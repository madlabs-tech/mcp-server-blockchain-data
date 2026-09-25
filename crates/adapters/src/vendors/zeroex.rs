//! `zeroex` Swap API v2 (AllowanceHolder). Owner: `market-trading` (T1.M3).
//! Disabled by default: 0x pricing lists no free tier (Standard $1000/mo, checked 2026-09-23).
//! `quote` → `/swap/allowance-holder/price` (indicative); `build` → `/quote` (firm, needs
//! `taker`) + approval to `issues.allowance.spender` when the current allowance is short.
//! Supports Robinhood Chain (4663).

use super::util;

use crate::http::HttpClient;
use async_trait::async_trait;
use bdm_config::{Loaded, Redacted, VendorStatus};
use bdm_domain::SwapQuote;
use bdm_ports::{PortHandle, PortResult, ProviderError, Registration, SwapQuoter, SwapRequest};
use serde_json::Value;
use std::sync::Arc;

pub const ID: &str = "zeroex";
const BASE: &str = "https://api.0x.org";
const CHAINS: &[u64] = &[1, 10, 56, 137, 8453, 42161, 43114, 4663];

pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status(ID) != VendorStatus::Active {
        return;
    }
    let Some(key) = loaded.key(ID, "api_key") else {
        return;
    };
    let a = Arc::new(ZeroEx::new(util::http(loaded, ID), BASE, key));
    out.push(Registration::new(loaded.vendor_meta(ID)).global_port(PortHandle::SwapQuote(a)));
}

pub struct ZeroEx {
    http: HttpClient,
    base: String,
    key: Redacted<String>,
}

impl ZeroEx {
    pub fn new(http: HttpClient, base: &str, key: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            key: Redacted::new(key.to_owned()),
        }
    }

    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn call(
        &self,
        req: &SwapRequest,
        endpoint: &str,
        taker: Option<&str>,
    ) -> PortResult<Value> {
        let chain = util::covered_evm_chain(&req.chain, CHAINS, "0x")?;
        let taker = taker.map(|t| format!("&taker={t}")).unwrap_or_default();
        let url = Redacted::new(format!(
            "{}/swap/allowance-holder/{endpoint}?chainId={chain}&sellToken={}&buyToken={}&sellAmount={}&slippageBps={}{taker}",
            self.base,
            util::evm_token_or_native(&req.sell_asset)?,
            util::evm_token_or_native(&req.buy_asset)?,
            req.sell_amount.raw,
            req.slippage_bps,
        ));
        let v = self
            .http
            .get_json(
                &url,
                &format!("/swap/allowance-holder/{endpoint}"),
                &[("0x-api-key", self.key.expose()), ("0x-version", "v2")],
            )
            .await?;
        if v["liquidityAvailable"].as_bool() == Some(false) {
            return Err(ProviderError::NotFound);
        }
        Ok(v)
    }

    fn to_quote(req: &SwapRequest, v: &Value) -> PortResult<SwapQuote> {
        let buy = util::u256_field(&v["buyAmount"], "buyAmount")?;
        Ok(util::swap_quote(
            req,
            ID,
            buy,
            None,
            util::u256(&v["minBuyAmount"]),
        ))
    }
}

#[async_trait]
impl SwapQuoter for ZeroEx {
    async fn quote(&self, req: &SwapRequest) -> PortResult<SwapQuote> {
        let v = self.call(req, "price", None).await?;
        Self::to_quote(req, &v)
    }

    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn build(&self, req: &SwapRequest) -> PortResult<SwapQuote> {
        let taker = util::evm_taker(req)?;
        let v = self.call(req, "quote", Some(&taker)).await?;
        let mut q = Self::to_quote(req, &v)?;
        q.tx = Some(util::evm_tx(
            util::evm_chain_id(&req.chain)?,
            &v["transaction"],
        )?);
        if let Some(spender) = v["issues"]["allowance"]["spender"].as_str() {
            q.required_approvals = util::sell_approval(req, spender)?;
        }
        Ok(q)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::DEFAULT_TIMEOUT;
    use alloy_primitives::U256;
    use bdm_domain::{AccountAddress, Amount, ChainId};
    use bdm_testkit::wiremock::{
        matchers::{header, method, path, query_param},
        Mock, MockServer, ResponseTemplate,
    };

    #[tokio::test]
    async fn firm_quote_with_allowance_holder_approval() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/swap/allowance-holder/quote"))
            .and(header("0x-version", "v2"))
            .and(query_param("chainId", "4663"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(bdm_testkit::vendor_fixture(
                    env!("CARGO_MANIFEST_DIR"),
                    ID,
                    "allowance_holder_quote",
                )),
            )
            .mount(&server)
            .await;
        let z = ZeroEx::new(
            HttpClient::new(ID, DEFAULT_TIMEOUT),
            &server.uri(),
            "zx-key",
        );
        let req = SwapRequest {
            chain: ChainId::evm(4663),
            sell_asset: "eip155:4663/erc20:0x322F0929c4625eD5bAd873c95208D54E1c003b2d"
                .parse()
                .unwrap(),
            buy_asset: "eip155:4663/slip44:60".parse().unwrap(),
            sell_amount: Amount::from_u128(1_000_000_000_000_000_000, 18),
            slippage_bps: 100,
            taker: Some(
                "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"
                    .parse::<AccountAddress>()
                    .unwrap(),
            ),
        };
        let q = z.build(&req).await.unwrap();
        assert_eq!(q.min_buy_amount.raw, U256::from(99_000_000_000_000_000u128));
        assert_eq!(q.required_approvals.len(), 1);
        assert!(q.tx.is_some());
    }
}
