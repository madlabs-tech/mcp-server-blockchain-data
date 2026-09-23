use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Stable, machine-readable error codes. Agents branch on these; never rename a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    /// Input failed validation (bad address, amount, chain id...).
    InvalidInput,
    /// The chain is unknown or disabled.
    UnsupportedChain,
    /// No enabled vendor offers this capability for this chain (often: add an API key).
    UnsupportedCapability,
    /// The entity does not exist (confirmed by more than one provider where it matters).
    NotFound,
    /// Every provider in the routing order failed.
    AllProvidersFailed,
    /// Upstream or client rate limit hit; see `retry_after_secs`.
    RateLimited,
    /// A vendor budget or client quota is used up until the reset time.
    QuotaExceeded,
    /// Missing or invalid client/admin credentials.
    Unauthorized,
    /// The answer cannot be determined from on-chain data (e.g. confidential transfers).
    Unverifiable,
    /// Providers disagree (quorum failure) or the data changed underneath us (reorg).
    Conflict,
    /// Data exists but is older than the freshness policy allows.
    StaleData,
    /// Bug or unexpected state.
    Internal,
}

impl ErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InvalidInput => "INVALID_INPUT",
            Self::UnsupportedChain => "UNSUPPORTED_CHAIN",
            Self::UnsupportedCapability => "UNSUPPORTED_CAPABILITY",
            Self::NotFound => "NOT_FOUND",
            Self::AllProvidersFailed => "ALL_PROVIDERS_FAILED",
            Self::RateLimited => "RATE_LIMITED",
            Self::QuotaExceeded => "QUOTA_EXCEEDED",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::Unverifiable => "UNVERIFIABLE",
            Self::Conflict => "CONFLICT",
            Self::StaleData => "STALE_DATA",
            Self::Internal => "INTERNAL",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned to callers (MCP and REST). Never contains secrets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, thiserror::Error)]
#[error("{code}: {message}")]
pub struct DomainError {
    pub code: ErrorCode,
    pub message: String,
    /// Actionable next step for the agent or operator, e.g. "set HELIUS_API_KEY".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
}

impl DomainError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
            retry_after_secs: None,
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidInput, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn with_retry_after(mut self, secs: u64) -> Self {
        self.retry_after_secs = Some(secs);
        self
    }
}
