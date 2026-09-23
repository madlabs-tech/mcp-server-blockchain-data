use chrono::{DateTime, Utc};
use ems_domain::{DomainError, ErrorCode};
use std::time::Duration;

/// Error from one provider call. The variant decides routing behavior:
///
/// | Variant | Retry same provider | Fail over to next provider |
/// |---|---|---|
/// | `Transient` | yes (bounded, with jitter) | yes |
/// | `RateLimited` | no | yes |
/// | `QuotaExhausted` | no (vendor marked exhausted until `resets_at`) | yes |
/// | `Unsupported` | no | yes (vendor can't serve this request) |
/// | `NotFound` | no | only when the operation asks for confirmation (payments) |
/// | `Invalid` / `Fatal` | no | no, returned to the caller |
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ProviderError {
    #[error("transient: {0}")]
    Transient(String),
    #[error("rate limited")]
    RateLimited { retry_after: Option<Duration> },
    #[error("quota exhausted")]
    QuotaExhausted { resets_at: Option<DateTime<Utc>> },
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("not found")]
    NotFound,
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("fatal: {0}")]
    Fatal(String),
}

impl ProviderError {
    /// Whether routing may try the next provider.
    pub fn allows_failover(&self) -> bool {
        matches!(
            self,
            Self::Transient(_)
                | Self::RateLimited { .. }
                | Self::QuotaExhausted { .. }
                | Self::Unsupported(_)
        )
    }

    /// Whether the same provider may be retried.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Transient(_))
    }

    /// Stable reason label for routing trails and metrics.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Transient(_) => "transient",
            Self::RateLimited { .. } => "rate_limited",
            Self::QuotaExhausted { .. } => "quota_exhausted",
            Self::Unsupported(_) => "unsupported",
            Self::NotFound => "not_found",
            Self::Invalid(_) => "invalid",
            Self::Fatal(_) => "fatal",
        }
    }

    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Transient(_) | Self::Fatal(_) => ErrorCode::AllProvidersFailed,
            Self::RateLimited { .. } => ErrorCode::RateLimited,
            Self::QuotaExhausted { .. } => ErrorCode::QuotaExceeded,
            Self::Unsupported(_) => ErrorCode::UnsupportedCapability,
            Self::NotFound => ErrorCode::NotFound,
            Self::Invalid(_) => ErrorCode::InvalidInput,
        }
    }

    /// Map an HTTP status from a vendor into the routing taxonomy.
    pub fn from_http_status(status: u16, body_hint: &str, retry_after: Option<Duration>) -> Self {
        match status {
            429 => Self::RateLimited { retry_after },
            402 => Self::QuotaExhausted { resets_at: None },
            401 | 403 => Self::Unsupported(format!("vendor rejected credentials ({status})")),
            404 => Self::NotFound,
            400 | 422 => Self::Invalid(truncate(body_hint)),
            500..=599 | 408 => Self::Transient(format!("HTTP {status}")),
            _ => Self::Fatal(format!("HTTP {status}: {}", truncate(body_hint))),
        }
    }
}

impl From<ProviderError> for DomainError {
    fn from(e: ProviderError) -> Self {
        let retry = match &e {
            ProviderError::RateLimited {
                retry_after: Some(d),
            } => Some(d.as_secs().max(1)),
            _ => None,
        };
        let mut d = DomainError::new(e.code(), e.to_string());
        d.retry_after_secs = retry;
        d
    }
}

fn truncate(s: &str) -> String {
    s.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failover_matrix() {
        assert!(ProviderError::Transient("x".into()).allows_failover());
        assert!(ProviderError::RateLimited { retry_after: None }.allows_failover());
        assert!(ProviderError::QuotaExhausted { resets_at: None }.allows_failover());
        assert!(ProviderError::Unsupported("x".into()).allows_failover());
        assert!(!ProviderError::NotFound.allows_failover());
        assert!(!ProviderError::Invalid("x".into()).allows_failover());
        assert!(!ProviderError::Fatal("x".into()).allows_failover());
        assert!(ProviderError::Transient("x".into()).is_retryable());
        assert!(!ProviderError::RateLimited { retry_after: None }.is_retryable());
    }

    #[test]
    fn http_mapping() {
        assert_eq!(
            ProviderError::from_http_status(429, "", None),
            ProviderError::RateLimited { retry_after: None }
        );
        assert!(matches!(
            ProviderError::from_http_status(503, "", None),
            ProviderError::Transient(_)
        ));
        assert!(matches!(
            ProviderError::from_http_status(401, "", None),
            ProviderError::Unsupported(_)
        ));
        assert!(matches!(
            ProviderError::from_http_status(400, "bad", None),
            ProviderError::Invalid(_)
        ));
    }
}
