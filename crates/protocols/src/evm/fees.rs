//! EIP-1559 tiers + L2 L1-data fee (OP GasPriceOracle, Arbitrum NodeInterface). Owner: `evm` (T1.E2).

use super::{eth_call, hex_u256, malformed, multicall3};
use alloy_primitives::{address, Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall};
use chrono::Utc;
use ems_config::ChainEntry;
use ems_domain::{Amount, FeeEstimate, FeeSpeed, FeeTier};
use ems_ports::{EvmRpc, PortResult};
use serde_json::{json, Value};

/// OP-stack predeploy. Source: https://docs.optimism.io/stack/smart-contracts (predeploys table),
/// https://specs.optimism.io/protocol/predeploys.html#gaspriceoracle
pub const GAS_PRICE_ORACLE: Address = address!("420000000000000000000000000000000000000F");
/// Arbitrum Nitro virtual contract (eth_call only, not callable from contracts, so no multicall).
/// Source: https://docs.arbitrum.io/build-decentralized-apps/nodeinterface/reference
pub const NODE_INTERFACE: Address = address!("00000000000000000000000000000000000000C8");

sol! {
    interface IGasPriceOracle {
        function getL1Fee(bytes memory _data) external view returns (uint256);
        function getOperatorFee(uint256 _gasUsed) external view returns (uint256);
    }
    interface INodeInterface {
        function gasEstimateL1Component(address to, bool contractCreation, bytes calldata data)
            external payable returns (uint64 gasEstimateForL1, uint256 baseFee, uint256 l1BaseFeeEstimate);
    }
}

const TRANSFER_GAS: u64 = 21_000;
const HISTORY_BLOCKS: u64 = 10;
/// Dummy recipient for fee estimates (not a contract; never sent to).
const DUMMY_TO: Address = Address::repeat_byte(0x11);
/// Representative unsigned EIP-1559 ETH transfer (`0x02 || rlp([chainId=8453, nonce=1,
/// maxPriority=1e6, maxFee=1e7, gas=21000, to=0x11…11, value=0.01 ETH, data=, accessList=[]])`),
/// the `_data` the OP GasPriceOracle expects. Only its size/compressibility affects the fee.
const REPRESENTATIVE_TX: &str = "02ee82210501830f424083989680825208941111111111111111111111111111111111111111872386f26fc1000080c0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stack {
    Op,
    Arbitrum,
    L1,
}

/// Rollup stack by chain id (`chains.toml` has no stack field yet).
/// OP-stack: Optimism 10, Base 8453 (https://docs.base.org). Arbitrum Nitro: Arbitrum One 42161,
/// Robinhood Chain 4663 (Arbitrum Orbit, https://docs.robinhood.com/chain/connecting).
fn stack(chain_id: Option<u64>) -> Stack {
    match chain_id {
        Some(10 | 8453) => Stack::Op,
        Some(42161 | 4663) => Stack::Arbitrum,
        _ => Stack::L1,
    }
}

/// Slow/standard/fast from the 25th/50th/75th percentile priority fees of the last 10 blocks
/// (`eth_feeHistory`, median per percentile). `max_fee = 2 × next base fee + priority`.
/// `estimated_total` is a 21k-gas native transfer at `base + priority`, plus the L1 data fee
/// on L2s (`l1_data_fee`: OP `getL1Fee` + operator fee, or Arbitrum's L1 component).
pub async fn fee_estimate(rpc: &dyn EvmRpc, chain: &ChainEntry) -> PortResult<FeeEstimate> {
    let h = rpc
        .request(
            "eth_feeHistory",
            json!([format!("0x{HISTORY_BLOCKS:x}"), "latest", [25, 50, 75]]),
        )
        .await?;
    let base = h["baseFeePerGas"]
        .as_array()
        .and_then(|a| a.last())
        .ok_or_else(|| malformed("feeHistory.baseFeePerGas"))
        .and_then(hex_u256)?;
    let l1 = match stack(chain.id.evm_chain_id()) {
        Stack::Op => Some(op_l1_fee(rpc).await?),
        Stack::Arbitrum => Some(arbitrum_l1_fee(rpc).await?),
        Stack::L1 => None,
    };
    let d = chain.native.decimals;
    let tiers = [FeeSpeed::Slow, FeeSpeed::Standard, FeeSpeed::Fast]
        .into_iter()
        .enumerate()
        .map(|(i, speed)| {
            let prio = median_reward(&h["reward"], i)?;
            let per_gas = base.saturating_add(prio);
            let total = per_gas
                .saturating_mul(U256::from(TRANSFER_GAS))
                .saturating_add(l1.unwrap_or_default());
            Ok(FeeTier {
                speed,
                max_fee_per_gas: Some(base.saturating_mul(U256::from(2)).saturating_add(prio)),
                max_priority_fee_per_gas: Some(prio),
                compute_unit_price_micro_lamports: None,
                estimated_total: Some(Amount::new(total, d)),
                estimated_total_fiat: None,
            })
        })
        .collect::<PortResult<Vec<_>>>()?;
    Ok(FeeEstimate {
        chain: chain.id.clone(),
        tiers,
        l1_data_fee: l1.map(|v| Amount::new(v, d)),
        tip: None,
        as_of: Utc::now(),
    })
}

/// Median of column `i` of `feeHistory.reward` (zero when the node returns no rewards).
fn median_reward(reward: &Value, i: usize) -> PortResult<U256> {
    let mut col = reward
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row.get(i))
        .map(hex_u256)
        .collect::<PortResult<Vec<_>>>()?;
    col.sort();
    Ok(col.get(col.len() / 2).copied().unwrap_or_default())
}

/// `getL1Fee(tx) + getOperatorFee(21000)` in one Multicall3 call. The operator fee (Isthmus)
/// counts as 0 on chains whose oracle doesn't have it yet.
async fn op_l1_fee(rpc: &dyn EvmRpc) -> PortResult<U256> {
    let tx = Bytes::from(hex::decode(REPRESENTATIVE_TX).expect("valid hex constant"));
    let calls = [
        multicall3::Call::new(
            GAS_PRICE_ORACLE,
            IGasPriceOracle::getL1FeeCall { _data: tx },
        ),
        multicall3::Call::new(
            GAS_PRICE_ORACLE,
            IGasPriceOracle::getOperatorFeeCall {
                _gasUsed: U256::from(TRANSFER_GAS),
            },
        ),
    ];
    let r = multicall3::aggregate3(rpc, &calls, "latest").await?;
    let word = |d: &Option<Vec<u8>>| {
        d.as_deref()
            .filter(|d| d.len() >= 32)
            .map(|d| U256::from_be_slice(&d[..32]))
    };
    let l1 = word(&r[0]).ok_or_else(|| malformed("GasPriceOracle.getL1Fee"))?;
    Ok(l1.saturating_add(word(&r[1]).unwrap_or_default()))
}

/// `gasEstimateForL1 × baseFee`: the L1 component is expressed in L2 gas.
async fn arbitrum_l1_fee(rpc: &dyn EvmRpc) -> PortResult<U256> {
    let call = INodeInterface::gasEstimateL1ComponentCall {
        to: DUMMY_TO,
        contractCreation: false,
        data: Bytes::new(),
    };
    let out = eth_call(rpc, NODE_INTERFACE, call.abi_encode(), "latest").await?;
    let r = INodeInterface::gasEstimateL1ComponentCall::abi_decode_returns(&out)
        .map_err(|_| malformed("NodeInterface.gasEstimateL1Component"))?;
    Ok(U256::from(r.gasEstimateForL1).saturating_mul(r.baseFee))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn representative_tx_is_well_formed_rlp() {
        let b = hex::decode(REPRESENTATIVE_TX).unwrap();
        assert_eq!(b[0], 0x02, "EIP-1559 type byte");
        assert_eq!(b[1] as usize, 0xc0 + (b.len() - 2), "short list header");
    }

    #[test]
    fn stacks() {
        assert_eq!(stack(Some(8453)), Stack::Op);
        assert_eq!(stack(Some(4663)), Stack::Arbitrum);
        assert_eq!(stack(Some(1)), Stack::L1);
    }
}
