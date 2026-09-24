#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )
)]
//! Layered configuration.
//!
//! Precedence (lowest → highest): built-in registry (`registry/*.toml`, embedded) →
//! `config.toml` (what the dashboard writes) → `secrets.toml` → environment variables.
//! Env vars use `BDM__<PATH>` with `__` between segments, plus the vendor key names from the
//! registry (`ALCHEMY_API_KEY`, `QN_ENDPOINT_NAME`, …) and the legacy `RPC_URL` (Ethereum only).
//! Anything set by env is *locked*: the dashboard shows it read-only and edits are refused.

mod edit;
mod loader;
mod redacted;
mod registry;
mod resolve;
mod settings;

pub use edit::{apply_edits, write_atomic, Edit, EditTarget};
pub use loader::{ConfigDir, ConfigLoader, EnvSource, Issue, Loaded, Severity};
pub use redacted::Redacted;
pub use registry::{
    ChainEntry, ChainRegistry, FinalityPolicy, NativeAsset, Registry, ResetRule, Unit, VendorEntry,
};
pub use resolve::{EffectiveBudget, OrderLevel, OrderResolution, VendorStatus};
pub use settings::{
    ClientLimits, ClientsSettings, CustomRpc, Mode, OnExhausted, OperationSettings, Order,
    RoutingSettings, ServerSettings, Settings, Strategy, VendorKeys, VendorSettings, WindowBudget,
};
