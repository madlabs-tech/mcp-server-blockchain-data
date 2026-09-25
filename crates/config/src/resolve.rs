//! Derived views over a [`Loaded`] config: routing orders, vendor status, budgets, URLs, costs.

use crate::{
    loader::Loaded,
    redacted::Redacted,
    registry::{ResetRule, Unit},
    settings::{ClientLimits, OnExhausted, OperationSettings, WindowBudget},
};
use bdm_domain::ChainId;
use bdm_ports::{Capability, VendorMeta, RPC_VENDOR};
use serde::Serialize;

/// Which config level produced an order (shown in the dashboard's effective-order view).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderLevel {
    Operation,
    Chain,
    Default,
    BuiltIn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrderResolution {
    pub vendors: Vec<String>,
    pub level: OrderLevel,
}

/// Static (config-level) vendor availability. Runtime state (breaker, quota) is in routing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum VendorStatus {
    Active,
    Disabled { unverified: bool },
    MissingKey { env_vars: Vec<String> },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffectiveBudget {
    pub unit: Unit,
    pub reset: ResetRule,
    /// Vendor quota: registry free tier overlaid by `vendors.<id>.limit`.
    pub limit: WindowBudget,
    /// Operator cap.
    pub cap: WindowBudget,
    pub reserve_pct: u8,
    pub on_exhausted: OnExhausted,
    /// What routing enforces: rate windows `min(cap, limit)`; daily/monthly
    /// `min(cap, limit × (1 − reserve))`.
    pub effective: WindowBudget,
    pub alert_pct: Vec<u8>,
}

pub(crate) fn overlay(base: WindowBudget, over: WindowBudget) -> WindowBudget {
    WindowBudget {
        rps: over.rps.or(base.rps),
        per_minute: over.per_minute.or(base.per_minute),
        daily: over.daily.or(base.daily),
        monthly: over.monthly.or(base.monthly),
    }
}

fn min_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    a.into_iter().chain(b).min()
}

impl Loaded {
    /// Ordered vendor ids for a capability. Most specific wins:
    /// `operations.<op>.order` > `routing.chains.<chain>` > `routing.defaults` > built-in.
    /// Built-in orders for chain RPC/broadcast put operator `custom_rpc` endpoints first.
    pub fn order(
        &self,
        cap: Capability,
        chain: Option<&ChainId>,
        op: Option<&str>,
    ) -> OrderResolution {
        let s = &self.settings;
        if let Some(o) = op
            .and_then(|op| s.operations.get(op))
            .and_then(|os| os.order.get(&cap))
        {
            return OrderResolution {
                vendors: o.0.clone(),
                level: OrderLevel::Operation,
            };
        }
        if let Some(chain) = chain {
            let hit = s.routing.chains.iter().find_map(|(k, caps)| {
                let matches = self.registry.chains.find(k).is_some_and(|c| &c.id == chain);
                matches.then(|| caps.get(&cap)).flatten()
            });
            if let Some(o) = hit {
                return OrderResolution {
                    vendors: o.0.clone(),
                    level: OrderLevel::Chain,
                };
            }
        }
        if let Some(o) = s.routing.defaults.get(&cap) {
            return OrderResolution {
                vendors: o.0.clone(),
                level: OrderLevel::Default,
            };
        }
        let builtin = chain
            .and_then(|c| self.registry.default_order_chain.get(c))
            .and_then(|m| m.get(&cap))
            .or_else(|| self.registry.default_order.get(&cap))
            .map(|o| o.0.clone())
            .unwrap_or_default();
        let mut vendors = Vec::new();
        if let Some(chain) = chain {
            if matches!(
                cap,
                Capability::EvmRpc | Capability::SolanaRpc | Capability::Broadcast
            ) {
                vendors.extend(self.custom_rpc_for(chain));
            }
        }
        vendors.extend(builtin);
        OrderResolution {
            vendors,
            level: OrderLevel::BuiltIn,
        }
    }

    fn custom_rpc_for(&self, chain: &ChainId) -> Vec<String> {
        self.settings
            .custom_rpc
            .iter()
            .filter(|(_, c)| {
                self.registry
                    .chains
                    .find(&c.chain)
                    .is_some_and(|e| &e.id == chain)
            })
            .map(|(name, _)| name.clone())
            .collect()
    }

    pub fn vendor_status(&self, vendor: &str) -> VendorStatus {
        if vendor == RPC_VENDOR || self.settings.custom_rpc.contains_key(vendor) {
            return VendorStatus::Active;
        }
        let Some(entry) = self.registry.vendors.get(vendor) else {
            return VendorStatus::Unknown;
        };
        let enabled = self
            .settings
            .vendors
            .get(vendor)
            .and_then(|v| v.enabled)
            .or(entry.enabled)
            .unwrap_or(entry.free_tier_verified);
        if !enabled {
            return VendorStatus::Disabled {
                unverified: !entry.free_tier_verified,
            };
        }
        if entry.requires_key {
            let missing: Vec<String> = entry
                .keys
                .iter()
                .filter(|(field, _)| self.key(vendor, field).is_none())
                .map(|(_, var)| var.clone())
                .collect();
            if !missing.is_empty() {
                return VendorStatus::MissingKey { env_vars: missing };
            }
        }
        VendorStatus::Active
    }

    /// Admin-facing metadata for a vendor, from its registry entry (bare id if unknown).
    pub fn vendor_meta(&self, vendor: &str) -> VendorMeta {
        let e = self.registry.vendors.get(vendor);
        VendorMeta {
            id: vendor.to_owned(),
            display_name: e.map_or_else(|| vendor.to_owned(), |e| e.display_name.clone()),
            requires_key: e.is_some_and(|e| e.requires_key),
            signup_url: e.and_then(|e| e.signup_url.clone()),
            rpc_features: e.map(|e| e.rpc_features.clone()).unwrap_or_default(),
        }
    }

    pub fn key(&self, vendor: &str, field: &str) -> Option<&str> {
        self.settings.keys.get(vendor).and_then(|k| k.get(field))
    }

    /// All secret values (for scrubbing logs and error messages).
    pub fn secret_values(&self) -> Vec<&str> {
        let keys = self
            .settings
            .keys
            .values()
            .flat_map(|k| k.0.values().map(|v| v.expose().as_str()));
        let urls = self
            .settings
            .custom_rpc
            .values()
            .map(|c| c.url.expose().as_str());
        keys.chain(urls).filter(|s| !s.is_empty()).collect()
    }

    /// Replace every secret occurring in `text` with `***`.
    pub fn scrub(&self, text: &str) -> String {
        crate::redacted::scrub(text, &self.secret_values())
    }

    /// RPC URL for a vendor on a chain (template filled with keys). `None` if the vendor has no
    /// endpoint for the chain or a placeholder's key is missing. `public` returns the first
    /// public RPC; use [`Loaded::public_rpc_urls`] for all of them.
    pub fn rpc_url(&self, vendor: &str, chain: &ChainId) -> Option<Redacted<String>> {
        if let Some(c) = self.settings.custom_rpc.get(vendor) {
            let matches = self
                .registry
                .chains
                .find(&c.chain)
                .is_some_and(|e| &e.id == chain);
            return matches.then(|| c.url.clone());
        }
        if vendor == "public" {
            return self
                .public_rpc_urls(chain)
                .into_iter()
                .next()
                .map(Redacted::new);
        }
        let template = self
            .registry
            .vendors
            .get(vendor)?
            .rpc_urls
            .get(&chain.to_string())?;
        let mut url = template.clone();
        while let Some(start) = url.find('{') {
            let end = start + url[start..].find('}')?;
            let field = &url[start + 1..end];
            let value = self.key(vendor, field)?.to_owned();
            url.replace_range(start..=end, &value);
        }
        Some(Redacted::new(url))
    }

    pub fn public_rpc_urls(&self, chain: &ChainId) -> Vec<String> {
        self.registry
            .chains
            .find(&chain.to_string())
            .map(|c| c.public_rpc.clone())
            .unwrap_or_default()
    }

    pub fn effective_budget(&self, vendor: &str) -> Option<EffectiveBudget> {
        let entry = self.registry.vendors.get(vendor)?;
        let vs = self
            .settings
            .vendors
            .get(vendor)
            .cloned()
            .unwrap_or_default();
        let limit = overlay(entry.limit, vs.limit);
        let reserve_pct = vs.reserve_pct.unwrap_or(10);
        let keep = |v: Option<u64>| v.map(|v| v.saturating_mul(100 - reserve_pct as u64) / 100);
        let effective = WindowBudget {
            rps: min_opt(vs.cap.rps, limit.rps),
            per_minute: min_opt(vs.cap.per_minute, limit.per_minute),
            daily: min_opt(vs.cap.daily, keep(limit.daily)),
            monthly: min_opt(vs.cap.monthly, keep(limit.monthly)),
        };
        Some(EffectiveBudget {
            unit: entry.unit,
            reset: entry.reset,
            limit,
            cap: vs.cap,
            reserve_pct,
            on_exhausted: vs.on_exhausted.unwrap_or(OnExhausted::Skip),
            effective,
            alert_pct: vs.alert_pct.unwrap_or_else(|| vec![75, 90]),
        })
    }

    /// Cost of one request in the vendor's unit (config override > registry table > default).
    pub fn cost(&self, vendor: &str, method: &str) -> u64 {
        if let Some(c) = self
            .settings
            .vendors
            .get(vendor)
            .and_then(|v| v.costs.get(method))
        {
            return *c;
        }
        self.registry
            .vendors
            .get(vendor)
            .map(|e| e.costs.get(method).copied().unwrap_or(e.default_cost))
            .unwrap_or(1)
    }

    pub fn operation(&self, op: &str) -> OperationSettings {
        self.settings
            .operations
            .get(op)
            .cloned()
            .unwrap_or_default()
    }

    /// Limits for a client id: per-client overrides field-by-field over `clients.default`.
    pub fn client_limits(&self, client: &str) -> ClientLimits {
        let d = self.settings.clients.default.clone();
        match self.settings.clients.overrides.get(client) {
            None => d,
            Some(o) => o.or(d),
        }
    }
}
