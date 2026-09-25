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
use futures::FutureExt;
use serde_json::Value;
use std::{
    collections::HashMap,
    panic::AssertUnwindSafe,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tracing::Instrument;

/// Admission hook (hosted mode: client auth is done by the transport; per-client rate limits and
/// quotas are enforced here by `bdm-store`).
#[async_trait]
pub trait CallGuard: Send + Sync {
    async fn admit(&self, caller: &Caller, op: &str) -> Result<(), DomainError>;
}

/// Hosted-mode client authentication (implemented in `bdm-store`). Transports
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
    cache: TtlCache,
    guard: Option<Arc<dyn CallGuard>>,
    observer: Option<Arc<dyn CallObserver>>,
    metrics: Arc<OpMetrics>,
    seq: AtomicU64,
}

impl App {
    pub fn new(catalog: Catalog, router: Arc<Router>, cache_max_entries: u64) -> Self {
        Self {
            catalog: Arc::new(catalog),
            router,
            cache: TtlCache {
                cap: usize::try_from(cache_max_entries).unwrap_or(usize::MAX),
                map: Mutex::default(),
            },
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
        // Defense in depth: a panicking tool must not take the transport (or the process) down.
        let result = match AssertUnwindSafe(self.call_inner(name, input, caller.clone()))
            .catch_unwind()
            .await
        {
            Ok(r) => r,
            Err(payload) => {
                let msg = panic_message(payload.as_ref());
                tracing::error!(
                    op = name,
                    "operation panicked: {}",
                    self.router.table().config.scrub(&msg)
                );
                self.metrics.record(name, false, false, started.elapsed());
                Err(DomainError::internal("internal error; see server log"))
            }
        };
        if let Some(o) = &self.observer {
            o.on_call(&caller, name, &result, started.elapsed());
        }
        // Legacy aliases keep their pre-refactor bare output (the envelope was for the observer).
        if self.catalog.get(name).is_some_and(|op| op.legacy()) {
            return result.map(|mut v| v.get_mut("data").map(Value::take).unwrap_or(v));
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
            if let Some(mut hit) = self.cache.get(key) {
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
        let ctx = Ctx::new(self.router.clone(), op.name());
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
            self.cache.insert(key, v.clone(), ttl);
        }
        result
    }
}

/// Text of a panic payload (`panic!("..")` gives `&str` or `String`; anything else is opaque).
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}

fn mark_cached(v: &mut Value) {
    if let Some(Value::Object(meta)) = v.get_mut("meta") {
        meta.insert("cached".into(), Value::Bool(true));
        meta.insert("source".into(), Value::String("cache".into()));
    }
}

/// Response cache: each entry expires on its own TTL; at most `cap` entries.
struct TtlCache {
    cap: usize,
    map: Mutex<HashMap<String, (Value, Instant)>>,
}

impl TtlCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, (Value, Instant)>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn get(&self, key: &str) -> Option<Value> {
        let mut map = self.lock();
        match map.get(key) {
            Some((v, expires)) if *expires > Instant::now() => Some(v.clone()),
            Some(_) => {
                map.remove(key);
                None
            }
            None => None,
        }
    }

    fn insert(&self, key: String, v: Value, ttl: Duration) {
        let now = Instant::now();
        // A TTL too large for the clock is not cached rather than panicking.
        let Some(expires) = now.checked_add(ttl) else {
            return;
        };
        if self.cap == 0 {
            return;
        }
        let mut map = self.lock();
        if map.len() >= self.cap && !map.contains_key(&key) {
            // ponytail: O(n) scan, only when full (cache_max_entries, default 20k); an expiry
            // heap if it ever shows in a profile.
            map.retain(|_, (_, e)| *e > now);
            if map.len() >= self.cap {
                let soonest = map
                    .iter()
                    .min_by_key(|(_, (_, e))| *e)
                    .map(|(k, _)| k.clone());
                if let Some(k) = soonest {
                    map.remove(&k);
                }
            }
        }
        map.insert(key, (v, expires));
    }
}
