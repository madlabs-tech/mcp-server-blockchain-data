//! ERC-8056 UI multiplier (Robinhood stock tokens: splits/dividends). Owner: `evm` (T1.E5). Used by rwa, wallet.
//!
//! Source for the interface: https://docs.robinhood.com/chain/building-with-stock-tokens/
//! (`uiMultiplier()`, `newUIMultiplier()`, `effectiveAt()`; 1e18 = 1.0).

use super::multicall3::{aggregate3, Call};
use alloy_primitives::{Address, U256};
use alloy_sol_types::sol;
use ems_ports::{EvmRpc, PortResult, ProviderError};

sol! {
    interface IERC8056 {
        function uiMultiplier() external view returns (uint256);
        function newUIMultiplier() external view returns (uint256);
        function effectiveAt() external view returns (uint256);
    }
}

/// Multipliers are 1e18-scaled (1e18 = 1.0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiMultiplier {
    pub current: U256,
    /// `(newUIMultiplier, effectiveAt unix seconds)` when a change is scheduled.
    pub pending: Option<(U256, u64)>,
}

/// Reads all three values in one Multicall3 call, so they come from the same block.
/// `Unsupported` if the token does not implement `uiMultiplier()`.
pub async fn ui_multiplier(rpc: &dyn EvmRpc, token: Address) -> PortResult<UiMultiplier> {
    let calls = [
        Call::new(token, IERC8056::uiMultiplierCall {}),
        Call::new(token, IERC8056::newUIMultiplierCall {}),
        Call::new(token, IERC8056::effectiveAtCall {}),
    ];
    let r = aggregate3(rpc, &calls, "latest").await?;
    let word = |i: usize| -> Option<U256> {
        let d = r.get(i)?.as_ref()?;
        (d.len() >= 32).then(|| U256::from_be_slice(&d[..32]))
    };
    let current = word(0).ok_or_else(|| {
        ProviderError::Unsupported(format!("{token} does not implement ERC-8056"))
    })?;
    // A change is pending when a different multiplier is scheduled. Once `effectiveAt` passes,
    // `uiMultiplier()` already returns the new value, so `new == current` and nothing is pending.
    let pending = match (word(1), word(2)) {
        (Some(new), Some(at)) if !at.is_zero() && new != current && !new.is_zero() => {
            Some((new, u64::try_from(at).unwrap_or(u64::MAX)))
        }
        _ => None,
    };
    Ok(UiMultiplier { current, pending })
}
