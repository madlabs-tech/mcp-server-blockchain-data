//! `rpc` pseudo-vendor for EVM chains: `token_balances` (Multicall3), `transfer_history` (logs),
//! `fee_estimate`, `simulate` (`eth_simulateV1` → `debug_traceCall` → `eth_call`), `token_metadata`.
//! Owner: `evm` (T1.E1–E3). Build each port on `ems_routing::RoutedEvmRpc::new(router, chain)`.

use ems_config::Loaded;
use ems_ports::Registration;
use ems_routing::Router;
use std::sync::Arc;

pub fn registrations(_loaded: &Loaded, _router: &Arc<Router>) -> Vec<Registration> {
    Vec::new()
}
