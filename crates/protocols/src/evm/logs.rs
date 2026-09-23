//! `eth_getLogs` transfer scanning with adaptive chunking. Owner: `evm` (T1.E1).
//! Cap `toBlock` at the head of the same node; split ranges on `Invalid` (range/size errors).

use crate::not_yet;
use ems_config::ChainEntry;
use ems_domain::Transfer;
use ems_ports::{EvmRpc, Page, PortResult, TransferQuery};

pub async fn scan_transfers(
    _rpc: &dyn EvmRpc,
    _chain: &ChainEntry,
    _query: &TransferQuery,
    _max_range: Option<u64>,
) -> PortResult<Page<Transfer>> {
    Err(not_yet("T1.E1 evm::logs::scan_transfers"))
}
