//! Built-in data: chains and vendors (free tiers, key env names, URL templates, default orders).

use crate::settings::{Order, WindowBudget};
use bdm_domain::{ChainFamily, ChainId, DomainError, ErrorCode};
use bdm_ports::{Capability, RpcFeatures};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

pub(crate) const BUILTIN_CHAINS: &str = include_str!("../../../registry/chains.toml");
pub(crate) const BUILTIN_VENDORS: &str = include_str!("../../../registry/vendors.toml");

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NativeAsset {
    pub symbol: String,
    pub decimals: u8,
    pub slip44: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FinalityPolicy {
    /// "tags" (EVM block tags) or "commitment" (Solana).
    pub policy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainEntry {
    pub id: ChainId,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub family: ChainFamily,
    pub native: NativeAsset,
    pub block_time_ms: u64,
    pub finality: FinalityPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multicall3: Option<String>,
    #[serde(default)]
    pub public_rpc: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explorer: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

/// Chains with alias lookup. Aliases and CAIP-2 ids are matched case-insensitively.
#[derive(Debug, Clone, Default)]
pub struct ChainRegistry {
    chains: Vec<ChainEntry>,
    index: HashMap<String, usize>,
}

impl ChainRegistry {
    pub fn new(chains: Vec<ChainEntry>) -> Result<Self, String> {
        let mut index = HashMap::new();
        for (i, c) in chains.iter().enumerate() {
            for key in std::iter::once(c.id.to_string()).chain(c.aliases.iter().cloned()) {
                if index.insert(key.to_lowercase(), i).is_some() {
                    return Err(format!("duplicate chain id/alias '{key}'"));
                }
            }
            if c.id.family() != Some(c.family) {
                return Err(format!("chain {} family mismatch", c.id));
            }
        }
        Ok(Self { chains, index })
    }

    /// Resolve a CAIP-2 id or alias ("base", "eip155:8453") to an enabled chain.
    pub fn resolve(&self, s: &str) -> Result<&ChainEntry, DomainError> {
        let entry = self
            .index
            .get(&s.trim().to_lowercase())
            .and_then(|&i| self.chains.get(i))
            .ok_or_else(|| {
                DomainError::new(ErrorCode::UnsupportedChain, format!("unknown chain '{s}'"))
                    .with_hint(format!("supported: {}", self.enabled_names().join(", ")))
            })?;
        if !entry.enabled {
            return Err(DomainError::new(
                ErrorCode::UnsupportedChain,
                format!("chain {} is disabled", entry.id),
            ));
        }
        Ok(entry)
    }

    /// Lookup regardless of `enabled` (config validation, dashboard).
    pub fn find(&self, s: &str) -> Option<&ChainEntry> {
        self.index
            .get(&s.trim().to_lowercase())
            .and_then(|&i| self.chains.get(i))
    }

    pub fn all(&self) -> &[ChainEntry] {
        &self.chains
    }

    pub fn enabled(&self) -> impl Iterator<Item = &ChainEntry> {
        self.chains.iter().filter(|c| c.enabled)
    }

    fn enabled_names(&self) -> Vec<String> {
        self.enabled()
            .map(|c| {
                c.aliases
                    .first()
                    .cloned()
                    .unwrap_or_else(|| c.id.to_string())
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetRule {
    CalendarMonth,
    DailyUtc,
    /// Sliding window (per-second / per-minute limits).
    Rolling,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Requests,
    Credits,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VendorEntry {
    pub display_name: String,
    pub requires_key: bool,
    /// Key field → env var name, e.g. `api_key = "ALCHEMY_API_KEY"`.
    #[serde(default)]
    pub keys: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signup_url: Option<String>,
    pub free_tier_verified: bool,
    /// Plain-language access tier (docs/VENDORS.md, dashboard): 1 = free, no key needed;
    /// 2 = free key with a big limit (>= 1M units/month); 3 = free key with a small limit;
    /// 4 = paid, trial-only or unverified (off by default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<u8>,
    /// Explicit default; otherwise enabled iff `free_tier_verified`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    pub unit: Unit,
    pub reset: ResetRule,
    #[serde(default)]
    pub limit: WindowBudget,
    #[serde(default = "one")]
    pub default_cost: u64,
    #[serde(default)]
    pub costs: BTreeMap<String, u64>,
    #[serde(default)]
    pub rpc_features: RpcFeatures,
    /// Chain id → URL template with `{api_key}`-style placeholders.
    #[serde(default)]
    pub rpc_urls: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn one() -> u64 {
    1
}

#[derive(Debug, Clone, Deserialize)]
struct VendorsFile {
    default_order: BTreeMap<Capability, Order>,
    #[serde(default)]
    default_order_chain: BTreeMap<String, BTreeMap<Capability, Order>>,
    vendor: BTreeMap<String, VendorEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChainsFile {
    chain: Vec<ChainEntry>,
}

/// Parsed built-in registry (plus `extra_chains` / `chain_overrides` applied by the loader).
#[derive(Debug, Clone)]
pub struct Registry {
    pub chains: ChainRegistry,
    pub vendors: BTreeMap<String, VendorEntry>,
    pub default_order: BTreeMap<Capability, Order>,
    /// Keyed by canonical CAIP-2 id.
    pub default_order_chain: BTreeMap<ChainId, BTreeMap<Capability, Order>>,
}

impl Registry {
    pub fn builtin() -> Result<Self, String> {
        Self::parse(BUILTIN_CHAINS, BUILTIN_VENDORS)
    }

    pub fn parse(chains_toml: &str, vendors_toml: &str) -> Result<Self, String> {
        let chains: ChainsFile =
            toml::from_str(chains_toml).map_err(|e| format!("registry/chains.toml: {e}"))?;
        let vendors: VendorsFile =
            toml::from_str(vendors_toml).map_err(|e| format!("registry/vendors.toml: {e}"))?;
        let chains = ChainRegistry::new(chains.chain)?;
        let mut default_order_chain = BTreeMap::new();
        for (k, v) in vendors.default_order_chain {
            let id = chains
                .find(&k)
                .ok_or_else(|| format!("vendors.toml default_order_chain: unknown chain '{k}'"))?
                .id
                .clone();
            default_order_chain.insert(id, v);
        }
        Ok(Self {
            chains,
            vendors: vendors.vendor,
            default_order: vendors.default_order,
            default_order_chain,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_parses_and_resolves_aliases() {
        let r = Registry::builtin().unwrap();
        assert_eq!(r.chains.all().len(), 9);
        for (alias, id) in [
            ("ethereum", "eip155:1"),
            ("BASE", "eip155:8453"),
            ("bsc", "eip155:56"),
            ("robinhood", "eip155:4663"),
            ("solana", "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp"),
            ("eip155:42161", "eip155:42161"),
        ] {
            assert_eq!(
                r.chains.resolve(alias).unwrap().id.to_string(),
                id,
                "{alias}"
            );
        }
        assert_eq!(r.chains.resolve("bsc").unwrap().name, "Binance Smart Chain");
        let err = r.chains.resolve("dogechain").unwrap_err();
        assert_eq!(err.code, ErrorCode::UnsupportedChain);
        assert!(r.vendors.contains_key("alchemy"));
        assert!(!r.vendors["ankr"].free_tier_verified);
        // tier follows the fixed rule in docs/VENDORS.md
        for (id, v) in &r.vendors {
            let l = &v.limit;
            let monthly = [
                l.monthly,
                l.daily.map(|n| n * 30),
                l.per_minute.map(|n| n * 43_200),
                l.rps.map(|n| n * 2_592_000),
            ]
            .into_iter()
            .flatten()
            .min();
            let want = if !v.free_tier_verified {
                4
            } else if !v.requires_key {
                1
            } else if monthly.is_some_and(|m| m >= 1_000_000) {
                2
            } else {
                3
            };
            assert_eq!(v.tier, Some(want), "vendor '{id}' tier");
        }
        // every vendor referenced by a default order exists in the registry
        let orders = r
            .default_order
            .values()
            .chain(r.default_order_chain.values().flat_map(|m| m.values()));
        for order in orders {
            for v in order.iter() {
                assert!(
                    r.vendors.contains_key(v),
                    "unknown vendor '{v}' in default order"
                );
            }
        }
    }
}
