//! Chainlink aggregator reads (price + equity feeds). Owner: `evm` (T1.E5). Used by market, rwa, stablecoin_peg.

use crate::not_yet;
use alloy_primitives::{Address, I256};
use ems_ports::{EvmRpc, PortResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundData {
    pub round_id: u128,
    pub answer: I256,
    pub decimals: u8,
    /// Unix seconds.
    pub updated_at: u64,
}

pub async fn latest_round(_rpc: &dyn EvmRpc, _feed: Address) -> PortResult<RoundData> {
    Err(not_yet("T1.E5 chainlink::latest_round"))
}
