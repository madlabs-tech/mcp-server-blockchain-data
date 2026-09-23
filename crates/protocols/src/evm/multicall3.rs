//! Multicall3 `aggregate3` batching pinned to one block. Owner: `evm` (T1.E5).
//!
//! Caveat: inside a multicall `msg.sender` is the Multicall3 contract, so never batch reads that
//! depend on the caller.

use super::eth_call;
use alloy_primitives::{address, Address, Bytes};
use alloy_sol_types::{sol, SolCall};
use ems_ports::{EvmRpc, PortResult};

/// Same address on every chain we support (deterministic deployment).
/// Source: https://github.com/mds1/multicall3 (README "Deployments"), also `registry/chains.toml`.
pub const ADDRESS: Address = address!("cA11bde05977b3631167028862bE2a173976CA11");

sol! {
    interface IMulticall3 {
        struct Call3 { address target; bool allowFailure; bytes callData; }
        struct Result { bool success; bytes returnData; }
        function aggregate3(Call3[] calldata calls) external payable returns (Result[] memory returnData);
        function getEthBalance(address addr) external view returns (uint256 balance);
    }
}

/// One sub-call. Every call is sent with `allowFailure = true`: one bad token never fails the batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub target: Address,
    pub data: Vec<u8>,
}

impl Call {
    pub fn new(target: Address, call: impl SolCall) -> Self {
        Self {
            target,
            data: call.abi_encode(),
        }
    }

    /// Native balance of `owner`, read by Multicall3 itself (same block as the other calls).
    pub fn eth_balance(owner: Address) -> Self {
        Self::new(ADDRESS, IMulticall3::getEthBalanceCall { addr: owner })
    }
}

/// Run `calls` in one `eth_call` at `block` (use a concrete `0x…` number to pin several batches
/// to the same state). Returns the return data per call, `None` where the sub-call reverted.
pub async fn aggregate3(
    rpc: &dyn EvmRpc,
    calls: &[Call],
    block: &str,
) -> PortResult<Vec<Option<Vec<u8>>>> {
    if calls.is_empty() {
        return Ok(Vec::new());
    }
    let req = IMulticall3::aggregate3Call {
        calls: calls
            .iter()
            .map(|c| IMulticall3::Call3 {
                target: c.target,
                allowFailure: true,
                callData: Bytes::from(c.data.clone()),
            })
            .collect(),
    };
    let out = eth_call(rpc, ADDRESS, req.abi_encode(), block).await?;
    let results = IMulticall3::aggregate3Call::abi_decode_returns(&out)
        .map_err(|_| super::malformed("aggregate3 return data"))?;
    if results.len() != calls.len() {
        return Err(super::malformed("aggregate3 result count"));
    }
    Ok(results
        .into_iter()
        .map(|r| r.success.then(|| r.returnData.to_vec()))
        .collect())
}
