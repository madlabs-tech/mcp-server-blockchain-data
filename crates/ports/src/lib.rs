//! Capability ports: small traits (Interface Segregation) that vendor adapters implement.
//!
//! A vendor implements only what it supports and registers each port through a
//! [`Registration`] returned by its factory. Routing (`bdm-routing`) picks among registered
//! vendors per `(capability, chain)` using the user's configured order.
//!
//! ## Capability → P0 tool map
//! | Capability (config key) | Port | Used by (Release 1 tools) |
//! |---|---|---|
//! | `evm_rpc` | [`EvmRpc`] | everything EVM (base transport; typed helpers live in `bdm-protocols`) |
//! | `solana_rpc` | [`SolanaRpc`] | everything Solana |
//! | `token_balances` | [`TokenBalances`] | `wallet_get_balances`, `neobank_card_funding_status` |
//! | `transfer_history` | [`TransferHistory`] | `wallet_get_transfers`, `payments_list_deposits`, `neobank_get_ledger` |
//! | `fee_estimate` | [`FeeOracle`] | `tx_estimate_fee`, `tx_build_transfer` |
//! | `simulate` | [`Simulator`] | `tx_simulate`, `tx_build_transfer`, `trade_build_swap_tx` |
//! | `broadcast` / `private_relay` | [`Broadcaster`] | `tx_broadcast` |
//! | `price` | [`PriceFeed`] | `market_get_price`, `stablecoin_peg`, fiat valuation |
//! | `price_history` | [`PriceHistory`] | `market_get_price_at`, `neobank_get_ledger` |
//! | `token_metadata` | [`TokenMetadata`] | `token_get_metadata`, `stablecoin_resolve` |
//! | `token_risk` | [`TokenRisk`] | `token_check_risk` |
//! | `swap_quote` | [`SwapQuoter`] | `trade_get_swap_quote`, `trade_build_swap_tx` |
//! | `sanctions` | [`SanctionsScreener`] | `compliance_screen_address` |
//! | `fx` | [`FxRates`] | `fiat_get_fx_rate`, `neobank_get_ledger` |
//!
//! Tools not listed (`chain_*`, `address_validate`, `payments_verify_transfer`,
//! `stablecoin_check_restrictions`, `rwa_*`, …) compose the chain RPC ports with
//! contract readers from `bdm-protocols`.
//!
//! The pseudo-vendor id [`RPC_VENDOR`] (`"rpc"`) denotes generic implementations built on the
//! *routed* chain RPC (e.g. Multicall3 balances, `eth_getLogs` transfer scans), so they can sit
//! in a routing order like any vendor: `token_balances = ["alchemy", "moralis", "rpc"]`.

mod error;
pub mod metering;
mod ports;
mod quota;
mod registration;

pub use error::ProviderError;
pub use ports::*;
pub use quota::{QuotaReporter, UsageUnit, UsageWindow, VendorUsage, WindowKind};
pub use registration::{Capability, PortHandle, PortKind, Registration, RpcFeatures, VendorMeta};

/// Result type for every port method.
pub type PortResult<T> = Result<T, ProviderError>;

/// Pseudo-vendor id for generic implementations over the routed chain RPC.
pub const RPC_VENDOR: &str = "rpc";
