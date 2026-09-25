//! Composition root: config → vendor factories → registry → router → catalog → app.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use bdm_app::{App, Catalog};
use bdm_config::{ConfigDir, ConfigLoader, EnvSource, Loaded, Mode, VendorStatus};
use bdm_domain::{ChainFamily, ChainId};
use bdm_ports::{
    Capability, EvmRpc, PortHandle, PortKind, PortResult, ProviderError, Registration,
};
use bdm_routing::{ProviderRegistry, Router, RouterOptions, RoutingTable};
use bdm_store::{ClientGuard, Store};
use futures::StreamExt;
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc, time::Duration};
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

pub fn render(issues: &[bdm_config::Issue]) -> String {
    let lines: Vec<String> = issues.iter().map(|i| format!("  - {i}")).collect();
    format!("invalid configuration:\n{}", lines.join("\n"))
}

/// Open `<data_dir>/bdm.db`. Self-hosted falls back to an in-memory store when the directory
/// isn't writable (e.g. Claude Desktop starting us with cwd `/`); hosted mode must persist.
pub fn open_store(loaded: &Loaded) -> Result<Store> {
    let path = loaded.settings.server.data_dir.join("bdm.db");
    match Store::open(&path) {
        Ok(s) => Ok(s),
        Err(e) if loaded.settings.server.mode == Mode::SelfHosted => {
            tracing::warn!(path = %path.display(), "store unavailable ({e}); usage counters are in-memory only");
            Store::open_in_memory().context("opening in-memory store")
        }
        Err(e) => Err(e).with_context(|| format!("opening {}", path.display())),
    }
}

/// Chain RPC + every compiled-in vendor module (feature-gated in `bdm-adapters`).
pub fn registrations(loaded: &Loaded) -> Vec<Registration> {
    bdm_adapters::factory::base_registrations(loaded)
        .into_iter()
        .chain(bdm_adapters::vendors::registrations(loaded))
        .map(guard_chain_ids)
        .collect()
}

/// Complete registry for a (re)loaded config: chain RPC + vendor modules + the second stage
/// (`rpc` pseudo-vendor and on-chain oracles, built on the routed RPC so they need the router).
/// Used at startup and by admin reloads / SIGHUP.
pub fn full_registry(loaded: &Loaded, router: &Arc<Router>) -> ProviderRegistry {
    let mut registry = ProviderRegistry::new(registrations(loaded));
    for reg in bdm_protocols::rpc_registrations(loaded, router) {
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
             `onchain-data-mcp clients create <name>` (or in the dashboard) and restart"
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
    bdm_app::ops::register_all(&mut catalog);
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

const WARMUP_CONCURRENCY: usize = 4;
const WARMUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Run the lazy chain-id check ([`ChainIdGuard`]) for every EVM RPC of every active vendor now,
/// in the background, so a wrong endpoint is logged at startup instead of on the first request.
/// Never blocks startup or fails the process; `server.warmup = false` skips it.
pub fn spawn_warmup(router: Arc<Router>) {
    tokio::spawn(warmup(router));
}

async fn warmup(router: Arc<Router>) {
    let table = router.table();
    let mut jobs: Vec<(String, ChainId, Arc<dyn EvmRpc>)> = Vec::new();
    for c in table.config.registry.chains.enabled() {
        if c.family != ChainFamily::Evm {
            continue;
        }
        let chain = &c.id;
        for vendor in table
            .registry
            .registered_for(Capability::EvmRpc, Some(chain))
        {
            if table.config.vendor_status(&vendor) != VendorStatus::Active {
                continue;
            }
            let port = table
                .registry
                .get(Capability::EvmRpc, Some(chain), &vendor)
                .and_then(<dyn EvmRpc>::extract);
            if let Some(port) = port {
                jobs.push((vendor, chain.clone(), port));
            }
        }
    }
    let total = jobs.len();
    let started = std::time::Instant::now();
    futures::stream::iter(jobs)
        .for_each_concurrent(WARMUP_CONCURRENCY, |(vendor, chain, port)| async move {
            // Straight to the port (not the router): no breaker or counter side effects.
            // A chain-id mismatch is logged at `error!` by `ChainIdGuard` itself.
            match tokio::time::timeout(WARMUP_TIMEOUT, port.request("eth_chainId", json!([]))).await
            {
                Ok(Ok(_)) => tracing::debug!(vendor, %chain, "warm-up ok"),
                Ok(Err(e)) => tracing::warn!(vendor, %chain, "warm-up failed: {e}"),
                Err(_) => tracing::warn!(vendor, %chain, "warm-up timed out"),
            }
        })
        .await;
    tracing::info!(
        endpoints = total,
        ms = started.elapsed().as_millis() as u64,
        "warm-up done"
    );
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
        if method == "eth_chainId" {
            // Verified above: answer without another round trip (warm-up, chain_get_info).
            return Ok(json!(format!("{:#x}", self.inner.chain_id())));
        }
        self.inner.request(method, params).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bdm_config::{ConfigDir, ConfigLoader, EnvSource};
    use bdm_ports::VendorMeta;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeRpc {
        chain: u64,
        reports: &'static str,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl EvmRpc for FakeRpc {
        fn chain_id(&self) -> u64 {
            self.chain
        }
        async fn request(&self, method: &str, _params: Value) -> PortResult<Value> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match method {
                "eth_chainId" => Ok(json!(self.reports)),
                _ => Ok(json!("0x1")),
            }
        }
    }

    #[tokio::test]
    async fn warmup_checks_every_active_evm_rpc_once_and_disables_mismatches() {
        let loader =
            ConfigLoader::new(ConfigDir::new("/nonexistent"), EnvSource::default()).unwrap();
        let config = Arc::new(loader.load_texts("", "").unwrap());
        let good = Arc::new(FakeRpc {
            chain: 1,
            reports: "0x1",
            calls: AtomicUsize::new(0),
        });
        let bad = Arc::new(FakeRpc {
            chain: 8453,
            reports: "0x1",
            calls: AtomicUsize::new(0),
        });
        let meta = VendorMeta {
            id: "public".into(),
            display_name: "public".into(),
            ..Default::default()
        };
        let reg = guard_chain_ids(
            Registration::new(meta)
                .chain_port(ChainId::evm(1), PortHandle::EvmRpc(good.clone()))
                .chain_port(ChainId::evm(8453), PortHandle::EvmRpc(bad.clone())),
        );
        let table = RoutingTable {
            config,
            registry: ProviderRegistry::new(vec![reg]),
        };
        let router = Router::new(
            table,
            Arc::new(bdm_routing::InMemoryCounterStore::default()),
            RouterOptions::default(),
        );
        warmup(router.clone()).await;
        assert_eq!(good.calls.load(Ordering::SeqCst), 1);
        assert_eq!(bad.calls.load(Ordering::SeqCst), 1);

        // The guard cached the verdicts: the good port passes through, the bad one is disabled.
        let table = router.table();
        let port = |chain: u64| {
            table
                .registry
                .get(Capability::EvmRpc, Some(&ChainId::evm(chain)), "public")
                .and_then(<dyn EvmRpc>::extract)
                .unwrap()
        };
        port(1).request("eth_blockNumber", json!([])).await.unwrap();
        assert_eq!(good.calls.load(Ordering::SeqCst), 2);
        let err = port(8453)
            .request("eth_blockNumber", json!([]))
            .await
            .unwrap_err();
        assert!(matches!(err, ProviderError::Fatal(_)), "{err}");
        assert_eq!(bad.calls.load(Ordering::SeqCst), 1, "never called again");
    }

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
