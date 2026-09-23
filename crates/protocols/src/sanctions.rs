//! Chainalysis on-chain sanctions oracle as the `chainalysis_oracle` vendor (`sanctions` port,
//! EVM only). Owner: `payments-stablecoin` (T1.P2).

use ems_config::Loaded;
use ems_ports::Registration;
use ems_routing::Router;
use std::sync::Arc;

pub fn registrations(_loaded: &Loaded, _router: &Arc<Router>) -> Vec<Registration> {
    Vec::new()
}
