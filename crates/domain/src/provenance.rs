use crate::{BlockRef, ChainId, ErrorCode, Finality};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// First provider in the routing order answered.
    Primary,
    /// A later provider answered after earlier ones failed or were skipped.
    Fallback,
    /// Combined from several providers (median, quorum, merged verdict).
    Aggregate,
    /// Served from the response cache.
    Cache,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Ok,
    /// Not tried: disabled, missing key, unsupported chain, breaker open, quota reserve reached.
    Skipped,
    Failed,
}

/// One entry of the routing trail (`meta.providersTried`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Attempt {
    pub vendor: String,
    pub outcome: AttemptOutcome,
    /// Why skipped/failed, e.g. "breaker_open", "quota_reserve", "missing_key", "timeout".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<ErrorCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
}

/// Where an answer came from. Rendered as `meta` in every response envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain: Option<ChainId>,
    /// Vendor that produced the answer (or "aggregate").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers_tried: Vec<Attempt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<BlockRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finality: Option<Finality>,
    pub source: SourceKind,
    pub cached: bool,
    pub latency_ms: u64,
    pub as_of: DateTime<Utc>,
}

impl Provenance {
    pub fn new(source: SourceKind) -> Self {
        Self {
            chain: None,
            provider: None,
            providers_tried: Vec::new(),
            block: None,
            finality: None,
            source,
            cached: false,
            latency_ms: 0,
            as_of: Utc::now(),
        }
    }
}
