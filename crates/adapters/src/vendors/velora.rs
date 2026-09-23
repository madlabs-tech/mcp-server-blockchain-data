//! `velora` (ParaSwap) Market API, keyless (anonymous use carries a 1 bps fee).
//! Owner: `market-trading` (T1.M3). `quote` → `GET /prices` (v6.2); `build` → `/prices` then
//! `POST /transactions/{network}` + ERC-20 approval to the route's token-transfer proxy.

use super::market_util as util;

use crate::http::HttpClient;
use async_trait::async_trait;
use ems_config::{Loaded, Redacted, VendorStatus};
use ems_domain::SwapQuote;
use ems_ports::{PortHandle, PortResult, ProviderError, Registration, SwapQuoter, SwapRequest};
use serde_json::{json, Value};
use std::sync::Arc;

pub const ID: &str = "velora";
const BASE: &str = "https://api.velora.xyz";
const CHAINS: &[u64] = &[1, 10, 56, 137, 8453, 42161, 43114];

pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status(ID) != VendorStatus::Active {
        return;
    }
    let a = Arc::new(Velora::new(util::http(loaded, ID), BASE));
    out.push(Registration::new(util::meta(loaded, ID)).global_port(PortHandle::SwapQuote(a)));
}

pub struct Velora {
    http: HttpClient,
    base: String,
}

impl Velora {
    pub fn new(http: HttpClient, base: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
        }
    }

    /// `(priceRoute, network)`.
    async fn price_route(&self, req: &SwapRequest) -> PortResult<(Value, u64)> {
        let chain = util::evm_chain_id(&req.chain)?;
        if !CHAINS.contains(&chain) {
            return Err(ProviderError::Unsupported(format!(
                "velora does not cover {}",
                req.chain
            )));
        }
        let url = Redacted::new(format!(
            "{}/prices?srcToken={}&destToken={}&amount={}&srcDecimals={}&side=SELL&network={chain}&version=6.2",
            self.base,
            util::evm_token_or_native(&req.sell_asset)?,
            util::evm_token_or_native(&req.buy_asset)?,
            req.sell_amount.raw,
            req.sell_amount.decimals,
        ));
        let v = self.http.get_json(&url, "/prices", &[]).await?;
        let route = v
            .get("priceRoute")
            .cloned()
            .ok_or(ProviderError::NotFound)?;
        Ok((route, chain))
    }

    fn to_quote(req: &SwapRequest, route: &Value) -> PortResult<SwapQuote> {
        let buy = util::u256_field(&route["destAmount"], "destAmount")?;
        let decimals = route["destDecimals"]
            .as_u64()
            .and_then(|d| u8::try_from(d).ok());
        Ok(util::swap_quote(req, ID, buy, decimals, None))
    }
}

#[async_trait]
impl SwapQuoter for Velora {
    async fn quote(&self, req: &SwapRequest) -> PortResult<SwapQuote> {
        let (route, _) = self.price_route(req).await?;
        Self::to_quote(req, &route)
    }

    async fn build(&self, req: &SwapRequest) -> PortResult<SwapQuote> {
        let taker = util::evm_taker(req)?;
        let (route, chain) = self.price_route(req).await?;
        let mut q = Self::to_quote(req, &route)?;
        let body = json!({
            "srcToken": route["srcToken"],
            "destToken": route["destToken"],
            "srcAmount": route["srcAmount"],
            "srcDecimals": route["srcDecimals"],
            "destDecimals": route["destDecimals"],
            "slippage": req.slippage_bps,
            "priceRoute": route,
            "userAddress": taker,
        });
        let url = Redacted::new(format!(
            "{}/transactions/{chain}?ignoreChecks=true",
            self.base
        ));
        let v = self.http.post_json(&url, "/transactions", &body).await?;
        q.tx = Some(util::evm_tx(chain, &v)?);
        let spender = route["tokenTransferProxy"]
            .as_str()
            .or_else(|| route["contractAddress"].as_str())
            .ok_or_else(|| ProviderError::Transient("velora route has no spender".into()))?;
        q.required_approvals = util::sell_approval(req, spender)?;
        Ok(q)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::DEFAULT_TIMEOUT;
    use alloy_primitives::U256;
    use ems_domain::{AccountAddress, Amount, ChainId};
    use ems_testkit::wiremock::{
        matchers::{body_partial_json, method, path, query_param},
        Mock, MockServer, ResponseTemplate,
    };

    #[tokio::test]
    async fn quote_and_build() {
        let server = MockServer::start().await;
        let fx = |c| ems_testkit::vendor_fixture(env!("CARGO_MANIFEST_DIR"), ID, c);
        Mock::given(method("GET"))
            .and(path("/prices"))
            .and(query_param("network", "1"))
            .and(query_param("srcDecimals", "6"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fx("prices")))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/transactions/1"))
            .and(body_partial_json(json!({"slippage": 50})))
            .respond_with(ResponseTemplate::new(200).set_body_json(fx("transactions")))
            .mount(&server)
            .await;
        let v = Velora::new(HttpClient::new(ID, DEFAULT_TIMEOUT), &server.uri());
        let mut req = SwapRequest {
            chain: ChainId::evm(1),
            sell_asset: "eip155:1/erc20:0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
                .parse()
                .unwrap(),
            buy_asset: "eip155:1/erc20:0xdAC17F958D2ee523a2206206994597C13D831ec7"
                .parse()
                .unwrap(),
            sell_amount: Amount::from_u128(1_000_000_000, 6),
            slippage_bps: 50,
            taker: None,
        };
        let q = v.quote(&req).await.unwrap();
        assert_eq!(q.buy_amount, Amount::new(U256::from(999_500_000u64), 6));
        req.taker = Some(
            "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"
                .parse::<AccountAddress>()
                .unwrap(),
        );
        let b = v.build(&req).await.unwrap();
        assert!(matches!(
            b.tx,
            Some(ems_domain::UnsignedTx::Evm { chain_id: 1, .. })
        ));
        assert_eq!(b.required_approvals.len(), 1);
    }
}
