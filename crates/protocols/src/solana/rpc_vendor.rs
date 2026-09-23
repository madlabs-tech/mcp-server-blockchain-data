//! `rpc` pseudo-vendor for Solana: balances (both token programs), transfers (wallet + every token
//! account), fee_estimate, simulate, token_metadata. Owner: `solana` (T1.S1–S2).

use ems_config::Loaded;
use ems_ports::Registration;
use ems_routing::Router;
use std::sync::Arc;

pub fn registrations(_loaded: &Loaded, _router: &Arc<Router>) -> Vec<Registration> {
    Vec::new()
}
