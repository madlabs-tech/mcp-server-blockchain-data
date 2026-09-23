//! EIP-1559 tiers + L2 L1-data fee (OP GasPriceOracle, Arbitrum NodeInterface). Owner: `evm` (T1.E2).

use crate::not_yet;
use ems_config::ChainEntry;
use ems_domain::FeeEstimate;
use ems_ports::{EvmRpc, PortResult};

pub async fn fee_estimate(_rpc: &dyn EvmRpc, _chain: &ChainEntry) -> PortResult<FeeEstimate> {
    Err(not_yet("T1.E2 evm::fees::fee_estimate"))
}
