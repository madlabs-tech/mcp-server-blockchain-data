//! Tool implementations, one module per domain. Each module owns its `register` function, so
//! Phase 1 teammates add tools without touching shared files.
//!
//! | Module | Owner (Phase 1) | Tools |
//! |---|---|---|
//! | `chain` | neobank-wallet | `chain_list`, `chain_finality`, `provider_health` |
//! | `wallet` | neobank-wallet | `wallet_get_balances`, `wallet_get_transfers`, `address_validate` |
//! | `tx` | neobank-wallet | `tx_get`, `tx_status`, `tx_estimate_fee`, `tx_simulate`, `tx_build_transfer`, `tx_broadcast` |
//! | `payments` | payments-stablecoin | `payments_verify_transfer`, `payments_list_deposits`, `payments_build_request` |
//! | `stablecoin` | payments-stablecoin | `stablecoin_resolve`, `stablecoin_check_restrictions`, `stablecoin_peg` |
//! | `compliance` | payments-stablecoin | `compliance_screen_address` |
//! | `neobank` | neobank-wallet | `neobank_card_funding_status`, `neobank_get_ledger`, `fiat_get_fx_rate` |
//! | `market` | market-trading | `market_get_price`, `market_get_price_at`, `token_get_metadata`, `token_check_risk` |
//! | `trade` | market-trading | `trade_get_swap_quote`, `trade_build_swap_tx` |
//! | `rwa` | market-trading | `rwa_token_info`, `rwa_price` |
//! | `defi` | (Release 2) | — |
//! | `legacy` | lead | `eth_get_balance`, `eth_get_code`, `eth_gas_price`, `eth_get_transaction_by_hash` |

use crate::Catalog;

pub mod chain;
pub mod compliance;
pub mod defi;
pub mod legacy;
pub mod market;
pub mod neobank;
pub mod payments;
pub mod rwa;
pub mod stablecoin;
pub mod trade;
pub mod tx;
pub mod wallet;

/// Register every tool of every domain.
pub fn register_all(c: &mut Catalog) {
    chain::register(c);
    wallet::register(c);
    tx::register(c);
    payments::register(c);
    stablecoin::register(c);
    compliance::register(c);
    neobank::register(c);
    market::register(c);
    trade::register(c);
    rwa::register(c);
    defi::register(c);
    legacy::register(c);
}
