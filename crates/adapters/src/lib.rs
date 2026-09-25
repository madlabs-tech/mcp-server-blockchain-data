//! Vendor adapters (Adapter + Anti-Corruption Layer): vendor DTOs never leave this crate.
//!
//! - [`http`]: shared vendor HTTP client (timeouts, metering, rate-limit headers, scrubbing).
//! - [`jsonrpc`]: JSON-RPC 2.0 client and vendor error mapping.
//! - [`chain_rpc`]: [`EvmRpcClient`] / [`chain_rpc::SolanaRpcClient`] transports (also broadcasters).
//! - [`factory`]: [`factory::base_registrations`] for chain RPC vendors from config.
//!
//! - [`vendors`]: one module per vendor with enhanced/REST APIs, each behind a cargo feature.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )
)]

pub mod chain_rpc;
pub mod factory;
pub mod http;
pub mod jsonrpc;
pub mod vendors;

pub use chain_rpc::EvmRpcClient;
pub use http::HttpClient;
