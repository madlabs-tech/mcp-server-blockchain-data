//! Runtime state per vendor (survives routing-table swaps): circuit breaker, health, quota marks,
//! rate limiters, and the usage sink that meters requests into the counter store.

use crate::{
    router::RoutingTable,
    store::{CounterStore, Dims, WindowKey},
};
use arc_swap::ArcSwap;
use bdm_config::{EffectiveBudget, OnExhausted};
use bdm_ports::{
    metering::{CallContext, RateLimitSnapshot, UsageSink},
    ProviderError,
};
use chrono::{DateTime, Utc};
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use serde::Serialize;
use std::{
    collections::HashMap,
    num::NonZeroU32,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakerState {
    Closed,
    Open,
    HalfOpen,
}

#[derive(Debug, Clone)]
enum Breaker {
    Closed { consecutive: u32 },
    Open { until: Instant },
    HalfOpen { probing: bool },
}

impl Breaker {
    fn view(&self) -> BreakerState {
        match self {
            Self::Closed { .. } => BreakerState::Closed,
            Self::Open { .. } => BreakerState::Open,
            Self::HalfOpen { .. } => BreakerState::HalfOpen,
        }
    }
}

#[derive(Debug, Clone)]
struct VendorState {
    breaker: Breaker,
    ok: u64,
    failed: u64,
    latency_ewma_ms: Option<f64>,
    last_error: Option<String>,
    exhausted_until: Option<DateTime<Utc>>,
    last_rate_limit: Option<(RateLimitSnapshot, DateTime<Utc>)>,
}

impl Default for VendorState {
    fn default() -> Self {
        Self {
            breaker: Breaker::Closed { consecutive: 0 },
            ok: 0,
            failed: 0,
            latency_ewma_ms: None,
            last_error: None,
            exhausted_until: None,
            last_rate_limit: None,
        }
    }
}

/// Usage of one vendor in the current windows vs its effective budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UsageSnapshot {
    pub day_used: u64,
    pub month_used: u64,
    pub day_budget: Option<u64>,
    pub month_budget: Option<u64>,
    pub rps_limit: Option<u64>,
    pub per_minute_limit: Option<u64>,
    /// Latest `remaining` reported by the vendor's rate-limit headers, if any.
    pub header_remaining: Option<u64>,
    pub header_limit: Option<u64>,
}

/// Health row for `provider_health` and the dashboard.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VendorHealth {
    pub vendor: String,
    pub breaker: BreakerState,
    pub ok: u64,
    pub failed: u64,
    pub consecutive_failures: u32,
    pub latency_ms: Option<u64>,
    pub last_error: Option<String>,
    pub exhausted_until: Option<DateTime<Utc>>,
    pub usage: UsageSnapshot,
}

struct Limiters {
    params: (Option<u64>, Option<u64>),
    rps: Option<DefaultDirectRateLimiter>,
    per_minute: Option<DefaultDirectRateLimiter>,
}

fn limiter(n: Option<u64>, per_minute: bool) -> Option<DefaultDirectRateLimiter> {
    let n = NonZeroU32::new(u32::try_from(n?).unwrap_or(u32::MAX))?;
    let quota = if per_minute {
        Quota::per_minute(n)
    } else {
        Quota::per_second(n)
    };
    Some(RateLimiter::direct(quota))
}

pub(crate) struct RuntimeState {
    vendors: Mutex<HashMap<String, VendorState>>,
    limiters: Mutex<HashMap<String, Arc<Limiters>>>,
    pub(crate) store: Arc<dyn CounterStore>,
    breaker_threshold: u32,
    breaker_cooldown: Duration,
}

impl RuntimeState {
    pub(crate) fn new(store: Arc<dyn CounterStore>, threshold: u32, cooldown: Duration) -> Self {
        Self {
            vendors: Mutex::default(),
            limiters: Mutex::default(),
            store,
            breaker_threshold: threshold.max(1),
            breaker_cooldown: cooldown,
        }
    }

    fn with<R>(&self, vendor: &str, f: impl FnOnce(&mut VendorState) -> R) -> R {
        let mut map = self.vendors.lock().unwrap_or_else(|e| e.into_inner());
        f(map.entry(vendor.to_owned()).or_default())
    }

    /// Whether the breaker lets a request through (an open breaker past cooldown admits one probe).
    pub(crate) fn breaker_allows(&self, vendor: &str) -> bool {
        self.with(vendor, |s| match s.breaker {
            Breaker::Closed { .. } => true,
            Breaker::Open { until } if Instant::now() >= until => {
                s.breaker = Breaker::HalfOpen { probing: true };
                true
            }
            Breaker::Open { .. } => false,
            Breaker::HalfOpen { probing: false } => {
                s.breaker = Breaker::HalfOpen { probing: true };
                true
            }
            Breaker::HalfOpen { probing: true } => false,
        })
    }

    /// Why the quota guard blocks this vendor right now, if it does.
    pub(crate) fn quota_block(
        &self,
        vendor: &str,
        budget: Option<&EffectiveBudget>,
    ) -> Option<&'static str> {
        let now = Utc::now();
        if self.with(vendor, |s| s.exhausted_until.is_some_and(|t| t > now)) {
            return Some("quota_exhausted");
        }
        let b = budget?;
        if b.on_exhausted == OnExhausted::AllowOverage {
            return None;
        }
        let over = |limit: Option<u64>, key: WindowKey| {
            limit.is_some_and(|l| self.store.total(vendor, &key) >= l)
        };
        if over(b.effective.daily, WindowKey::day(now))
            || over(b.effective.monthly, WindowKey::month(now))
        {
            return Some("quota_reserve");
        }
        None
    }

    /// Wait (bounded) for rate-limit tokens. `false` means "skip this vendor for now".
    pub(crate) async fn acquire(
        &self,
        vendor: &str,
        budget: Option<&EffectiveBudget>,
        max_wait: Duration,
    ) -> bool {
        let params = budget
            .map(|b| (b.effective.rps, b.effective.per_minute))
            .unwrap_or_default();
        if params == (None, None) {
            return true;
        }
        let lim = {
            let mut map = self.limiters.lock().unwrap_or_else(|e| e.into_inner());
            let entry = map.entry(vendor.to_owned()).or_insert_with(|| {
                Arc::new(Limiters {
                    params,
                    rps: limiter(params.0, false),
                    per_minute: limiter(params.1, true),
                })
            });
            if entry.params != params {
                *entry = Arc::new(Limiters {
                    params,
                    rps: limiter(params.0, false),
                    per_minute: limiter(params.1, true),
                });
            }
            entry.clone()
        };
        for l in [&lim.rps, &lim.per_minute].into_iter().flatten() {
            if tokio::time::timeout(max_wait, l.until_ready())
                .await
                .is_err()
            {
                return false;
            }
        }
        true
    }

    pub(crate) fn record(
        &self,
        vendor: &str,
        result: Result<(), &ProviderError>,
        latency: Duration,
    ) {
        let now = Utc::now();
        self.with(vendor, |s| {
            let ms = latency.as_secs_f64() * 1000.0;
            s.latency_ewma_ms = Some(s.latency_ewma_ms.map_or(ms, |prev| prev * 0.8 + ms * 0.2));
            let breaker_failure = match result {
                Ok(()) => false,
                Err(e) => {
                    s.last_error = Some(e.reason().to_owned());
                    match e {
                        ProviderError::RateLimited { retry_after } => {
                            let d = retry_after.unwrap_or(Duration::from_secs(10));
                            s.exhausted_until =
                                Some(now + chrono::Duration::from_std(d).unwrap_or_default());
                            false
                        }
                        ProviderError::QuotaExhausted { resets_at } => {
                            s.exhausted_until =
                                Some(resets_at.unwrap_or(now + chrono::Duration::hours(1)));
                            false
                        }
                        // The vendor answered correctly; the request was the problem.
                        ProviderError::Invalid(_) | ProviderError::NotFound => false,
                        ProviderError::Transient(_)
                        | ProviderError::Fatal(_)
                        | ProviderError::Unsupported(_) => true,
                    }
                }
            };
            if breaker_failure {
                s.failed += 1;
                s.breaker = match s.breaker {
                    Breaker::Closed { consecutive } if consecutive + 1 < self.breaker_threshold => {
                        Breaker::Closed {
                            consecutive: consecutive + 1,
                        }
                    }
                    _ => Breaker::Open {
                        until: Instant::now() + self.breaker_cooldown,
                    },
                };
            } else {
                if result.is_ok() {
                    s.ok += 1;
                }
                s.breaker = Breaker::Closed { consecutive: 0 };
            }
        });
    }

    pub(crate) fn record_rate_limit(&self, vendor: &str, snap: &RateLimitSnapshot) {
        let now = Utc::now();
        self.with(vendor, |s| {
            if snap.remaining == Some(0) || snap.retry_after.is_some() {
                let d = snap
                    .retry_after
                    .or(snap.reset_after)
                    .unwrap_or(Duration::from_secs(60));
                s.exhausted_until = Some(now + chrono::Duration::from_std(d).unwrap_or_default());
            }
            s.last_rate_limit = Some((snap.clone(), now));
        });
    }

    pub(crate) fn health(&self, vendor: &str, budget: Option<&EffectiveBudget>) -> VendorHealth {
        let now = Utc::now();
        let s = self.with(vendor, |s| s.clone());
        let usage = UsageSnapshot {
            day_used: self.store.total(vendor, &WindowKey::day(now)),
            month_used: self.store.total(vendor, &WindowKey::month(now)),
            day_budget: budget.and_then(|b| b.effective.daily),
            month_budget: budget.and_then(|b| b.effective.monthly),
            rps_limit: budget.and_then(|b| b.effective.rps),
            per_minute_limit: budget.and_then(|b| b.effective.per_minute),
            header_remaining: s.last_rate_limit.as_ref().and_then(|(r, _)| r.remaining),
            header_limit: s.last_rate_limit.as_ref().and_then(|(r, _)| r.limit),
        };
        VendorHealth {
            vendor: vendor.to_owned(),
            breaker: s.breaker.view(),
            ok: s.ok,
            failed: s.failed,
            consecutive_failures: match s.breaker {
                Breaker::Closed { consecutive } => consecutive,
                _ => self.breaker_threshold,
            },
            latency_ms: s.latency_ewma_ms.map(|v| v.round() as u64),
            last_error: s.last_error,
            exhausted_until: s.exhausted_until.filter(|t| *t > now),
            usage,
        }
    }
}

/// Meters requests reported by adapters (via `bdm_ports::metering`) into the counter store,
/// converting to credits with the vendor cost table of the *current* config.
pub(crate) struct QuotaSink {
    pub(crate) table: Arc<ArcSwap<RoutingTable>>,
    pub(crate) state: Arc<RuntimeState>,
}

impl UsageSink for QuotaSink {
    fn record_request(&self, vendor: &str, method: &str, ctx: &CallContext) {
        let cost = self.table.load().config.cost(vendor, method);
        let dims = Dims {
            method: method.to_owned(),
            tool: ctx.tool.clone(),
            chain: ctx.chain.as_ref().map(ToString::to_string),
            client: ctx.client.clone(),
        };
        let now = Utc::now();
        self.state
            .store
            .add(vendor, &WindowKey::day(now), cost, &dims);
        self.state
            .store
            .add(vendor, &WindowKey::month(now), cost, &dims);
    }

    fn record_rate_limit(&self, vendor: &str, snapshot: &RateLimitSnapshot) {
        self.state.record_rate_limit(vendor, snapshot);
    }
}
