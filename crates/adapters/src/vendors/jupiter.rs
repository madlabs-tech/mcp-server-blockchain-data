//! `jupiter` vendor adapter.
//!
//! - `price`: Price API v3 `GET /price/v3?ids=<mint>`. Tokens without a reliable price are
//!   omitted from the response → `NotFound`, never 0.
//! - `swap_quote`: Swap API v2 `GET /swap/v2/order`. Without `taker` it is a quote; with
//!   `taker` it returns an assembled, unsigned transaction, which is reduced to its message
//!   for `UnsignedTx::Solana` (signed by the user, sent through our broadcasters, not through
//!   Jupiter `/execute`). A partially signed tx (e.g. an RFQ maker's signature) cannot travel
//!   as a bare message and is refused as `Unsupported`. The order response has no output
//!   decimals, so they come from Price v3 (`decimals`): one extra call per non-SOL quote.
//!
//! Keyless works at 0.5 RPS; `JUPITER_API_KEY` (free) raises it to 1 RPS via `x-api-key`.
//! Sources: <https://developers.jup.ag/docs/price>, <https://developers.jup.ag/docs/swap/order-and-execute>,
//! <https://developers.jup.ag/docs/portal/rate-limits>.

use super::util;

use crate::http::HttpClient;
use async_trait::async_trait;
use bdm_config::{ChainEntry, Loaded, Redacted, VendorStatus};
use bdm_domain::{Amount, AssetId, AssetRef, Price, SwapQuote, UnsignedTx};
use bdm_ports::{
    PortHandle, PortResult, PriceFeed, ProviderError, Registration, SwapQuoter, SwapRequest,
};
use bdm_protocols::solana::{spl::WRAPPED_SOL_MINT, tx};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::Value;
use std::sync::Arc;

pub const BASE_URL: &str = "https://api.jup.ag";

/// Push `jupiter` (price + swap quotes on Solana mainnet) when active.
pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status("jupiter") != VendorStatus::Active {
        return;
    }
    let Some(chain) = bdm_protocols::solana::enabled_mainnet(loaded) else {
        return;
    };
    let meta = loaded.vendor_meta("jupiter");
    let http = util::http(loaded, "jupiter");
    let key = loaded
        .key("jupiter", "api_key")
        .map(|k| Redacted::new(k.to_owned()));
    let j = Arc::new(Jupiter::new(http, BASE_URL, key, chain.clone()));
    out.push(
        Registration::new(meta)
            .global_port(PortHandle::Price(j.clone()))
            .global_port(PortHandle::SwapQuote(j)),
    );
}

pub struct Jupiter {
    http: HttpClient,
    base: String,
    key: Option<Redacted<String>>,
    chain: ChainEntry,
}

impl Jupiter {
    pub fn new(
        http: HttpClient,
        base: &str,
        key: Option<Redacted<String>>,
        chain: ChainEntry,
    ) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            key,
            chain,
        }
    }

    async fn get(&self, path_query: &str, label: &str) -> PortResult<Value> {
        let url = Redacted::new(format!("{}{path_query}", self.base));
        let headers: Vec<(&str, &str)> = self
            .key
            .as_ref()
            .map(|k| ("x-api-key", k.expose().as_str()))
            .into_iter()
            .collect();
        self.http.get_json(&url, label, &headers).await
    }

    fn mint_of(&self, asset: &AssetId) -> PortResult<String> {
        if asset.chain != self.chain.id {
            return Err(ProviderError::Unsupported(format!(
                "jupiter does not serve {}",
                asset.chain
            )));
        }
        match asset.asset {
            AssetRef::SplToken(m) => Ok(m.to_string()),
            AssetRef::Native { .. } => Ok(WRAPPED_SOL_MINT.to_owned()),
            AssetRef::Erc20(_) => Err(ProviderError::Unsupported("ERC-20".into())),
        }
    }

    /// Price v3 entry for one mint; absent → `NotFound`.
    async fn price_entry(&self, mint: &str) -> PortResult<Value> {
        let mut v = self
            .get(&format!("/price/v3?ids={mint}"), "price_v3")
            .await?;
        match v.get_mut(mint).map(Value::take) {
            Some(e) if !e.is_null() => Ok(e),
            _ => Err(ProviderError::NotFound),
        }
    }

    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn decimals_of(&self, asset: &AssetId, mint: &str) -> PortResult<u8> {
        if asset.is_native() {
            return Ok(self.chain.native.decimals);
        }
        self.price_entry(mint).await?["decimals"]
            .as_u64()
            .and_then(|d| u8::try_from(d).ok())
            .ok_or_else(|| ProviderError::Unsupported(format!("unknown decimals for {mint}")))
    }

    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn order(&self, req: &SwapRequest, taker: Option<String>) -> PortResult<SwapQuote> {
        if req.chain != self.chain.id {
            return Err(ProviderError::Unsupported(format!(
                "jupiter does not serve {}",
                req.chain
            )));
        }
        let (input, output) = (
            self.mint_of(&req.sell_asset)?,
            self.mint_of(&req.buy_asset)?,
        );
        let mut pq = format!(
            "/swap/v2/order?inputMint={input}&outputMint={output}&amount={}&slippageBps={}",
            req.sell_amount.raw, req.slippage_bps
        );
        if let Some(t) = &taker {
            pq.push_str(&format!("&taker={t}"));
        }
        let v = self.get(&pq, "swap_v2_order").await?;
        let int = |k: &str| {
            v[k].as_str()
                .and_then(|s| s.parse::<u128>().ok())
                .ok_or_else(|| ProviderError::Transient(format!("order response without {k}")))
        };
        let (out_amount, min_out) = (int("outAmount")?, int("otherAmountThreshold")?);
        let decimals = self.decimals_of(&req.buy_asset, &output).await?;
        let tx = match taker {
            None => None,
            Some(_) => {
                let raw = v["transaction"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        ProviderError::Unsupported(format!(
                            "jupiter could not assemble a transaction: {}",
                            v["errorMessage"].as_str().unwrap_or("no transaction")
                        ))
                    })?;
                let (message_base64, recent_blockhash) = tx::unsigned_message_of(raw)
                    .map_err(|e| ProviderError::Unsupported(format!("jupiter: {e}")))?;
                Some(UnsignedTx::Solana {
                    message_base64,
                    recent_blockhash,
                    last_valid_block_height: v["lastValidBlockHeight"].as_u64().ok_or_else(
                        || ProviderError::Transient("order without lastValidBlockHeight".into()),
                    )?,
                })
            }
        };
        Ok(SwapQuote {
            chain: req.chain.clone(),
            sell_asset: req.sell_asset.clone(),
            sell_amount: req.sell_amount,
            buy_asset: req.buy_asset.clone(),
            buy_amount: Amount::from_u128(out_amount, decimals),
            min_buy_amount: Amount::from_u128(min_out, decimals),
            // `priceImpactPct` units (percent vs fraction) are not documented; not guessed.
            price_impact_bps: None,
            source: "jupiter".into(),
            // ponytail: 30 s default when `expireAt` is absent; blockhash expiry still applies.
            expires_at: v["expireAt"]
                .as_i64()
                .and_then(DateTime::from_timestamp_millis)
                .unwrap_or_else(|| Utc::now() + chrono::Duration::seconds(30)),
            tx,
            required_approvals: Vec::new(),
        })
    }
}

#[async_trait]
impl PriceFeed for Jupiter {
    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn price(&self, asset: &AssetId, currency: &str) -> PortResult<Price> {
        if !currency.eq_ignore_ascii_case("USD") {
            return Err(ProviderError::Unsupported(format!(
                "jupiter quotes USD only, not {currency}"
            )));
        }
        let mint = self.mint_of(asset)?;
        let e = self.price_entry(&mint).await?;
        let value = util::dec(&e["usdPrice"])
            .filter(|p| *p > Decimal::ZERO)
            .ok_or(ProviderError::NotFound)?;
        Ok(Price {
            asset: asset.clone(),
            currency: "USD".into(),
            value,
            as_of: Utc::now(),
            source: "jupiter".into(),
            liquidity_usd: util::dec(&e["liquidity"]),
        })
    }
}

#[async_trait]
impl SwapQuoter for Jupiter {
    async fn quote(&self, req: &SwapRequest) -> PortResult<SwapQuote> {
        self.order(req, None).await
    }

    async fn build(&self, req: &SwapRequest) -> PortResult<SwapQuote> {
        let taker = req
            .taker
            .as_ref()
            .ok_or_else(|| ProviderError::Invalid("taker is required to build a swap".into()))?;
        self.order(req, Some(taker.to_string())).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use bdm_config::Registry;
    use bdm_protocols::solana::SOLANA_MAINNET;
    use bdm_testkit::wiremock::{
        matchers::{header, method, path, query_param},
        Mock, MockServer, ResponseTemplate,
    };
    use serde_json::json;
    use std::str::FromStr;
    use std::time::Duration;

    const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    const TAKER: &str = "Go5EVXxV3ob4CJVaq6YiGGUKqPmEfDo32kSmbWsYevsF";

    fn chain() -> ChainEntry {
        Registry::builtin()
            .unwrap()
            .chains
            .resolve("solana")
            .unwrap()
            .clone()
    }

    fn jup(server: &MockServer, key: Option<&str>) -> Jupiter {
        Jupiter::new(
            HttpClient::new("jupiter", Duration::from_secs(2)),
            &server.uri(),
            key.map(|k| Redacted::new(k.to_owned())),
            chain(),
        )
    }

    fn usdc() -> AssetId {
        format!("{SOLANA_MAINNET}/token:{USDC}").parse().unwrap()
    }

    fn sol() -> AssetId {
        format!("{SOLANA_MAINNET}/slip44:501").parse().unwrap()
    }

    async fn price_mock(server: &MockServer) {
        // Shape from the Price v3 docs sample (usdPrice, blockId, decimals, priceChange24h).
        Mock::given(method("GET"))
            .and(path("/price/v3"))
            .and(query_param("ids", USDC))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                USDC: {"usdPrice": 0.9998, "blockId": 348004026, "decimals": 6, "priceChange24h": -0.01}
            })))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn price_and_missing_price_is_not_found() {
        let server = MockServer::start().await;
        price_mock(&server).await;
        Mock::given(method("GET"))
            .and(path("/price/v3"))
            .and(query_param("ids", WRAPPED_SOL_MINT))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        let j = jup(&server, None);
        let p = j.price(&usdc(), "usd").await.unwrap();
        assert_eq!(p.value, Decimal::from_str("0.9998").unwrap());
        assert_eq!(p.source, "jupiter");
        assert_eq!(j.price(&sol(), "USD").await, Err(ProviderError::NotFound));
        assert!(matches!(
            j.price(&usdc(), "EUR").await,
            Err(ProviderError::Unsupported(_))
        ));
    }

    #[tokio::test]
    async fn api_key_header_when_configured() {
        let server = MockServer::start().await;
        Mock::given(header("x-api-key", "jup-free-key-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                USDC: {"usdPrice": 1, "decimals": 6}})))
            .mount(&server)
            .await;
        let p = jup(&server, Some("jup-free-key-1"))
            .price(&usdc(), "USD")
            .await
            .unwrap();
        assert_eq!(p.value, Decimal::ONE);
    }

    fn unsigned_swap_tx() -> (String, Vec<u8>) {
        // Legacy message: 1 signer, keys [taker, program], blockhash [9;32], no instructions.
        let mut m = vec![1u8, 0, 1, 2];
        m.extend(bs58::decode(TAKER).into_vec().unwrap());
        m.extend([3u8; 32]);
        m.extend([9u8; 32]);
        m.push(0);
        let mut t = vec![1u8];
        t.extend([0u8; 64]);
        t.extend(&m);
        (base64::engine::general_purpose::STANDARD.encode(t), m)
    }

    #[tokio::test]
    async fn swap_quote_and_build() {
        let server = MockServer::start().await;
        price_mock(&server).await;
        let (tx_b64, message) = unsigned_swap_tx();
        let body = |tx: Value| {
            json!({"inputMint": WRAPPED_SOL_MINT, "outputMint": USDC, "inAmount": "100000000",
                   "outAmount": "2103", "otherAmountThreshold": "2081", "swapMode": "ExactIn",
                   "slippageBps": 100, "priceImpactPct": "0.5", "transaction": tx,
                   "requestId": "r1", "lastValidBlockHeight": 250000000u64,
                   "expireAt": 1704067200000i64, "router": "metis", "mode": "ultra"})
        };
        Mock::given(method("GET"))
            .and(path("/swap/v2/order"))
            .and(query_param("taker", TAKER))
            .respond_with(ResponseTemplate::new(200).set_body_json(body(json!(tx_b64))))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/swap/v2/order"))
            .and(query_param("inputMint", WRAPPED_SOL_MINT))
            .and(query_param("amount", "100000000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body(Value::Null)))
            .with_priority(2)
            .mount(&server)
            .await;
        let j = jup(&server, None);
        let mut req = SwapRequest {
            chain: chain().id,
            sell_asset: sol(),
            buy_asset: usdc(),
            sell_amount: Amount::from_u128(100_000_000, 9),
            slippage_bps: 100,
            taker: None,
        };
        let q = j.quote(&req).await.unwrap();
        assert_eq!(q.buy_amount.format_units(), "0.002103");
        assert_eq!(q.min_buy_amount.raw.to_string(), "2081");
        assert!(q.tx.is_none());
        assert!(matches!(
            j.build(&req).await,
            Err(ProviderError::Invalid(_))
        ));

        req.taker = Some(TAKER.parse().unwrap());
        let b = j.build(&req).await.unwrap();
        let Some(UnsignedTx::Solana {
            message_base64,
            recent_blockhash,
            last_valid_block_height,
        }) = b.tx
        else {
            panic!("no tx");
        };
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(message_base64)
                .unwrap(),
            message
        );
        assert_eq!(recent_blockhash, bs58::encode([9u8; 32]).into_string());
        assert_eq!(last_valid_block_height, 250_000_000);
    }
}
