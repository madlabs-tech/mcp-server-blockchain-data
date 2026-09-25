//! `market` tools: `market_get_price`, `market_get_price_at`,
//! `token_get_metadata`, `token_check_risk`.

use crate::{Catalog, Ctx, Domain, OpOutput, Operation, Profile};
use async_trait::async_trait;
use bdm_domain::{
    AssetId, AssetRef, Attempt, AttemptOutcome, DomainError, ErrorCode, Price, PriceAggregate,
    PriceStatus, Provenance, RiskFlag, RiskReport, Severity, SourceKind,
};
use bdm_ports::{Capability, EvmRpc, PriceFeed, PriceHistory, SolanaRpc, TokenMetadata, TokenRisk};
use bdm_routing::RouteError;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

pub fn register(c: &mut Catalog) {
    c.register(MarketGetPrice);
    c.register(MarketGetPriceAt);
    c.register(TokenGetMetadata);
    c.register(TokenCheckRisk);
}

const TRADING: &[Profile] = &[Profile::Trading];
const TRADING_DEFI_NEOBANK: &[Profile] = &[Profile::Trading, Profile::Defi, Profile::Neobank];

// ------------------------------------------------------------------ shared helpers

/// Parse a CAIP-19 asset and check its chain is enabled.
pub(crate) fn parse_asset(ctx: &Ctx, s: &str) -> Result<AssetId, DomainError> {
    let asset: AssetId = s.trim().parse()?;
    ctx.chain(&asset.chain.to_string())?;
    Ok(asset)
}

pub(crate) fn fan_out(ctx: &Ctx, op: &str, default: usize) -> usize {
    ctx.config()
        .operation(op)
        .fan_out
        .map_or(default, usize::from)
        .max(1)
}

/// True when every vendor that was actually tried said "not found" / "can't price this", i.e.
/// the answer is *unknown* rather than an infrastructure failure.
pub(crate) fn only_unknown(e: &RouteError) -> bool {
    let tried: Vec<&Attempt> = e
        .attempts
        .iter()
        .filter(|a| a.outcome == AttemptOutcome::Failed)
        .collect();
    e.error.code == ErrorCode::NotFound
        || (!tried.is_empty()
            && tried.iter().all(|a| {
                matches!(
                    a.error_code,
                    Some(ErrorCode::NotFound | ErrorCode::UnsupportedCapability)
                )
            }))
}

pub(crate) fn provenance_from(e: RouteError, chain: &AssetId) -> Provenance {
    let mut p = Provenance::new(SourceKind::Aggregate);
    p.chain = Some(chain.chain.clone());
    p.providers_tried = e.attempts;
    p
}

pub(crate) fn rpc_provenance(chain: &bdm_domain::ChainId) -> Provenance {
    let mut p = Provenance::new(SourceKind::Primary);
    p.chain = Some(chain.clone());
    p.provider = Some(bdm_ports::RPC_VENDOR.into());
    p
}

// ------------------------------------------------------------------ market_get_price

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PriceIn {
    /// CAIP-19 asset id, e.g. `eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913`
    /// (USDC on Base) or `eip155:1/slip44:60` (ETH).
    pub asset: String,
    /// Quote currency (ISO 4217). Default `USD`.
    #[serde(default)]
    pub currency: Option<String>,
    /// Spread between sources above which the status is `divergent`. Default 200 (2%).
    #[serde(default)]
    pub max_spread_bps: Option<u32>,
    /// Sources older than this are ignored for the median; if all are, status is `stale`.
    /// Default 3600.
    #[serde(default)]
    pub max_age_secs: Option<u64>,
}

pub struct MarketGetPrice;

pub(crate) const DEFAULT_MAX_SPREAD_BPS: u32 = 200;

/// `(median, lowest, highest)` of an ascending-sorted list; `None` when empty.
pub(crate) fn median_lo_hi(sorted: &[Decimal]) -> Option<(Decimal, Decimal, Decimal)> {
    let (lo, hi) = (*sorted.first()?, *sorted.last()?);
    let n = sorted.len();
    let median = if n % 2 == 1 {
        *sorted.get(n / 2)?
    } else {
        (*sorted.get(n / 2 - 1)? + *sorted.get(n / 2)?) / Decimal::TWO
    };
    Some((median, lo, hi))
}

/// Median + spread over the sources that are fresh enough. Pure; unit-tested.
pub(crate) fn aggregate_prices(
    asset: &AssetId,
    currency: &str,
    mut sources: Vec<Price>,
    max_spread_bps: u32,
    max_age: ChronoDuration,
    now: DateTime<Utc>,
) -> PriceAggregate {
    sources.retain(|p| p.value > Decimal::ZERO && p.currency.eq_ignore_ascii_case(currency));
    let fresh: Vec<Decimal> = sources
        .iter()
        .filter(|p| now - p.as_of <= max_age)
        .map(|p| p.value)
        .collect();
    let (mut values, stale) = if fresh.is_empty() {
        (sources.iter().map(|p| p.value).collect::<Vec<_>>(), true)
    } else {
        (fresh, false)
    };
    values.sort();
    let stats = median_lo_hi(&values);
    let median = stats.map(|(m, _, _)| m);
    let spread_bps = stats.map(|(m, lo, hi)| {
        ((hi - lo) / m * Decimal::from(10_000))
            .round()
            .to_u32()
            .unwrap_or(u32::MAX)
    });
    let status = match (median, stale) {
        (None, _) => PriceStatus::Unknown,
        (Some(_), true) => PriceStatus::Stale,
        _ if values.len() >= 2 && spread_bps.unwrap_or(0) > max_spread_bps => {
            PriceStatus::Divergent
        }
        _ => PriceStatus::Ok,
    };
    PriceAggregate {
        asset: asset.clone(),
        currency: currency.to_owned(),
        status,
        median,
        spread_bps,
        sources,
    }
}

#[async_trait]
impl Operation for MarketGetPrice {
    type Input = PriceIn;
    type Output = PriceAggregate;
    const NAME: &'static str = "market_get_price";
    const DOMAIN: Domain = Domain::Market;
    const DESCRIPTION: &'static str = "Current price of a token from several independent sources: median, spread in basis points, and every source with its timestamp and pool liquidity. status is ok, divergent (sources disagree beyond max_spread_bps: possible manipulation or thin liquidity), stale, or unknown. A missing price is reported as unknown with no median, never as 0.";
    const PROFILES: &'static [Profile] = TRADING_DEFI_NEOBANK;

    fn cache_ttl(&self) -> Option<Duration> {
        Some(Duration::from_secs(15))
    }

    async fn execute(
        &self,
        ctx: &Ctx,
        input: PriceIn,
    ) -> Result<OpOutput<PriceAggregate>, DomainError> {
        let asset = parse_asset(ctx, &input.asset)?;
        let currency = input
            .currency
            .unwrap_or_else(|| "USD".into())
            .to_uppercase();
        let n = fan_out(ctx, Self::NAME, 3);
        let req = ctx.route(Capability::Price).chain(asset.chain.clone());
        let (a, cur) = (&asset, currency.as_str());
        let routed = ctx
            .router()
            .aggregate::<dyn PriceFeed, _, _, _>(req, n, |p| async move { p.price(a, cur).await })
            .await;
        let max_spread = input.max_spread_bps.unwrap_or(DEFAULT_MAX_SPREAD_BPS);
        let max_age = ChronoDuration::seconds(input.max_age_secs.unwrap_or(3600) as i64);
        match routed {
            Ok(r) => {
                let prices = r.value.into_iter().map(|(_, p)| p).collect();
                let agg =
                    aggregate_prices(&asset, &currency, prices, max_spread, max_age, Utc::now());
                Ok(OpOutput::new(agg, r.provenance))
            }
            Err(e) if only_unknown(&e) => {
                let agg =
                    aggregate_prices(&asset, &currency, vec![], max_spread, max_age, Utc::now());
                Ok(OpOutput::new(agg, provenance_from(e, &asset)))
            }
            Err(e) => Err(e.error),
        }
    }
}

// ------------------------------------------------------------------ market_get_price_at

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PriceAtIn {
    /// CAIP-19 asset id.
    pub asset: String,
    /// RFC 3339 timestamp, e.g. `2026-09-01T12:00:00Z`.
    pub at: DateTime<Utc>,
    /// Quote currency (ISO 4217). Default `USD`.
    #[serde(default)]
    pub currency: Option<String>,
    /// If the nearest data point is further than this from `at`, status is `stale`. Default 3600.
    #[serde(default)]
    pub max_gap_secs: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PriceAtOut {
    pub asset: AssetId,
    pub currency: String,
    pub requested_at: DateTime<Utc>,
    /// `ok`, `stale` (nearest point too far from `at`) or `unknown` (no source had data).
    pub status: PriceStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<Price>,
    /// Seconds between the data point and `requested_at`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap_secs: Option<i64>,
}

pub struct MarketGetPriceAt;

#[async_trait]
impl Operation for MarketGetPriceAt {
    type Input = PriceAtIn;
    type Output = PriceAtOut;
    const NAME: &'static str = "market_get_price_at";
    const DOMAIN: Domain = Domain::Market;
    const DESCRIPTION: &'static str = "Historical price of a token at a point in time, from the first source in the price_history order that has data (CoinGecko Demo covers the last 365 days). Returns the nearest data point and its distance from the requested time; unknown when no source has data (never 0).";
    const PROFILES: &'static [Profile] = TRADING_DEFI_NEOBANK;

    fn cache_ttl(&self) -> Option<Duration> {
        Some(Duration::from_secs(3600))
    }

    async fn execute(
        &self,
        ctx: &Ctx,
        input: PriceAtIn,
    ) -> Result<OpOutput<PriceAtOut>, DomainError> {
        let asset = parse_asset(ctx, &input.asset)?;
        let currency = input
            .currency
            .unwrap_or_else(|| "USD".into())
            .to_uppercase();
        if input.at > Utc::now() {
            return Err(DomainError::invalid("'at' is in the future"));
        }
        let req = ctx
            .route(Capability::PriceHistory)
            .chain(asset.chain.clone());
        let (a, cur, at) = (&asset, currency.as_str(), input.at);
        // Aggregate of 1 = failover that also moves past "not found" answers.
        let routed = ctx
            .router()
            .aggregate::<dyn PriceHistory, _, _, _>(req, 1, |p| async move {
                p.price_at(a, cur, at).await
            })
            .await;
        let max_gap = input.max_gap_secs.unwrap_or(3600) as i64;
        let mut out = PriceAtOut {
            asset: asset.clone(),
            currency,
            requested_at: input.at,
            status: PriceStatus::Unknown,
            price: None,
            gap_secs: None,
        };
        match routed {
            Ok(r) => {
                let mut meta = r.provenance;
                let (vendor, price) = r
                    .value
                    .into_iter()
                    .next()
                    .ok_or_else(|| DomainError::internal("aggregate returned no answers"))?;
                meta.provider = Some(vendor);
                let gap = (price.as_of - input.at).num_seconds();
                out.status = if gap.abs() > max_gap {
                    PriceStatus::Stale
                } else {
                    PriceStatus::Ok
                };
                out.gap_secs = Some(gap);
                out.price = Some(price);
                Ok(OpOutput::new(out, meta))
            }
            Err(e) if only_unknown(&e) => Ok(OpOutput::new(out, provenance_from(e, &asset))),
            Err(e) => Err(e.error),
        }
    }
}

// ------------------------------------------------------------------ token_get_metadata

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AssetIn {
    /// CAIP-19 asset id, e.g. `eip155:1/erc20:0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48`.
    pub asset: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TokenMetadataOut {
    pub asset: AssetId,
    pub decimals: u8,
    /// Which source the decimals came from (on-chain `rpc` first when available).
    pub decimals_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logo_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
    /// Every source that answered.
    pub sources: Vec<String>,
    /// Disagreements between sources (e.g. different decimals). Trust `decimals_source`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<String>,
}

pub struct TokenGetMetadata;

/// Decimals from the first source in routing order; other fields from the first that has them.
pub(crate) fn merge_metadata(
    asset: &AssetId,
    infos: Vec<(String, bdm_ports::TokenInfo)>,
) -> Option<TokenMetadataOut> {
    let (first_vendor, first) = infos.first()?;
    let pick =
        |f: fn(&bdm_ports::TokenInfo) -> Option<String>| infos.iter().find_map(|(_, i)| f(i));
    let conflicts = infos
        .iter()
        .skip(1)
        .filter(|(_, i)| i.decimals != first.decimals)
        .map(|(v, i)| {
            format!(
                "{v} reports {} decimals, {first_vendor} reports {}",
                i.decimals, first.decimals
            )
        })
        .collect();
    Some(TokenMetadataOut {
        asset: asset.clone(),
        decimals: first.decimals,
        decimals_source: first_vendor.clone(),
        symbol: pick(|i| i.symbol.clone()),
        name: pick(|i| i.name.clone()),
        logo_url: pick(|i| i.logo_url.clone()),
        verified: infos.iter().find_map(|(_, i)| i.verified),
        sources: infos.iter().map(|(v, _)| v.clone()).collect(),
        conflicts,
    })
}

/// Decimals of any asset: native from the chain registry, tokens via the `token_metadata` order.
pub(crate) async fn resolve_decimals(ctx: &Ctx, asset: &AssetId) -> Result<u8, DomainError> {
    if asset.is_native() {
        return Ok(ctx.chain(&asset.chain.to_string())?.native.decimals);
    }
    let req = ctx
        .route(Capability::TokenMetadata)
        .chain(asset.chain.clone());
    ctx.router()
        .aggregate::<dyn TokenMetadata, _, _, _>(req, 1, |p| async move { p.metadata(asset).await })
        .await
        .map_err(|e| e.error)?
        .value
        .first()
        .map(|(_, m)| m.decimals)
        .ok_or_else(|| DomainError::internal("aggregate returned no answers"))
}

#[async_trait]
impl Operation for TokenGetMetadata {
    type Input = AssetIn;
    type Output = TokenMetadataOut;
    const NAME: &'static str = "token_get_metadata";
    const DOMAIN: Domain = Domain::Market;
    const DESCRIPTION: &'static str = "Token decimals, symbol, name and logo. Decimals come from the chain itself (decimals() / mint account) when an on-chain source is available, then from vendor metadata; disagreements are listed in conflicts. Always use these decimals, never assume 18.";
    const PROFILES: &'static [Profile] = &[
        Profile::Trading,
        Profile::Payments,
        Profile::Defi,
        Profile::Neobank,
    ];

    fn cache_ttl(&self) -> Option<Duration> {
        Some(Duration::from_secs(3600))
    }

    async fn execute(
        &self,
        ctx: &Ctx,
        input: AssetIn,
    ) -> Result<OpOutput<TokenMetadataOut>, DomainError> {
        let asset = parse_asset(ctx, &input.asset)?;
        if asset.is_native() {
            let chain = ctx.chain(&asset.chain.to_string())?;
            return Ok(OpOutput::local(TokenMetadataOut {
                asset: asset.clone(),
                decimals: chain.native.decimals,
                decimals_source: "chain_registry".into(),
                symbol: Some(chain.native.symbol.clone()),
                name: None,
                logo_url: None,
                verified: Some(true),
                sources: vec!["chain_registry".into()],
                conflicts: vec![],
            }));
        }
        let n = fan_out(ctx, Self::NAME, 2);
        let req = ctx
            .route(Capability::TokenMetadata)
            .chain(asset.chain.clone());
        let a = &asset;
        let r = ctx
            .router()
            .aggregate::<dyn TokenMetadata, _, _, _>(req, n, |p| async move { p.metadata(a).await })
            .await
            .map_err(|e| e.error)?;
        let out = merge_metadata(&asset, r.value)
            .ok_or_else(|| DomainError::internal("aggregate returned no answers"))?;
        Ok(OpOutput::new(out, r.provenance))
    }
}

// ------------------------------------------------------------------ token_check_risk

pub struct TokenCheckRisk;

const EIP1967_IMPL: &str = "0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc";
const EIP1967_ADMIN: &str = "0xb53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d6103";

fn rpc_flag(code: &str, severity: Severity, detail: Option<String>) -> RiskFlag {
    RiskFlag {
        code: code.into(),
        severity,
        source: bdm_ports::RPC_VENDOR.into(),
        detail,
    }
}

/// Address stored in a 32-byte slot, `None` if zero.
fn slot_address(v: &Value) -> Option<String> {
    let hex = v.as_str()?.trim_start_matches("0x");
    let addr = &hex[hex.len().saturating_sub(40)..];
    (addr.len() == 40 && addr.chars().any(|c| c != '0')).then(|| format!("0x{addr}"))
}

/// EIP-1967 proxy slots (two `eth_getStorageAt` calls).
async fn evm_authority_flags(rpc: &dyn EvmRpc, token: &str) -> Result<Vec<RiskFlag>, DomainError> {
    let mut flags = Vec::new();
    for (slot, code) in [
        (EIP1967_IMPL, "proxy_upgradeable"),
        (EIP1967_ADMIN, "proxy_admin"),
    ] {
        let v = rpc
            .request("eth_getStorageAt", json!([token, slot, "latest"]))
            .await
            .map_err(DomainError::from)?;
        if let Some(a) = slot_address(&v) {
            let detail = if code == "proxy_upgradeable" {
                format!("implementation {a}")
            } else {
                format!("admin {a}")
            };
            flags.push(rpc_flag(code, Severity::Low, Some(detail)));
        }
    }
    Ok(flags)
}

/// Mint authorities and Token-2022 extensions from the parsed mint account. Pure; unit-tested.
pub(crate) fn solana_mint_flags(account: &Value) -> Option<Vec<RiskFlag>> {
    let info = account
        .pointer("/value/data/parsed/info")
        .filter(|i| i.is_object())?;
    let mut flags = Vec::new();
    if let Some(a) = info["mintAuthority"].as_str() {
        flags.push(rpc_flag(
            "mint_authority_active",
            Severity::Medium,
            Some(a.into()),
        ));
    }
    if let Some(a) = info["freezeAuthority"].as_str() {
        flags.push(rpc_flag(
            "freeze_authority_active",
            Severity::Medium,
            Some(a.into()),
        ));
    }
    for ext in info["extensions"].as_array().into_iter().flatten() {
        let state = &ext["state"];
        match ext["extension"].as_str() {
            Some("permanentDelegate") if state["delegate"].is_string() => flags.push(rpc_flag(
                "permanent_delegate",
                Severity::High,
                state["delegate"].as_str().map(str::to_owned),
            )),
            Some("transferHook") if state["programId"].is_string() => flags.push(rpc_flag(
                "transfer_hook",
                Severity::Medium,
                state["programId"].as_str().map(str::to_owned),
            )),
            Some("nonTransferable") => {
                flags.push(rpc_flag("non_transferable", Severity::High, None))
            }
            Some("defaultAccountState") if state["accountState"].as_str() == Some("frozen") => {
                flags.push(rpc_flag("default_frozen", Severity::High, None))
            }
            Some("transferFeeConfig") => {
                let bps = state
                    .pointer("/newerTransferFee/transferFeeBasisPoints")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if bps > 0 {
                    flags.push(rpc_flag(
                        "transfer_fee",
                        Severity::Low,
                        Some(format!("{bps} bps")),
                    ));
                }
            }
            Some("confidentialTransferMint") => {
                flags.push(rpc_flag("confidential_transfers", Severity::Info, None))
            }
            _ => {}
        }
    }
    Some(flags)
}

async fn solana_authority_flags(
    rpc: &dyn SolanaRpc,
    mint: &str,
) -> Result<Vec<RiskFlag>, DomainError> {
    let v = rpc
        .request("getAccountInfo", json!([mint, {"encoding": "jsonParsed"}]))
        .await
        .map_err(DomainError::from)?;
    solana_mint_flags(&v)
        .ok_or_else(|| DomainError::new(ErrorCode::NotFound, "mint account not found"))
}

#[async_trait]
impl Operation for TokenCheckRisk {
    type Input = AssetIn;
    type Output = RiskReport;
    const NAME: &'static str = "token_check_risk";
    const DOMAIN: Domain = Domain::Market;
    const DESCRIPTION: &'static str = "Scam and honeypot check before buying a token. Asks every configured risk source (GoPlus, honeypot.is on Ethereum/BSC/Base, RugCheck on Solana) plus cheap on-chain checks (Solana mint/freeze authority and Token-2022 extensions such as permanent delegate or transfer hook; EVM EIP-1967 proxy slots). Returns one merged level (the worst finding) with every flag attributed to its source. level unknown means no source answered: treat as unsafe.";
    const PROFILES: &'static [Profile] = TRADING;

    fn cache_ttl(&self) -> Option<Duration> {
        Some(Duration::from_secs(300))
    }

    async fn execute(
        &self,
        ctx: &Ctx,
        input: AssetIn,
    ) -> Result<OpOutput<RiskReport>, DomainError> {
        let asset = parse_asset(ctx, &input.asset)?;
        if asset.is_native() {
            return Err(DomainError::invalid(
                "native coins have no token risk; pass a token asset",
            ));
        }
        let chain = ctx.chain(&asset.chain.to_string())?.clone();
        let req = ctx.route(Capability::TokenRisk).chain(asset.chain.clone());
        let a = &asset;
        let vendors = ctx
            .router()
            .fan_out::<dyn TokenRisk, _, _, _>(req, |p| async move { p.assess(a).await });
        let onchain = async {
            match &asset.asset {
                AssetRef::Erc20(t) => {
                    evm_authority_flags(&ctx.evm_rpc(&chain)?, &t.to_checksum(None)).await
                }
                AssetRef::SplToken(m) => {
                    solana_authority_flags(&ctx.solana_rpc(&chain)?, &m.to_string()).await
                }
                AssetRef::Native { .. } => Ok(vec![]),
            }
        };
        // Concurrent on the current task, so metering's task-local context still applies.
        let (vendors, onchain) = tokio::join!(vendors, onchain);

        let mut flags = Vec::new();
        let mut sources = Vec::new();
        let mut meta = match vendors {
            Ok(r) => {
                for (vendor, res) in r.value {
                    if let Ok(assessment) = res {
                        sources.push(vendor);
                        flags.extend(assessment.flags);
                    }
                }
                r.provenance
            }
            Err(e) => provenance_from(e, &asset),
        };
        match onchain {
            Ok(f) => {
                sources.push(bdm_ports::RPC_VENDOR.into());
                flags.extend(f);
            }
            Err(e) => meta.providers_tried.push(Attempt {
                vendor: bdm_ports::RPC_VENDOR.into(),
                outcome: AttemptOutcome::Failed,
                reason: Some(ctx.config().scrub(&e.message)),
                error_code: Some(e.code),
                latency_ms: None,
            }),
        }
        meta.source = SourceKind::Aggregate;
        meta.provider = Some("aggregate".into());
        flags.sort_by(|a, b| b.severity.cmp(&a.severity));
        Ok(OpOutput::new(
            RiskReport {
                subject: asset.to_string(),
                level: RiskReport::merge_level(&flags, sources.len()),
                flags,
                sources,
                as_of: Utc::now(),
            },
            meta,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bdm_domain::RiskLevel;

    fn p(v: &str, secs_ago: i64, source: &str) -> Price {
        Price {
            asset: "eip155:1/slip44:60".parse().unwrap(),
            currency: "USD".into(),
            value: v.parse().unwrap(),
            as_of: Utc::now() - ChronoDuration::seconds(secs_ago),
            source: source.into(),
            liquidity_usd: None,
        }
    }

    #[test]
    fn median_spread_and_status() {
        let a: AssetId = "eip155:1/slip44:60".parse().unwrap();
        let hour = ChronoDuration::hours(1);
        let now = Utc::now();
        let ok = aggregate_prices(
            &a,
            "USD",
            vec![p("100", 0, "a"), p("101", 0, "b"), p("99", 0, "c")],
            300,
            hour,
            now,
        );
        assert_eq!(ok.median, Some(Decimal::from(100)));
        assert_eq!(ok.spread_bps, Some(200));
        assert_eq!(ok.status, PriceStatus::Ok);

        let even = aggregate_prices(
            &a,
            "USD",
            vec![p("100", 0, "a"), p("110", 0, "b")],
            200,
            hour,
            now,
        );
        assert_eq!(even.median, Some(Decimal::from(105)));
        assert_eq!(even.status, PriceStatus::Divergent, "952 bps > 200");

        // A stale source is listed but excluded from the median.
        let mixed = aggregate_prices(
            &a,
            "USD",
            vec![p("100", 0, "a"), p("50", 7200, "b")],
            200,
            hour,
            now,
        );
        assert_eq!(
            (mixed.median, mixed.status, mixed.sources.len()),
            (Some(Decimal::from(100)), PriceStatus::Ok, 2)
        );

        let stale = aggregate_prices(&a, "USD", vec![p("100", 7200, "a")], 200, hour, now);
        assert_eq!(stale.status, PriceStatus::Stale);

        // Missing price → unknown, never 0.
        let none = aggregate_prices(&a, "USD", vec![p("0", 0, "a")], 200, hour, now);
        assert_eq!(
            (none.status, none.median, none.spread_bps),
            (PriceStatus::Unknown, None, None)
        );
        let json = serde_json::to_value(&none).unwrap();
        assert!(json.get("median").is_none() || json["median"].is_null());
    }

    #[test]
    fn metadata_decimals_come_from_first_source() {
        let asset: AssetId = "eip155:56/erc20:0x55d398326f99059fF775485246999027B3197955"
            .parse()
            .unwrap();
        let info = |d, sym: Option<&str>, src: &str| bdm_ports::TokenInfo {
            asset: asset.clone(),
            decimals: d,
            symbol: sym.map(str::to_owned),
            name: None,
            logo_url: None,
            verified: None,
            source: src.into(),
        };
        let m = merge_metadata(
            &asset,
            vec![
                ("rpc".into(), info(18, None, "rpc")),
                ("coingecko".into(), info(6, Some("USDT"), "coingecko")),
            ],
        )
        .unwrap();
        assert_eq!(
            (m.decimals, m.decimals_source.as_str(), m.symbol.as_deref()),
            (18, "rpc", Some("USDT"))
        );
        assert_eq!(m.conflicts.len(), 1);
    }

    #[test]
    fn solana_mint_authorities_and_extensions() {
        let acct = json!({"value": {"data": {"parsed": {"info": {
            "mintAuthority": "Auth1111111111111111111111111111111111111111",
            "freezeAuthority": null,
            "extensions": [
                {"extension": "permanentDelegate", "state": {"delegate": "Del11111111111111111111111111111111111111111"}},
                {"extension": "transferFeeConfig", "state": {"newerTransferFee": {"transferFeeBasisPoints": 25}}}
            ]
        }}}}});
        let flags = solana_mint_flags(&acct).unwrap();
        let codes: Vec<&str> = flags.iter().map(|f| f.code.as_str()).collect();
        assert_eq!(
            codes,
            [
                "mint_authority_active",
                "permanent_delegate",
                "transfer_fee"
            ]
        );
        assert_eq!(RiskReport::merge_level(&flags, 1), RiskLevel::High);
        assert!(solana_mint_flags(&json!({"value": null})).is_none());
    }

    #[test]
    fn proxy_slot_decoding() {
        assert_eq!(slot_address(&json!(format!("0x{}", "0".repeat(64)))), None);
        let v = json!("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
        assert_eq!(
            slot_address(&v).as_deref(),
            Some("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")
        );
    }
}
