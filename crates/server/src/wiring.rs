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

/// Registry for a (re)loaded config: used at startup and by admin reloads / SIGHUP.
pub fn rebuild_registry(loaded: &Loaded) -> ProviderRegistry {
    ProviderRegistry::new(registrations(loaded))
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
            registry: rebuild_registry(&loaded),
        },
        Arc::new(store.clone()),
        RouterOptions::default(),
    );
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
    // TODO(lead, on merge with f577fb2): log every call on every transport (stdio, /mcp, REST):
    //
    //     struct CallLog(Store);
    //     impl ems_app::CallObserver for CallLog {
    //         fn on_call(&self, c: &ems_app::Caller, op: &str, r: &Result<Value, DomainError>, d: Duration) {
    //             self.0.log_call(ems_store::CallRecord::from_result(c.client.as_deref(), op, r, d));
    //         }
    //     }
    //     app = app.with_observer(Arc::new(CallLog(store.clone())));
    //
    // and remove the REST-only `.layer(Extension(store))` call logging in `main.rs`
    // (otherwise REST calls are logged twice).
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
