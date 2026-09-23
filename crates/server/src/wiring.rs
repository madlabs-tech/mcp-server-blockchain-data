//! Composition root: config → vendor factories → registry → router → catalog → app.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use ems_app::{App, Catalog};
use ems_config::{ConfigDir, ConfigLoader, EnvSource, Loaded, Mode};
use ems_ports::{EvmRpc, PortHandle, PortResult, ProviderError, Registration};
use ems_routing::{ProviderRegistry, Router, RouterOptions, RoutingTable};
use ems_store::{ClientGuard, Store};
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::OnceCell;

pub struct Built {
    pub loaded: Arc<Loaded>,
    pub app: Arc<App>,
    pub store: Store,
}

pub fn load_config(dir: PathBuf) -> Result<(ConfigLoader, Loaded)> {
    let loader = ConfigLoader::new(ConfigDir::new(dir), EnvSource::from_process())
        .map_err(|issues| anyhow::anyhow!(render(&issues)))?;
    let loaded = loader
        .load()
        .map_err(|issues| anyhow::anyhow!(render(&issues)))?;
    for w in &loaded.warnings {
        tracing::warn!(path = %w.path, "{}", w.message);
    }
    Ok((loader, loaded))
}

pub fn render(issues: &[ems_config::Issue]) -> String {
    let lines: Vec<String> = issues.iter().map(|i| format!("  - {i}")).collect();
    format!("invalid configuration:\n{}", lines.join("\n"))
}

/// Open `<data_dir>/ems.db`. Self-hosted falls back to an in-memory store when the directory
/// isn't writable (e.g. Claude Desktop starting us with cwd `/`); hosted mode must persist.
pub fn open_store(loaded: &Loaded) -> Result<Store> {
    let path = loaded.settings.server.data_dir.join("ems.db");
    match Store::open(&path) {
        Ok(s) => Ok(s),
        Err(e) if loaded.settings.server.mode == Mode::SelfHosted => {
            tracing::warn!(path = %path.display(), "store unavailable ({e}); usage counters are in-memory only");
            Store::open_in_memory().context("opening in-memory store")
        }
        Err(e) => Err(e).with_context(|| format!("opening {}", path.display())),
    }
}

/// Chain RPC + every compiled-in vendor module (feature-gated in `ems-adapters`).
pub fn registrations(loaded: &Loaded) -> Vec<Registration> {
    ems_adapters::factory::base_registrations(loaded)
        .into_iter()
        .chain(ems_adapters::vendors::registrations(loaded))
        .map(guard_chain_ids)
        .collect()
}

/// Complete registry for a (re)loaded config: chain RPC + vendor modules + the second stage
/// (`rpc` pseudo-vendor and on-chain oracles, built on the routed RPC so they need the router).
/// Used at startup and by admin reloads / SIGHUP.
pub fn full_registry(loaded: &Loaded, router: &Arc<Router>) -> ProviderRegistry {
    let mut registry = ProviderRegistry::new(registrations(loaded));
    for reg in ems_protocols::rpc_registrations(loaded, router) {
        registry.add(reg);
    }
    registry
}

pub fn build(loaded: Loaded, store: Store) -> Result<Built> {
    let hosted = loaded.settings.server.mode == Mode::Hosted;
    if hosted && store.active_client_count() == 0 {
        // Fail closed: never serve shared vendor quotas without client keys.
        bail!(
            "hosted mode requires at least one client key; create one with \
             `evm-mcp-server clients create <name>` (or in the dashboard) and restart"
        );
    }
    let loaded = Arc::new(loaded);
    let router = Router::new(
        RoutingTable {
            config: loaded.clone(),
            registry: ProviderRegistry::new(registrations(&loaded)),
        },
        Arc::new(store.clone()),
        RouterOptions::default(),
    );
    router.swap(RoutingTable {
        config: loaded.clone(),
        registry: full_registry(&loaded, &router),
    });
    let mut catalog = Catalog::new();
    ems_app::ops::register_all(&mut catalog);
    let mut app = App::new(
        catalog,
        router.clone(),
        loaded.settings.server.cache_max_entries,
    );
    if hosted {
        app = app.with_guard(Arc::new(ClientGuard::new(store.clone(), router)));
    }
    // Every call on every transport (stdio, /mcp, REST) reaches the call log / SSE stream.
    app = app.with_observer(Arc::new(store.clone()));
    Ok(Built {
        loaded,
        app: Arc::new(app),
        store,
    })
}

/// Wrap every EVM RPC port so its chain id is checked against `eth_chainId` once, on first use
/// (lazy, so startup never blocks on the network). A mismatch disables that port permanently.
fn guard_chain_ids(mut reg: Registration) -> Registration {
    for (_, handle) in reg.ports.iter_mut() {
        if let PortHandle::EvmRpc(inner) = handle {
            let vendor = reg.vendor.id.clone();
            *handle = PortHandle::EvmRpc(Arc::new(ChainIdGuard {
                inner: inner.clone(),
                vendor,
                checked: OnceCell::new(),
            }));
        }
    }
    reg
}

struct ChainIdGuard {
    inner: Arc<dyn EvmRpc>,
    vendor: String,
    checked: OnceCell<Result<(), ProviderError>>,
}

#[async_trait]
impl EvmRpc for ChainIdGuard {
    fn chain_id(&self) -> u64 {
        self.inner.chain_id()
    }

    async fn request(&self, method: &str, params: Value) -> PortResult<Value> {
        let check = self
            .checked
            .get_or_try_init(|| async {
                let v = self.inner.request("eth_chainId", json!([])).await?;
                let got = v.as_str().and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok());
                let want = self.inner.chain_id();
                Ok::<_, ProviderError>(if got == Some(want) {
                    Ok(())
                } else {
                    tracing::error!(vendor = %self.vendor, want, ?got, "RPC endpoint serves the wrong chain; disabled");
                    Err(ProviderError::Fatal(format!("chain id mismatch: expected {want}, endpoint reports {got:?}")))
                })
            })
            .await?;
        check.clone()?;
        self.inner.request(method, params).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ems_config::{ConfigDir, ConfigLoader, EnvSource};
    use ems_domain::ChainId;
    use ems_ports::Capability;

    #[test]
    fn second_stage_registers_rpc_pseudo_vendor() {
        let loader =
            ConfigLoader::new(ConfigDir::new("/nonexistent"), EnvSource::default()).unwrap();
        let store = Store::open_in_memory().unwrap();
        let built = build(loader.load_texts("", "").unwrap(), store).unwrap();
        let table = built.app.router().table();
        let base = ChainId::evm(8453);
        for cap in [
            Capability::TokenBalances,
            Capability::TransferHistory,
            Capability::FeeEstimate,
        ] {
            assert!(
                table
                    .registry
                    .registered_for(cap, Some(&base))
                    .contains(&"rpc".to_string()),
                "{cap} missing rpc pseudo-vendor on Base"
            );
        }
    }
}
