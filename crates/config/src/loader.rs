use crate::{
    registry::{ChainRegistry, Registry},
    settings::{Mode, Settings, Strategy},
};
use figment::{
    providers::{Format, Serialized, Toml},
    Figment,
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

/// A validation finding. Errors prevent the config from being applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Issue {
    pub severity: Severity,
    /// Dotted key path, e.g. `routing.defaults.evm_rpc`.
    pub path: String,
    pub message: String,
}

impl Issue {
    pub fn error(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            path: path.into(),
            message: message.into(),
        }
    }
    pub fn warning(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} at {}: {}", self.severity, self.path, self.message)
    }
}

/// Directory holding `config.toml`, `secrets.toml` (0600) and the admin token.
#[derive(Debug, Clone)]
pub struct ConfigDir {
    pub root: PathBuf,
}

impl ConfigDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn config_path(&self) -> PathBuf {
        self.root.join("config.toml")
    }
    pub fn secrets_path(&self) -> PathBuf {
        self.root.join("secrets.toml")
    }
    pub(crate) fn read(path: &Path) -> Result<String, Issue> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(Issue::error(path.display().to_string(), e.to_string())),
        }
    }
}

/// Environment snapshot (injectable for tests; never read the process env elsewhere).
#[derive(Debug, Clone, Default)]
pub struct EnvSource(pub BTreeMap<String, String>);

impl EnvSource {
    pub fn from_process() -> Self {
        Self(std::env::vars().collect())
    }
    pub fn from_pairs<I: IntoIterator<Item = (K, V)>, K: Into<String>, V: Into<String>>(
        pairs: I,
    ) -> Self {
        Self(
            pairs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        )
    }
    fn get(&self, k: &str) -> Option<&str> {
        self.0
            .get(k)
            .map(String::as_str)
            .filter(|v| !v.trim().is_empty())
    }
}

/// Fully loaded, validated configuration.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub settings: Settings,
    pub registry: Registry,
    /// Key paths set by env → the env var name that set it ("locked by env").
    pub locked: BTreeMap<Vec<String>, String>,
    pub warnings: Vec<Issue>,
    pub dir: ConfigDir,
}

impl Loaded {
    /// If `path` (or a parent/child of it) is set by env, the env var responsible.
    pub fn locked_by(&self, path: &[String]) -> Option<&str> {
        self.locked
            .iter()
            .find(|(p, _)| p.starts_with(path) || path.starts_with(p))
            .map(|(_, var)| var.as_str())
    }
}

pub struct ConfigLoader {
    pub dir: ConfigDir,
    pub env: EnvSource,
    registry: Registry,
}

const ENV_PREFIX: &str = "BDM__";

impl ConfigLoader {
    pub fn new(dir: ConfigDir, env: EnvSource) -> Result<Self, Vec<Issue>> {
        let registry = Registry::builtin().map_err(|e| vec![Issue::error("registry", e)])?;
        Ok(Self { dir, env, registry })
    }

    pub fn load(&self) -> Result<Loaded, Vec<Issue>> {
        let config = ConfigDir::read(&self.dir.config_path()).map_err(|e| vec![e])?;
        let secrets = ConfigDir::read(&self.dir.secrets_path()).map_err(|e| vec![e])?;
        self.load_texts(&config, &secrets)
    }

    /// Load from explicit file contents (used to validate edits before writing them).
    pub fn load_texts(&self, config_toml: &str, secrets_toml: &str) -> Result<Loaded, Vec<Issue>> {
        let mut issues = Vec::new();
        let (env_tree, locked) = self.env_layer(&mut issues);

        let figment = Figment::new()
            .merge(Toml::string(config_toml))
            .merge(Toml::string(secrets_toml))
            .merge(Serialized::defaults(env_tree));
        let settings: Settings = match figment.extract() {
            Ok(s) => s,
            Err(err) => {
                issues.extend(err.into_iter().map(|e| {
                    let path = e.path.join(".");
                    Issue::error(
                        if path.is_empty() {
                            "config".into()
                        } else {
                            path
                        },
                        e.kind.to_string(),
                    )
                }));
                return Err(issues);
            }
        };

        let registry = match self.apply_chain_changes(&settings) {
            Ok(r) => r,
            Err(e) => {
                issues.push(Issue::error("extra_chains", e));
                return Err(issues);
            }
        };

        let loaded = Loaded {
            settings,
            registry,
            locked,
            warnings: Vec::new(),
            dir: self.dir.clone(),
        };
        issues.extend(validate(&loaded));
        let (errors, warnings): (Vec<_>, Vec<_>) = issues
            .into_iter()
            .partition(|i| i.severity == Severity::Error);
        if !errors.is_empty() {
            return Err(errors.into_iter().chain(warnings).collect());
        }
        Ok(Loaded { warnings, ..loaded })
    }

    /// Build the env layer as a nested value tree plus the set of locked paths.
    fn env_layer(
        &self,
        issues: &mut Vec<Issue>,
    ) -> (serde_json::Value, BTreeMap<Vec<String>, String>) {
        let mut root = serde_json::Map::new();
        let mut locked = BTreeMap::new();

        for (var, raw) in &self.env.0 {
            let Some(rest) = var.strip_prefix(ENV_PREFIX) else {
                continue;
            };
            let path: Vec<String> = rest.split("__").map(str::to_lowercase).collect();
            if path.iter().any(String::is_empty) {
                issues.push(Issue::warning(var.clone(), "ignored: empty path segment"));
                continue;
            }
            insert(&mut root, &path, typed(raw));
            locked.insert(path, var.clone());
        }

        for (vendor, entry) in &self.registry.vendors {
            for (field, var) in &entry.keys {
                if let Some(v) = self.env.get(var) {
                    let path = vec!["keys".to_string(), vendor.clone(), field.clone()];
                    insert(&mut root, &path, serde_json::Value::String(v.to_owned()));
                    locked.insert(path, var.clone());
                }
            }
        }

        if let Some(url) = self.env.get("RPC_URL") {
            issues.push(Issue::warning(
                "RPC_URL",
                "deprecated: RPC_URL now applies to Ethereum (eip155:1) only; use [custom_rpc] or vendor keys",
            ));
            let path = vec!["custom_rpc".to_string(), "rpc_url".to_string()];
            insert(
                &mut root,
                &path,
                serde_json::json!({ "chain": "eip155:1", "url": url }),
            );
            locked.insert(path, "RPC_URL".into());
        }

        (serde_json::Value::Object(root), locked)
    }

    fn apply_chain_changes(&self, s: &Settings) -> Result<Registry, String> {
        let mut chains: Vec<_> = self.registry.chains.all().to_vec();
        chains.extend(s.extra_chains.iter().cloned());
        let tmp = ChainRegistry::new(chains.clone())?;
        for (key, ov) in &s.chain_overrides {
            let id = tmp
                .find(key)
                .ok_or_else(|| format!("chain_overrides: unknown chain '{key}'"))?
                .id
                .clone();
            let Some(c) = chains.iter_mut().find(|c| c.id == id) else {
                continue;
            };
            if let Some(e) = ov.enabled {
                c.enabled = e;
            }
            if let Some(rpcs) = &ov.public_rpc {
                c.public_rpc = rpcs.clone();
            }
        }
        Ok(Registry {
            chains: ChainRegistry::new(chains)?,
            ..self.registry.clone()
        })
    }
}

fn typed(raw: &str) -> serde_json::Value {
    let t = raw.trim();
    match t {
        "true" => serde_json::Value::Bool(true),
        "false" => serde_json::Value::Bool(false),
        _ => t
            .parse::<u64>()
            .map(serde_json::Value::from)
            .unwrap_or_else(|_| serde_json::Value::String(t.to_owned())),
    }
}

fn insert(
    root: &mut serde_json::Map<String, serde_json::Value>,
    path: &[String],
    value: serde_json::Value,
) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut cur = root;
    for seg in parents {
        let next = cur
            .entry(seg.clone())
            .or_insert_with(|| serde_json::Value::Object(Default::default()));
        if !next.is_object() {
            *next = serde_json::Value::Object(Default::default());
        }
        let serde_json::Value::Object(m) = next else {
            return;
        };
        cur = m;
    }
    cur.insert(last.clone(), value);
}

fn validate(l: &Loaded) -> Vec<Issue> {
    let s = &l.settings;
    let mut out = Vec::new();

    let known_vendor = |v: &str| l.registry.vendors.contains_key(v) || s.custom_rpc.contains_key(v);
    let mut check_order = |path: String, order: &crate::Order| {
        for v in order.iter() {
            if !known_vendor(v) {
                out.push(Issue::error(path.clone(), format!("unknown vendor '{v}'")));
            }
        }
    };
    for (cap, order) in &s.routing.defaults {
        check_order(format!("routing.defaults.{cap}"), order);
    }
    for (chain, caps) in &s.routing.chains {
        for (cap, order) in caps {
            check_order(format!("routing.chains.\"{chain}\".{cap}"), order);
        }
    }
    for (op, os) in &s.operations {
        for (cap, order) in &os.order {
            check_order(format!("operations.{op}.order.{cap}"), order);
        }
    }

    for chain in s.routing.chains.keys() {
        if l.registry.chains.find(chain).is_none() {
            out.push(Issue::error(
                format!("routing.chains.\"{chain}\""),
                format!("unknown chain '{chain}'"),
            ));
        }
    }
    for (name, c) in &s.custom_rpc {
        if l.registry.chains.find(&c.chain).is_none() {
            out.push(Issue::error(
                format!("custom_rpc.{name}.chain"),
                format!("unknown chain '{}'", c.chain),
            ));
        }
        let url = c.url.expose();
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            out.push(Issue::error(
                format!("custom_rpc.{name}.url"),
                "must be an http(s) URL",
            ));
        }
        if l.registry.vendors.contains_key(name) {
            out.push(Issue::error(
                format!("custom_rpc.{name}"),
                "name collides with a built-in vendor id",
            ));
        }
    }

    let bind = |path: &str, v: &str, out: &mut Vec<Issue>| {
        if v.parse::<SocketAddr>().is_err() {
            out.push(Issue::error(
                path,
                format!("'{v}' is not a socket address (host:port)"),
            ));
        }
    };
    bind("server.http_bind", &s.server.http_bind, &mut out);
    if let Some(b) = &s.server.public_bind {
        bind("server.public_bind", b, &mut out);
    }
    if let Some(b) = &s.server.admin_bind {
        bind("server.admin_bind", b, &mut out);
    }
    if s.server.mode == Mode::Hosted {
        if s.server.public_bind.is_none() {
            out.push(Issue::error(
                "server.public_bind",
                "hosted mode requires public_bind",
            ));
        }
        if s.server.admin_bind.is_some() && s.server.admin_bind == s.server.public_bind {
            out.push(Issue::error(
                "server.admin_bind",
                "admin_bind must differ from public_bind",
            ));
        }
    }

    for (v, vs) in &s.vendors {
        if !l.registry.vendors.contains_key(v) && !s.custom_rpc.contains_key(v) {
            out.push(Issue::warning(
                format!("vendors.{v}"),
                format!("unknown vendor '{v}' (ignored)"),
            ));
        }
        if vs.reserve_pct.is_some_and(|r| r > 100) {
            out.push(Issue::error(
                format!("vendors.{v}.reserve_pct"),
                "must be 0..=100",
            ));
        }
        let limit = l
            .registry
            .vendors
            .get(v)
            .map(|e| e.limit)
            .unwrap_or_default();
        let limit = crate::resolve::overlay(limit, vs.limit);
        for (w, cap, lim) in [
            ("rps", vs.cap.rps, limit.rps),
            ("per_minute", vs.cap.per_minute, limit.per_minute),
            ("daily", vs.cap.daily, limit.daily),
            ("monthly", vs.cap.monthly, limit.monthly),
        ] {
            if let (Some(c), Some(li)) = (cap, lim) {
                if c > li {
                    out.push(Issue::warning(
                        format!("vendors.{v}.cap.{w}"),
                        format!("cap {c} is above the vendor limit {li}; the limit still applies"),
                    ));
                }
            }
        }
    }

    for (op, os) in &s.operations {
        if os.strategy == Some(Strategy::Quorum) && os.quorum.unwrap_or(2) < 2 {
            out.push(Issue::error(
                format!("operations.{op}.quorum"),
                "quorum must be >= 2",
            ));
        }
        if os.fan_out == Some(0) {
            out.push(Issue::error(
                format!("operations.{op}.fan_out"),
                "fan_out must be >= 1",
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OrderLevel, VendorStatus};
    use bdm_domain::ChainId;
    use bdm_ports::Capability;

    fn loader(env: &[(&str, &str)]) -> (tempfile::TempDir, ConfigLoader) {
        let dir = tempfile::tempdir().unwrap();
        let l = ConfigLoader::new(
            ConfigDir::new(dir.path()),
            EnvSource::from_pairs(env.iter().copied()),
        )
        .unwrap();
        (dir, l)
    }

    fn eth() -> ChainId {
        "eip155:1".parse().unwrap()
    }

    #[test]
    fn defaults_load_with_no_files() {
        let (_d, l) = loader(&[]);
        let loaded = l.load().unwrap();
        let r = loaded.order(Capability::EvmRpc, Some(&eth()), None);
        assert_eq!(r.vendors, ["alchemy", "quicknode", "public"]);
        assert_eq!(r.level, OrderLevel::BuiltIn);
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn precedence_env_over_config_and_locking() {
        let (_d, l) = loader(&[("BDM__ROUTING__DEFAULTS__EVM_RPC", "public,alchemy")]);
        let cfg = "[routing.defaults]\nevm_rpc = [\"quicknode\", \"alchemy\"]\n";
        let loaded = l.load_texts(cfg, "").unwrap();
        let r = loaded.order(Capability::EvmRpc, Some(&eth()), None);
        assert_eq!(r.vendors, ["public", "alchemy"]);
        assert_eq!(r.level, OrderLevel::Default);
        let path: Vec<String> = ["routing", "defaults", "evm_rpc"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            loaded.locked_by(&path),
            Some("BDM__ROUTING__DEFAULTS__EVM_RPC")
        );
        assert_eq!(
            loaded.locked_by(&["routing".to_string()]),
            Some("BDM__ROUTING__DEFAULTS__EVM_RPC")
        );
        assert_eq!(loaded.locked_by(&["vendors".to_string()]), None);
    }

    #[test]
    fn op_over_chain_over_default() {
        let (_d, l) = loader(&[]);
        let cfg = r#"
[routing.defaults]
price = ["defillama"]
[routing.chains.base]
price = ["dexscreener"]
[operations.market_get_price.order]
price = ["geckoterminal"]
"#;
        let loaded = l.load_texts(cfg, "").unwrap();
        let base: ChainId = "eip155:8453".parse().unwrap();
        assert_eq!(
            loaded
                .order(Capability::Price, Some(&base), Some("market_get_price"))
                .vendors,
            ["geckoterminal"]
        );
        assert_eq!(
            loaded
                .order(Capability::Price, Some(&base), Some("other"))
                .vendors,
            ["dexscreener"]
        );
        assert_eq!(
            loaded.order(Capability::Price, Some(&eth()), None).vendors,
            ["defillama"]
        );
        let sol: ChainId = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp".parse().unwrap();
        let (_d2, l2) = loader(&[]);
        let plain = l2.load().unwrap();
        assert_eq!(
            plain.order(Capability::Price, Some(&sol), None).vendors[0],
            "jupiter"
        );
    }

    #[test]
    fn env_numbers_aliases_and_keys() {
        let (_d, l) = loader(&[
            ("BDM__VENDORS__ALCHEMY__CAP__MONTHLY_CREDITS", "15000000"),
            ("BDM__VENDORS__ALCHEMY__RESERVE_PCT", "20"),
            ("ALCHEMY_API_KEY", "alc_secret_123"),
            ("BDM__SERVER__DASHBOARD", "false"),
        ]);
        let loaded = l.load().unwrap();
        assert_eq!(
            loaded.settings.vendors["alchemy"].cap.monthly,
            Some(15_000_000)
        );
        assert!(!loaded.settings.server.dashboard);
        assert_eq!(loaded.vendor_status("alchemy"), VendorStatus::Active);
        assert!(matches!(
            loaded.vendor_status("helius"),
            VendorStatus::MissingKey { .. }
        ));
        assert_eq!(loaded.vendor_status("public"), VendorStatus::Active);
        assert!(matches!(
            loaded.vendor_status("ankr"),
            VendorStatus::Disabled { unverified: true }
        ));
        let b = loaded.effective_budget("alchemy").unwrap();
        assert_eq!(b.limit.monthly, Some(30_000_000));
        // min(cap 15M, 30M * 0.8 = 24M) = 15M
        assert_eq!(b.effective.monthly, Some(15_000_000));
        let url = loaded.rpc_url("alchemy", &eth()).unwrap();
        assert_eq!(
            url.expose(),
            "https://eth-mainnet.g.alchemy.com/v2/alc_secret_123"
        );
        assert!(loaded
            .rpc_url(
                "helius",
                &"solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp".parse().unwrap()
            )
            .is_none());
        assert!(!format!("{:?}", loaded.settings).contains("alc_secret_123"));
    }

    #[test]
    fn secrets_file_shorthand_and_quicknode_pair() {
        let (_d, l) = loader(&[
            ("QN_ENDPOINT_NAME", "my-node"),
            ("QN_TOKEN_ID", "tok123456"),
        ]);
        let loaded = l
            .load_texts("", "[keys]\nhelius = \"hel_key_123\"\n")
            .unwrap();
        assert_eq!(loaded.vendor_status("helius"), VendorStatus::Active);
        let base: ChainId = "eip155:8453".parse().unwrap();
        assert_eq!(
            loaded.rpc_url("quicknode", &base).unwrap().expose(),
            "https://my-node.base-mainnet.quiknode.pro/tok123456/"
        );
    }

    #[test]
    fn legacy_rpc_url_is_ethereum_only() {
        let (_d, l) = loader(&[("RPC_URL", "http://127.0.0.1:9999")]);
        let loaded = l.load().unwrap();
        assert!(loaded.warnings.iter().any(|w| w.path == "RPC_URL"));
        assert_eq!(
            loaded.order(Capability::EvmRpc, Some(&eth()), None).vendors,
            ["rpc_url", "alchemy", "quicknode", "public"]
        );
        let base: ChainId = "eip155:8453".parse().unwrap();
        assert_eq!(
            loaded.order(Capability::EvmRpc, Some(&base), None).vendors,
            ["alchemy", "quicknode", "public"]
        );
        assert_eq!(
            loaded.rpc_url("rpc_url", &eth()).unwrap().expose(),
            "http://127.0.0.1:9999"
        );
        assert!(loaded.rpc_url("rpc_url", &base).is_none());
    }

    #[test]
    fn validation_collects_all_errors() {
        let (_d, l) = loader(&[]);
        let cfg = r#"
[server]
http_bind = "not-an-addr"
[routing.defaults]
evm_rpc = ["alchemy", "nosuchvendor"]
[routing.chains.dogechain]
price = ["coingecko"]
[vendors.alchemy]
reserve_pct = 150
"#;
        let errs = l.load_texts(cfg, "").unwrap_err();
        let paths: Vec<_> = errs
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .map(|i| i.path.as_str())
            .collect();
        assert!(paths.contains(&"server.http_bind"), "{errs:?}");
        assert!(paths.contains(&"routing.defaults.evm_rpc"), "{errs:?}");
        assert!(paths.iter().any(|p| p.contains("dogechain")), "{errs:?}");
        assert!(paths.contains(&"vendors.alchemy.reserve_pct"), "{errs:?}");
    }

    #[test]
    fn typos_and_unknown_capabilities_are_errors() {
        let (_d, l) = loader(&[]);
        assert!(l
            .load_texts("[server]\nhttp_bnid = \"127.0.0.1:1\"\n", "")
            .is_err());
        assert!(l
            .load_texts("[routing.defaults]\nprices = [\"coingecko\"]\n", "")
            .is_err());
    }

    #[test]
    fn cap_above_limit_is_a_warning() {
        let (_d, l) = loader(&[]);
        let loaded = l
            .load_texts("[vendors.coingecko.cap]\nmonthly = 50000\n", "")
            .unwrap();
        assert!(loaded
            .warnings
            .iter()
            .any(|w| w.path == "vendors.coingecko.cap.monthly"));
    }

    #[test]
    fn hosted_mode_requires_public_bind_and_separate_admin() {
        let (_d, l) = loader(&[]);
        assert!(l.load_texts("[server]\nmode = \"hosted\"\n", "").is_err());
        let bad = "[server]\nmode = \"hosted\"\npublic_bind = \"0.0.0.0:8787\"\nadmin_bind = \"0.0.0.0:8787\"\n";
        assert!(l.load_texts(bad, "").is_err());
        let ok = "[server]\nmode = \"hosted\"\npublic_bind = \"0.0.0.0:8787\"\nadmin_bind = \"127.0.0.1:8788\"\n";
        assert!(l.load_texts(ok, "").is_ok());
    }

    #[test]
    fn chain_overrides_disable_chains() {
        let (_d, l) = loader(&[]);
        let loaded = l
            .load_texts("[chain_overrides.bsc]\nenabled = false\n", "")
            .unwrap();
        assert!(loaded.registry.chains.resolve("bsc").is_err());
        assert!(loaded.registry.chains.resolve("base").is_ok());
    }

    #[test]
    fn example_config_is_valid() {
        let (_d, l) = loader(&[]);
        let example = include_str!("../../../config/config.example.toml");
        let loaded = l.load_texts(example, "").unwrap();
        assert_eq!(loaded.settings.server.tool_profile, "payments");
    }
}
