//! Protocol readers over the chain RPC ports, plus the `rpc` pseudo-vendor.
//!
//! Everything here takes `&dyn EvmRpc` / `&dyn SolanaRpc`, so passing a *routed* RPC
//! (`bdm_routing::RoutedEvmRpc`) gives every reader the user's vendor order, breakers and quota
//! guard for free.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )
)]

pub mod evm;
pub mod issuer;
pub mod market_hours;
pub mod rwa;
pub mod sanctions;
pub mod solana;
pub mod stablecoins;

use bdm_config::Loaded;
use bdm_ports::Registration;
use bdm_routing::Router;
use std::sync::Arc;

/// Registrations built on the routed chain RPC (`rpc` pseudo-vendor, on-chain oracles). Called by
/// the composition root after the router exists.
pub fn rpc_registrations(loaded: &Loaded, router: &Arc<Router>) -> Vec<Registration> {
    let mut out = Vec::new();
    out.extend(evm::rpc_vendor::registrations(loaded, router));
    out.extend(solana::rpc_vendor::registrations(loaded, router));
    out.extend(sanctions::registrations(loaded, router));
    out
}
