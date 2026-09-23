//! Chainlink aggregator reads (price + equity feeds). Owner: `evm` (T1.E5). Used by market, rwa, stablecoin_peg.
//!
//! Interface: `AggregatorV3Interface` (https://docs.chain.link/data-feeds/api-reference).

use super::multicall3::{aggregate3, Call};
use alloy_primitives::{Address, I256};
use alloy_sol_types::{sol, SolCall};
use bdm_ports::{EvmRpc, PortResult, ProviderError};

sol! {
    interface IAggregatorV3 {
        function decimals() external view returns (uint8);
        function latestRoundData() external view returns (
            uint80 roundId, int256 answer, uint256 startedAt, uint256 updatedAt, uint80 answeredInRound
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundData {
    pub round_id: u128,
    pub answer: I256,
    pub decimals: u8,
    /// Unix seconds.
    pub updated_at: u64,
}

/// `latestRoundData()` + `decimals()` in one Multicall3 call (same block). Staleness and
/// market-session checks are the caller's job (see `market_hours`).
pub async fn latest_round(rpc: &dyn EvmRpc, feed: Address) -> PortResult<RoundData> {
    let calls = [
        Call::new(feed, IAggregatorV3::latestRoundDataCall {}),
        Call::new(feed, IAggregatorV3::decimalsCall {}),
    ];
    let r = aggregate3(rpc, &calls, "latest").await?;
    let not_feed = || ProviderError::Unsupported(format!("{feed} is not a Chainlink feed"));
    let round = r[0]
        .as_deref()
        .and_then(|d| IAggregatorV3::latestRoundDataCall::abi_decode_returns(d).ok())
        .ok_or_else(not_feed)?;
    let decimals = r[1]
        .as_deref()
        .and_then(super::erc20::decode_decimals)
        .ok_or_else(not_feed)?;
    Ok(RoundData {
        round_id: round.roundId.to::<u128>(),
        answer: round.answer,
        decimals,
        updated_at: u64::try_from(round.updatedAt).unwrap_or(u64::MAX),
    })
}
