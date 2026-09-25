//! The configuration schema (what `config.toml`, `secrets.toml` and `ODM__*` env vars set).

use crate::{redacted::Redacted, registry::ChainEntry};
use bdm_ports::Capability;
use schemars::JsonSchema;
use serde::{de, Deserialize, Deserializer, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub server: ServerSettings,
    pub vendors: BTreeMap<String, VendorSettings>,
    /// Vendor API keys (normally from env or `secrets.toml`, never `config.toml`).
    #[schemars(skip)]
    pub keys: BTreeMap<String, VendorKeys>,
    pub routing: RoutingSettings,
    pub operations: BTreeMap<String, OperationSettings>,
    pub clients: ClientsSettings,
    /// Operator-provided RPC endpoints, usable in orders by their name.
    pub custom_rpc: BTreeMap<String, CustomRpc>,
    pub chain_overrides: BTreeMap<String, ChainOverride>,
    pub extra_chains: Vec<ChainEntry>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    SelfHosted,
    Hosted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ServerSettings {
    pub mode: Mode,
    pub http_bind: String,
    pub public_bind: Option<String>,
    pub admin_bind: Option<String>,
    pub dashboard: bool,
    pub tool_profile: String,
    /// For `tool_profile = "custom"`.
    pub enabled_tools: Vec<String>,
    pub disabled_tools: Vec<String>,
    pub data_dir: PathBuf,
    pub cache_max_entries: u64,
    pub quota_poll_secs: u64,
    /// Check every active EVM RPC's chain id in the background right after startup.
    pub warmup: bool,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            mode: Mode::SelfHosted,
            http_bind: "127.0.0.1:8787".into(),
            public_bind: None,
            admin_bind: None,
            dashboard: true,
            tool_profile: "all".into(),
            enabled_tools: Vec::new(),
            disabled_tools: Vec::new(),
            data_dir: PathBuf::from("./data"),
            cache_max_entries: 20_000,
            quota_poll_secs: 300,
            warmup: true,
        }
    }
}

/// Budget per window, in the vendor's unit (credits or requests; see the registry entry).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct WindowBudget {
    pub rps: Option<u64>,
    pub per_minute: Option<u64>,
    #[serde(alias = "daily_credits", alias = "daily_requests")]
    pub daily: Option<u64>,
    #[serde(alias = "monthly_credits", alias = "monthly_requests")]
    pub monthly: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnExhausted {
    /// Skip the vendor (route to the next one) until the window resets.
    Skip,
    /// Keep using it (paid overage or vendor soft limit).
    AllowOverage,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct VendorSettings {
    pub enabled: Option<bool>,
    /// Vendor's real quota (overrides the registry's free tier, e.g. after upgrading).
    pub limit: WindowBudget,
    /// Operator budget, normally below `limit`.
    pub cap: WindowBudget,
    pub reserve_pct: Option<u8>,
    pub on_exhausted: Option<OnExhausted>,
    /// Per-method cost overrides.
    pub costs: BTreeMap<String, u64>,
    /// Alert thresholds in percent of the effective budget (default 75, 90).
    pub alert_pct: Option<Vec<u8>>,
}

/// Ordered vendor list: primary first, then fallbacks. Accepts a TOML/JSON array or a
/// comma-separated string (`ODM__ROUTING__DEFAULTS__EVM_RPC=alchemy,quicknode,public`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct Order(pub Vec<String>);

impl Order {
    pub fn iter(&self) -> impl Iterator<Item = &String> {
        self.0.iter()
    }
}

impl<'de> Deserialize<'de> for Order {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            List(Vec<String>),
            Csv(String),
        }
        let list = match Repr::deserialize(d)? {
            Repr::List(v) => v,
            Repr::Csv(s) => s.split(',').map(str::to_owned).collect(),
        };
        let list: Vec<String> = list
            .into_iter()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        if list.is_empty() {
            return Err(de::Error::custom("vendor order must not be empty"));
        }
        Ok(Order(list))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RoutingSettings {
    pub defaults: BTreeMap<Capability, Order>,
    /// Keyed by CAIP-2 id or alias.
    pub chains: BTreeMap<String, BTreeMap<Capability, Order>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    Failover,
    Quorum,
    Aggregate,
    FanOut,
    Hedged,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct OperationSettings {
    pub enabled: Option<bool>,
    pub strategy: Option<Strategy>,
    pub quorum: Option<u8>,
    pub fan_out: Option<u8>,
    pub hedge_delay_ms: Option<u64>,
    pub cache_ttl_secs: Option<u64>,
    /// Per-capability order for this operation only (highest precedence).
    pub order: BTreeMap<Capability, Order>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ClientLimits {
    pub requests_per_minute: Option<u32>,
    pub daily_requests: Option<u64>,
    /// Vendor credits spent on the client's behalf per calendar month.
    pub monthly_credits: Option<u64>,
    pub tool_profile: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ClientsSettings {
    pub default: ClientLimits,
    /// Per client id (created in the dashboard); fields fall back to `default`.
    pub overrides: BTreeMap<String, ClientLimits>,
}

impl Default for ClientsSettings {
    fn default() -> Self {
        Self {
            default: ClientLimits {
                requests_per_minute: Some(30),
                daily_requests: Some(1_000),
                monthly_credits: Some(200_000),
                tool_profile: Some("payments".into()),
            },
            overrides: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CustomRpc {
    /// CAIP-2 id or alias.
    pub chain: String,
    #[schemars(with = "String")]
    pub url: Redacted<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ChainOverride {
    pub enabled: Option<bool>,
    pub public_rpc: Option<Vec<String>>,
}

/// Keys of one vendor: `field → secret`. Accepts a plain string as shorthand for `api_key`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct VendorKeys(pub BTreeMap<String, Redacted<String>>);

impl VendorKeys {
    pub fn get(&self, field: &str) -> Option<&str> {
        self.0
            .get(field)
            .map(|v| v.expose().as_str())
            .filter(|s| !s.is_empty())
    }
}

impl<'de> Deserialize<'de> for VendorKeys {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Short(String),
            Map(BTreeMap<String, String>),
        }
        let map = match Repr::deserialize(d)? {
            Repr::Short(s) => BTreeMap::from([("api_key".to_string(), s)]),
            Repr::Map(m) => m,
        };
        Ok(Self(
            map.into_iter()
                .map(|(k, v)| (k, Redacted::new(v)))
                .collect(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_accepts_list_or_csv() {
        let a: Order = serde_json::from_str(r#"["alchemy", "public"]"#).unwrap();
        let b: Order = serde_json::from_str(r#""alchemy, public""#).unwrap();
        assert_eq!(a, b);
        assert!(serde_json::from_str::<Order>(r#""""#).is_err());
    }

    #[test]
    fn vendor_keys_shorthand() {
        let k: VendorKeys = serde_json::from_str(r#""abc""#).unwrap();
        assert_eq!(k.get("api_key"), Some("abc"));
        let k: VendorKeys = serde_json::from_str(r#"{"app_key":"a","app_secret":"b"}"#).unwrap();
        assert_eq!(k.get("app_secret"), Some("b"));
        assert!(!format!("{k:?}").contains('b') || format!("{k:?}").contains("***"));
    }

    #[test]
    fn window_aliases() {
        let w: WindowBudget =
            serde_json::from_str(r#"{"monthly_credits": 5, "daily_requests": 2}"#).unwrap();
        assert_eq!((w.monthly, w.daily), (Some(5), Some(2)));
    }
}
