use crate::{
    catalog::{Catalog, ProfileSelection},
    ctx::{Caller, Ctx},
    metrics::OpMetrics,
    op::DynOperation,
};
use async_trait::async_trait;
use bdm_domain::{DomainError, ErrorCode};
use bdm_ports::metering::{self, CallContext};
use bdm_routing::Router;
use serde_json::Value;
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tracing::Instrument;

/// Admission hook (hosted mode: client auth is done by the transport; per-client rate limits and
/// quotas are enforced here by the platform module, T1.D3).
#[async_trait]
pub trait CallGuard: Send + Sync {
    async fn admit(&self, caller: &Caller, op: &str) -> Result<(), DomainError>;
}

/// Hosted-mode client authentication (implemented by the platform module, T1.D3). Transports
/// call it with the bearer token and attach the resulting [`Caller`] to the request.
#[async_trait]
pub trait ClientAuth: Send + Sync {
    async fn authenticate(&self, bearer: Option<&str>) -> Result<Caller, DomainError>;
}

/// Post-call hook (call log / SSE stream). Runs for every call on every transport, including
/// cache hits and rejections (unknown tool, guard). Must be cheap and non-blocking.
pub trait CallObserver: Send + Sync {
    fn on_call(
        &self,
        caller: &Caller,
        op: &str,
        result: &Result<Value, DomainError>,
        latency: Duration,
    );
}

/// Executes operations through the decorator chain shared by every transport.
pub struct App {
    catalog: Arc<Catalog>,
    router: Arc<Router>,
    cache: moka::future::Cache<String, Value>,
    guard: Option<Arc<dyn CallGuard>>,
    observer: Option<Arc<dyn CallObserver>>,
    metrics: Arc<OpMetrics>,
    seq: AtomicU64,
}

impl App {
    pub fn new(catalog: Catalog, router: Arc<Router>, cache_max_entries: u64) -> Self {
        let cache = moka::future::Cache::builder()
            .max_capacity(cache_max_entries)
            .expire_after(PerEntryTtl)
            .build();
        Self {
            catalog: Arc::new(catalog),
            router,
            cache,
            guard: None,
            observer: None,
            metrics: Arc::new(OpMetrics::default()),
            seq: AtomicU64::new(0),
        }
    }

    pub fn with_observer(mut self, observer: Arc<dyn CallObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    pub fn with_guard(mut self, guard: Arc<dyn CallGuard>) -> Self {
        self.guard = Some(guard);
        self
    }

    pub fn router(&self) -> &Arc<Router> {
        &self.router
    }

    pub fn catalog(&self) -> &Arc<Catalog> {
        &self.catalog
    }

    pub fn metrics(&self) -> &Arc<OpMetrics> {
        &self.metrics
    }

    /// Tools visible to this caller under the current config (server profile, or the client's).
    pub fn visible(&self, caller: &Caller) -> Vec<Arc<dyn DynOperation>> {
        let table = self.router.table();
        let sel = caller
            .profile
            .clone()
            .unwrap_or_else(|| ProfileSelection::from_config(&table.config));
        self.catalog
            .iter()
            .filter(|op| sel.includes(op.as_ref(), &table.config))
            .cloned()
            .collect()
    }

    /// Run one tool call. Returns the rendered JSON (`{data, meta}`, or bare data for legacy).
    pub async fn call(
        &self,
        name: &str,
        input: Value,
        caller: Caller,
    ) -> Result<Value, DomainError> {
        let started = Instant::now();
        let result = self.call_inner(name, input, caller.clone()).await;
        if let Some(o) = &self.observer {
            o.on_call(&caller, name, &result, started.elapsed());
        }
        result
    }

    async fn call_inner(
        &self,
        name: &str,
        input: Value,
        caller: Caller,
    ) -> Result<Value, DomainError> {
        let started = Instant::now();
        let op = self
            .visible(&caller)
            .into_iter()
            .find(|o| o.name() == name)
            .ok_or_else(|| {
                DomainError::new(ErrorCode::InvalidInput, format!("unknown tool '{name}'"))
            })?;
        if let Some(g) = &self.guard {
            g.admit(&caller, name).await?;
        }

        let table = self.router.table();
        let ttl = table
            .config
            .operation(name)
            .cache_ttl_secs
            .map(Duration::from_secs)
            .or_else(|| op.cache_ttl())
            .filter(|t| !t.is_zero());
        let cache_key = ttl.map(|_| format!("{name}:{input}"));
        if let Some(key) = &cache_key {
            if let Some(mut hit) = self.cache.get(key).await {
                mark_cached(&mut hit);
                self.metrics.record(name, true, true, started.elapsed());
                return Ok(hit);
            }
        }

        let request_id = format!(
            "r{:x}-{}",
            chrono::Utc::now().timestamp_millis(),
            self.seq.fetch_add(1, Ordering::Relaxed)
        );
        let ctx = Ctx::new(
            self.router.clone(),
            op.name(),
            caller.clone(),
            request_id.clone(),
        );
        let call_ctx = CallContext {
            tool: Some(name.to_owned()),
            client: caller.client.clone(),
            chain: None,
            sink: self.router.usage_sink(),
        };
        let span = tracing::info_span!("op", op = name, request_id = %request_id, client = caller.client.as_deref().unwrap_or("-"));
        let result = metering::scope(call_ctx, op.call(&ctx, input))
            .instrument(span)
            .await;
        let result = result.map_err(|mut e| {
            e.message = table.config.scrub(&e.message);
            e
        });

        self.metrics
            .record(name, result.is_ok(), false, started.elapsed());
        if let (Ok(v), Some(key), Some(ttl)) = (&result, cache_key, ttl) {
            self.cache.insert(key, with_ttl(v.clone(), ttl)).await;
        }
        result.map(strip_ttl)
    }
}

// The cache stores the TTL alongside the value so each entry can expire on its own schedule.
const TTL_KEY: &str = "__ems_cache_ttl_ms";

fn with_ttl(mut v: Value, ttl: Duration) -> Value {
    if let Value::Object(m) = &mut v {
        m.insert(TTL_KEY.into(), Value::from(ttl.as_millis() as u64));
    }
    v
}

fn strip_ttl(mut v: Value) -> Value {
    if let Value::Object(m) = &mut v {
        m.remove(TTL_KEY);
    }
    v
}

fn mark_cached(v: &mut Value) {
    if let Value::Object(m) = v {
        m.remove(TTL_KEY);
        if let Some(Value::Object(meta)) = m.get_mut("meta") {
            meta.insert("cached".into(), Value::Bool(true));
            meta.insert("source".into(), Value::String("cache".into()));
        }
    }
}

struct PerEntryTtl;

impl moka::Expiry<String, Value> for PerEntryTtl {
    fn expire_after_create(
        &self,
        _k: &String,
        v: &Value,
        _now: std::time::Instant,
    ) -> Option<Duration> {
        v.get(TTL_KEY)
            .and_then(Value::as_u64)
            .map(Duration::from_millis)
    }
}
