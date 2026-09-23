use crate::PortResult;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum WindowKind {
    Second,
    Minute,
    Day,
    Month,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UsageUnit {
    Requests,
    /// Vendor-specific credits / compute units.
    Credits,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UsageWindow {
    pub kind: WindowKind,
    pub unit: UsageUnit,
    pub used: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<DateTime<Utc>>,
}

/// Usage as reported by the vendor's own account/usage endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VendorUsage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    pub windows: Vec<UsageWindow>,
    pub fetched_at: DateTime<Utc>,
}

/// Optional: implemented by adapters whose vendor exposes a usage endpoint that does not
/// itself consume credits (CoinGecko `/key`, QuickNode console usage, …).
#[async_trait]
pub trait QuotaReporter: Send + Sync {
    async fn usage(&self) -> PortResult<VendorUsage>;
}
