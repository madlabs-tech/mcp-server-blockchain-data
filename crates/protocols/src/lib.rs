//! Protocol readers over the chain RPC ports, plus the `rpc` pseudo-vendor.
//!
//! Everything here takes `&dyn EvmRpc` / `&dyn SolanaRpc`, so passing a *routed* RPC
//! (`bdm_routing::RoutedEvmRpc`) gives every reader the user's vendor order, breakers and quota
//! guard for free. Functions shared across Phase 1 teams have their signatures fixed here;
//! stubs return `ProviderError::Unsupported("not implemented yet (<task>)")` until filled in.
//!
//! | Module | Owner | Task |
//! |---|---|---|
//! | `evm::{erc20, multicall3, erc8056, chainlink, fees, logs, tx, rpc_vendor}` | evm | T1.E1–E5 |
//! | `solana::{spl, tx, fees, rpc_vendor}` | solana | T1.S1–S2 |
//! | `stablecoins`, `issuer`, `sanctions` | payments-stablecoin | T1.P1–P2 |
//! | `rwa`, `market_hours` | market-trading | T1.M4 |

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
