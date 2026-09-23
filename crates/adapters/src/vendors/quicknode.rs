//! `quicknode` vendor adapter. Owner: `evm` (T1.E4).
//!
//! The base `evm_rpc`/`broadcast` ports (incl. `eth_simulateV1`, used by the `rpc` simulator)
//! come from `factory::base_registrations` via the `rpc_urls` in `registry/vendors.toml`
//! (`QN_ENDPOINT_NAME` + `QN_TOKEN_ID`). QuickNode's enhanced APIs (Token API, etc.) are
//! marketplace add-ons without a confirmed free tier, so nothing else is registered here.
//! No `QuotaReporter`: the Admin API usage endpoint is documented for paid plans only.

use ems_config::Loaded;
use ems_ports::Registration;

/// Nothing to add beyond the base RPC registration (see module docs).
pub fn register(_loaded: &Loaded, _out: &mut Vec<Registration>) {}
