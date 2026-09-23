//! `rwa` tools (T1.M4): `rwa_token_info`, `rwa_price`. See the ownership table in `ops/mod.rs`.
//!
//! Tokenized stocks (Robinhood first): the issuer list lives in `registry/rwa.toml`; prices come
//! from the per-token Chainlink feed, judged stale by US market session (24/5), not a fixed
//! timeout. `oraclePaused()` on the token means "no price".

use crate::{
    ops::market::{parse_asset, rpc_provenance},
    Catalog, Ctx, Domain, OpOutput, Operation, Profile,
};
use alloy_primitives::{Address, U256};
use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use ems_domain::{Amount, AssetId, AssetRef, DomainError, ErrorCode, PriceStatus};
use ems_protocols::{
    evm::{chainlink, erc8056},
    market_hours::{self, Session},
    rwa::{self, RwaRegistry},
};
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{sync::OnceLock, time::Duration};

pub fn register(c: &mut Catalog) {
    c.register(RwaTokenInfo);
    c.register(RwaPrice);
}

const TRADING: &[Profile] = &[Profile::Trading];

fn registry() -> Result<&'static RwaRegistry, DomainError> {
    static REG: OnceLock<Result<RwaRegistry, String>> = OnceLock::new();
    REG.get_or_init(RwaRegistry::builtin)
        .as_ref()
        .map_err(|e| DomainError::internal(e.clone()))
}

fn evm_token(asset: &AssetId) -> Result<Address, DomainError> {
    match asset.asset {
        AssetRef::Erc20(a) => Ok(a),
        _ => Err(DomainError::invalid(
            "tokenized stocks are ERC-20 tokens; pass eip155:<chain>/erc20:<address>",
        )),
    }
}

/// 1e18-scaled multiplier as a decimal string ("1.5").
fn scaled_1e18(v: U256) -> String {
    Amount::new(v, 18).format_units()
}

fn session_name(s: Session) -> &'static str {
    match s {
        Session::PreMarket => "pre_market",
        Session::Regular => "regular",
        Session::PostMarket => "post_market",
        Session::Overnight => "overnight",
        Session::Closed => "closed",
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RwaIn {
    /// CAIP-19 token, e.g. `eip155:4663/erc20:0x322F0929c4625eD5bAd873c95208D54E1c003b2d`
    /// (Robinhood TSLA stock token).
    pub asset: String,
}

// ------------------------------------------------------------------ rwa_token_info

#[derive(Debug, Serialize, JsonSchema)]
pub struct IssuerOut {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub legal_entity: Option<String>,
    pub standard: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub official_list_url: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PendingMultiplier {
    /// 1e18-scaled raw value.
    pub raw: String,
    /// Human value, e.g. "2" after a 2-for-1 split.
    pub value: String,
    pub effective_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct MultiplierOut {
    /// 1e18-scaled raw value (ERC-8056 `uiMultiplier()`).
    pub raw: String,
    /// Shares per token, e.g. "1.0125" after reinvested dividends.
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<PendingMultiplier>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TokenInfoOut {
    pub asset: AssetId,
    /// True only when this exact address is on the issuer's official list. A token with the right
    /// ticker but another address is NOT the issuer's token.
    pub on_official_list: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer: Option<IssuerOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub underlying_ticker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decimals: Option<u8>,
    /// How splits/dividends reach holders: `ui_multiplier` (ERC-8056) or `rebase`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corporate_actions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multiplier: Option<MultiplierOut>,
    /// `oraclePaused()`: true during corporate actions (treat the price as unavailable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oracle_paused: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_feed: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

pub struct RwaTokenInfo;

#[async_trait]
impl Operation for RwaTokenInfo {
    type Input = RwaIn;
    type Output = TokenInfoOut;
    const NAME: &'static str = "rwa_token_info";
    const DOMAIN: Domain = Domain::Rwa;
    const DESCRIPTION: &'static str = "Tokenized-stock facts: issuer, underlying ticker, token standard, whether this exact address is on the issuer's official list (a matching ticker at another address is a lookalike), the ERC-8056 share multiplier (current and any scheduled change with its effective time) and oraclePaused. Robinhood Chain stock tokens first; xStocks/Ondo/Dinari lists are not loaded yet.";
    const PROFILES: &'static [Profile] = TRADING;

    fn cache_ttl(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }

    async fn execute(
        &self,
        ctx: &Ctx,
        input: RwaIn,
    ) -> Result<OpOutput<TokenInfoOut>, DomainError> {
        let asset = parse_asset(ctx, &input.asset)?;
        let token = evm_token(&asset)?;
        let reg = registry()?;
        let mut out = TokenInfoOut {
            asset: asset.clone(),
            on_official_list: false,
            issuer: None,
            underlying_ticker: None,
            decimals: None,
            corporate_actions: None,
            multiplier: None,
            oracle_paused: None,
            price_feed: None,
            warnings: vec![],
        };
        let Some((issuer, entry)) = reg.lookup(&asset) else {
            let names: Vec<&str> = reg
                .issuers_on(&asset.chain)
                .map(|i| i.name.as_str())
                .collect();
            out.warnings.push(if names.is_empty() {
                "no tokenized-stock issuer list is loaded for this chain".into()
            } else {
                format!(
                    "not on the official list of {}; a token with a matching ticker at a different address is not the issuer's token",
                    names.join(", ")
                )
            });
            return Ok(OpOutput::local(out));
        };
        out.on_official_list = true;
        out.issuer = Some(IssuerOut {
            id: issuer.id.clone(),
            name: issuer.name.clone(),
            legal_entity: issuer.legal_entity.clone(),
            standard: issuer.standard.clone(),
            official_list_url: issuer.official_list_url.clone(),
        });
        out.underlying_ticker = Some(entry.ticker.clone());
        out.decimals = issuer.decimals;
        out.corporate_actions = issuer.corporate_actions.clone();
        out.price_feed = entry.feed.clone();

        let chain = ctx.chain(&asset.chain.to_string())?.clone();
        let rpc = ctx.evm_rpc(&chain)?;
        let (mult, paused) = tokio::join!(
            async {
                if issuer.corporate_actions.as_deref() == Some("ui_multiplier") {
                    Some(erc8056::ui_multiplier(&rpc, token).await)
                } else {
                    None
                }
            },
            rwa::oracle_paused(&rpc, token)
        );
        match mult {
            Some(Ok(m)) => {
                out.multiplier = Some(MultiplierOut {
                    raw: m.current.to_string(),
                    value: scaled_1e18(m.current),
                    pending: m.pending.and_then(|(v, at)| {
                        Some(PendingMultiplier {
                            raw: v.to_string(),
                            value: scaled_1e18(v),
                            effective_at: DateTime::from_timestamp(i64::try_from(at).ok()?, 0)?,
                        })
                    }),
                })
            }
            Some(Err(e)) => out.warnings.push(format!("multiplier unavailable: {e}")),
            None => {}
        }
        match paused {
            Ok(p) => out.oracle_paused = Some(p),
            Err(e) => out.warnings.push(format!("oraclePaused unavailable: {e}")),
        }
        Ok(OpOutput::new(out, rpc_provenance(&asset.chain)))
    }
}

// ------------------------------------------------------------------ rwa_price

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RwaPriceIn {
    /// CAIP-19 token, e.g. `eip155:4663/erc20:0x322F0929c4625eD5bAd873c95208D54E1c003b2d`.
    pub asset: String,
    /// Max feed age measured from the last moment the market was open. Default 87000 s
    /// (the feeds' 24 h heartbeat + 10 min).
    #[serde(default)]
    pub max_open_age_secs: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SequencerOut {
    pub up: bool,
    pub since: DateTime<Utc>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RwaPriceOut {
    pub asset: AssetId,
    pub ticker: String,
    pub currency: String,
    /// `ok`, `stale` (no update since the market was last open, beyond the heartbeat) or
    /// `unknown` (oracle paused, sequencer down, or no answer). Never a 0 price.
    pub status: PriceStatus,
    /// Decimal price per token (multiplier already applied by the feed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round_id: Option<String>,
    pub feed: String,
    /// US equity session now: pre_market, regular, post_market, overnight or closed.
    pub session: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oracle_paused: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequencer: Option<SequencerOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Decide price + status. Pure; unit-tested.
pub(crate) fn assess_price(
    round: &chainlink::RoundData,
    paused: Option<bool>,
    sequencer_up: Option<bool>,
    now: DateTime<Utc>,
    max_open_age: ChronoDuration,
) -> (
    PriceStatus,
    Option<Decimal>,
    Option<DateTime<Utc>>,
    Option<String>,
) {
    let updated = DateTime::from_timestamp(i64::try_from(round.updated_at).unwrap_or(0), 0);
    if paused == Some(true) {
        return (
            PriceStatus::Unknown,
            None,
            updated,
            Some("oraclePaused: corporate action in progress".into()),
        );
    }
    if sequencer_up == Some(false) {
        return (
            PriceStatus::Unknown,
            None,
            updated,
            Some("L2 sequencer is down".into()),
        );
    }
    let price = i128::try_from(round.answer)
        .ok()
        .filter(|a| *a > 0)
        .and_then(|a| Decimal::try_from_i128_with_scale(a, u32::from(round.decimals)).ok());
    let (Some(price), Some(updated)) = (price, updated.filter(|_| round.updated_at > 0)) else {
        return (
            PriceStatus::Unknown,
            None,
            updated,
            Some("feed returned no valid answer".into()),
        );
    };
    if market_hours::is_stale(updated, now, max_open_age) {
        let reason = format!(
            "last update {} is older than {}s before the last open session",
            updated.to_rfc3339(),
            max_open_age.num_seconds()
        );
        return (PriceStatus::Stale, Some(price), Some(updated), Some(reason));
    }
    (PriceStatus::Ok, Some(price), Some(updated), None)
}

pub struct RwaPrice;

#[async_trait]
impl Operation for RwaPrice {
    type Input = RwaPriceIn;
    type Output = RwaPriceOut;
    const NAME: &'static str = "rwa_price";
    const DOMAIN: Domain = Domain::Rwa;
    const DESCRIPTION: &'static str = "Oracle price of a tokenized stock from its Chainlink equity feed (updates 24/5). Staleness is judged by the US market session (pre/regular/post/overnight/closed with the NYSE holiday calendar), so a Friday-evening price is fresh all weekend but stale on Tuesday. oraclePaused (corporate action) or a down sequencer means status unknown and no price. Only tokens on an issuer's official list are priced.";
    const PROFILES: &'static [Profile] = TRADING;

    fn cache_ttl(&self) -> Option<Duration> {
        Some(Duration::from_secs(15))
    }

    async fn execute(
        &self,
        ctx: &Ctx,
        input: RwaPriceIn,
    ) -> Result<OpOutput<RwaPriceOut>, DomainError> {
        let asset = parse_asset(ctx, &input.asset)?;
        let token = evm_token(&asset)?;
        let reg = registry()?;
        let (_, entry) = reg.lookup(&asset).ok_or_else(|| {
            DomainError::new(
                ErrorCode::NotFound,
                "not a tokenized stock on any loaded official list",
            )
            .with_hint("call rwa_token_info to check the address")
        })?;
        let feed: Address = entry
            .feed
            .as_deref()
            .and_then(|f| f.parse().ok())
            .ok_or_else(|| {
                DomainError::new(
                    ErrorCode::UnsupportedCapability,
                    format!("no verified price feed for {}", entry.ticker),
                )
            })?;
        let chain = ctx.chain(&asset.chain.to_string())?.clone();
        let rpc = ctx.evm_rpc(&chain)?;
        let seq_feed = reg.sequencer_feed(&asset.chain);
        let (round, paused, seq) = tokio::join!(
            chainlink::latest_round(&rpc, feed),
            rwa::oracle_paused(&rpc, token),
            async {
                match seq_feed {
                    Some(f) => Some(chainlink::latest_round(&rpc, f).await),
                    None => None,
                }
            }
        );
        let round = round.map_err(DomainError::from)?;
        let now = Utc::now();
        let mut warnings = Vec::new();
        let paused = match paused {
            Ok(p) => Some(p),
            Err(e) => {
                warnings.push(format!(
                    "oraclePaused unavailable ({e}); price not confirmed unpaused"
                ));
                None
            }
        };
        // Chainlink uptime feeds: answer 0 = up, 1 = down; updated_at = last status change.
        let sequencer = match seq {
            Some(Ok(r)) => DateTime::from_timestamp(i64::try_from(r.updated_at).unwrap_or(0), 0)
                .map(|since| SequencerOut {
                    up: r.answer.is_zero(),
                    since,
                }),
            Some(Err(e)) => {
                warnings.push(format!("sequencer uptime feed unavailable ({e})"));
                None
            }
            None => {
                warnings.push("no verified sequencer uptime feed for this chain".into());
                None
            }
        };
        if !market_hours::calendar_covers(market_hours::to_eastern(now).date()) {
            warnings.push(
                "holiday calendar does not cover this year; only weekends are treated as closed"
                    .into(),
            );
        }
        let max_age = ChronoDuration::seconds(input.max_open_age_secs.unwrap_or(87_000) as i64);
        let (status, price, updated_at, reason) = assess_price(
            &round,
            paused,
            sequencer.as_ref().map(|s| s.up),
            now,
            max_age,
        );
        Ok(OpOutput::new(
            RwaPriceOut {
                asset: asset.clone(),
                ticker: entry.ticker.clone(),
                currency: "USD".into(),
                status,
                price: price.map(|p| p.normalize().to_string()),
                updated_at,
                round_id: Some(round.round_id.to_string()),
                feed: feed.to_checksum(None),
                session: session_name(market_hours::session_at(now)).into(),
                oracle_paused: paused,
                sequencer,
                reason,
                warnings,
            },
            rpc_provenance(&asset.chain),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::I256;

    fn round(answer: i64, updated: DateTime<Utc>) -> chainlink::RoundData {
        chainlink::RoundData {
            round_id: 7,
            answer: I256::try_from(answer).unwrap(),
            decimals: 8,
            updated_at: updated.timestamp() as u64,
        }
    }

    /// New York local → UTC for 2026 dates in EDT (UTC−4).
    fn ny_edt(m: u32, d: u32, h: u32) -> DateTime<Utc> {
        chrono::NaiveDate::from_ymd_opt(2026, m, d)
            .unwrap()
            .and_hms_opt(h + 4, 0, 0)
            .unwrap()
            .and_utc()
    }

    #[test]
    fn weekend_price_is_fresh_tuesday_is_stale() {
        let max = ChronoDuration::seconds(87_000);
        let fri = ny_edt(9, 25, 19); // Fri 19:00 ET
        let r = round(25_012_345_678, fri);
        let (s, p, _, _) = assess_price(&r, Some(false), None, ny_edt(9, 26, 12), max);
        assert_eq!(
            (s, p),
            (PriceStatus::Ok, Some(Decimal::new(25_012_345_678, 8)))
        );
        let (s, p, _, reason) = assess_price(&r, Some(false), None, ny_edt(9, 29, 11), max);
        assert_eq!(s, PriceStatus::Stale);
        assert!(p.is_some() && reason.is_some());
    }

    #[test]
    fn paused_or_sequencer_down_means_no_price() {
        let now = ny_edt(9, 23, 11);
        let r = round(25_000_000_000, now);
        let max = ChronoDuration::hours(1);
        let (s, p, _, reason) = assess_price(&r, Some(true), None, now, max);
        assert_eq!((s, p), (PriceStatus::Unknown, None));
        assert!(reason.unwrap().contains("oraclePaused"));
        assert_eq!(
            assess_price(&r, Some(false), Some(false), now, max).0,
            PriceStatus::Unknown
        );
        // Non-positive answers are unknown, never 0.
        assert_eq!(assess_price(&round(0, now), None, None, now, max).1, None);
        assert_eq!(
            assess_price(&round(-5, now), None, None, now, max).0,
            PriceStatus::Unknown
        );
    }

    #[test]
    fn builtin_registry_prices_tsla() {
        let reg = registry().unwrap();
        let tsla: AssetId = "eip155:4663/erc20:0x322F0929c4625eD5bAd873c95208D54E1c003b2d"
            .parse()
            .unwrap();
        let (issuer, t) = reg.lookup(&tsla).unwrap();
        assert_eq!(
            (issuer.id.as_str(), t.ticker.as_str()),
            ("robinhood", "TSLA")
        );
        assert!(t.feed.is_some());
        assert_eq!(
            scaled_1e18(U256::from(1_500_000_000_000_000_000u128)),
            "1.5"
        );
    }
}
