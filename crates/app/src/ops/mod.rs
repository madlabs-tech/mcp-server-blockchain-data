//! Tool implementations, one module per domain; each module owns its `register` function.
//!
//! | Module | Tools |
//! |---|---|
//! | `chain` | `chain_list`, `chain_finality`, `provider_health` |
//! | `wallet` | `wallet_get_balances`, `wallet_get_transfers`, `address_validate` |
//! | `tx` | `tx_get`, `tx_status`, `tx_estimate_fee`, `tx_simulate`, `tx_build_transfer`, `tx_broadcast` |
//! | `payments` | `payments_verify_transfer`, `payments_list_deposits`, `payments_build_request` |
//! | `stablecoin` | `stablecoin_resolve`, `stablecoin_check_restrictions`, `stablecoin_peg` |
//! | `compliance` | `compliance_screen_address` |
//! | `neobank` | `neobank_card_funding_status`, `neobank_get_ledger`, `fiat_get_fx_rate` |
//! | `market` | `market_get_price`, `market_get_price_at`, `token_get_metadata`, `token_check_risk` |
//! | `trade` | `trade_get_swap_quote`, `trade_build_swap_tx` |
//! | `rwa` | `rwa_token_info`, `rwa_price` |
//! | `legacy` | `eth_get_balance`, `eth_get_code`, `eth_gas_price`, `eth_get_transaction_by_hash` |

use crate::Catalog;
use bdm_domain::DomainError;

pub mod chain;
pub mod compliance;
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
    legacy::register(c);
}

/// `#[serde(default = "crate::ops::yes")]` for flags that default to on.
pub(crate) fn yes() -> bool {
    true
}

/// ISO 4217 code, upper-cased.
pub(crate) fn currency_code(c: &str) -> Result<String, DomainError> {
    let c = c.trim().to_ascii_uppercase();
    if c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic()) {
        Ok(c)
    } else {
        Err(DomainError::invalid(format!(
            "'{c}' is not an ISO 4217 currency code"
        )))
    }
}
