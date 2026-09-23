use crate::{
    registry::ProviderRegistry,
    state::{QuotaSink, RuntimeState, VendorHealth},
    store::CounterStore,
};
use arc_swap::ArcSwap;
use bdm_config::{Loaded, VendorStatus};
use bdm_domain::{
    Attempt, AttemptOutcome, ChainId, DomainError, ErrorCode, Provenance, SourceKind,
};
use bdm_ports::{metering::UsageSink, Capability, PortKind, ProviderError};
use futures::{stream::FuturesUnordered, StreamExt};
use rand::Rng;
use std::{future::Future, sync::Arc, time::Duration};
use tokio::time::Instant;

#[derive(Debug, Clone)]
pub struct RouterOptions {
    pub attempt_timeout: Duration,
    /// Extra attempts on the same vendor for `Transient` errors.
    pub retries: u32,
    pub breaker_threshold: u32,
    pub breaker_cooldown: Duration,
    /// Max time to wait for a local rate-limit token before skipping to the next vendor.
    pub max_rate_wait: Duration,
}

impl Default for RouterOptions {
    fn default() -> Self {
        Self {
            attempt_timeout: Duration::from_secs(10),
            retries: 1,
            breaker_threshold: 5,
            breaker_cooldown: Duration::from_secs(30),
            max_rate_wait: Duration::from_secs(1),
        }
    }
}

/// Immutable snapshot: config + registered ports. Swapped atomically on reload.
pub struct RoutingTable {
    pub config: Arc<Loaded>,
    pub registry: ProviderRegistry,
}

/// What to route: capability, chain (for chain-bound ports) and the operation name (for
/// per-operation orders and usage attribution).
#[derive(Debug, Clone)]
pub struct RouteReq {
    pub capability: Capability,
    pub chain: Option<ChainId>,
    pub op: Option<String>,
    /// Payments: ask a second provider before believing `NotFound` (providers lag each other).
    pub confirm_not_found: bool,
}

impl RouteReq {
    pub fn new(capability: Capability) -> Self {
        Self {
            capability,
            chain: None,
            op: None,
            confirm_not_found: false,
        }
    }
    pub fn chain(mut self, chain: ChainId) -> Self {
        self.chain = Some(chain);
        self
    }
    pub fn op(mut self, op: impl Into<String>) -> Self {
        self.op = Some(op.into());
        self
    }
    pub fn confirm_not_found(mut self) -> Self {
        self.confirm_not_found = true;
        self
    }
}

pub struct Candidate<P: ?Sized> {
    pub vendor: String,
    pub port: Arc<P>,
}

pub struct Candidates<P: ?Sized> {
    pub list: Vec<Candidate<P>>,
    /// Vendors in the order that were not usable, with the reason.
    pub skipped: Vec<Attempt>,
    /// First vendor of the configured order (decides Primary vs Fallback).
    pub order_head: Option<String>,
}

/// A routed answer with its provenance (`meta` in the response envelope).
#[derive(Debug, Clone)]
pub struct Routed<T> {
    pub value: T,
    pub provenance: Provenance,
}

#[derive(Debug, Clone)]
pub struct RouteError {
    pub error: DomainError,
    pub attempts: Vec<Attempt>,
}

impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for RouteError {}

/// Quorum result: `agreeing` providers returned the same key; `required` were asked for.
#[derive(Debug, Clone)]
pub struct QuorumOutcome<T> {
    pub value: T,
    pub agreeing: usize,
    pub required: usize,
}

impl<T> QuorumOutcome<T> {
    pub fn met(&self) -> bool {
        self.agreeing >= self.required
    }
}

pub struct Router {
    table: Arc<ArcSwap<RoutingTable>>,
    state: Arc<RuntimeState>,
    opts: RouterOptions,
}

impl Router {
    pub fn new(
        table: RoutingTable,
        store: Arc<dyn CounterStore>,
        opts: RouterOptions,
    ) -> Arc<Self> {
        let state = Arc::new(RuntimeState::new(
            store,
            opts.breaker_threshold,
            opts.breaker_cooldown,
        ));
        Arc::new(Self {
            table: Arc::new(ArcSwap::from_pointee(table)),
            state,
            opts,
        })
    }

    /// Current snapshot (hold it for the duration of one request).
    pub fn table(&self) -> Arc<RoutingTable> {
        self.table.load_full()
    }

    /// Hot-swap config + registry. Runtime state (breakers, counters) is kept.
    pub fn swap(&self, table: RoutingTable) {
        self.table.store(Arc::new(table));
    }

    /// Sink to install in `bdm_ports::metering::scope` for every operation.
    pub fn usage_sink(&self) -> Arc<dyn UsageSink> {
        Arc::new(QuotaSink {
            table: self.table.clone(),
            state: self.state.clone(),
        })
    }

    pub fn counters(&self) -> Arc<dyn CounterStore> {
        self.state.store.clone()
    }

    /// Health for every vendor known to config or registry.
    pub fn health(&self) -> Vec<VendorHealth> {
        let t = self.table();
        let mut ids: Vec<String> = t.config.registry.vendors.keys().cloned().collect();
        ids.extend(t.config.settings.custom_rpc.keys().cloned());
        ids.extend(t.registry.vendors().map(|v| v.id.clone()));
        ids.sort();
        ids.dedup();
        ids.iter()
            .map(|v| self.state.health(v, t.config.effective_budget(v).as_ref()))
            .collect()
    }

    /// Resolve the configured order and filter out vendors that can't serve now.
    pub fn candidates<P: PortKind + ?Sized>(
        &self,
        table: &RoutingTable,
        req: &RouteReq,
    ) -> Candidates<P> {
        let order = table
            .config
            .order(req.capability, req.chain.as_ref(), req.op.as_deref())
            .vendors;
        let mut list = Vec::new();
        let mut skipped = Vec::new();
        let skip = |vendor: &str, reason: String| Attempt {
            vendor: vendor.to_owned(),
            outcome: AttemptOutcome::Skipped,
            reason: Some(reason),
            error_code: None,
            latency_ms: None,
        };
        for vendor in &order {
            match table.config.vendor_status(vendor) {
                VendorStatus::Active => {}
                VendorStatus::MissingKey { env_vars } => {
                    skipped.push(skip(
                        vendor,
                        format!("missing_key ({})", env_vars.join(", ")),
                    ));
                    continue;
                }
                VendorStatus::Disabled { unverified } => {
                    let r = if unverified {
                        "disabled (free tier unverified)"
                    } else {
                        "disabled"
                    };
                    skipped.push(skip(vendor, r.into()));
                    continue;
                }
                VendorStatus::Unknown => {
                    skipped.push(skip(vendor, "unknown_vendor".into()));
                    continue;
                }
            }
            let Some(port) = table
                .registry
                .get(req.capability, req.chain.as_ref(), vendor)
                .and_then(P::extract)
            else {
                skipped.push(skip(vendor, "not_available_for_chain".into()));
                continue;
            };
            let budget = table.config.effective_budget(vendor);
            if let Some(reason) = self.state.quota_block(vendor, budget.as_ref()) {
                skipped.push(skip(vendor, reason.into()));
                continue;
            }
            if !self.state.breaker_allows(vendor) {
                skipped.push(skip(vendor, "breaker_open".into()));
                continue;
            }
            list.push(Candidate {
                vendor: vendor.clone(),
                port,
            });
        }
        Candidates {
            list,
            skipped,
            order_head: order.first().cloned(),
        }
    }

    /// One attempt through the resilience decorator: rate limit → timeout → retry → bookkeeping.
    async fn attempt<P, T, F, Fut>(
        &self,
        table: &RoutingTable,
        c: &Candidate<P>,
        f: &F,
    ) -> (Result<T, ProviderError>, Attempt)
    where
        P: ?Sized,
        F: Fn(Arc<P>) -> Fut,
        Fut: Future<Output = Result<T, ProviderError>>,
    {
        let budget = table.config.effective_budget(&c.vendor);
        let started = Instant::now();
        if !self
            .state
            .acquire(&c.vendor, budget.as_ref(), self.opts.max_rate_wait)
            .await
        {
            let err = ProviderError::RateLimited { retry_after: None };
            return (
                Err(err),
                attempt_row(
                    &c.vendor,
                    AttemptOutcome::Skipped,
                    Some("local_rate_limit".into()),
                    None,
                    started,
                ),
            );
        }
        let mut tries = 0;
        loop {
            let t0 = Instant::now();
            let r = match tokio::time::timeout(self.opts.attempt_timeout, f(c.port.clone())).await {
                Ok(r) => r,
                Err(_) => Err(ProviderError::Transient("timeout".into())),
            };
            self.state
                .record(&c.vendor, r.as_ref().map(|_| ()), t0.elapsed());
            match r {
                Err(e) if e.is_retryable() && tries < self.opts.retries => {
                    tries += 1;
                    let jitter = rand::rng().random_range(50..150);
                    tokio::time::sleep(Duration::from_millis(jitter)).await;
                }
                Ok(v) => {
                    return (
                        Ok(v),
                        attempt_row(&c.vendor, AttemptOutcome::Ok, None, None, started),
                    )
                }
                Err(e) => {
                    let row = attempt_row(
                        &c.vendor,
                        AttemptOutcome::Failed,
                        Some(table.config.scrub(&e.to_string())),
                        Some(e.code()),
                        started,
                    );
                    return (Err(e), row);
                }
            }
        }
    }

    /// Priority failover: try candidates in order; next one only on errors that allow failover
    /// (and on `NotFound` when `confirm_not_found` is set, until two providers agree).
    pub async fn failover<P, T, F, Fut>(&self, req: RouteReq, f: F) -> Result<Routed<T>, RouteError>
    where
        P: PortKind + ?Sized,
        F: Fn(Arc<P>) -> Fut,
        Fut: Future<Output = Result<T, ProviderError>>,
    {
        let table = self.table();
        let started = Instant::now();
        let cands = self.candidates::<P>(&table, &req);
        let mut attempts = cands.skipped.clone();
        let mut last_err = None;
        let mut not_found = 0;
        for c in &cands.list {
            let (r, row) = self.attempt(&table, c, &f).await;
            attempts.push(row);
            match r {
                Ok(v) => {
                    let prov = provenance(
                        &req,
                        attempts,
                        Some(&c.vendor),
                        cands.order_head.as_deref(),
                        started,
                    );
                    return Ok(Routed {
                        value: v,
                        provenance: prov,
                    });
                }
                Err(ProviderError::NotFound) if req.confirm_not_found && not_found == 0 => {
                    not_found += 1;
                    last_err = Some(ProviderError::NotFound);
                }
                Err(e) if e.allows_failover() => last_err = Some(e),
                Err(e) => return Err(route_error(&req, Some(e), attempts)),
            }
        }
        Err(route_error(&req, last_err, attempts))
    }

    /// Hedged: start candidate `i` after `i × delay`; the first success wins, the rest are dropped.
    pub async fn hedged<P, T, F, Fut>(
        &self,
        req: RouteReq,
        delay: Duration,
        f: F,
    ) -> Result<Routed<T>, RouteError>
    where
        P: PortKind + ?Sized,
        F: Fn(Arc<P>) -> Fut,
        Fut: Future<Output = Result<T, ProviderError>>,
    {
        let table = self.table();
        let started = Instant::now();
        let cands = self.candidates::<P>(&table, &req);
        let mut attempts = cands.skipped.clone();
        let mut running: FuturesUnordered<_> = cands
            .list
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let (table, f) = (&table, &f);
                async move {
                    tokio::time::sleep(delay * i as u32).await;
                    (c.vendor.clone(), self.attempt(table, c, f).await)
                }
            })
            .collect();
        let mut last_err = None;
        while let Some((vendor, (r, row))) = running.next().await {
            attempts.push(row);
            match r {
                Ok(v) => {
                    let prov = provenance(
                        &req,
                        attempts,
                        Some(&vendor),
                        cands.order_head.as_deref(),
                        started,
                    );
                    return Ok(Routed {
                        value: v,
                        provenance: prov,
                    });
                }
                Err(e) => last_err = Some(e),
            }
        }
        Err(route_error(&req, last_err, attempts))
    }

    /// Ask up to `n` providers (backfilling past failures) and compare `key(answer)`, e.g. the
    /// block hash of a receipt. Disagreement is a `CONFLICT`; fewer than `n` answers returns the
    /// first answer with `agreeing < required` so the operation can report it.
    pub async fn quorum<P, T, K, F, Fut>(
        &self,
        req: RouteReq,
        n: usize,
        key: K,
        f: F,
    ) -> Result<Routed<QuorumOutcome<T>>, RouteError>
    where
        P: PortKind + ?Sized,
        K: Fn(&T) -> String,
        F: Fn(Arc<P>) -> Fut,
        Fut: Future<Output = Result<T, ProviderError>>,
    {
        let (table, started) = (self.table(), Instant::now());
        let (oks, mut attempts, last_err, head) = self.collect_n(&table, &req, n.max(1), &f).await;
        if oks.is_empty() {
            return Err(route_error(&req, last_err, attempts));
        }
        let keys: Vec<String> = oks.iter().map(|(_, v)| key(v)).collect();
        if keys.iter().any(|k| k != &keys[0]) {
            let detail: Vec<String> = oks
                .iter()
                .zip(&keys)
                .map(|((v, _), k)| format!("{v}={k}"))
                .collect();
            let err = DomainError::new(
                ErrorCode::Conflict,
                format!("providers disagree: {}", detail.join(", ")),
            );
            attempts.sort_by_key(|a| a.outcome != AttemptOutcome::Skipped);
            return Err(RouteError {
                error: err,
                attempts,
            });
        }
        let agreeing = oks.len();
        let (vendor, value) = oks.into_iter().next().expect("non-empty");
        let mut prov = provenance(&req, attempts, Some(&vendor), head.as_deref(), started);
        if agreeing > 1 {
            prov.source = SourceKind::Aggregate;
        }
        Ok(Routed {
            value: QuorumOutcome {
                value,
                agreeing,
                required: n,
            },
            provenance: prov,
        })
    }

    /// Collect up to `n` successful answers (concurrently, backfilling failures) for median /
    /// spread style aggregation. Errors only if nobody answered.
    pub async fn aggregate<P, T, F, Fut>(
        &self,
        req: RouteReq,
        n: usize,
        f: F,
    ) -> Result<Routed<Vec<(String, T)>>, RouteError>
    where
        P: PortKind + ?Sized,
        F: Fn(Arc<P>) -> Fut,
        Fut: Future<Output = Result<T, ProviderError>>,
    {
        let (table, started) = (self.table(), Instant::now());
        let (oks, attempts, last_err, head) = self.collect_n(&table, &req, n.max(1), &f).await;
        if oks.is_empty() {
            return Err(route_error(&req, last_err, attempts));
        }
        let first = oks[0].0.clone();
        let mut prov = provenance(&req, attempts, Some(&first), head.as_deref(), started);
        prov.source = SourceKind::Aggregate;
        prov.provider = Some("aggregate".into());
        Ok(Routed {
            value: oks,
            provenance: prov,
        })
    }

    /// Send to every candidate concurrently (e.g. broadcast). Ok if at least one succeeded.
    #[allow(clippy::type_complexity)]
    pub async fn fan_out<P, T, F, Fut>(
        &self,
        req: RouteReq,
        f: F,
    ) -> Result<Routed<Vec<(String, Result<T, ProviderError>)>>, RouteError>
    where
        P: PortKind + ?Sized,
        F: Fn(Arc<P>) -> Fut,
        Fut: Future<Output = Result<T, ProviderError>>,
    {
        let (table, started) = (self.table(), Instant::now());
        let cands = self.candidates::<P>(&table, &req);
        let mut attempts = cands.skipped.clone();
        let results = futures::future::join_all(
            cands
                .list
                .iter()
                .map(|c| async { (c.vendor.clone(), self.attempt(&table, c, &f).await) }),
        )
        .await;
        let mut out = Vec::new();
        let mut last_err = None;
        for (vendor, (r, row)) in results {
            attempts.push(row);
            if let Err(e) = &r {
                last_err = Some(e.clone());
            }
            out.push((vendor, r));
        }
        if !out.iter().any(|(_, r)| r.is_ok()) {
            return Err(route_error(&req, last_err, attempts));
        }
        let first_ok = out.iter().find(|(_, r)| r.is_ok()).map(|(v, _)| v.clone());
        let mut prov = provenance(
            &req,
            attempts,
            first_ok.as_deref(),
            cands.order_head.as_deref(),
            started,
        );
        prov.source = SourceKind::Aggregate;
        Ok(Routed {
            value: out,
            provenance: prov,
        })
    }

    #[allow(clippy::type_complexity)]
    async fn collect_n<P, T, F, Fut>(
        &self,
        table: &RoutingTable,
        req: &RouteReq,
        n: usize,
        f: &F,
    ) -> (
        Vec<(String, T)>,
        Vec<Attempt>,
        Option<ProviderError>,
        Option<String>,
    )
    where
        P: PortKind + ?Sized,
        F: Fn(Arc<P>) -> Fut,
        Fut: Future<Output = Result<T, ProviderError>>,
    {
        let cands = self.candidates::<P>(table, req);
        let mut attempts = cands.skipped.clone();
        let mut oks = Vec::new();
        let mut last_err = None;
        let mut rest = cands.list.iter();
        let first_wave: Vec<_> = rest.by_ref().take(n).collect();
        let results = futures::future::join_all(
            first_wave
                .iter()
                .map(|c| async { (c.vendor.clone(), self.attempt(table, c, f).await) }),
        )
        .await;
        for (vendor, (r, row)) in results {
            attempts.push(row);
            match r {
                Ok(v) => oks.push((vendor, v)),
                Err(e) => last_err = Some(e),
            }
        }
        for c in rest {
            if oks.len() >= n {
                break;
            }
            let (r, row) = self.attempt(table, c, f).await;
            attempts.push(row);
            match r {
                Ok(v) => oks.push((c.vendor.clone(), v)),
                Err(e) => last_err = Some(e),
            }
        }
        (oks, attempts, last_err, cands.order_head)
    }
}

fn attempt_row(
    vendor: &str,
    outcome: AttemptOutcome,
    reason: Option<String>,
    code: Option<ErrorCode>,
    started: Instant,
) -> Attempt {
    Attempt {
        vendor: vendor.to_owned(),
        outcome,
        reason,
        error_code: code,
        latency_ms: Some(started.elapsed().as_millis() as u64),
    }
}

fn provenance(
    req: &RouteReq,
    attempts: Vec<Attempt>,
    winner: Option<&str>,
    head: Option<&str>,
    started: Instant,
) -> Provenance {
    let source = if winner.is_some() && winner == head {
        SourceKind::Primary
    } else {
        SourceKind::Fallback
    };
    let mut p = Provenance::new(source);
    p.chain = req.chain.clone();
    p.provider = winner.map(str::to_owned);
    p.providers_tried = attempts;
    p.latency_ms = started.elapsed().as_millis() as u64;
    p
}

fn route_error(req: &RouteReq, last: Option<ProviderError>, attempts: Vec<Attempt>) -> RouteError {
    let tried = attempts
        .iter()
        .any(|a| a.outcome != AttemptOutcome::Skipped);
    let error = match last {
        None if !tried => {
            let missing: Vec<&str> = attempts
                .iter()
                .filter_map(|a| a.reason.as_deref())
                .filter_map(|r| {
                    r.strip_prefix("missing_key (")
                        .and_then(|s| s.strip_suffix(')'))
                })
                .collect();
            let chain = req
                .chain
                .as_ref()
                .map(|c| format!(" on {c}"))
                .unwrap_or_default();
            let mut e = DomainError::new(
                ErrorCode::UnsupportedCapability,
                format!("no usable provider for '{}'{chain}", req.capability),
            );
            e.hint = Some(if missing.is_empty() {
                "check the routing order and vendor status in the dashboard".to_owned()
            } else {
                format!("add an API key: {}", missing.join(" or "))
            });
            e
        }
        None => DomainError::new(ErrorCode::AllProvidersFailed, "all providers failed"),
        Some(ProviderError::Invalid(m)) => DomainError::invalid(m),
        Some(ProviderError::NotFound) => DomainError::new(ErrorCode::NotFound, "not found"),
        Some(e @ (ProviderError::RateLimited { .. } | ProviderError::QuotaExhausted { .. })) => {
            e.into()
        }
        // Every vendor that was tried said "unsupported": keep that meaning (don't call it an outage).
        Some(ProviderError::Unsupported(m))
            if attempts
                .iter()
                .filter(|a| a.outcome == AttemptOutcome::Failed)
                .all(|a| a.error_code == Some(ErrorCode::UnsupportedCapability)) =>
        {
            DomainError::new(ErrorCode::UnsupportedCapability, m)
        }
        Some(e) => {
            let vendors: Vec<String> = attempts
                .iter()
                .filter(|a| a.outcome == AttemptOutcome::Failed)
                .map(|a| format!("{} ({})", a.vendor, a.reason.as_deref().unwrap_or("error")))
                .collect();
            DomainError::new(
                ErrorCode::AllProvidersFailed,
                format!("all providers failed: {}", vendors.join(", ")),
            )
            .with_hint(format!("last error: {}", e.reason()))
        }
    };
    RouteError { error, attempts }
}
