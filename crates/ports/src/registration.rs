use crate::{
    Broadcaster, EvmRpc, FeeOracle, FxRates, PriceFeed, PriceHistory, QuotaReporter,
    SanctionsScreener, Simulator, SolanaRpc, SwapQuoter, TokenBalances, TokenMetadata, TokenRisk,
    TransferHistory,
};
use bdm_domain::ChainId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Capability ids. The snake_case name is the config key in `[routing.*]`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Capability {
    EvmRpc,
    SolanaRpc,
    TokenBalances,
    TransferHistory,
    FeeEstimate,
    Simulate,
    Broadcast,
    PrivateRelay,
    Price,
    PriceHistory,
    TokenMetadata,
    TokenRisk,
    SwapQuote,
    Sanctions,
    Fx,
}

impl Capability {
    pub const ALL: &'static [Capability] = &[
        Self::EvmRpc,
        Self::SolanaRpc,
        Self::TokenBalances,
        Self::TransferHistory,
        Self::FeeEstimate,
        Self::Simulate,
        Self::Broadcast,
        Self::PrivateRelay,
        Self::Price,
        Self::PriceHistory,
        Self::TokenMetadata,
        Self::TokenRisk,
        Self::SwapQuote,
        Self::Sanctions,
        Self::Fx,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::EvmRpc => "evm_rpc",
            Self::SolanaRpc => "solana_rpc",
            Self::TokenBalances => "token_balances",
            Self::TransferHistory => "transfer_history",
            Self::FeeEstimate => "fee_estimate",
            Self::Simulate => "simulate",
            Self::Broadcast => "broadcast",
            Self::PrivateRelay => "private_relay",
            Self::Price => "price",
            Self::PriceHistory => "price_history",
            Self::TokenMetadata => "token_metadata",
            Self::TokenRisk => "token_risk",
            Self::SwapQuote => "swap_quote",
            Self::Sanctions => "sanctions",
            Self::Fx => "fx",
        }
    }

    /// True for ports registered per chain (their instances are bound to one chain).
    pub fn is_chain_bound(&self) -> bool {
        matches!(
            self,
            Self::EvmRpc
                | Self::SolanaRpc
                | Self::TokenBalances
                | Self::TransferHistory
                | Self::FeeEstimate
                | Self::Simulate
                | Self::Broadcast
                | Self::PrivateRelay
        )
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Capability {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| format!("unknown capability '{s}'"))
    }
}

/// Optional JSON-RPC features a chain RPC vendor supports; routing filters by these.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RpcFeatures {
    /// Max `eth_getLogs` block range on the configured plan (`None` = unknown/unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub get_logs_max_range: Option<u64>,
    #[serde(default)]
    pub simulate_v1: bool,
    #[serde(default)]
    pub debug_trace: bool,
}

/// Static facts about a vendor, shown in the dashboard and used for filtering.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VendorMeta {
    /// Stable id, the name used in config orders (e.g. "alchemy", "public", "rpc").
    pub id: String,
    pub display_name: String,
    pub requires_key: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signup_url: Option<String>,
    #[serde(default)]
    pub rpc_features: RpcFeatures,
}

/// A registered port instance (enum-dispatched so the registry can hold every kind).
#[derive(Clone)]
pub enum PortHandle {
    EvmRpc(Arc<dyn EvmRpc>),
    SolanaRpc(Arc<dyn SolanaRpc>),
    TokenBalances(Arc<dyn TokenBalances>),
    TransferHistory(Arc<dyn TransferHistory>),
    FeeEstimate(Arc<dyn FeeOracle>),
    Simulate(Arc<dyn Simulator>),
    Broadcast(Arc<dyn Broadcaster>),
    PrivateRelay(Arc<dyn Broadcaster>),
    Price(Arc<dyn PriceFeed>),
    PriceHistory(Arc<dyn PriceHistory>),
    TokenMetadata(Arc<dyn TokenMetadata>),
    TokenRisk(Arc<dyn TokenRisk>),
    SwapQuote(Arc<dyn SwapQuoter>),
    Sanctions(Arc<dyn SanctionsScreener>),
    Fx(Arc<dyn FxRates>),
}

impl PortHandle {
    pub fn capability(&self) -> Capability {
        match self {
            Self::EvmRpc(_) => Capability::EvmRpc,
            Self::SolanaRpc(_) => Capability::SolanaRpc,
            Self::TokenBalances(_) => Capability::TokenBalances,
            Self::TransferHistory(_) => Capability::TransferHistory,
            Self::FeeEstimate(_) => Capability::FeeEstimate,
            Self::Simulate(_) => Capability::Simulate,
            Self::Broadcast(_) => Capability::Broadcast,
            Self::PrivateRelay(_) => Capability::PrivateRelay,
            Self::Price(_) => Capability::Price,
            Self::PriceHistory(_) => Capability::PriceHistory,
            Self::TokenMetadata(_) => Capability::TokenMetadata,
            Self::TokenRisk(_) => Capability::TokenRisk,
            Self::SwapQuote(_) => Capability::SwapQuote,
            Self::Sanctions(_) => Capability::Sanctions,
            Self::Fx(_) => Capability::Fx,
        }
    }
}

/// Typed access to a [`PortHandle`]: `P::extract(&handle)` for `P = dyn PriceFeed`, etc.
pub trait PortKind: Send + Sync + 'static {
    fn extract(handle: &PortHandle) -> Option<Arc<Self>>;
}

macro_rules! port_kind {
    ($tr:path => $($variant:ident),+) => {
        impl PortKind for dyn $tr {
            fn extract(handle: &PortHandle) -> Option<Arc<Self>> {
                match handle {
                    $(PortHandle::$variant(p) => Some(p.clone()),)+
                    #[allow(unreachable_patterns)]
                    _ => None,
                }
            }
        }
    };
}

port_kind!(EvmRpc => EvmRpc);
port_kind!(SolanaRpc => SolanaRpc);
port_kind!(TokenBalances => TokenBalances);
port_kind!(TransferHistory => TransferHistory);
port_kind!(FeeOracle => FeeEstimate);
port_kind!(Simulator => Simulate);
port_kind!(Broadcaster => Broadcast, PrivateRelay);
port_kind!(PriceFeed => Price);
port_kind!(PriceHistory => PriceHistory);
port_kind!(TokenMetadata => TokenMetadata);
port_kind!(TokenRisk => TokenRisk);
port_kind!(SwapQuoter => SwapQuote);
port_kind!(SanctionsScreener => Sanctions);
port_kind!(FxRates => Fx);

/// What a vendor factory returns: its metadata and every port it implements.
///
/// `chain = None` registers a chain-agnostic port (prices, FX, …); chain-bound capabilities
/// must be registered with `Some(chain)`.
pub struct Registration {
    pub vendor: VendorMeta,
    pub ports: Vec<(Option<ChainId>, PortHandle)>,
    pub quota_reporter: Option<Arc<dyn QuotaReporter>>,
}

impl Registration {
    pub fn new(vendor: VendorMeta) -> Self {
        Self {
            vendor,
            ports: Vec::new(),
            quota_reporter: None,
        }
    }

    pub fn chain_port(mut self, chain: ChainId, port: PortHandle) -> Self {
        debug_assert!(
            port.capability().is_chain_bound(),
            "{} is not chain-bound",
            port.capability()
        );
        self.ports.push((Some(chain), port));
        self
    }

    pub fn global_port(mut self, port: PortHandle) -> Self {
        debug_assert!(
            !port.capability().is_chain_bound(),
            "{} is chain-bound",
            port.capability()
        );
        self.ports.push((None, port));
        self
    }

    pub fn with_quota_reporter(mut self, r: Arc<dyn QuotaReporter>) -> Self {
        self.quota_reporter = Some(r);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_ids_round_trip() {
        for c in Capability::ALL {
            assert_eq!(c.as_str().parse::<Capability>().unwrap(), *c);
            assert_eq!(
                serde_json::to_value(c).unwrap(),
                serde_json::json!(c.as_str())
            );
        }
        assert!("nope".parse::<Capability>().is_err());
    }
}
