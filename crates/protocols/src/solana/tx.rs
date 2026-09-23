//! Normalized Solana transactions. Owner: `solana` (T1.S1). Used by `tx_get`, `payments_verify_transfer`.
//!
//! Amounts come from `pre/postTokenBalances` deltas per owner (not instruction amounts): handles
//! inner instructions, multiple transfers, Token-2022 transfer fees (net + `withheld_fee`);
//! confidential transfers → `Finality::Unverifiable`.

use crate::not_yet;
use ems_config::ChainEntry;
use ems_domain::{DomainError, Tx};
use ems_ports::{PortResult, SolanaRpc};
use serde_json::Value;

pub async fn get_tx(
    _rpc: &dyn SolanaRpc,
    _chain: &ChainEntry,
    _signature: &str,
) -> PortResult<Option<Tx>> {
    Err(not_yet("T1.S1 solana::tx::get_tx"))
}

/// Pure parser over a `getTransaction` (jsonParsed, maxSupportedTransactionVersion 0) result.
pub fn parse_tx(_chain: &ChainEntry, _tx: &Value) -> Result<Tx, DomainError> {
    Err(DomainError::internal(
        "not implemented yet (T1.S1 solana::tx::parse_tx)",
    ))
}
