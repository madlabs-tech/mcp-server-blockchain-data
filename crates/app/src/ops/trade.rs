//! `trade` tools (T1.M3): `trade_get_swap_quote`, `trade_build_swap_tx`.
//! See the ownership table in `ops/mod.rs`.

use crate::{
    ops::market::{fan_out, parse_asset, resolve_decimals},
    Catalog, Ctx, Domain, OpOutput, Operation, Profile,
};
use alloy_primitives::{Address, U256};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ems_domain::{
    AccountAddress, Amount, AssetId, ChainFamily, DomainError, ErrorCode, SwapQuote, UnsignedTx,
};
use ems_ports::{Capability, SwapQuoter, SwapRequest};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub fn register(c: &mut Catalog) {
    c.register(TradeGetSwapQuote);
    c.register(TradeBuildSwapTx);
}

const TRADING: &[Profile] = &[Profile::Trading];
const DEFAULT_SLIPPAGE_BPS: u32 = 50;
const MAX_SLIPPAGE_BPS: u32 = 5_000;
/// Above this spread between vendors, a warning is added (manipulation / thin liquidity signal).
const WIDE_SPREAD_BPS: u32 = 100;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SwapIn {
    /// CAIP-19 asset to sell, e.g. `eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913`.
    pub sell_asset: String,
    /// CAIP-19 asset to buy on the same chain, e.g. `eip155:8453/slip44:60` (native ETH).
    pub buy_asset: String,
    /// Amount to sell in integer base units (e.g. "1000000" = 1 USDC).
    pub sell_amount: String,
    /// Max slippage in basis points (default 50 = 0.5%, max 5000). Sets min_buy_amount.
    #[serde(default)]
    pub slippage_bps: Option<u32>,
    /// Wallet that will sign. Optional for quotes (some vendors quote better with it).
    #[serde(default)]
    pub taker: Option<String>,
    /// Override token decimals if on-chain metadata is unavailable.
    #[serde(default)]
    pub sell_decimals: Option<u8>,
    #[serde(default)]
    pub buy_decimals: Option<u8>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BuildIn {
    #[serde(flatten)]
    pub swap: SwapIn,
    /// Minimum acceptable buy amount (base units), typically `best.min_buy_amount.raw` from
    /// trade_get_swap_quote. The build fails if the fresh quote cannot guarantee it.
    #[serde(default)]
    pub min_buy_amount: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Rejected {
    pub source: String,
    pub reason: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct QuoteOut {
    /// Highest buy amount among valid quotes.
    pub best: SwapQuote,
    /// All valid quotes, best first.
    pub quotes: Vec<SwapQuote>,
    /// (best − worst) / best in basis points across valid quotes. A wide spread is itself a
    /// manipulation / thin-liquidity signal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spread_bps: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejected: Vec<Rejected>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

fn min_out(buy: U256, slippage_bps: u32) -> U256 {
    buy * U256::from(10_000 - slippage_bps) / U256::from(10_000u32)
}

/// Validate vendor quotes: re-stamp buy decimals from on-chain metadata, drop expired quotes and
/// quotes whose min_buy is looser than the requested slippage (or above buy), sort best first.
/// Pure; unit-tested.
pub(crate) fn vet_quotes(
    quotes: Vec<(String, SwapQuote)>,
    slippage_bps: u32,
    buy_decimals: u8,
    need_tx: bool,
    now: DateTime<Utc>,
) -> (Vec<SwapQuote>, Vec<Rejected>) {
    let mut ok = Vec::new();
    let mut rejected = Vec::new();
    for (vendor, mut q) in quotes {
        q.buy_amount.decimals = buy_decimals;
        q.min_buy_amount.decimals = buy_decimals;
        let reason = if q.expires_at <= now {
            Some("quote already expired".to_owned())
        } else if q.buy_amount.raw.is_zero() {
            Some("zero buy amount".to_owned())
        } else if q.min_buy_amount.raw > q.buy_amount.raw {
            Some("min_buy_amount above buy_amount".to_owned())
        // 1 bp tolerance for vendor rounding.
        } else if q.min_buy_amount.raw < min_out(q.buy_amount.raw, (slippage_bps + 1).min(10_000)) {
            Some(format!(
                "min_buy_amount allows more than the requested {slippage_bps} bps slippage"
            ))
        } else if need_tx && q.tx.is_none() {
            Some("no transaction returned".to_owned())
        } else {
            None
        };
        match reason {
            Some(reason) => rejected.push(Rejected {
                source: vendor,
                reason,
            }),
            None => ok.push(q),
        }
    }
    ok.sort_by(|a, b| {
        b.buy_amount
            .raw
            .cmp(&a.buy_amount.raw)
            .then(b.min_buy_amount.raw.cmp(&a.min_buy_amount.raw))
    });
    (ok, rejected)
}

/// (best − worst) / best in bps.
pub(crate) fn spread_bps(quotes: &[SwapQuote]) -> Option<u32> {
    let best = quotes.first()?.buy_amount.raw;
    let worst = quotes.last()?.buy_amount.raw;
    let bps = (best - worst) * U256::from(10_000u32) / best;
    Some(bps.try_into().unwrap_or(u32::MAX))
}

struct Prepared {
    req: SwapRequest,
    buy_decimals: u8,
}

async fn prepare(ctx: &Ctx, input: &SwapIn, need_taker: bool) -> Result<Prepared, DomainError> {
    let sell: AssetId = parse_asset(ctx, &input.sell_asset)?;
    let buy: AssetId = parse_asset(ctx, &input.buy_asset)?;
    if sell.chain != buy.chain {
        return Err(DomainError::invalid(
            "sell_asset and buy_asset must be on the same chain",
        ));
    }
    if sell == buy {
        return Err(DomainError::invalid(
            "sell_asset and buy_asset are the same",
        ));
    }
    let chain = ctx.chain(&sell.chain.to_string())?;
    let raw = U256::from_str_radix(input.sell_amount.trim(), 10)
        .map_err(|_| DomainError::invalid("sell_amount must be an integer in base units"))?;
    if raw.is_zero() {
        return Err(DomainError::invalid("sell_amount must be > 0"));
    }
    let slippage_bps = input.slippage_bps.unwrap_or(DEFAULT_SLIPPAGE_BPS);
    if slippage_bps > MAX_SLIPPAGE_BPS {
        return Err(DomainError::invalid(format!(
            "slippage_bps must be ≤ {MAX_SLIPPAGE_BPS}"
        )));
    }
    let taker = input
        .taker
        .as_deref()
        .map(|t| AccountAddress::parse(chain.family, t))
        .transpose()?;
    if need_taker && taker.is_none() {
        return Err(DomainError::invalid(
            "taker is required to build a swap transaction",
        ));
    }
    let decimals = |over: Option<u8>, asset: AssetId| async move {
        match over {
            Some(d) => Ok(d),
            None => resolve_decimals(ctx, &asset).await.map_err(|e| {
                e.with_hint("pass sell_decimals / buy_decimals if token metadata is unavailable")
            }),
        }
    };
    let (sell_decimals, buy_decimals) = tokio::try_join!(
        decimals(input.sell_decimals, sell.clone()),
        decimals(input.buy_decimals, buy.clone())
    )?;
    Ok(Prepared {
        req: SwapRequest {
            chain: sell.chain.clone(),
            sell_asset: sell,
            buy_asset: buy,
            sell_amount: Amount::new(raw, sell_decimals),
            slippage_bps,
            taker,
        },
        buy_decimals,
    })
}

fn no_valid_quote(rejected: &[Rejected]) -> DomainError {
    let why: Vec<String> = rejected
        .iter()
        .map(|r| format!("{}: {}", r.source, r.reason))
        .collect();
    DomainError::new(
        ErrorCode::AllProvidersFailed,
        format!("no valid quote ({})", why.join("; ")),
    )
}

fn warnings_for(spread: Option<u32>) -> Vec<String> {
    spread
        .filter(|s| *s > WIDE_SPREAD_BPS)
        .map(|s| format!("vendors disagree by {s} bps: thin liquidity or manipulation; re-check before trading"))
        .into_iter()
        .collect()
}

// ------------------------------------------------------------------ trade_get_swap_quote

pub struct TradeGetSwapQuote;

#[async_trait]
impl Operation for TradeGetSwapQuote {
    type Input = SwapIn;
    type Output = QuoteOut;
    const NAME: &'static str = "trade_get_swap_quote";
    const DOMAIN: Domain = Domain::Trade;
    const DESCRIPTION: &'static str = "Swap quotes from several aggregators in parallel (default 1inch, Velora, CoW; Jupiter on Solana). Returns the best quote, all valid quotes, and the spread between vendors in bps (a wide spread signals thin liquidity or manipulation). Amounts are integer base units; decimals come from on-chain metadata. Every quote carries min_buy_amount for the requested slippage and an expires_at: re-quote after it. Indicative only; use trade_build_swap_tx to get a transaction.";
    const PROFILES: &'static [Profile] = TRADING;

    fn cache_ttl(&self) -> Option<Duration> {
        Some(Duration::from_secs(10))
    }

    async fn execute(&self, ctx: &Ctx, input: SwapIn) -> Result<OpOutput<QuoteOut>, DomainError> {
        let Prepared { req, buy_decimals } = prepare(ctx, &input, false).await?;
        let n = fan_out(ctx, Self::NAME, 3);
        let route = ctx.route(Capability::SwapQuote).chain(req.chain.clone());
        let r = &req;
        let routed = ctx
            .router()
            .aggregate::<dyn SwapQuoter, _, _, _>(route, n, |p| async move { p.quote(r).await })
            .await
            .map_err(|e| e.error)?;
        let (quotes, rejected) = vet_quotes(
            routed.value,
            req.slippage_bps,
            buy_decimals,
            false,
            Utc::now(),
        );
        if quotes.is_empty() {
            return Err(no_valid_quote(&rejected));
        }
        let spread = spread_bps(&quotes);
        Ok(OpOutput::new(
            QuoteOut {
                best: quotes[0].clone(),
                warnings: warnings_for(spread),
                quotes,
                spread_bps: spread,
                rejected,
            },
            routed.provenance,
        ))
    }
}

// ------------------------------------------------------------------ trade_build_swap_tx

#[derive(Debug, Serialize, JsonSchema)]
pub struct BuildOut {
    /// Firm quote with `tx` (unsigned) and the approvals still needed, in signing order:
    /// sign and send every `required_approvals` entry first, then `tx`.
    pub quote: SwapQuote,
    /// Spread across the vendors that built a transaction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spread_bps: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejected: Vec<Rejected>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// `(token, spender, amount)` of an ERC-20 `approve` transaction.
pub(crate) fn decode_approve(tx: &UnsignedTx) -> Option<(Address, Address, U256)> {
    let UnsignedTx::Evm { to, data, .. } = tx else {
        return None;
    };
    let hex = data.strip_prefix("0x")?;
    if hex.len() != 8 + 128 || !hex.starts_with("095ea7b3") {
        return None;
    }
    let spender: Address = format!("0x{}", &hex[8 + 24..8 + 64]).parse().ok()?;
    let amount = U256::from_str_radix(&hex[8 + 64..], 16).ok()?;
    Some((to.parse().ok()?, spender, amount))
}

pub struct TradeBuildSwapTx;

#[async_trait]
impl Operation for TradeBuildSwapTx {
    type Input = BuildIn;
    type Output = BuildOut;
    const NAME: &'static str = "trade_build_swap_tx";
    const DOMAIN: Domain = Domain::Trade;
    const DESCRIPTION: &'static str = "Build an unsigned swap transaction for an external signer (non-custodial). Re-quotes firm from the aggregators that can build transactions, picks the best, and fails if it cannot guarantee min_buy_amount (pass the value from trade_get_swap_quote). Returns the tx plus any ERC-20 approvals still needed (spender = router / AllowanceHolder / Permit2), after checking the taker's current allowance on-chain. The tx enforces min_buy_amount; sign before expires_at.";
    const PROFILES: &'static [Profile] = TRADING;

    async fn execute(&self, ctx: &Ctx, input: BuildIn) -> Result<OpOutput<BuildOut>, DomainError> {
        let floor = input
            .min_buy_amount
            .as_deref()
            .map(|s| U256::from_str_radix(s.trim(), 10))
            .transpose()
            .map_err(|_| DomainError::invalid("min_buy_amount must be an integer in base units"))?;
        let Prepared { req, buy_decimals } = prepare(ctx, &input.swap, true).await?;
        let chain = ctx.chain(&req.chain.to_string())?.clone();
        let n = fan_out(ctx, Self::NAME, 2);
        let route = ctx.route(Capability::SwapQuote).chain(req.chain.clone());
        let r = &req;
        let routed = ctx
            .router()
            .aggregate::<dyn SwapQuoter, _, _, _>(route, n, |p| async move { p.build(r).await })
            .await
            .map_err(|e| e.error)?;
        let (quotes, rejected) = vet_quotes(
            routed.value,
            req.slippage_bps,
            buy_decimals,
            true,
            Utc::now(),
        );
        let spread = spread_bps(&quotes);
        let mut best = quotes
            .into_iter()
            .next()
            .ok_or_else(|| no_valid_quote(&rejected))?;
        if let Some(floor) = floor {
            if best.min_buy_amount.raw < floor {
                return Err(DomainError::new(
                    ErrorCode::StaleData,
                    format!(
                        "fresh quote guarantees {} but min_buy_amount is {floor}; the price moved",
                        best.min_buy_amount.raw
                    ),
                )
                .with_hint("re-run trade_get_swap_quote and confirm the new price"));
            }
        }

        // Drop approvals the taker already has (allowance ≥ amount). Unknown allowance → keep.
        let mut warnings = warnings_for(spread);
        if chain.family == ChainFamily::Evm && !best.required_approvals.is_empty() {
            let rpc = ctx.evm_rpc(&chain)?;
            let Some(AccountAddress::Evm(owner)) = req.taker else {
                return Err(DomainError::internal("EVM swap without an EVM taker"));
            };
            let mut needed = Vec::new();
            for tx in std::mem::take(&mut best.required_approvals) {
                let Some((token, spender, amount)) = decode_approve(&tx) else {
                    needed.push(tx);
                    continue;
                };
                match ems_protocols::evm::erc20::allowance(&rpc, token, owner, spender, "latest")
                    .await
                {
                    Ok(have) if have >= amount => {}
                    Ok(_) => needed.push(tx),
                    Err(e) => {
                        warnings.push(format!(
                            "could not read current allowance ({}); approval included to be safe",
                            e.reason()
                        ));
                        needed.push(tx);
                    }
                }
            }
            best.required_approvals = needed;
        }
        Ok(OpOutput::new(
            BuildOut {
                quote: best,
                spread_bps: spread,
                rejected,
                warnings,
            },
            routed.provenance,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;

    fn q(source: &str, buy: u64, min: u64, ttl_secs: i64) -> (String, SwapQuote) {
        (
            source.into(),
            SwapQuote {
                chain: ems_domain::ChainId::evm(1),
                sell_asset: "eip155:1/slip44:60".parse().unwrap(),
                sell_amount: Amount::from_u128(1, 18),
                buy_asset: "eip155:1/erc20:0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
                    .parse()
                    .unwrap(),
                buy_amount: Amount::new(U256::from(buy), 0),
                min_buy_amount: Amount::new(U256::from(min), 0),
                price_impact_bps: None,
                source: source.into(),
                expires_at: Utc::now() + ChronoDuration::seconds(ttl_secs),
                tx: None,
                required_approvals: vec![],
            },
        )
    }

    #[test]
    fn vets_sorts_and_spreads() {
        let (ok, rejected) = vet_quotes(
            vec![
                q("velora", 1_000, 995, 30),
                q("oneinch", 1_010, 1_005, 30),
                q("stale", 2_000, 1_990, -1),
                q("loose", 1_020, 900, 30),
                q("tight", 990, 990, 30),
            ],
            50,
            6,
            false,
            Utc::now(),
        );
        let order: Vec<&str> = ok.iter().map(|q| q.source.as_str()).collect();
        assert_eq!(order, ["oneinch", "velora", "tight"]);
        assert!(ok
            .iter()
            .all(|q| q.buy_amount.decimals == 6 && q.min_buy_amount.decimals == 6));
        let reasons: Vec<&str> = rejected.iter().map(|r| r.source.as_str()).collect();
        assert_eq!(reasons, ["stale", "loose"]);
        // (1010 − 990) / 1010 = 198 bps
        assert_eq!(spread_bps(&ok), Some(198));
        assert_eq!(warnings_for(Some(198)).len(), 1);
        assert!(warnings_for(Some(20)).is_empty());

        let (ok, rejected) = vet_quotes(vec![q("velora", 1_000, 995, 30)], 50, 6, true, Utc::now());
        assert!(ok.is_empty());
        assert_eq!(rejected[0].reason, "no transaction returned");
    }

    #[test]
    fn decodes_approve_calldata() {
        let tx = UnsignedTx::Evm {
            chain_id: 1,
            to: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".into(),
            data: format!(
                "0x095ea7b3000000000000000000000000111111125421ca6dc452d289314280a0f8842a65{:064x}",
                1_000_000u64
            ),
            value: "0".into(),
            gas_limit: None,
            max_fee_per_gas: None,
            max_priority_fee_per_gas: None,
            nonce: None,
        };
        let (token, spender, amount) = decode_approve(&tx).unwrap();
        assert_eq!(
            token,
            "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
                .parse::<Address>()
                .unwrap()
        );
        assert_eq!(
            spender,
            "0x111111125421ca6dc452d289314280a0f8842a65"
                .parse::<Address>()
                .unwrap()
        );
        assert_eq!(amount, U256::from(1_000_000u64));
    }
}
