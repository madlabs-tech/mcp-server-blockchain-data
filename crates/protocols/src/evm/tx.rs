//! Normalized EVM transactions. Owner: `evm` (T1.E1). Used by `tx_get`, `payments_verify_transfer`.
//!
//! `get_tx` decodes ERC-20 `Transfer` logs (drops 4-topic ERC-721 logs, ignores `removed`), fills
//! `balance_deltas` per recipient, fee paid, block ref with hash, and `finality` per chain policy.

use super::{
    block_number, block_tag, decode_transfer_log, hex_u256, hex_u64, to_transfers, token_decimals,
};
use bdm_config::ChainEntry;
use bdm_domain::{AccountAddress, Amount, BalanceDelta, BlockRef, Finality, Tx, TxStatus};
use bdm_ports::{EvmRpc, PortResult, ProviderError};
use serde_json::{json, Value};

/// `Ok(None)` when no node knows the hash. A mined tx whose receipt the node doesn't have yet
/// (load-balanced RPC lag) is reported as `Pending` rather than failing.
///
/// `balance_deltas`: one row per (recipient, token), summing every decoded `Transfer` to that
/// recipient in this tx. EVM receipts carry no balances, so the delta is log-derived:
/// `before = 0`, `after = total received` (`BalanceDelta::received()` is the amount). Assets are
/// keyed by contract; callers match canonical tokens by contract via the stablecoin registry.
pub async fn get_tx(rpc: &dyn EvmRpc, chain: &ChainEntry, hash: &str) -> PortResult<Option<Tx>> {
    let t = rpc
        .request("eth_getTransactionByHash", json!([hash]))
        .await?;
    if t.is_null() {
        return Ok(None);
    }
    let addr = |v: &Value| {
        v.as_str()
            .and_then(|s| s.parse().ok())
            .map(AccountAddress::Evm)
    };
    let mut tx = Tx {
        chain: chain.id.clone(),
        hash: t["hash"].as_str().unwrap_or(hash).to_owned(),
        status: TxStatus::Pending,
        finality: Finality::Pending,
        block: None,
        from: addr(&t["from"]),
        to: addr(&t["to"]),
        fee: None,
        transfers: Vec::new(),
        balance_deltas: Vec::new(),
        raw: None,
    };
    if t["blockNumber"].is_null() {
        return Ok(Some(tx));
    }
    let r = rpc
        .request("eth_getTransactionReceipt", json!([hash]))
        .await?;
    if r.is_null() {
        return Ok(Some(tx));
    }

    let number = hex_u64(&r["blockNumber"])?;
    tx.status = if r["status"].as_str() == Some("0x1") {
        TxStatus::Success
    } else {
        TxStatus::Failed
    };
    tx.block = Some(BlockRef {
        number,
        hash: r["blockHash"].as_str().map(str::to_owned),
        timestamp: None,
    });
    tx.fee = Some(Amount::new(fee_paid(&r, &t)?, chain.native.decimals));

    let raw: Vec<_> = r["logs"]
        .as_array()
        .map(|logs| logs.iter().filter_map(decode_transfer_log).collect())
        .unwrap_or_default();
    let decimals = token_decimals(rpc, raw.iter().map(|t| t.token), &block_tag(number)).await?;
    tx.transfers = to_transfers(&chain.id, raw, &decimals);
    tx.balance_deltas = recipient_deltas(&tx.transfers)?;
    tx.finality = finality_of(rpc, chain, number).await?;
    Ok(Some(tx))
}

/// `gasUsed × effectiveGasPrice`, plus the OP-stack `l1Fee` receipt field when present.
/// (Arbitrum's `gasUsed` already includes the L1 component.)
fn fee_paid(receipt: &Value, tx: &Value) -> PortResult<alloy_primitives::U256> {
    let price = if receipt["effectiveGasPrice"].is_null() {
        hex_u256(&tx["gasPrice"])?
    } else {
        hex_u256(&receipt["effectiveGasPrice"])?
    };
    let mut fee = hex_u256(&receipt["gasUsed"])?.saturating_mul(price);
    if !receipt["l1Fee"].is_null() {
        fee = fee.saturating_add(hex_u256(&receipt["l1Fee"])?);
    }
    Ok(fee)
}

fn recipient_deltas(transfers: &[bdm_domain::Transfer]) -> PortResult<Vec<BalanceDelta>> {
    let mut out: Vec<BalanceDelta> = Vec::new();
    for t in transfers {
        match out
            .iter_mut()
            .find(|d| d.owner == t.to && d.asset == t.asset)
        {
            Some(d) => {
                d.after = d
                    .after
                    .checked_add(&t.amount)
                    .map_err(|e| ProviderError::Invalid(e.message))?
            }
            None => out.push(BalanceDelta {
                owner: t.to,
                asset: t.asset.clone(),
                before: Amount::zero(t.amount.decimals),
                after: t.amount,
                withheld_fee: None,
            }),
        }
    }
    Ok(out)
}

/// Finality of `block` using `safe`/`finalized` tags (confirmations for `Confirmed`).
///
/// On L2s `latest` is only the sequencer's word, so anything above the `safe` head is
/// `Confirmed(n)` no matter how many blocks sit on top.
pub async fn finality_of(rpc: &dyn EvmRpc, chain: &ChainEntry, block: u64) -> PortResult<Finality> {
    if chain.finality.policy == "tags" {
        for (tag, level) in [("finalized", Finality::Finalized), ("safe", Finality::Safe)] {
            if tagged_block(rpc, tag).await?.is_some_and(|n| block <= n) {
                return Ok(level);
            }
        }
    }
    let head = block_number(rpc).await?;
    Ok(Finality::Confirmed {
        confirmations: head.saturating_sub(block) + 1,
    })
}

/// Number of the block behind a tag, `None` if the node doesn't support the tag.
async fn tagged_block(rpc: &dyn EvmRpc, tag: &str) -> PortResult<Option<u64>> {
    match rpc
        .request("eth_getBlockByNumber", json!([tag, false]))
        .await
    {
        Ok(v) if v.is_null() => Ok(None),
        Ok(v) => hex_u64(&v["number"]).map(Some),
        Err(ProviderError::Invalid(_) | ProviderError::Unsupported(_)) => Ok(None),
        Err(e) => Err(e),
    }
}
