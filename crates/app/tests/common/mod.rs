//! Setup shared by the app integration tests (each test binary uses a subset).
#![allow(dead_code)]

use bdm_app::{App, Caller, Catalog};
use bdm_config::{ConfigDir, ConfigLoader, EnvSource};
use bdm_domain::DomainError;
use bdm_ports::{Registration, VendorMeta};
use bdm_routing::{InMemoryCounterStore, ProviderRegistry, Router, RouterOptions, RoutingTable};
use serde_json::Value;
use std::sync::Arc;

/// Router over `regs`; `env` and `cfg` (config.toml text) are the only config inputs.
pub fn router(env: &[(&str, &str)], cfg: &str, regs: Vec<Registration>) -> Arc<Router> {
    let loader = ConfigLoader::new(
        ConfigDir::new("/nonexistent"),
        EnvSource::from_pairs(env.iter().copied()),
    )
    .unwrap();
    Router::new(
        RoutingTable {
            config: Arc::new(loader.load_texts(cfg, "").unwrap()),
            registry: ProviderRegistry::new(regs),
        },
        Arc::new(InMemoryCounterStore::default()),
        RouterOptions::default(),
    )
}

/// App with every built-in tool.
pub fn app(env: &[(&str, &str)], cfg: &str, regs: Vec<Registration>) -> App {
    let mut catalog = Catalog::new();
    bdm_app::ops::register_all(&mut catalog);
    App::new(catalog, router(env, cfg, regs), 1000)
}

pub fn meta(id: &str) -> VendorMeta {
    VendorMeta {
        id: id.into(),
        display_name: id.into(),
        ..Default::default()
    }
}

pub async fn call(app: &App, tool: &str, input: Value) -> Result<Value, DomainError> {
    app.call(tool, input, Caller::local()).await
}
