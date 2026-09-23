//! `stablecoin` tools. See the ownership table in `ops/mod.rs`.
//!
//! Also hosts the helpers shared with `payments` and `compliance`: the built-in registry,
//! canonical-token resolution, and issuer-restriction checks over the routed chain RPC.

use crate::{Catalog, Ctx, Domain, OpOutput, Operation, Profile};
use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use ems_config::ChainEntry;
use ems_domain::{
    AccountAddress, AssetId, ChainFamily, DomainError, Price, PriceStatus, Provenance, SourceKind,
};
use ems_ports::{Capability, PriceFeed, TokenMetadata};
use ems_protocols::{
    evm::chainlink,
    issuer::{self, ChainRpc, Restrictions},
    stablecoins::{asset_on, FreezeCheck, PauseCheck, StablecoinEntry, StablecoinRegistry},
};
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{sync::LazyLock, time::Duration};

pub fn register(c: &mut Catalog) {
    c.register(Resolve);
    c.register(CheckRestrictions);
    c.register(Peg);
}

const PROFILES: &[Profile] = &[Profile::Payments, Profile::Neobank];

// ------------------------------------------------------------------ shared helpers

static REGISTRY: LazyLock<Result<StablecoinRegistry, String>> =
    LazyLock::new(StablecoinRegistry::builtin);

/// The built-in canonical stablecoin registry (validated at first use; CI tests keep it valid).
pub(crate) fn registry() -> Result<&'static StablecoinRegistry, DomainError> {
    REGISTRY
        .as_ref()
        .map_err(|e| DomainError::internal(format!("stablecoin registry invalid: {e}")))
}

/// Canonical entry for `token` (CAIP-19, contract/mint, or symbol) on `chain`, or an error that
/// lists what the chain supports. Unknown contracts are never matched by symbol.
pub(crate) fn resolve_canonical(
    chain: &ChainEntry,
    token: &str,
) -> Result<&'static StablecoinEntry, DomainError> {
    let reg = registry()?;
    reg.resolve(&chain.id, token).ok_or_else(|| {
        let known: Vec<&str> = reg
            .for_chain(&chain.id)
            .map(|e| e.symbol.as_str())
            .collect();
        DomainError::invalid(format!(
            "'{token}' is not a canonical stablecoin on {} ({})",
            chain.name, chain.id
        ))
        .with_hint(if known.is_empty() {
            "no verified stablecoins are registered for this chain".to_owned()
        } else {
            format!("registered here: {}", known.join(", "))
        })
    })
}

pub(crate) fn parse_account(chain: &ChainEntry, s: &str) -> Result<AccountAddress, DomainError> {
    AccountAddress::parse(chain.family, s)
}

/// Entries on `chain` that have any issuer control worth reading.
pub(crate) fn controlled(chain: &ChainEntry) -> Result<Vec<&'static StablecoinEntry>, DomainError> {
    Ok(registry()?
        .for_chain(&chain.id)
        .filter(|e| {
            e.freeze_check != FreezeCheck::None
                || e.pause_check != PauseCheck::None
                || e.deprecated_check
        })
        .collect())
}

/// Issuer restrictions for `address` on each entry; per-token failures are returned, not fatal.
pub(crate) async fn restrictions_for(
    ctx: &Ctx,
    chain: &ChainEntry,
    address: &AccountAddress,
    entries: &[&StablecoinEntry],
) -> Result<(Vec<Restrictions>, Vec<TokenError>), DomainError> {
    let (evm, sol) = match chain.family {
        ChainFamily::Evm => (Some(ctx.evm_rpc(chain)?), None),
        ChainFamily::Solana => (None, Some(ctx.solana_rpc(chain)?)),
    };
    let rpc = match (&evm, &sol) {
        (Some(e), _) => ChainRpc::Evm(e),
        (_, Some(s)) => ChainRpc::Solana(s),
        _ => unreachable!("one transport per family"),
    };
    let (mut ok, mut errors) = (Vec::new(), Vec::new());
    // ponytail: tokens are checked sequentially (≤ 7 per chain today).
    for e in entries {
        match issuer::check_restrictions(rpc, e, address).await {
            Ok(r) => ok.push(r),
            Err(err) => errors.push(TokenError {
                symbol: e.symbol.clone(),
                error: DomainError::from(err).to_string(),
            }),
        }
    }
    Ok((ok, errors))
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct TokenError {
    pub symbol: String,
    pub error: String,
}

// ------------------------------------------------------------------ stablecoin_resolve

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ResolveIn {
    /// Contract/mint address, CAIP-19 id, or symbol (with `chain`).
    #[serde(default)]
    pub token: Option<String>,
    /// Chain alias or CAIP-2 id. Omit (with a symbol) to list every chain's deployment.
    #[serde(default)]
    pub chain: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ResolveOut {
    /// True when `token` is a verified canonical deployment.
    pub canonical: bool,
    /// Matching registry entries (one for an address; one per chain for a bare symbol).
    pub entries: Vec<StablecoinEntry>,
    /// Symbol reported on-chain/by metadata for an unknown token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_symbol: Option<String>,
    /// Unknown token whose symbol matches a canonical stablecoin on the chain (likely a spoof).
    /// Null when the symbol could not be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lookalike: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lookalike_of: Vec<AssetId>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Canonical entries on the token's chain sharing `symbol` (the spoof targets).
pub fn lookalikes(reg: &StablecoinRegistry, asset: &AssetId, symbol: &str) -> Vec<AssetId> {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_uppercase()
    };
    let target = norm(symbol);
    reg.for_chain(&asset.chain)
        .filter(|e| {
            &e.asset != asset
                && (norm(&e.symbol) == target || e.aliases.iter().any(|a| norm(a) == target))
        })
        .map(|e| e.asset.clone())
        .collect()
}

pub struct Resolve;

#[async_trait]
impl Operation for Resolve {
    type Input = ResolveIn;
    type Output = ResolveOut;
    const NAME: &'static str = "stablecoin_resolve";
    const DOMAIN: Domain = Domain::Stablecoin;
    const DESCRIPTION: &'static str = "Resolve a stablecoin against the verified registry: address or CAIP-19 → issuer, decimals, issuance (native/oft/bridged/exchange_peg), freeze/pause methods, CCTP domain, Solana program/extensions and source URL; or (chain, symbol) → the canonical contract; or a bare symbol → every chain's deployment. \
Use it before sending, quoting or accepting a token: it's the only safe way to map \"USDC on Base\" to a contract. \
An unknown contract whose symbol matches a canonical coin is flagged lookalike=true (likely spoof). \
Caveats: the registry only contains issuer-documented deployments; canonical=false means \"not verified here\", not necessarily malicious.";
    const PROFILES: &'static [Profile] = PROFILES;

    fn cache_ttl(&self) -> Option<Duration> {
        Some(Duration::from_secs(3600))
    }

    async fn execute(
        &self,
        ctx: &Ctx,
        input: ResolveIn,
    ) -> Result<OpOutput<ResolveOut>, DomainError> {
        let reg = registry()?;
        let token = input
            .token
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty());
        let caip: Option<AssetId> = token.and_then(|t| t.parse().ok());
        let chain = match (&input.chain, &caip) {
            (Some(c), _) => Some(ctx.chain(c)?),
            (None, Some(a)) => Some(ctx.chain(&a.chain.to_string())?),
            (None, None) => None,
        };
        let mut out = ResolveOut {
            canonical: false,
            entries: Vec::new(),
            observed_symbol: None,
            lookalike: None,
            lookalike_of: Vec::new(),
            notes: Vec::new(),
        };
        let Some(token) = token else {
            return Err(DomainError::invalid(
                "give `token` (address, CAIP-19 or symbol)",
            ));
        };
        let Some(chain) = chain else {
            // Bare symbol: every chain.
            out.entries = reg
                .all()
                .iter()
                .filter(|e| e.matches_symbol(token))
                .cloned()
                .collect();
            out.canonical = !out.entries.is_empty();
            return Ok(OpOutput::local(out));
        };
        if let Some(e) = reg.resolve(&chain.id, token) {
            out.canonical = true;
            out.entries.push(e.clone());
            let mut o = OpOutput::local(out);
            o.meta.chain = Some(chain.id.clone());
            return Ok(o);
        }
        let asset = match caip.or_else(|| asset_on(&chain.id, token)) {
            Some(a) => a,
            None => {
                out.notes
                    .push(format!("no canonical '{token}' registered on {}", chain.id));
                let mut o = OpOutput::local(out);
                o.meta.chain = Some(chain.id.clone());
                return Ok(o);
            }
        };
        // Unknown contract: read its symbol to detect look-alikes.
        let meta = ctx
            .router()
            .failover::<dyn TokenMetadata, _, _, _>(
                ctx.route(Capability::TokenMetadata).chain(chain.id.clone()),
                |p| {
                    let a = asset.clone();
                    async move { p.metadata(&a).await }
                },
            )
            .await;
        let mut prov = match meta {
            Ok(r) => {
                if let Some(sym) = &r.value.symbol {
                    out.lookalike_of = lookalikes(reg, &asset, sym);
                    out.lookalike = Some(!out.lookalike_of.is_empty());
                    out.observed_symbol = Some(sym.clone());
                }
                r.provenance
            }
            Err(e) => {
                out.notes
                    .push(format!("could not read token symbol: {}", e.error.message));
                let mut p = Provenance::new(SourceKind::Primary);
                p.providers_tried = e.attempts;
                p
            }
        };
        out.notes
            .push(format!("{asset} is not a verified canonical stablecoin"));
        prov.chain = Some(chain.id.clone());
        Ok(OpOutput::new(out, prov))
    }
}

// ------------------------------------------------------------------ stablecoin_check_restrictions

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CheckRestrictionsIn {
    /// Chain alias or CAIP-2 id.
    pub chain: String,
    /// Address to check (EVM address; Solana owner wallet).
    pub address: String,
    /// One token (symbol, address, CAIP-19). Omit to check every registered stablecoin on the chain.
    #[serde(default)]
    pub token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct CheckRestrictionsOut {
    pub address: AccountAddress,
    /// True if any token blocks this address (frozen/blacklisted) or is paused.
    pub restricted: bool,
    pub results: Vec<Restrictions>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<TokenError>,
}

pub struct CheckRestrictions;

#[async_trait]
impl Operation for CheckRestrictions {
    type Input = CheckRestrictionsIn;
    type Output = CheckRestrictionsOut;
    const NAME: &'static str = "stablecoin_check_restrictions";
    const DOMAIN: Domain = Domain::Stablecoin;
    const DESCRIPTION: &'static str = "Check issuer controls for an address on-chain: is it blacklisted/blocked/frozen by the issuer (Circle isBlacklisted, Tether isBlackListed/isBlocked, Paxos isFrozen, Ripple accountPaused, Solana frozen token account), is the token paused or deprecated, does a Solana mint have a permanent delegate or frozen-by-default accounts. \
Use it right before sending or accepting a stablecoin, or to explain a failed transfer. Omit `token` to check every registered stablecoin on the chain. \
Every read is pinned to one block, reported per token (`block`). \
Caveats: an address can be frozen after this check (re-check right before sending); freezes are per token and per chain; on Solana pass the owner wallet.";
    const PROFILES: &'static [Profile] = PROFILES;

    async fn execute(
        &self,
        ctx: &Ctx,
        input: CheckRestrictionsIn,
    ) -> Result<OpOutput<CheckRestrictionsOut>, DomainError> {
        let chain = ctx.chain(&input.chain)?;
        let address = parse_account(chain, &input.address)?;
        let entries = match &input.token {
            Some(t) => vec![resolve_canonical(chain, t)?],
            None => controlled(chain)?,
        };
        if entries.is_empty() {
            return Err(DomainError::invalid(format!(
                "no stablecoins with issuer controls are registered for {}",
                chain.id
            )));
        }
        let (results, errors) = restrictions_for(ctx, chain, &address, &entries).await?;
        if results.is_empty() {
            return Err(DomainError::new(
                ems_domain::ErrorCode::AllProvidersFailed,
                errors
                    .iter()
                    .map(|e| format!("{}: {}", e.symbol, e.error))
                    .collect::<Vec<_>>()
                    .join("; "),
            ));
        }
        let mut out = OpOutput::local(CheckRestrictionsOut {
            address,
            restricted: results.iter().any(|r| r.restricted),
            results,
            errors,
        });
        out.meta.provider = Some("rpc".into());
        out.meta.chain = Some(chain.id.clone());
        out.meta.block = out.data.results.first().map(|r| r.block.clone());
        Ok(out)
    }
}

// ------------------------------------------------------------------ stablecoin_peg

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PegIn {
    /// Chain alias or CAIP-2 id.
    pub chain: String,
    /// Token symbol, address or CAIP-19 (canonical registry stablecoin).
    pub token: String,
    /// Flag a depeg when the median deviates from 1.00 (in the peg currency) by more than this
    /// many basis points (default 50 = 0.5%).
    #[serde(default)]
    pub depeg_threshold_bps: Option<u32>,
    /// Flag sources as divergent when (max − min) / median exceeds this (default 100 bps).
    #[serde(default)]
    pub spread_threshold_bps: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct PegOut {
    pub asset: AssetId,
    pub symbol: String,
    /// Peg currency the prices are quoted in (ISO 4217).
    pub peg: String,
    pub status: PriceStatus,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "rust_decimal::serde::str_option"
    )]
    #[schemars(with = "Option<String>")]
    pub median: Option<Decimal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spread_bps: Option<u32>,
    /// |median − 1| in basis points.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deviation_bps: Option<u32>,
    /// True when deviation_bps > depeg_threshold_bps; null when no source could price it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depeg: Option<bool>,
    pub depeg_threshold_bps: u32,
    pub spread_threshold_bps: u32,
    pub sources: Vec<Price>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

fn bps(num: Decimal, den: Decimal) -> Option<u32> {
    if den.is_zero() {
        return None;
    }
    let v = (num / den * Decimal::from(10_000)).round();
    u32::try_from(v.mantissa() / 10i128.pow(v.scale())).ok()
}

/// Median + spread + deviation from par. Pure. A missing price is `unknown`, never 0.
pub fn peg_verdict(
    entry: &StablecoinEntry,
    sources: Vec<Price>,
    depeg: u32,
    spread: u32,
) -> PegOut {
    let mut values: Vec<Decimal> = sources.iter().map(|p| p.value).collect();
    values.sort();
    let median = match values.len() {
        0 => None,
        n if n % 2 == 1 => Some(values[n / 2]),
        n => Some((values[n / 2 - 1] + values[n / 2]) / Decimal::TWO),
    };
    let spread_bps = median.and_then(|m| bps(values[values.len() - 1] - values[0], m));
    let deviation_bps = median.and_then(|m| bps((m - Decimal::ONE).abs(), Decimal::ONE));
    let status = match (median, spread_bps) {
        (None, _) => PriceStatus::Unknown,
        (_, Some(s)) if s > spread => PriceStatus::Divergent,
        _ => PriceStatus::Ok,
    };
    let mut notes = Vec::new();
    if sources.len() == 1 {
        notes.push("single source: no cross-check".into());
    }
    PegOut {
        asset: entry.asset.clone(),
        symbol: entry.symbol.clone(),
        peg: entry.peg.clone(),
        status,
        median,
        spread_bps,
        deviation_bps,
        depeg: deviation_bps.map(|d| d > depeg),
        depeg_threshold_bps: depeg,
        spread_threshold_bps: spread,
        sources,
        notes,
    }
}

pub struct Peg;

#[async_trait]
impl Operation for Peg {
    type Input = PegIn;
    type Output = PegOut;
    const NAME: &'static str = "stablecoin_peg";
    const DOMAIN: Domain = Domain::Stablecoin;
    const DESCRIPTION: &'static str = "Check whether a canonical stablecoin is holding its peg: asks up to 3 price sources (the configured `price` vendors, plus the Chainlink feed when the registry has one), returns the median in the peg currency (USD, or EUR for EURC), the spread between sources, the deviation from 1.00, and depeg=true when the deviation exceeds the threshold (default 50 bps). \
Use it before treating a stablecoin as worth exactly 1 unit (valuations, swaps, collateral) or to monitor issuer risk. \
Caveats: status=divergent means sources disagree beyond the spread threshold (possible thin liquidity or manipulation); oracle feeds update rarely by design; status=unknown means no source priced it (never read that as 0).";
    const PROFILES: &'static [Profile] = &[Profile::Payments, Profile::Neobank, Profile::Trading];

    fn cache_ttl(&self) -> Option<Duration> {
        Some(Duration::from_secs(30))
    }

    async fn execute(&self, ctx: &Ctx, input: PegIn) -> Result<OpOutput<PegOut>, DomainError> {
        let chain = ctx.chain(&input.chain)?;
        let entry = resolve_canonical(chain, &input.token)?;
        let mut notes = Vec::new();
        let (mut sources, mut meta) = match ctx
            .router()
            .aggregate::<dyn PriceFeed, _, _, _>(
                ctx.route(Capability::Price).chain(chain.id.clone()),
                3,
                |p| async move { p.price(&entry.asset, &entry.peg).await },
            )
            .await
        {
            Ok(r) => (
                r.value.into_iter().map(|(_, p)| p).collect::<Vec<_>>(),
                r.provenance,
            ),
            Err(e) => {
                notes.push(format!("price vendors: {}", e.error.message));
                let mut p = Provenance::new(SourceKind::Aggregate);
                p.providers_tried = e.attempts;
                (Vec::new(), p)
            }
        };
        if let (Some(feed), ChainFamily::Evm) = (&entry.chainlink_feed, chain.family) {
            match chainlink_price(ctx, chain, entry, feed).await {
                Ok(p) => sources.push(p),
                Err(e) => notes.push(format!("chainlink: {}", e.message)),
            }
        }
        let mut out = peg_verdict(
            entry,
            sources,
            input.depeg_threshold_bps.unwrap_or(50),
            input.spread_threshold_bps.unwrap_or(100),
        );
        out.notes.extend(notes);
        meta.chain = Some(chain.id.clone());
        Ok(OpOutput::new(out, meta))
    }
}

async fn chainlink_price(
    ctx: &Ctx,
    chain: &ChainEntry,
    entry: &StablecoinEntry,
    feed: &str,
) -> Result<Price, DomainError> {
    let feed = feed
        .parse()
        .map_err(|_| DomainError::internal(format!("bad chainlink feed {feed}")))?;
    let rpc = ctx.evm_rpc(chain)?;
    let r = chainlink::latest_round(&rpc, feed).await?;
    let mut value: Decimal = r
        .answer
        .to_string()
        .parse()
        .map_err(|_| DomainError::internal("chainlink answer out of range"))?;
    value
        .set_scale(r.decimals as u32)
        .map_err(|_| DomainError::internal("chainlink decimals out of range"))?;
    Ok(Price {
        asset: entry.asset.clone(),
        currency: entry.peg.clone(),
        value,
        as_of: Utc
            .timestamp_opt(r.updated_at as i64, 0)
            .single()
            .unwrap_or_else(Utc::now),
        source: "chainlink".into(),
        liquidity_usd: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ems_domain::ChainId;

    fn usdc() -> &'static StablecoinEntry {
        registry()
            .unwrap()
            .by_symbol(&ChainId::evm(8453), "USDC")
            .unwrap()
    }

    fn price(v: &str, source: &str) -> Price {
        Price {
            asset: usdc().asset.clone(),
            currency: "USD".into(),
            value: v.parse().unwrap(),
            as_of: Utc::now(),
            source: source.into(),
            liquidity_usd: None,
        }
    }

    #[test]
    fn peg_ok_depeg_divergent_unknown() {
        let ok = peg_verdict(
            usdc(),
            vec![
                price("0.9999", "a"),
                price("1.0001", "b"),
                price("1.0000", "c"),
            ],
            50,
            100,
        );
        assert_eq!(ok.status, PriceStatus::Ok);
        assert_eq!(ok.median, Some("1.0000".parse().unwrap()));
        assert_eq!(ok.spread_bps, Some(2));
        assert_eq!(ok.depeg, Some(false));

        let dep = peg_verdict(
            usdc(),
            vec![price("0.97", "a"), price("0.975", "b")],
            50,
            100,
        );
        assert_eq!(dep.median, Some("0.9725".parse().unwrap()));
        assert_eq!(dep.deviation_bps, Some(275));
        assert_eq!(dep.depeg, Some(true));
        assert_eq!(dep.status, PriceStatus::Ok);

        let div = peg_verdict(
            usdc(),
            vec![price("1.00", "a"), price("0.95", "b"), price("1.00", "c")],
            50,
            100,
        );
        assert_eq!(div.status, PriceStatus::Divergent);
        assert_eq!(div.depeg, Some(false), "median still at par");

        let none = peg_verdict(usdc(), vec![], 50, 100);
        assert_eq!(none.status, PriceStatus::Unknown);
        assert_eq!(none.median, None);
        assert_eq!(none.depeg, None, "missing price is unknown, never 0");
    }

    #[test]
    fn lookalike_detection_ignores_punctuation_and_case() {
        let reg = registry().unwrap();
        let fake: AssetId = "eip155:8453/erc20:0x2222222222222222222222222222222222222222"
            .parse()
            .unwrap();
        assert_eq!(lookalikes(reg, &fake, "usdc"), vec![usdc().asset.clone()]);
        assert_eq!(lookalikes(reg, &fake, "USDC.e"), Vec::<AssetId>::new());
        assert_eq!(
            lookalikes(reg, &fake, "U-S-D-C"),
            vec![usdc().asset.clone()]
        );
        assert!(
            lookalikes(reg, &usdc().asset, "USDC").is_empty(),
            "canonical itself"
        );
        let arb_fake: AssetId = "eip155:42161/erc20:0x2222222222222222222222222222222222222222"
            .parse()
            .unwrap();
        assert_eq!(lookalikes(reg, &arb_fake, "USDT").len(), 1, "USDT0 alias");
    }

    #[test]
    fn controlled_skips_tokens_without_controls() {
        let reg = ems_config::Registry::builtin().unwrap();
        let eth = reg.chains.resolve("ethereum").unwrap();
        let syms: Vec<&str> = controlled(eth)
            .unwrap()
            .iter()
            .map(|e| e.symbol.as_str())
            .collect();
        assert!(syms.contains(&"USDC") && syms.contains(&"USDT"));
        assert!(!syms.contains(&"DAI") && !syms.contains(&"USDS"));
    }
}
