//! Use cases. Each tool is one [`Operation`] (Command pattern): typed input/output with JSON
//! schemas, metadata (domain, profiles, read-only), and `execute`. The [`Catalog`] type-erases
//! operations so MCP, REST and OpenAPI are all generated from the same definitions, and
//! [`App`] runs them through the decorator chain: visibility/guard → cache → metering scope →
//! tracing/metrics → execute → envelope.
//!
//! ## Adding a tool (Phase 1 teammates)
//! 1. Implement [`Operation`] in your domain module under `ops/` (e.g. `ops/payments.rs`).
//! 2. Register it in that module's `register(catalog)` function; `ops::register_all` already
//!    calls every domain module, so you never edit shared files.
//! 3. Route vendor calls through `ctx.router()` with `ctx.route(capability)` so the user's order,
//!    breakers and quota guard apply, and return the provenance in [`OpOutput`].

mod app;
mod catalog;
mod ctx;
mod metrics;
mod op;
pub mod ops;

pub use app::{App, CallGuard, ClientAuth};
pub use catalog::{Catalog, ProfileSelection};
pub use ctx::{Caller, Ctx};
pub use metrics::{OpMetrics, OpStats};
pub use op::{Domain, DynOperation, OpOutput, Operation, Profile};
