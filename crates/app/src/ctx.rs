use crate::catalog::ProfileSelection;
use bdm_config::{ChainEntry, Loaded};
use bdm_domain::{ChainFamily, DomainError};
use bdm_ports::Capability;
use bdm_routing::{RouteReq, RoutedEvmRpc, RoutedSolanaRpc, Router, RoutingTable};
use std::sync::Arc;

/// Who is calling. `client` is the hosted-mode client key id (`None` for local/self-hosted).
#[derive(Debug, Clone, Default)]
pub struct Caller {
    pub client: Option<String>,
    /// Hosted mode: the client's allowed tools (from its limits); `None` = server profile.
    pub profile: Option<ProfileSelection>,
}

impl Caller {
    pub fn local() -> Self {
        Self::default()
    }
}

/// Per-request context handed to operations. Holds one routing-table snapshot for the whole
/// request, so a hot reload mid-request never mixes two configs.
pub struct Ctx {
    router: Arc<Router>,
    table: Arc<RoutingTable>,
    pub op: &'static str,
}

impl Ctx {
    pub fn new(router: Arc<Router>, op: &'static str) -> Self {
        let table = router.table();
        Self { router, table, op }
    }

    pub fn router(&self) -> &Arc<Router> {
        &self.router
    }

    pub fn config(&self) -> &Loaded {
        &self.table.config
    }

    pub fn table(&self) -> &Arc<RoutingTable> {
        &self.table
    }

    /// Resolve a chain id or alias ("base", "eip155:8453") to an enabled chain.
    pub fn chain(&self, s: &str) -> Result<&ChainEntry, DomainError> {
        self.config().registry.chains.resolve(s)
    }

    /// Route request for a capability, tagged with this operation (per-op orders apply).
    pub fn route(&self, cap: Capability) -> RouteReq {
        RouteReq::new(cap).op(self.op)
    }

    /// EVM JSON-RPC that fails over across the configured `evm_rpc` order for `chain`.
    pub fn evm_rpc(&self, chain: &ChainEntry) -> Result<RoutedEvmRpc, DomainError> {
        if chain.family != ChainFamily::Evm {
            return Err(DomainError::invalid(format!(
                "{} is not an EVM chain",
                chain.id
            )));
        }
        RoutedEvmRpc::new(self.router.clone(), chain.id.clone())
            .ok_or_else(|| DomainError::internal(format!("{} has no EIP-155 chain id", chain.id)))
    }

    /// Solana JSON-RPC that fails over across the configured `solana_rpc` order.
    pub fn solana_rpc(&self, chain: &ChainEntry) -> Result<RoutedSolanaRpc, DomainError> {
        if chain.family != ChainFamily::Solana {
            return Err(DomainError::invalid(format!(
                "{} is not a Solana chain",
                chain.id
            )));
        }
        Ok(RoutedSolanaRpc::new(self.router.clone(), chain.id.clone()))
    }
}
