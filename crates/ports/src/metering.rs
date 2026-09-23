//! Per-request usage metering without polluting port signatures.
//!
//! Routing wraps each operation in [`scope`] with a [`CallContext`]. Adapters' shared HTTP
//! client calls [`record_request`] for every outgoing vendor request and [`record_rate_limit`]
//! for parsed rate-limit headers. The sink (quota engine) turns requests into credits using the
//! vendor cost table and attributes them to tool / chain / client.
//!
//! Note: tokio task-locals do not cross `tokio::spawn`. Fan out with `join_all` /
//! `FuturesUnordered` inside the scope, or re-enter [`scope`] in the spawned task.

use crate::quota::WindowKind;
use ems_domain::ChainId;
use std::{future::Future, sync::Arc, time::Duration};

/// Rate-limit state parsed from response headers (`X-RateLimit-*`, IETF `RateLimit-*`, `Retry-After`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RateLimitSnapshot {
    pub limit: Option<u64>,
    pub remaining: Option<u64>,
    pub reset_after: Option<Duration>,
    pub window: Option<WindowKind>,
    pub retry_after: Option<Duration>,
}

pub trait UsageSink: Send + Sync {
    /// One outgoing request to `vendor`; `method` is the JSON-RPC method or REST endpoint label.
    fn record_request(&self, vendor: &str, method: &str, ctx: &CallContext);
    fn record_rate_limit(&self, vendor: &str, snapshot: &RateLimitSnapshot);
}

#[derive(Clone)]
pub struct CallContext {
    /// Operation (tool) name, e.g. "wallet_get_balances".
    pub tool: Option<String>,
    /// Client key id in hosted mode.
    pub client: Option<String>,
    pub chain: Option<ChainId>,
    pub sink: Arc<dyn UsageSink>,
}

tokio::task_local! {
    static CALL: CallContext;
}

/// Run `fut` with `ctx` as the current call context.
pub async fn scope<F: Future>(ctx: CallContext, fut: F) -> F::Output {
    CALL.scope(ctx, fut).await
}

/// Current context, if inside [`scope`].
pub fn current() -> Option<CallContext> {
    CALL.try_with(|c| c.clone()).ok()
}

pub fn record_request(vendor: &str, method: &str) {
    let _ = CALL.try_with(|c| c.sink.record_request(vendor, method, c));
}

pub fn record_rate_limit(vendor: &str, snapshot: &RateLimitSnapshot) {
    let _ = CALL.try_with(|c| c.sink.record_rate_limit(vendor, snapshot));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Sink(Mutex<Vec<String>>);
    impl UsageSink for Sink {
        fn record_request(&self, vendor: &str, method: &str, ctx: &CallContext) {
            self.0.lock().unwrap().push(format!(
                "{vendor}:{method}:{}",
                ctx.tool.clone().unwrap_or_default()
            ));
        }
        fn record_rate_limit(&self, _: &str, _: &RateLimitSnapshot) {}
    }

    #[tokio::test]
    async fn records_only_inside_scope() {
        let sink = Arc::new(Sink::default());
        record_request("alchemy", "eth_call"); // outside scope: ignored
        let ctx = CallContext {
            tool: Some("t".into()),
            client: None,
            chain: None,
            sink: sink.clone(),
        };
        scope(ctx, async { record_request("alchemy", "eth_getBalance") }).await;
        assert_eq!(
            *sink.0.lock().unwrap(),
            vec!["alchemy:eth_getBalance:t".to_string()]
        );
    }
}
