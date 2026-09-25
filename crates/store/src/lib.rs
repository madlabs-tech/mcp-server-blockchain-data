#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )
)]
//! Platform crate for the hosted/dashboard side of the server.
//!
//! - [`Store`]: sqlite (WAL) on a dedicated thread. It holds usage counters (it implements
//!   `bdm_routing::CounterStore`, so quota counters survive restarts), the call-log ring,
//!   client keys (SHA-256 hashes only) and per-client usage counters. Hot-path reads (`total`,
//!   client lookup, client counters) are served from memory; writes are queued to the thread.
//! - [`QuotaEngine`]: aggregates limit / cap / effective budget and usage from every source
//!   (local metering, rate-limit headers, `QuotaReporter` polling), burn rate, run-out
//!   projection and alert thresholds.
//! - [`ClientKeyAuth`] / [`ClientGuard`]: hosted-mode `ClientAuth` and `CallGuard`.

mod db;
mod hosted;
mod quota;

pub use db::{
    hash_key, random_hex, BreakdownRow, CallRecord, ClientCounters, ClientRecord, Exceeded, Store,
    StoreError,
};
pub use hosted::{effective_client_limits, ClientGuard, ClientKeyAuth};
pub use quota::{
    project, Breakdown, Projection, QuotaEngine, QuotaState, SourceReading, UsageSource,
    VendorQuota, WindowQuota,
};
