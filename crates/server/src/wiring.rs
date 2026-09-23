//! Composition root: config → vendor factories → registry → router → catalog → app.

use anyhow::{bail, Result};
use async_trait::async_trait;
use ems_app::{App, Catalog};
use ems_config::{ConfigDir, ConfigLoader, EnvSource, Loaded, Mode};
use ems_ports::{EvmRpc, PortHandle, PortResult, ProviderError, Registration};
use ems_routing::{InMemoryCounterStore, ProviderRegistry, Router, RouterOptions, RoutingTable};
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::OnceCell;

pub struct Built {
    pub loaded: Arc<Loaded>,
    pub app: Arc<App>,
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

fn render(issues: &[ems_config::Issue]) -> String {
    let lines: Vec<String> = issues.iter().map(|i| format!("  - {i}")).collect();
    format!("invalid configuration:\n{}", lines.join("\n"))
}

/// Chain RPC + every compiled-in vendor module (feature-gated in `ems-adapters`).
pub fn registrations(loaded: &Loaded) -> Vec<Registration> {
    ems_adapters::factory::base_registrations(loaded)
        .into_iter()
        .chain(ems_adapters::vendors::registrations(loaded))
        .map(guard_chain_ids)
        .collect()
}

pub fn build(loaded: Loaded) -> Result<Built> {
    if loaded.settings.server.mode == Mode::Hosted {
        // Fail closed until client-key auth (T1.D3) is wired in: never serve shared quotas openly.
        bail!("hosted mode requires client-key authentication, which is not available in this build yet");
    }
    let loaded = Arc::new(loaded);
    let registry = ProviderRegistry::new(registrations(&loaded));
    let router = Router::new(
        RoutingTable {
            config: loaded.clone(),
            registry: registry.clone(),
        },
        Arc::new(InMemoryCounterStore::default()),
        RouterOptions::default(),
    );
    // Second stage: `rpc` pseudo-vendor + on-chain oracles are built on the routed RPC, so they
    // need the router; add them and swap in the complete table.
    let mut registry = registry;
    for reg in ems_protocols::rpc_registrations(&loaded, &router) {
        registry.add(reg);
    }
    router.swap(RoutingTable {
        config: loaded.clone(),
        registry,
    });
    let mut catalog = Catalog::new();
    ems_app::ops::register_all(&mut catalog);
    let app = Arc::new(App::new(
        catalog,
        router,
        loaded.settings.server.cache_max_entries,
    ));
    Ok(Built { loaded, app })
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
        let built = build(loader.load_texts("", "").unwrap()).unwrap();
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
