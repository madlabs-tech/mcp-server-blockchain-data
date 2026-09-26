//! `eth_getLogs` transfer scanning with adaptive chunking.
//! Cap `toBlock` at the head of the same node; split ranges on `Invalid` (range/size errors).

use super::{block_number, block_tag, decode_transfer_log, to_transfers, token_decimals};
use alloy_primitives::{Address, B256};
use alloy_sol_types::SolEvent;
use bdm_config::ChainEntry;
use bdm_domain::{AccountAddress, AssetRef, Transfer};
use bdm_ports::{Direction, EvmRpc, Page, PortResult, ProviderError, TransferQuery};
use serde_json::{json, Map, Value};

/// Chunk size when the vendor plan has no known `getLogs` range limit.
const DEFAULT_CHUNK: u64 = 2_000;
/// `eth_getLogs` calls per page; the cursor continues the scan. Keeps one page's credit cost bounded.
const MAX_CALLS: usize = 20;
const CURSOR_PREFIX: &str = "evm-logs:";

/// ERC-20 transfers to/from `query.owner`, newest first, scanning backwards from `to_block`
/// (default: head) towards `from_block` (default: genesis) in chunks of `max_range` blocks.
///
/// - `toBlock` is capped at the head reported by the RPC, so a lagging load-balanced node
///   can't silently return "no logs" for blocks it hasn't seen.
///   ponytail: with a routed RPC the head and the logs can come from different vendors on failover.
/// - A range/size error (`Invalid`) halves the chunk and retries; the smaller size is kept for
///   the rest of the page.
/// - The page ends after `limit` transfers or `MAX_CALLS` calls, always on a chunk boundary, so a
///   page may hold more than `limit` items (never loses any across the cursor).
/// - Native transfers emit no logs; a query restricted to native assets returns an empty page.
pub async fn scan_transfers(
    rpc: &dyn EvmRpc,
    chain: &ChainEntry,
    query: &TransferQuery,
    max_range: Option<u64>,
) -> PortResult<Page<Transfer>> {
    let AccountAddress::Evm(owner) = query.owner else {
        return Err(ProviderError::Invalid("owner is not an EVM address".into()));
    };
    let tokens: Vec<Address> = query
        .assets
        .iter()
        .flatten()
        .filter_map(|a| match a.asset {
            AssetRef::Erc20(t) => Some(t),
            _ => None,
        })
        .collect();
    if query.assets.is_some() && tokens.is_empty() {
        return Ok(Page {
            items: Vec::new(),
            next_cursor: None,
        });
    }

    let head = block_number(rpc).await?;
    let mut hi = query.to_block.unwrap_or(head).min(head);
    if let Some(c) = &query.cursor {
        hi = hi.min(parse_cursor(c)?);
    }
    let from = query.from_block.unwrap_or(0);
    let limit = query.limit.max(1) as usize;
    let topic_sets = topics(owner, query.direction);

    let mut chunk = max_range.unwrap_or(DEFAULT_CHUNK).max(1);
    let mut calls = 0;
    let mut raw = Vec::new();
    let mut next = (hi >= from).then_some(hi);
    while let Some(top) = next {
        if calls + topic_sets.len() > MAX_CALLS || raw.len() >= limit {
            break;
        }
        let lo = top.saturating_sub(chunk - 1).max(from);
        match get_logs(rpc, lo, top, &tokens, &topic_sets, &mut calls).await {
            Ok(logs) => {
                raw.extend(logs.iter().filter_map(decode_transfer_log));
                next = (lo > from).then(|| lo - 1);
            }
            Err(ProviderError::Invalid(_)) if top > lo => chunk = (top - lo).div_ceil(2).max(1),
            Err(e) => return Err(e),
        }
    }

    // A self-transfer matches both the "in" and "out" filters.
    raw.sort_by(|a, b| {
        let key = |t: &super::RawTransfer| (t.block.as_ref().map(|b| b.number), t.log_index);
        key(b).cmp(&key(a))
    });
    raw.dedup_by(|a, b| a.tx_hash == b.tx_hash && a.log_index == b.log_index);

    let decimals = token_decimals(rpc, raw.iter().map(|t| t.token), "latest").await?;
    Ok(Page {
        items: to_transfers(&chain.id, raw, &decimals),
        next_cursor: next.map(|n| format!("{CURSOR_PREFIX}{n}")),
    })
}

fn topics(owner: Address, direction: Direction) -> Vec<Value> {
    let sig = format!("{:#x}", super::erc20::IERC20::Transfer::SIGNATURE_HASH);
    let who = format!("{:#x}", B256::from(owner.into_word()));
    let incoming = json!([sig, null, who]);
    let outgoing = json!([sig, who]);
    match direction {
        Direction::In => vec![incoming],
        Direction::Out => vec![outgoing],
        Direction::Both => vec![incoming, outgoing],
    }
}

async fn get_logs(
    rpc: &dyn EvmRpc,
    lo: u64,
    hi: u64,
    tokens: &[Address],
    topic_sets: &[Value],
    calls: &mut usize,
) -> PortResult<Vec<Value>> {
    let mut out = Vec::new();
    for topics in topic_sets {
        let mut filter = Map::new();
        filter.insert("fromBlock".into(), json!(block_tag(lo)));
        filter.insert("toBlock".into(), json!(block_tag(hi)));
        filter.insert("topics".into(), topics.clone());
        if !tokens.is_empty() {
            filter.insert("address".into(), json!(tokens));
        }
        *calls += 1;
        let logs = rpc.request("eth_getLogs", json!([filter])).await?;
        out.extend(logs.as_array().cloned().unwrap_or_default());
    }
    Ok(out)
}

fn parse_cursor(c: &str) -> PortResult<u64> {
    c.strip_prefix(CURSOR_PREFIX)
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| ProviderError::Invalid(format!("cursor '{c}' is not from this source")))
}
