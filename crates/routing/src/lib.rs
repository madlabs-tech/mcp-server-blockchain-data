#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )
)]
//! Routing: which vendor answers, in what order, and what happens when one fails.
//!
//! - [`ProviderRegistry`] holds every registered port per `(capability, chain)`.
//! - [`Router`] resolves the user's order from config, filters vendors that are inactive
//!   (disabled, missing key, unsupported chain, breaker open, quota reserve reached) and runs a
//!   strategy: failover, hedged, quorum, aggregate or fan-out.
//! - Every attempt goes through the resilience decorator: rate limiter → timeout → bounded retry
//!   on `Transient` → circuit breaker + health + quota bookkeeping. The `public` pseudo-vendor
//!   gets a shorter attempt timeout ([`PUBLIC_ATTEMPT_TIMEOUT`]) than keyed vendors.
//! - The routing table (config + registry) is hot-swappable via `ArcSwap`; runtime state
//!   (breakers, counters, limiters) survives swaps. In-flight requests finish on their snapshot.

mod registry;
mod router;
mod rpc;
mod state;
mod store;

pub use registry::ProviderRegistry;
pub use router::{
    Candidate, Candidates, QuorumOutcome, RouteError, RouteReq, Routed, Router, RouterOptions,
    RoutingTable, PUBLIC_ATTEMPT_TIMEOUT,
};
pub use rpc::{RoutedEvmRpc, RoutedSolanaRpc};
pub use state::{BreakerState, TokenBucket, UsageSnapshot, VendorHealth};
pub use store::{CounterStore, Dims, InMemoryCounterStore, WindowKey};
