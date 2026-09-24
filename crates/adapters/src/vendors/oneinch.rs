//! `oneinch` Swap API (key, 1 req/s, 100k/month). Owner: `market-trading` (T1.M3).
//! `quote` → `/quote`; `build` → `/swap` (needs `taker`) + ERC-20 approval to the router
//! (the swap tx's `to`). Robinhood Chain (4663) is supported per 1inch docs.

use super::market_util as util;

use crate::http::HttpClient;
use async_trait::async_trait;
use bdm_config::{Loaded, Redacted, VendorStatus};
use bdm_domain::SwapQuote;
use bdm_ports::{PortHandle, PortResult, ProviderError, Registration, SwapQuoter, SwapRequest};
use serde_json::Value;
use std::sync::Arc;

pub const ID: &str = "oneinch";
const BASE: &str = "https://api.1inch.com/swap/v6.1";
const CHAINS: &[u64] = &[1, 10, 56, 137, 8453, 42161, 43114, 4663];

pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status(ID) != VendorStatus::Active {
        return;
    }
    let Some(key) = loaded.key(ID, "api_key") else {
        return;
    };
    let a = Arc::new(OneInch::new(util::http(loaded, ID), BASE, key));
    out.push(Registration::new(util::meta(loaded, ID)).global_port(PortHandle::SwapQuote(a)));
}

pub struct OneInch {
    http: HttpClient,
    base: String,
    auth: Redacted<String>,
}

impl OneInch {
    pub fn new(http: HttpClient, base: &str, key: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            auth: Redacted::new(format!("Bearer {key}")),
        }
    }

    async fn call(&self, req: &SwapRequest, endpoint: &str, extra: &str) -> PortResult<Value> {
        let chain = util::evm_chain_id(&req.chain)?;
        if !CHAINS.contains(&chain) {
            return Err(ProviderError::Unsupported(format!(
                "1inch does not cover {}",
                req.chain
            )));
        }
        let url = Redacted::new(format!(
            "{}/{chain}/{endpoint}?src={}&dst={}&amount={}&includeTokensInfo=true{extra}",
            self.base,
            util::evm_token_or_native(&req.sell_asset)?,
            util::evm_token_or_native(&req.buy_asset)?,
            req.sell_amount.raw,
        ));
        self.http
            .get_json(
                &url,
                &format!("/{endpoint}"),
                &[("authorization", self.auth.expose())],
            )
            .await
    }

    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    fn to_quote(req: &SwapRequest, v: &Value) -> PortResult<SwapQuote> {
        let buy = util::u256_field(&v["dstAmount"], "dstAmount")?;
        let decimals = v["dstToken"]["decimals"]
            .as_u64()
            .and_then(|d| u8::try_from(d).ok());
        Ok(util::swap_quote(req, ID, buy, decimals, None))
    }
}

#[async_trait]
impl SwapQuoter for OneInch {
    async fn quote(&self, req: &SwapRequest) -> PortResult<SwapQuote> {
        let v = self.call(req, "quote", "").await?;
        Self::to_quote(req, &v)
    }

    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn build(&self, req: &SwapRequest) -> PortResult<SwapQuote> {
        let taker = util::evm_taker(req)?;
        let extra = format!(
            "&from={taker}&origin={taker}&slippage={}&disableEstimate=true",
            util::slippage_percent(req.slippage_bps)
        );
        let v = self.call(req, "swap", &extra).await?;
        let mut q = Self::to_quote(req, &v)?;
        let tx = util::evm_tx(util::evm_chain_id(&req.chain)?, &v["tx"])?;
        if let bdm_domain::UnsignedTx::Evm { to, .. } = &tx {
            q.required_approvals = util::sell_approval(req, to)?;
        }
        q.tx = Some(tx);
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

    fn req(taker: Option<&str>) -> SwapRequest {
        SwapRequest {
            chain: ChainId::evm(8453),
            sell_asset: "eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
                .parse()
                .unwrap(),
            buy_asset: "eip155:8453/slip44:60".parse().unwrap(),
            sell_amount: Amount::from_u128(1_000_000_000, 6),
            slippage_bps: 50,
            taker: taker.map(|t| t.parse::<AccountAddress>().unwrap()),
        }
    }

    #[tokio::test]
    async fn quote_and_build_with_approval() {
        let server = MockServer::start().await;
        let fx = |c| bdm_testkit::vendor_fixture(env!("CARGO_MANIFEST_DIR"), ID, c);
        Mock::given(method("GET"))
            .and(path("/8453/quote"))
            .and(header("authorization", "Bearer 1inch-key"))
            .and(query_param("dst", util::EVM_NATIVE))
            .respond_with(ResponseTemplate::new(200).set_body_json(fx("quote")))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/8453/swap"))
            .and(query_param("slippage", "0.5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fx("swap")))
            .mount(&server)
            .await;
        let o = OneInch::new(
            HttpClient::new(ID, DEFAULT_TIMEOUT),
            &server.uri(),
            "1inch-key",
        );
        let q = o.quote(&req(None)).await.unwrap();
        assert_eq!(
            q.buy_amount,
            Amount::new(U256::from(382_000_000_000_000_000u128), 18)
        );
        assert_eq!(
            q.min_buy_amount.raw,
            U256::from(380_090_000_000_000_000u128)
        );
        assert!(q.tx.is_none());

        assert!(matches!(
            o.build(&req(None)).await,
            Err(ProviderError::Invalid(_))
        ));
        let b = o
            .build(&req(Some("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")))
            .await
            .unwrap();
        assert!(b.tx.is_some());
        assert_eq!(b.required_approvals.len(), 1);
    }
}
