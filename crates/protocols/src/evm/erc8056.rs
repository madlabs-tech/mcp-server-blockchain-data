//! ERC-8056 UI multiplier (Robinhood stock tokens: splits/dividends). Owner: `evm` (T1.E5). Used by rwa, wallet.

use crate::not_yet;
use alloy_primitives::{Address, U256};
use ems_ports::{EvmRpc, PortResult};

/// Multipliers are 1e18-scaled (1e18 = 1.0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiMultiplier {
    pub current: U256,
    /// `(newUIMultiplier, effectiveAt unix seconds)` when a change is scheduled.
    pub pending: Option<(U256, u64)>,
}

pub async fn ui_multiplier(_rpc: &dyn EvmRpc, _token: Address) -> PortResult<UiMultiplier> {
    Err(not_yet("T1.E5 erc8056::ui_multiplier"))
}
