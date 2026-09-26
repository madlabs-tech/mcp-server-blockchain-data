use crate::{serde_str, Amount, AssetId, ChainId, UnsignedTx};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One source's price. A missing price is represented by absence / [`PriceStatus::Unknown`], never 0.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Price {
    pub asset: AssetId,
    /// Quote currency (ISO 4217), usually "USD".
    pub currency: String,
    #[serde(with = "crate::serde_str")]
    #[schemars(with = "String")]
    pub value: Decimal,
    pub as_of: DateTime<Utc>,
    /// Vendor or oracle id, e.g. "coingecko", "chainlink", "jupiter".
    pub source: String,
    /// Pool liquidity backing the price, when the source reports it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "serde_str::opt"
    )]
    #[schemars(with = "Option<String>")]
    pub liquidity_usd: Option<Decimal>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PriceStatus {
    Ok,
    /// Sources disagree beyond the configured spread threshold (possible manipulation).
    Divergent,
    /// Newest source is older than the freshness policy (e.g. equity feed outside market hours).
    Stale,
    /// No source could price the asset.
    Unknown,
}

/// Aggregated price across sources: median + spread, with every contributing source listed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PriceAggregate {
    pub asset: AssetId,
    pub currency: String,
    pub status: PriceStatus,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "serde_str::opt"
    )]
    #[schemars(with = "Option<String>")]
    pub median: Option<Decimal>,
    /// (max - min) / median in basis points.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spread_bps: Option<u32>,
    pub sources: Vec<Price>,
}

/// Swap quote from one aggregator. `min_buy` is what the built transaction enforces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SwapQuote {
    pub chain: ChainId,
    pub sell_asset: AssetId,
    pub sell_amount: Amount,
    pub buy_asset: AssetId,
    pub buy_amount: Amount,
    pub min_buy_amount: Amount,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_impact_bps: Option<i32>,
    /// Aggregator id, e.g. "oneinch", "velora", "cow", "jupiter".
    pub source: String,
    pub expires_at: DateTime<Utc>,
    /// Unsigned tx when the quote was built for execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx: Option<UnsignedTx>,
    /// Approvals the signer must grant first (ERC-20 approve / Permit2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_approvals: Vec<UnsignedTx>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Unknown,
    Low,
    Medium,
    High,
    Critical,
}

/// One finding, always attributed to its source (we merge verdicts, never hide who said what).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RiskFlag {
    /// Stable code, e.g. "honeypot", "mint_authority_active", "permanent_delegate", "sanctioned".
    pub code: String,
    pub severity: Severity,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RiskReport {
    /// CAIP-10 account or CAIP-19 asset being assessed.
    pub subject: String,
    pub level: RiskLevel,
    pub flags: Vec<RiskFlag>,
    /// Sources consulted (including ones that found nothing).
    pub sources: Vec<String>,
    pub as_of: DateTime<Utc>,
}

impl RiskReport {
    /// Merged level = highest flag severity; `Unknown` if no source answered.
    pub fn merge_level(flags: &[RiskFlag], answered_sources: usize) -> RiskLevel {
        if answered_sources == 0 {
            return RiskLevel::Unknown;
        }
        match flags.iter().map(|f| f.severity).max() {
            None | Some(Severity::Info | Severity::Low) => RiskLevel::Low,
            Some(Severity::Medium) => RiskLevel::Medium,
            Some(Severity::High) => RiskLevel::High,
            Some(Severity::Critical) => RiskLevel::Critical,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merged_risk_level() {
        let f = |severity| RiskFlag {
            code: "x".into(),
            severity,
            source: "goplus".into(),
            detail: None,
        };
        assert_eq!(RiskReport::merge_level(&[], 0), RiskLevel::Unknown);
        assert_eq!(RiskReport::merge_level(&[], 2), RiskLevel::Low);
        assert_eq!(
            RiskReport::merge_level(&[f(Severity::Medium), f(Severity::Critical)], 2),
            RiskLevel::Critical
        );
    }
}
