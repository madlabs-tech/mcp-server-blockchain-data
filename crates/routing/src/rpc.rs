//! Routed chain RPC: an `EvmRpc` / `SolanaRpc` that fails over across every configured RPC
//! vendor. `ems-protocols` readers and the `rpc` pseudo-vendor build on these, so on-chain
//! reads get the user's order, breakers and quota guard for free.

use crate::router::{RouteReq, Router};
use async_trait::async_trait;
use ems_domain::ChainId;
use ems_ports::{Capability, EvmRpc, PortResult, ProviderError, SolanaRpc};
use serde_json::Value;
use std::sync::Arc;

pub struct RoutedEvmRpc {
    router: Arc<Router>,
    chain: ChainId,
    chain_id: u64,
}

impl RoutedEvmRpc {
    pub fn new(router: Arc<Router>, chain: ChainId) -> Option<Self> {
        let chain_id = chain.evm_chain_id()?;
        Some(Self {
            router,
            chain,
            chain_id,
        })
    }
}

#[async_trait]
impl EvmRpc for RoutedEvmRpc {
    fn chain_id(&self) -> u64 {
        self.chain_id
    }

    async fn request(&self, method: &str, params: Value) -> PortResult<Value> {
        let req = RouteReq::new(Capability::EvmRpc).chain(self.chain.clone());
        self.router
            .failover::<dyn EvmRpc, _, _, _>(req, |p| {
                let params = params.clone();
                async move { p.request(method, params).await }
            })
            .await
            .map(|r| r.value)
            .map_err(route_to_provider)
    }
}

pub struct RoutedSolanaRpc {
    router: Arc<Router>,
    chain: ChainId,
}

impl RoutedSolanaRpc {
    pub fn new(router: Arc<Router>, chain: ChainId) -> Self {
        Self { router, chain }
    }
}

#[async_trait]
impl SolanaRpc for RoutedSolanaRpc {
    async fn request(&self, method: &str, params: Value) -> PortResult<Value> {
        let req = RouteReq::new(Capability::SolanaRpc).chain(self.chain.clone());
        self.router
            .failover::<dyn SolanaRpc, _, _, _>(req, |p| {
                let params = params.clone();
                async move { p.request(method, params).await }
            })
            .await
            .map(|r| r.value)
            .map_err(route_to_provider)
    }
}

/// Collapse a routed failure back into the port error taxonomy (callers of the routed RPC are
/// themselves ports, e.g. the `rpc` pseudo-vendor, so the outer router sees the right kind).
fn route_to_provider(e: crate::router::RouteError) -> ProviderError {
    use ems_domain::ErrorCode::*;
    match e.error.code {
        InvalidInput => ProviderError::Invalid(e.error.message),
        NotFound => ProviderError::NotFound,
        RateLimited => ProviderError::RateLimited {
            retry_after: e.error.retry_after_secs.map(std::time::Duration::from_secs),
        },
        QuotaExceeded => ProviderError::QuotaExhausted { resets_at: None },
        UnsupportedCapability => ProviderError::Unsupported(e.error.message),
        _ => ProviderError::Transient(e.error.message),
    }
}
