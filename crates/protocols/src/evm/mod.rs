//! EVM readers. Owner: `evm` (Phase 1). Block tags are strings: "latest" | "safe" | "finalized" | "0x…".

pub mod chainlink;
pub mod erc20;
pub mod erc8056;
pub mod fees;
pub mod logs;
pub mod multicall3;
pub mod rpc_vendor;
pub mod tx;

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use bdm_domain::TransferKind;
use bdm_domain::{AccountAddress, Amount, AssetId, AssetRef, BlockRef, ChainId, Transfer};
use bdm_ports::{EvmRpc, PortResult, ProviderError};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// Hex quantity tag for a block number (`0x1b4`).
pub fn block_tag(n: u64) -> String {
    format!("0x{n:x}")
}

/// `eth_blockNumber` of whichever node answers.
pub async fn block_number(rpc: &dyn EvmRpc) -> PortResult<u64> {
    hex_u64(&rpc.request("eth_blockNumber", json!([])).await?)
}

/// `eth_call` returning the raw return data. A revert surfaces as `ProviderError::Invalid`.
pub async fn eth_call(
    rpc: &dyn EvmRpc,
    to: Address,
    data: Vec<u8>,
    block: &str,
) -> PortResult<Vec<u8>> {
    let v = rpc
        .request(
            "eth_call",
            json!([{"to": to, "data": format!("0x{}", hex::encode(data))}, block]),
        )
        .await?;
    hex_bytes(&v)
}

pub(crate) fn malformed(what: &str) -> ProviderError {
    // Transient: a garbled answer is the node's fault, so routing may try another vendor.
    ProviderError::Transient(format!("malformed RPC response: {what}"))
}

pub(crate) fn hex_u64(v: &Value) -> PortResult<u64> {
    v.as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .ok_or_else(|| malformed("expected hex quantity"))
}

pub(crate) fn hex_u256(v: &Value) -> PortResult<U256> {
    v.as_str()
        .and_then(|s| U256::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .ok_or_else(|| malformed("expected hex quantity"))
}

pub(crate) fn hex_bytes(v: &Value) -> PortResult<Vec<u8>> {
    v.as_str()
        .and_then(|s| hex::decode(s.trim_start_matches("0x")).ok())
        .ok_or_else(|| malformed("expected hex data"))
}

/// One decoded ERC-20 `Transfer` log (amount still without decimals).
#[derive(Debug, Clone)]
pub(crate) struct RawTransfer {
    pub token: Address,
    pub from: Address,
    pub to: Address,
    pub value: U256,
    pub tx_hash: String,
    pub log_index: Option<u64>,
    pub block: Option<BlockRef>,
}

/// Decode an ERC-20 `Transfer` log. Returns `None` for anything else, including:
/// - `removed: true` logs (reorged out),
/// - ERC-721 `Transfer` (same topic0, but 4 topics: the token id is indexed),
/// - malformed data.
///
/// The token is the log's emitting contract: a fake token can emit a `Transfer` that looks like
/// USDC, but never with USDC's address, so callers must match assets by contract, never symbol.
pub(crate) fn decode_transfer_log(log: &Value) -> Option<RawTransfer> {
    if log["removed"].as_bool() == Some(true) {
        return None;
    }
    let topics = log["topics"].as_array()?;
    if topics.len() != 3 {
        return None;
    }
    let topic = |i: usize| -> Option<B256> { topics[i].as_str()?.parse().ok() };
    if topic(0)? != erc20::IERC20::Transfer::SIGNATURE_HASH {
        return None;
    }
    let data = hex_bytes(&log["data"]).ok()?;
    if data.len() != 32 {
        return None;
    }
    let block = log["blockNumber"].as_str().and_then(|_| {
        Some(BlockRef {
            number: hex_u64(&log["blockNumber"]).ok()?,
            hash: log["blockHash"].as_str().map(str::to_owned),
            timestamp: None,
        })
    });
    Some(RawTransfer {
        token: log["address"].as_str()?.parse().ok()?,
        from: Address::from_word(topic(1)?),
        to: Address::from_word(topic(2)?),
        value: U256::from_be_slice(&data),
        tx_hash: log["transactionHash"].as_str()?.to_owned(),
        log_index: hex_u64(&log["logIndex"]).ok(),
        block,
    })
}

/// `decimals()` of each distinct token, batched through Multicall3. Tokens that don't answer
/// are left out (callers drop their transfers: without decimals no exact amount exists).
pub(crate) async fn token_decimals(
    rpc: &dyn EvmRpc,
    tokens: impl IntoIterator<Item = Address>,
    block: &str,
) -> PortResult<HashMap<Address, u8>> {
    let unique: Vec<Address> = tokens
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if unique.is_empty() {
        return Ok(HashMap::new());
    }
    let calls: Vec<_> = unique
        .iter()
        .map(|t| multicall3::Call::new(*t, erc20::IERC20::decimalsCall {}))
        .collect();
    let results = multicall3::aggregate3(rpc, &calls, block).await?;
    Ok(unique
        .into_iter()
        .zip(results)
        .filter_map(|(t, r)| Some((t, erc20::decode_decimals(&r?)?)))
        .collect())
}

/// Turn decoded logs into domain transfers (drops tokens without `decimals()`).
pub(crate) fn to_transfers(
    chain: &ChainId,
    raw: Vec<RawTransfer>,
    decimals: &HashMap<Address, u8>,
) -> Vec<Transfer> {
    raw.into_iter()
        .filter_map(|r| {
            let d = *decimals.get(&r.token)?;
            Some(Transfer {
                chain: chain.clone(),
                tx_hash: r.tx_hash,
                log_index: r.log_index,
                kind: TransferKind::Token,
                asset: AssetId {
                    chain: chain.clone(),
                    asset: AssetRef::Erc20(r.token),
                },
                from: Some(AccountAddress::Evm(r.from)),
                to: AccountAddress::Evm(r.to),
                amount: Amount::new(r.value, d),
                block: r.block,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(a: &str) -> String {
        format!("0x{:0>64}", a.trim_start_matches("0x"))
    }

    #[test]
    fn decodes_only_live_erc20_transfer_logs() {
        let topic0 = format!("{:#x}", erc20::IERC20::Transfer::SIGNATURE_HASH);
        let from = word("1111111111111111111111111111111111111111");
        let to = word("2222222222222222222222222222222222222222");
        let base = json!({
            "address": "0x3333333333333333333333333333333333333333",
            "topics": [topic0, from, to],
            "data": word("64"),
            "transactionHash": "0xabc",
            "logIndex": "0x5",
            "blockNumber": "0x10",
            "blockHash": "0xdef",
            "removed": false,
        });
        let t = decode_transfer_log(&base).unwrap();
        assert_eq!(t.value, U256::from(100u8));
        assert_eq!(t.log_index, Some(5));
        assert_eq!(t.block.unwrap().number, 16);
        assert_eq!(
            t.to,
            "0x2222222222222222222222222222222222222222"
                .parse::<Address>()
                .unwrap()
        );

        let mut removed = base.clone();
        removed["removed"] = json!(true);
        assert!(decode_transfer_log(&removed).is_none());

        let mut nft = base.clone();
        nft["topics"].as_array_mut().unwrap().push(json!(word("7")));
        nft["data"] = json!("0x");
        assert!(decode_transfer_log(&nft).is_none());

        let mut other = base;
        other["topics"][0] = json!(word("dead"));
        assert!(decode_transfer_log(&other).is_none());
    }
}
