//! Multicall3 `aggregate3` batching pinned to one block. Owner: `evm` (T1.E5).
//!
//! Caveat: inside a multicall `msg.sender` is the Multicall3 contract, so never batch reads that
//! depend on the caller.

use super::eth_call;
use alloy_primitives::{address, Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall};
use bdm_ports::{EvmRpc, PortResult};

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

/// Return data of sub-call `i`; `None` when it reverted or the node returned fewer results.
pub(crate) fn data_at(r: &[Option<Vec<u8>>], i: usize) -> Option<&[u8]> {
    r.get(i)?.as_deref()
}

/// First 32-byte word of sub-call `i`'s return data (`None` when shorter than a word).
pub(crate) fn word_at(r: &[Option<Vec<u8>>], i: usize) -> Option<U256> {
    first_word(data_at(r, i)?)
}

/// First 32-byte word of ABI return data (`None` when shorter than a word).
pub(crate) fn first_word(d: &[u8]) -> Option<U256> {
    d.get(..32).map(U256::from_be_slice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evm::{chainlink, fees};
    use bdm_ports::ProviderError;
    use bdm_testkit::mocks::MockEvmRpc;
    use serde_json::json;

    /// `MockEvmRpc` whose one `eth_call` answers with `results` (regardless of how many calls
    /// were batched).
    fn node(results: Vec<IMulticall3::Result>) -> MockEvmRpc {
        let rpc = MockEvmRpc::default();
        let out = IMulticall3::aggregate3Call::abi_encode_returns(&results);
        rpc.script
            .always(Ok(json!(format!("0x{}", hex::encode(out)))));
        rpc
    }

    fn ok(data: &[u8]) -> IMulticall3::Result {
        IMulticall3::Result {
            success: true,
            returnData: Bytes::copy_from_slice(data),
        }
    }

    #[tokio::test]
    async fn fewer_results_than_calls_is_transient() {
        let rpc = node(vec![ok(&[0u8; 32])]);
        let calls = [Call::eth_balance(Address::ZERO), Call::eth_balance(ADDRESS)];
        assert!(matches!(
            aggregate3(&rpc, &calls, "latest").await,
            Err(ProviderError::Transient(_))
        ));
        assert!(matches!(
            chainlink::latest_round(&rpc, ADDRESS).await,
            Err(ProviderError::Transient(_))
        ));
    }

    #[tokio::test]
    async fn short_return_data_never_panics() {
        let rpc = node(vec![ok(&[1, 2, 3, 4]), ok(&[1, 2, 3, 4])]);
        assert!(matches!(
            chainlink::latest_round(&rpc, ADDRESS).await,
            Err(ProviderError::Unsupported(_))
        ));
        assert!(matches!(
            fees::op_l1_fee(&rpc).await,
            Err(ProviderError::Transient(_))
        ));
        assert_eq!(word_at(&[Some(vec![1, 2, 3, 4]), None], 0), None);
        assert_eq!(word_at(&[Some(vec![1, 2, 3, 4]), None], 1), None);
        assert_eq!(word_at(&[Some(vec![1, 2, 3, 4]), None], 2), None);
        assert_eq!(word_at(&[Some(vec![0u8; 32])], 0), Some(U256::ZERO));
    }
}
