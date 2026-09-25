//! `chain` tools: `chain_list`, `chain_finality`, `provider_health`. See `ops/mod.rs`.
//!
//! Also hosts small helpers shared by the neobank-wallet modules (`wallet`, `tx`, `neobank`).

use crate::{Catalog, Ctx, Domain, OpOutput, Operation, Profile};
use async_trait::async_trait;
use bdm_config::{ChainEntry, FinalityPolicy, NativeAsset};
use bdm_domain::{
    AccountAddress, Amount, AssetId, AssetRef, BlockRef, ChainFamily, ChainId, DomainError, Fiat,
    Price, Provenance, SourceKind,
};
use bdm_ports::{Capability, EvmRpc, SolanaRpc};
use bdm_protocols::stablecoins::{StablecoinEntry, StablecoinRegistry};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::LazyLock};

pub fn register(c: &mut Catalog) {
    c.register(ChainList);
    c.register(ChainFinality);
    c.register(ProviderHealth);
}

// ------------------------------------------------------------------ shared helpers

/// Canonical stablecoin registry, or an empty one if the built-in file is invalid (the
/// stablecoin tools report that error; lookups here just find nothing).
pub(crate) fn stablecoins() -> &'static StablecoinRegistry {
    static EMPTY: LazyLock<StablecoinRegistry> = LazyLock::new(StablecoinRegistry::default);
    super::stablecoin::registry().unwrap_or(&EMPTY)
}

pub(crate) fn native_asset(c: &ChainEntry) -> AssetId {
    AssetId::native(c.id.clone(), c.native.slip44)
}

pub(crate) fn parse_address(c: &ChainEntry, s: &str) -> Result<AccountAddress, DomainError> {
    AccountAddress::parse(c.family, s)
        .map_err(|e| DomainError::invalid(format!("{} (chain {})", e.message, c.id)))
}

/// Asset on `chain` from: nothing / "native", a CAIP-19 id, a contract address / mint, or a
/// stablecoin symbol from the canonical registry ("USDC").
pub(crate) fn resolve_asset(c: &ChainEntry, s: Option<&str>) -> Result<AssetId, DomainError> {
    let s = s.map(str::trim).unwrap_or("native");
    if s.is_empty() || s.eq_ignore_ascii_case("native") || s.eq_ignore_ascii_case(&c.native.symbol)
    {
        return Ok(native_asset(c));
    }
    if s.contains('/') {
        let a: AssetId = s.parse()?;
        if a.chain != c.id {
            return Err(DomainError::invalid(format!(
                "asset {a} is on {}, not {}",
                a.chain, c.id
            )));
        }
        return Ok(a);
    }
    if let Some(e) = stablecoins().by_symbol(&c.id, s) {
        return Ok(e.asset.clone());
    }
    let asset = match parse_address(c, s)? {
        AccountAddress::Evm(a) => AssetRef::Erc20(a),
        AccountAddress::Solana(m) => AssetRef::SplToken(m),
    };
    Ok(AssetId {
        chain: c.id.clone(),
        asset,
    })
}

/// Registry entry when `asset` is a canonical (issuer-listed) stablecoin.
pub(crate) fn stablecoin(asset: &AssetId) -> Option<&'static StablecoinEntry> {
    stablecoins().by_asset(asset)
}

/// Provenance for answers built from the routed chain RPC (vendor trail is per RPC call).
pub(crate) fn rpc_meta(chain: &ChainId) -> Provenance {
    let mut p = Provenance::new(SourceKind::Primary);
    p.chain = Some(chain.clone());
    p.provider = Some("rpc".into());
    p
}

/// Combine several routed answers into one `meta` (all attempts kept).
pub(crate) fn merge_meta(chain: Option<ChainId>, mut metas: Vec<Provenance>) -> Provenance {
    if metas.len() == 1 {
        if let Some(mut m) = metas.pop() {
            m.chain = chain.or(m.chain);
            return m;
        }
    }
    let mut p = Provenance::new(SourceKind::Aggregate);
    p.chain = chain;
    p.provider = Some("aggregate".into());
    for m in metas {
        p.latency_ms = p.latency_ms.max(m.latency_ms);
        p.providers_tried.extend(m.providers_tried);
    }
    p
}

/// `amount × price`, keeping the price's time and source. `None` when it does not fit.
pub(crate) fn fiat_value(amount: &Amount, price: &Price) -> Option<Fiat> {
    Some(Fiat {
        amount: amount.to_decimal()?.checked_mul(price.value)?.normalize(),
        currency: price.currency.clone(),
        as_of: price.as_of,
        source: price.source.clone(),
    })
}

pub(crate) fn hex_u64(v: &Value) -> Option<u64> {
    u64::from_str_radix(v.as_str()?.trim_start_matches("0x"), 16).ok()
}

pub(crate) fn serde_label<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

const ALL: &[Profile] = Profile::ALL;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ChainFilter {
    /// Optional chain (CAIP-2 id or alias such as "base", "solana"). Omit for every enabled chain.
    #[serde(default)]
    pub chain: Option<String>,
}

fn chains<'a>(ctx: &'a Ctx, filter: Option<&str>) -> Result<Vec<&'a ChainEntry>, DomainError> {
    Ok(match filter {
        Some(c) => vec![ctx.chain(c)?],
        None => ctx.config().registry.chains.enabled().collect(),
    })
}

fn caps_for(family: ChainFamily) -> impl Iterator<Item = Capability> {
    Capability::ALL.iter().copied().filter(move |c| match c {
        Capability::EvmRpc => family == ChainFamily::Evm,
        Capability::SolanaRpc => family == ChainFamily::Solana,
        _ => true,
    })
}

// ------------------------------------------------------------------ chain_list

#[derive(Debug, Serialize, JsonSchema)]
pub struct CapabilityRow {
    pub capability: Capability,
    /// Configured vendor order (primary first).
    pub order: Vec<String>,
    /// Which config level set the order: operation | chain | default | built_in.
    pub order_level: String,
    /// Vendors from `order` that are enabled, have their keys, and are registered for this chain.
    pub usable: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ChainRow {
    pub id: ChainId,
    pub name: String,
    pub aliases: Vec<String>,
    pub family: ChainFamily,
    pub native: NativeAsset,
    pub block_time_ms: u64,
    pub finality: FinalityPolicy,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explorer: Option<String>,
    pub capabilities: Vec<CapabilityRow>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ChainListOut {
    pub chains: Vec<ChainRow>,
}

pub struct ChainList;

#[async_trait]
impl Operation for ChainList {
    type Input = ChainFilter;
    type Output = ChainListOut;
    const NAME: &'static str = "chain_list";
    const DOMAIN: Domain = Domain::Chain;
    const DESCRIPTION: &'static str = "List the enabled chains (CAIP-2 id, aliases, native asset, \
        block time, finality policy) and, per chain, which capabilities are available and through \
        which vendors in the user's configured order. Use it first to learn valid `chain` values and \
        whether a tool will work on a chain (empty `usable` = no provider; usually a missing API key).";
    const PROFILES: &'static [Profile] = ALL;

    async fn execute(
        &self,
        ctx: &Ctx,
        input: ChainFilter,
    ) -> Result<OpOutput<ChainListOut>, DomainError> {
        let cfg = ctx.config();
        let registry = &ctx.table().registry;
        let rows = chains(ctx, input.chain.as_deref())?
            .into_iter()
            .map(|c| {
                let capabilities = caps_for(c.family)
                    .filter_map(|cap| {
                        let o = cfg.order(cap, Some(&c.id), None);
                        (!o.vendors.is_empty()).then(|| CapabilityRow {
                            capability: cap,
                            usable: o
                                .vendors
                                .iter()
                                .filter(|v| {
                                    cfg.vendor_status(v) == bdm_config::VendorStatus::Active
                                        && registry.get(cap, Some(&c.id), v).is_some()
                                })
                                .cloned()
                                .collect(),
                            order_level: serde_label(&o.level),
                            order: o.vendors,
                        })
                    })
                    .collect();
                ChainRow {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    aliases: c.aliases.clone(),
                    family: c.family,
                    native: c.native.clone(),
                    block_time_ms: c.block_time_ms,
                    finality: c.finality.clone(),
                    explorer: c.explorer.clone(),
                    capabilities,
                }
            })
            .collect();
        Ok(OpOutput::local(ChainListOut { chains: rows }))
    }
}

// ------------------------------------------------------------------ chain_finality

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FinalityIn {
    /// Chain (CAIP-2 id or alias).
    pub chain: String,
    /// Also ask every configured RPC vendor for its head and report lag (one cheap call each).
    #[serde(default)]
    pub per_provider: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FinalityLevel {
    /// EVM: latest | safe | finalized. Solana: processed | confirmed | finalized.
    pub level: String,
    /// Block (EVM) or slot (Solana); absent if the provider does not support this level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<BlockRef>,
    /// Blocks/slots behind the head (`latest` / `processed`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind_head: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ProviderHead {
    pub vendor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<u64>,
    /// Blocks/slots behind the highest head reported by any provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lag: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FinalityOut {
    pub chain: ChainId,
    pub policy: FinalityPolicy,
    pub levels: Vec<FinalityLevel>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub provider_heads: Vec<ProviderHead>,
}

async fn evm_block(rpc: &dyn EvmRpc, tag: &str) -> Option<BlockRef> {
    let b = rpc
        .request("eth_getBlockByNumber", json!([tag, false]))
        .await
        .ok()?;
    Some(BlockRef {
        number: hex_u64(b.get("number")?)?,
        hash: b.get("hash").and_then(Value::as_str).map(str::to_owned),
        timestamp: b
            .get("timestamp")
            .and_then(hex_u64)
            .and_then(|t| chrono::DateTime::from_timestamp(t as i64, 0)),
    })
}

async fn sol_slot(rpc: &dyn SolanaRpc, commitment: &str) -> Option<BlockRef> {
    let v = rpc
        .request("getSlot", json!([{ "commitment": commitment }]))
        .await
        .ok()?;
    Some(BlockRef {
        number: v.as_u64()?,
        hash: None,
        timestamp: None,
    })
}

fn heads(results: Vec<(String, Result<Option<u64>, String>)>) -> Vec<ProviderHead> {
    let max = results
        .iter()
        .filter_map(|(_, r)| r.clone().ok().flatten())
        .max();
    results
        .into_iter()
        .map(|(vendor, r)| {
            let (head, error) = match r {
                Ok(h) => (h, None),
                Err(e) => (None, Some(e)),
            };
            ProviderHead {
                vendor,
                lag: head.zip(max).map(|(h, m)| m - h),
                head,
                error,
            }
        })
        .collect()
}

pub struct ChainFinality;

#[async_trait]
impl Operation for ChainFinality {
    type Input = FinalityIn;
    type Output = FinalityOut;
    const NAME: &'static str = "chain_finality";
    const DOMAIN: Domain = Domain::Chain;
    const DESCRIPTION: &'static str = "Current head and finality checkpoints of a chain. EVM: \
        latest / safe / finalized block (number, hash, time); on L2s `latest` is only the \
        sequencer's word, so wait for `safe` or `finalized` before treating money as settled. \
        Solana: slot per commitment (processed / confirmed / finalized). Set `per_provider` to \
        see how far each configured RPC vendor lags the highest head.";
    const PROFILES: &'static [Profile] = ALL;

    async fn execute(
        &self,
        ctx: &Ctx,
        input: FinalityIn,
    ) -> Result<OpOutput<FinalityOut>, DomainError> {
        let chain = ctx.chain(&input.chain)?;
        let route = ctx.route(Capability::EvmRpc).chain(chain.id.clone());
        let (names, blocks, provider_heads): (&[&str], Vec<Option<BlockRef>>, _) =
            match chain.family {
                ChainFamily::Evm => {
                    let rpc = ctx.evm_rpc(chain)?;
                    let (l, s, f) = tokio::join!(
                        evm_block(&rpc, "latest"),
                        evm_block(&rpc, "safe"),
                        evm_block(&rpc, "finalized")
                    );
                    let heads = if input.per_provider {
                        per_provider_heads::<dyn EvmRpc>(ctx, route, "eth_blockNumber").await
                    } else {
                        Vec::new()
                    };
                    (&["latest", "safe", "finalized"], vec![l, s, f], heads)
                }
                ChainFamily::Solana => {
                    let rpc = ctx.solana_rpc(chain)?;
                    let (p, c, f) = tokio::join!(
                        sol_slot(&rpc, "processed"),
                        sol_slot(&rpc, "confirmed"),
                        sol_slot(&rpc, "finalized")
                    );
                    let heads = if input.per_provider {
                        let route = ctx.route(Capability::SolanaRpc).chain(chain.id.clone());
                        per_provider_heads::<dyn SolanaRpc>(ctx, route, "getSlot").await
                    } else {
                        Vec::new()
                    };
                    (
                        &["processed", "confirmed", "finalized"],
                        vec![p, c, f],
                        heads,
                    )
                }
            };
        let Some(Some(head_block)) = blocks.first() else {
            return Err(DomainError::new(
                bdm_domain::ErrorCode::AllProvidersFailed,
                format!("could not read the head of {}", chain.id),
            ));
        };
        let head = Some(head_block.number);
        let levels = names
            .iter()
            .zip(blocks)
            .map(|(n, block)| FinalityLevel {
                level: (*n).to_owned(),
                behind_head: head
                    .zip(block.as_ref())
                    .map(|(h, b)| h.saturating_sub(b.number)),
                block,
            })
            .collect();
        let meta = rpc_meta(&chain.id);
        Ok(OpOutput::new(
            FinalityOut {
                chain: chain.id.clone(),
                policy: chain.finality.clone(),
                levels,
                provider_heads,
            },
            meta,
        ))
    }
}

/// One head query per configured RPC vendor, through the router (breakers, quota, metering).
async fn per_provider_heads<P>(
    ctx: &Ctx,
    route: bdm_routing::RouteReq,
    method: &'static str,
) -> Vec<ProviderHead>
where
    P: bdm_ports::PortKind + ?Sized + RawRpc,
{
    match ctx
        .router()
        .fan_out::<P, _, _, _>(route, |p| async move { p.raw(method).await })
        .await
    {
        Ok(r) => heads(
            r.value
                .into_iter()
                .map(|(v, res)| {
                    let res = res
                        .map(|val| val.as_u64().or_else(|| hex_u64(&val)))
                        .map_err(|e| e.reason().to_owned());
                    (v, res)
                })
                .collect(),
        ),
        Err(e) => e
            .attempts
            .into_iter()
            .map(|a| ProviderHead {
                vendor: a.vendor,
                head: None,
                lag: None,
                error: a.reason.or_else(|| Some("failed".into())),
            })
            .collect(),
    }
}

/// Parameterless JSON-RPC call on either chain transport.
#[async_trait]
pub(crate) trait RawRpc: Send + Sync {
    async fn raw(&self, method: &str) -> bdm_ports::PortResult<Value>;
}

#[async_trait]
impl RawRpc for dyn EvmRpc {
    async fn raw(&self, method: &str) -> bdm_ports::PortResult<Value> {
        self.request(method, json!([])).await
    }
}

#[async_trait]
impl RawRpc for dyn SolanaRpc {
    async fn raw(&self, method: &str) -> bdm_ports::PortResult<Value> {
        self.request(method, json!([])).await
    }
}

// ------------------------------------------------------------------ provider_health

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HealthIn {
    /// Optional chain (CAIP-2 id or alias) for the effective-order view. Omit for all chains.
    #[serde(default)]
    pub chain: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct VendorRow {
    pub vendor: String,
    /// Config status: active | disabled | missing_key (with the env vars to set) | unknown.
    pub config: Value,
    /// Runtime health: breaker state, ok/failed counts, latency, last error kind, quota usage.
    pub runtime: Value,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct HealthOut {
    pub vendors: Vec<VendorRow>,
    /// chain id → capability → effective vendor order and the config level that set it.
    pub orders: BTreeMap<String, BTreeMap<String, Value>>,
}

pub struct ProviderHealth;

#[async_trait]
impl Operation for ProviderHealth {
    type Input = HealthIn;
    type Output = HealthOut;
    const NAME: &'static str = "provider_health";
    const DOMAIN: Domain = Domain::Chain;
    const DESCRIPTION: &'static str = "Read-only health of every data vendor: config status \
        (active, disabled, missing API key), circuit-breaker state, success/failure counts, \
        latency, quota used vs budget, and the effective vendor order per chain and capability. \
        Use it to explain why a tool fell back to another provider or failed with \
        UNSUPPORTED_CAPABILITY. It cannot change routing.";
    const PROFILES: &'static [Profile] = ALL;

    async fn execute(
        &self,
        ctx: &Ctx,
        input: HealthIn,
    ) -> Result<OpOutput<HealthOut>, DomainError> {
        let cfg = ctx.config();
        let vendors = ctx
            .router()
            .health()
            .into_iter()
            .map(|h| VendorRow {
                config: serde_json::to_value(cfg.vendor_status(&h.vendor)).unwrap_or_default(),
                vendor: h.vendor.clone(),
                runtime: serde_json::to_value(&h).unwrap_or_default(),
            })
            .collect();
        let orders = chains(ctx, input.chain.as_deref())?
            .into_iter()
            .map(|c| {
                let per_cap = caps_for(c.family)
                    .map(|cap| {
                        let o = cfg.order(cap, Some(&c.id), None);
                        (cap.to_string(), serde_json::to_value(o).unwrap_or_default())
                    })
                    .collect();
                (c.id.to_string(), per_cap)
            })
            .collect();
        Ok(OpOutput::local(HealthOut { vendors, orders }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    #[test]
    fn heads_compute_lag_against_max() {
        let h = heads(vec![
            ("a".into(), Ok(Some(100))),
            ("b".into(), Ok(Some(97))),
            ("c".into(), Err("timeout".into())),
        ]);
        assert_eq!(h[0].lag, Some(0));
        assert_eq!(h[1].lag, Some(3));
        assert_eq!((h[2].head, h[2].error.as_deref()), (None, Some("timeout")));
    }

    #[test]
    fn fiat_value_is_exact() {
        let price = Price {
            asset: "eip155:1/slip44:60".parse().unwrap(),
            currency: "USD".into(),
            value: Decimal::from_str("2500.10").unwrap(),
            as_of: chrono::Utc::now(),
            source: "coingecko".into(),
            liquidity_usd: None,
        };
        // 0.000021 ETH (21000 gas × 1 gwei)
        let fee = Amount::new(U256::from(21_000_000_000_000u64), 18);
        let v = fiat_value(&fee, &price).unwrap();
        assert_eq!(v.amount, Decimal::from_str("0.0525021").unwrap());
        assert_eq!(v.source, "coingecko");
    }
}
