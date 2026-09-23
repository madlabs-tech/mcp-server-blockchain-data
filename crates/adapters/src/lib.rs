//! Vendor adapters (Adapter + Anti-Corruption Layer): vendor DTOs never leave this crate.
//!
//! - [`http`]: shared vendor HTTP client (timeouts, metering, rate-limit headers, scrubbing).
//! - [`jsonrpc`]: JSON-RPC 2.0 client and vendor error mapping.
//! - [`chain_rpc`]: [`EvmRpcClient`] / [`SolanaRpcClient`] transports (also broadcasters).
//! - [`factory`]: [`base_registrations`] for chain RPC vendors from config.
//!
//! - [`vendors`]: one module per vendor with enhanced/REST APIs, each behind a cargo feature.

pub mod chain_rpc;
pub mod factory;
pub mod http;
pub mod jsonrpc;
pub mod vendors;

pub use chain_rpc::{EvmRpcClient, SolanaRpcClient};
pub use factory::base_registrations;
pub use http::{parse_rate_limit, HttpClient, DEFAULT_TIMEOUT};
pub use jsonrpc::{map_rpc_error, JsonRpcClient};
