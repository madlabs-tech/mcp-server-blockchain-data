//! Normalized EVM transactions. Owner: `evm` (T1.E1). Used by `tx_get`, `payments_verify_transfer`.
//!
//! `get_tx` must: decode ERC-20 `Transfer` logs (drop 4-topic ERC-721 logs, ignore `removed`),
//! fill `balance_deltas` for recipients (sum of canonical-token transfers to them), fee paid, block
//! ref with hash, and `finality` per the chain policy.

use crate::not_yet;
use ems_config::ChainEntry;
use ems_domain::{Finality, Tx};
use ems_ports::{EvmRpc, PortResult};

pub async fn get_tx(_rpc: &dyn EvmRpc, _chain: &ChainEntry, _hash: &str) -> PortResult<Option<Tx>> {
    Err(not_yet("T1.E1 evm::tx::get_tx"))
}

/// Finality of `block` using `safe`/`finalized` tags (confirmations for `Confirmed`).
pub async fn finality_of(
    _rpc: &dyn EvmRpc,
    _chain: &ChainEntry,
    _block: u64,
) -> PortResult<Finality> {
    Err(not_yet("T1.E1 evm::tx::finality_of"))
}
