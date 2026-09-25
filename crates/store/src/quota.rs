//! Quota engine: per vendor and window, limit / cap / effective budget next to usage from every
//! source, the source badge, burn rate, run-out projection, state and alert thresholds.
//!
//! Sources:
//! - `estimated`: local metering (the counter store, credits from the cost table).
//! - `headers`: the latest rate-limit response headers (`VendorHealth.usage`).
//! - `vendor_api`: `QuotaReporter` polling every `server.quota_poll_secs`.
//!
//! The guard uses the most pessimistic source. Routing (frozen) only reads the local counters
//! and `exhausted_until`, so after each poll the engine *reconciles* upward: when the vendor
//! reports more usage than we metered for the same calendar window and unit, the difference is
//! added to the counter store (method `vendor_reconcile`). Header snapshots with
//! `remaining = 0` already mark the vendor exhausted inside routing.

use crate::db::Store;
use bdm_config::{EffectiveBudget, Loaded, OnExhausted, ResetRule, Unit, VendorStatus};
use bdm_ports::{UsageUnit, VendorUsage, WindowKind};
use bdm_routing::{CounterStore, Dims, Router, VendorHealth, WindowKey};
use chrono::{DateTime, Datelike, Duration as ChronoDuration, TimeZone, Utc};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    Estimated,
    Headers,
    VendorApi,
}

/// Ordered by severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaState {
    Ok,
    Warning,
    /// The effective budget (cap / reserve) is used up: routing skips the vendor.
    Reserve,
    /// The vendor's own limit is used up, or it answered 429 / quota errors.
    Exhausted,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SourceReading {
    pub source: UsageSource,
    pub used: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<DateTime<Utc>>,
    /// False when the vendor reports in another unit than our budget (shown, not enforced).
    pub comparable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WindowQuota {
    /// `rps`, `per_minute`, `daily`, `monthly`, or `live` (rate-limit headers, window unknown).
    pub kind: String,
    pub limit: Option<u64>,
    pub cap: Option<u64>,
    pub effective: Option<u64>,
    pub used: Option<u64>,
    pub remaining: Option<u64>,
    pub used_pct: Option<u64>,
    /// Source of `used` (the most pessimistic comparable reading).
    pub source: Option<UsageSource>,
    pub sources: Vec<SourceReading>,
    pub resets_at: Option<DateTime<Utc>>,
    pub state: QuotaState,
    /// Alert thresholds (percent of effective) already crossed.
    pub alerts: Vec<u8>,
    pub burn_per_day: Option<u64>,
    pub runs_out_at: Option<DateTime<Utc>>,
    /// Env var locking `limit` / `cap` for this window, if any.
    pub limit_locked_by: Option<String>,
    pub cap_locked_by: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Breakdown {
    pub methods: Vec<(String, u64)>,
    pub tools: Vec<(String, u64)>,
    pub chains: Vec<(String, u64)>,
    pub clients: Vec<(String, u64)>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VendorQuota {
    pub vendor: String,
    pub display_name: String,
    pub status: VendorStatus,
    pub unit: Unit,
    pub reset: ResetRule,
    pub reserve_pct: u8,
    pub on_exhausted: OnExhausted,
    pub alert_pct: Vec<u8>,
    /// No `QuotaReporter` for this vendor: numbers are local estimates (plus headers).
    pub estimated_only: bool,
    pub plan: Option<String>,
    pub reported_at: Option<DateTime<Utc>>,
    pub report_error: Option<String>,
    pub state: QuotaState,
    pub exhausted_until: Option<DateTime<Utc>>,
    pub windows: Vec<WindowQuota>,
    /// Current-month breakdown (detail reports only).
    pub breakdown: Option<Breakdown>,
    /// Last 30 days of local usage, oldest first (detail reports only).
    pub series: Option<Vec<(String, u64)>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Projection {
    pub burn_per_day: u64,
    /// When the budget runs out at the current rate; `None` if not before the window resets
    /// (or nothing is used yet).
    pub runs_out_at: Option<DateTime<Utc>>,
}

/// Burn rate over the window so far, and the run-out time at that rate.
pub fn project(
    used: u64,
    budget: Option<u64>,
    window_start: DateTime<Utc>,
    now: DateTime<Utc>,
    resets_at: DateTime<Utc>,
) -> Projection {
    // ponytail: average since window start; a trailing-hour rate reacts faster if needed.
    let elapsed = (now - window_start).num_seconds().max(60) as u128;
    let burn_per_day = (u128::from(used) * 86_400 / elapsed).min(u128::from(u64::MAX)) as u64;
    let runs_out_at = budget.and_then(|b| {
        if used >= b {
            return Some(now);
        }
        if used == 0 {
            return None;
        }
        let secs = u128::from(b - used) * elapsed / u128::from(used);
        let secs = i64::try_from(secs).ok()?;
        let t = now.checked_add_signed(ChronoDuration::try_seconds(secs)?)?;
        (t < resets_at).then_some(t)
    });
    Projection {
        burn_per_day,
        runs_out_at,
    }
}

struct Reported {
    usage: Option<VendorUsage>,
    error: Option<String>,
}

pub struct QuotaEngine {
    router: Arc<Router>,
    store: Store,
    reported: Mutex<HashMap<String, Reported>>,
    alerted: Mutex<HashSet<String>>,
}

fn same_unit(a: Unit, b: UsageUnit) -> bool {
    matches!(
        (a, b),
        (Unit::Requests, UsageUnit::Requests) | (Unit::Credits, UsageUnit::Credits)
    )
}

fn window_start(kind: WindowKind, now: DateTime<Utc>) -> DateTime<Utc> {
    match kind {
        WindowKind::Month => Utc
            .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
            .single()
            .unwrap_or(now),
        _ => now
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .map(|d| d.and_utc())
            .unwrap_or(now),
    }
}

fn top(map: HashMap<String, u64>) -> Vec<(String, u64)> {
    let mut v: Vec<_> = map.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.truncate(10);
    v
}

fn locked_window(cfg: &Loaded, vendor: &str, which: &str, window: &str) -> Option<String> {
    let names: &[&str] = match window {
        "daily" => &["daily", "daily_credits", "daily_requests"],
        "monthly" => &["monthly", "monthly_credits", "monthly_requests"],
        other => return lock(cfg, &["vendors", vendor, which, other]),
    };
    names
        .iter()
        .find_map(|n| lock(cfg, &["vendors", vendor, which, n]))
}

fn lock(cfg: &Loaded, path: &[&str]) -> Option<String> {
    let path: Vec<String> = path.iter().map(|s| s.to_string()).collect();
    cfg.locked_by(&path).map(str::to_owned)
}

impl QuotaEngine {
    pub fn new(router: Arc<Router>, store: Store) -> Arc<Self> {
        Arc::new(Self {
            router,
            store,
            reported: Mutex::default(),
            alerted: Mutex::default(),
        })
    }

    /// Poll every registered `QuotaReporter` once, then reconcile local counters upward.
    pub async fn poll(&self) {
        let table = self.router.table();
        let reporters: Vec<_> = table
            .registry
            .quota_reporters()
            .map(|(v, r)| (v.clone(), r.clone()))
            .collect();
        let now = Utc::now();
        // ponytail: sequential; a handful of reporters, each bounded by a timeout.
        for (vendor, reporter) in reporters {
            let r = tokio::time::timeout(Duration::from_secs(15), reporter.usage()).await;
            let entry = match r {
                Ok(Ok(u)) => {
                    if let Some(b) = table.config.effective_budget(&vendor) {
                        self.reconcile(&vendor, &u, &b, now);
                    }
                    Reported {
                        usage: Some(u),
                        error: None,
                    }
                }
                Ok(Err(e)) => Reported {
                    usage: None,
                    error: Some(table.config.scrub(&e.to_string())),
                },
                Err(_) => Reported {
                    usage: None,
                    error: Some("usage endpoint timed out".into()),
                },
            };
            if let Some(e) = &entry.error {
                tracing::warn!(vendor, "quota reporter failed: {e}");
            }
            self.reported
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(vendor, entry);
        }
    }

    fn reconcile(
        &self,
        vendor: &str,
        usage: &VendorUsage,
        b: &EffectiveBudget,
        now: DateTime<Utc>,
    ) {
        for w in &usage.windows {
            let key = match w.kind {
                WindowKind::Day => WindowKey::day(now),
                WindowKind::Month => WindowKey::month(now),
                _ => continue,
            };
            if !same_unit(b.unit, w.unit) {
                continue;
            }
            // ponytail: assumes the vendor's window is the calendar window; billing-cycle vendors
            // may over-count by < 1 cycle, which errs on the safe (pessimistic) side.
            let local = self.store.total(vendor, &key);
            if w.used > local {
                let dims = Dims {
                    method: "vendor_reconcile".into(),
                    ..Default::default()
                };
                self.store.add(vendor, &key, w.used - local, &dims);
            }
        }
    }

    /// Quota report for every registry vendor (keyless ones included; the `rpc` pseudo-vendor
    /// is metered on the underlying RPC vendor and left out). `detail` adds the breakdown and
    /// the 30-day series.
    pub async fn report(&self, now: DateTime<Utc>, detail: bool) -> Vec<VendorQuota> {
        let table = self.router.table();
        let cfg = &table.config;
        let health: HashMap<String, VendorHealth> = self
            .router
            .health()
            .into_iter()
            .map(|h| (h.vendor.clone(), h))
            .collect();
        let reporters: HashSet<String> = table
            .registry
            .quota_reporters()
            .map(|(v, _)| v.clone())
            .collect();
        let mut out = Vec::new();
        for (vendor, entry) in &cfg.registry.vendors {
            if vendor == bdm_ports::RPC_VENDOR {
                continue;
            }
            let Some(b) = cfg.effective_budget(vendor) else {
                continue;
            };
            let (plan, reported_at, report_error, usage) = {
                let r = self.reported.lock().unwrap_or_else(|e| e.into_inner());
                match r.get(vendor) {
                    Some(r) => (
                        r.usage.as_ref().and_then(|u| u.plan.clone()),
                        r.usage.as_ref().map(|u| u.fetched_at),
                        r.error.clone(),
                        r.usage.clone(),
                    ),
                    None => (None, None, None, None),
                }
            };
            let h = health.get(vendor);
            let windows = self.windows(cfg, vendor, &b, h, usage.as_ref(), now);
            let exhausted_until = h.and_then(|h| h.exhausted_until);
            let mut state = windows
                .iter()
                .map(|w| w.state)
                .max()
                .unwrap_or(QuotaState::Ok);
            if exhausted_until.is_some() {
                state = QuotaState::Exhausted;
            }
            let (breakdown, series) = if detail {
                (
                    Some(self.breakdown(vendor, now).await),
                    self.store.daily_series(vendor, 30, now).await.ok(),
                )
            } else {
                (None, None)
            };
            out.push(VendorQuota {
                vendor: vendor.clone(),
                display_name: entry.display_name.clone(),
                status: cfg.vendor_status(vendor),
                unit: b.unit,
                reset: b.reset,
                reserve_pct: b.reserve_pct,
                on_exhausted: b.on_exhausted,
                alert_pct: b.alert_pct.clone(),
                estimated_only: !reporters.contains(vendor),
                plan,
                reported_at,
                report_error,
                state,
                exhausted_until,
                windows,
                breakdown,
                series,
            });
        }
        out
    }

    fn windows(
        &self,
        cfg: &Loaded,
        vendor: &str,
        b: &EffectiveBudget,
        h: Option<&VendorHealth>,
        usage: Option<&VendorUsage>,
        now: DateTime<Utc>,
    ) -> Vec<WindowQuota> {
        let min_alert = b.alert_pct.iter().copied().min();
        let mut rows = Vec::new();
        for (kind, wk, limit, cap, eff) in [
            (
                "rps",
                WindowKind::Second,
                b.limit.rps,
                b.cap.rps,
                b.effective.rps,
            ),
            (
                "per_minute",
                WindowKind::Minute,
                b.limit.per_minute,
                b.cap.per_minute,
                b.effective.per_minute,
            ),
            (
                "daily",
                WindowKind::Day,
                b.limit.daily,
                b.cap.daily,
                b.effective.daily,
            ),
            (
                "monthly",
                WindowKind::Month,
                b.limit.monthly,
                b.cap.monthly,
                b.effective.monthly,
            ),
        ] {
            let calendar = match wk {
                WindowKind::Day => Some(WindowKey::day(now)),
                WindowKind::Month => Some(WindowKey::month(now)),
                _ => None,
            };
            let mut sources = Vec::new();
            if let Some(key) = &calendar {
                sources.push(SourceReading {
                    source: UsageSource::Estimated,
                    used: self.store.total(vendor, key),
                    limit: None,
                    resets_at: Some(key.resets_at(now)),
                    comparable: true,
                });
            }
            for w in usage
                .iter()
                .flat_map(|u| &u.windows)
                .filter(|w| w.kind == wk)
            {
                sources.push(SourceReading {
                    source: UsageSource::VendorApi,
                    used: w.used,
                    limit: w.limit,
                    resets_at: w.resets_at,
                    comparable: same_unit(b.unit, w.unit),
                });
            }
            if limit.is_none()
                && cap.is_none()
                && eff.is_none()
                && sources.iter().all(|s| s.used == 0)
            {
                continue;
            }
            let best = sources
                .iter()
                .filter(|s| s.comparable)
                .max_by_key(|s| (s.used, s.source));
            let used = best.map(|s| s.used);
            let resets_at = calendar
                .as_ref()
                .map(|k| k.resets_at(now))
                .or_else(|| best.and_then(|s| s.resets_at));
            let used_pct = match (used, eff) {
                (Some(u), Some(e)) if e > 0 => Some(u.saturating_mul(100) / e),
                (Some(u), Some(0)) if u > 0 => Some(100),
                _ => None,
            };
            let state = match used {
                Some(u) if limit.is_some_and(|l| u >= l) => QuotaState::Exhausted,
                Some(u) if eff.is_some_and(|e| u >= e) => QuotaState::Reserve,
                _ if used_pct
                    .zip(min_alert)
                    .is_some_and(|(p, a)| p >= u64::from(a)) =>
                {
                    QuotaState::Warning
                }
                _ => QuotaState::Ok,
            };
            let alerts = b
                .alert_pct
                .iter()
                .copied()
                .filter(|a| used_pct.is_some_and(|p| p >= u64::from(*a)))
                .collect();
            let proj = match (used, &calendar, resets_at) {
                (Some(u), Some(_), Some(r)) => Some(project(u, eff, window_start(wk, now), now, r)),
                _ => None,
            };
            rows.push(WindowQuota {
                kind: kind.into(),
                limit,
                cap,
                effective: eff,
                used,
                remaining: used.zip(eff).map(|(u, e)| e.saturating_sub(u)),
                used_pct,
                source: best.map(|s| s.source),
                sources,
                resets_at,
                state,
                alerts,
                burn_per_day: proj.map(|p| p.burn_per_day),
                runs_out_at: proj.and_then(|p| p.runs_out_at),
                limit_locked_by: locked_window(cfg, vendor, "limit", kind),
                cap_locked_by: locked_window(cfg, vendor, "cap", kind),
            });
        }
        if let Some(u) = h.map(|h| &h.usage) {
            if u.header_limit.is_some() || u.header_remaining.is_some() {
                let used = u
                    .header_limit
                    .zip(u.header_remaining)
                    .map(|(l, r)| l.saturating_sub(r));
                rows.push(WindowQuota {
                    kind: "live".into(),
                    limit: u.header_limit,
                    cap: None,
                    effective: u.header_limit,
                    used,
                    remaining: u.header_remaining,
                    used_pct: used
                        .zip(u.header_limit)
                        .and_then(|(x, l)| (l > 0).then(|| x * 100 / l)),
                    source: Some(UsageSource::Headers),
                    sources: vec![SourceReading {
                        source: UsageSource::Headers,
                        used: used.unwrap_or(0),
                        limit: u.header_limit,
                        resets_at: None,
                        comparable: true,
                    }],
                    resets_at: h.and_then(|h| h.exhausted_until),
                    state: if u.header_remaining == Some(0) {
                        QuotaState::Exhausted
                    } else {
                        QuotaState::Ok
                    },
                    alerts: Vec::new(),
                    burn_per_day: None,
                    runs_out_at: None,
                    limit_locked_by: None,
                    cap_locked_by: None,
                });
            }
        }
        rows
    }

    async fn breakdown(&self, vendor: &str, now: DateTime<Utc>) -> Breakdown {
        let rows = self
            .store
            .breakdown_rows(vendor, &WindowKey::month(now))
            .await
            .unwrap_or_default();
        let mut maps: [HashMap<String, u64>; 4] = Default::default();
        for r in rows {
            let keys = [Some(r.method), r.tool, r.chain, r.client];
            for (map, k) in maps.iter_mut().zip(keys) {
                *map.entry(k.unwrap_or_else(|| "-".into())).or_default() += r.amount;
            }
        }
        let [methods, tools, chains, clients] = maps;
        Breakdown {
            methods: top(methods),
            tools: top(tools),
            chains: top(chains),
            clients: top(clients),
        }
    }

    /// Log each crossed alert threshold / reserve / exhaustion once per window.
    pub fn check_alerts(&self, report: &[VendorQuota]) {
        let mut seen = self.alerted.lock().unwrap_or_else(|e| e.into_inner());
        for v in report {
            for w in &v.windows {
                let window_id = w.resets_at.map(|t| t.to_rfc3339()).unwrap_or_default();
                for a in &w.alerts {
                    if seen.insert(format!("{}:{}:{window_id}:{a}", v.vendor, w.kind)) {
                        tracing::warn!(
                            vendor = %v.vendor,
                            window = %w.kind,
                            used = w.used,
                            effective = w.effective,
                            "quota alert: {a}% of the effective budget used"
                        );
                    }
                }
                if w.state >= QuotaState::Reserve
                    && seen.insert(format!("{}:{}:{window_id}:{:?}", v.vendor, w.kind, w.state))
                {
                    tracing::warn!(vendor = %v.vendor, window = %w.kind, state = ?w.state, "quota budget reached; routing skips this vendor until the window resets");
                }
            }
        }
    }

    /// Background loop: alert checks every minute, `QuotaReporter` polling every
    /// `server.quota_poll_secs` (read from the live config, so reloads apply).
    pub fn spawn(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut last_poll: Option<tokio::time::Instant> = None;
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                let every = Duration::from_secs(
                    self.router
                        .table()
                        .config
                        .settings
                        .server
                        .quota_poll_secs
                        .max(30),
                );
                if last_poll.is_none_or(|t| t.elapsed() >= every) {
                    self.poll().await;
                    last_poll = Some(tokio::time::Instant::now());
                }
                let report = self.report(Utc::now(), false).await;
                self.check_alerts(&report);
            }
        })
    }

    /// CSV export (one row per vendor window).
    pub fn to_csv(report: &[VendorQuota]) -> String {
        let mut out = String::from(
            "vendor,window,unit,limit,cap,effective,used,remaining,used_pct,source,state,burn_per_day,runs_out_at,resets_at,estimated_only\n",
        );
        let n = |v: Option<u64>| v.map(|x| x.to_string()).unwrap_or_default();
        let t = |v: Option<DateTime<Utc>>| v.map(|x| x.to_rfc3339()).unwrap_or_default();
        for v in report {
            for w in &v.windows {
                let row = [
                    v.vendor.clone(),
                    w.kind.clone(),
                    format!("{:?}", v.unit).to_lowercase(),
                    n(w.limit),
                    n(w.cap),
                    n(w.effective),
                    n(w.used),
                    n(w.remaining),
                    n(w.used_pct),
                    w.source.map(|s| to_snake(&s)).unwrap_or_default(),
                    to_snake(&w.state),
                    n(w.burn_per_day),
                    t(w.runs_out_at),
                    t(w.resets_at),
                    v.estimated_only.to_string(),
                ];
                out += &row.map(|c| csv_cell(&c)).join(",");
                out.push('\n');
            }
        }
        out
    }
}

fn to_snake<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn csv_cell(s: &str) -> String {
    // Quote cells with separators and neutralize spreadsheet formulas.
    let s = if s.starts_with(['=', '+', '-', '@']) {
        format!("'{s}")
    } else {
        s.to_owned()
    };
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use bdm_config::{ConfigDir, ConfigLoader, EnvSource};
    use bdm_ports::{PortResult, QuotaReporter, Registration, UsageWindow, VendorMeta};
    use bdm_routing::{ProviderRegistry, RouterOptions, RoutingTable};

    fn at(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap()
    }

    #[test]
    fn projection_math() {
        let start = at(2026, 9, 1, 0);
        let now = at(2026, 9, 11, 0); // 10 days in
        let reset = at(2026, 10, 1, 0);
        // 1000/day, 5000 left → 5 days
        let p = project(10_000, Some(15_000), start, now, reset);
        assert_eq!(p.burn_per_day, 1_000);
        assert_eq!(p.runs_out_at, Some(at(2026, 9, 16, 0)));
        // would run out after reset → none
        assert_eq!(
            project(1_000, Some(1_000_000), start, now, reset).runs_out_at,
            None
        );
        // already over
        assert_eq!(
            project(20, Some(10), start, now, reset).runs_out_at,
            Some(now)
        );
        // nothing used
        assert_eq!(
            project(0, Some(10), start, now, reset),
            Projection {
                burn_per_day: 0,
                runs_out_at: None
            }
        );
    }

    struct FakeReporter(u64);

    #[async_trait]
    impl QuotaReporter for FakeReporter {
        async fn usage(&self) -> PortResult<VendorUsage> {
            Ok(VendorUsage {
                plan: Some("Demo".into()),
                windows: vec![UsageWindow {
                    kind: WindowKind::Month,
                    unit: UsageUnit::Requests,
                    used: self.0,
                    limit: Some(10_000),
                    resets_at: None,
                }],
                fetched_at: Utc::now(),
            })
        }
    }

    fn engine(env: &[(&str, &str)], cfg: &str, reporter: Option<u64>) -> (Arc<QuotaEngine>, Store) {
        let loader = ConfigLoader::new(
            ConfigDir::new("/nonexistent"),
            EnvSource::from_pairs(env.iter().copied()),
        )
        .unwrap();
        let loaded = Arc::new(loader.load_texts(cfg, "").unwrap());
        let mut regs = vec![];
        if let Some(n) = reporter {
            regs.push(
                Registration::new(VendorMeta {
                    id: "coingecko".into(),
                    display_name: "CoinGecko".into(),
                    requires_key: true,
                    signup_url: None,
                    rpc_features: Default::default(),
                })
                .with_quota_reporter(Arc::new(FakeReporter(n))),
            );
        }
        let store = Store::open_in_memory().unwrap();
        let router = Router::new(
            RoutingTable {
                config: loaded,
                registry: ProviderRegistry::new(regs),
            },
            Arc::new(store.clone()),
            RouterOptions::default(),
        );
        (QuotaEngine::new(router, store.clone()), store)
    }

    #[tokio::test]
    async fn effective_budget_window_state_and_locks() {
        // alchemy: limit 30M, cap 15M (env, locked), reserve 20 → min(15M, 24M) = 15M
        let (e, store) = engine(
            &[("ODM__VENDORS__ALCHEMY__CAP__MONTHLY_CREDITS", "15000000")],
            "[vendors.alchemy]\nreserve_pct = 20\n",
            None,
        );
        let now = Utc::now();
        store.add(
            "alchemy",
            &WindowKey::month(now),
            12_000_000,
            &Dims::default(),
        );
        let report = e.report(now, true).await;
        let a = report.iter().find(|v| v.vendor == "alchemy").unwrap();
        let m = a.windows.iter().find(|w| w.kind == "monthly").unwrap();
        assert_eq!(
            (m.limit, m.cap, m.effective),
            (Some(30_000_000), Some(15_000_000), Some(15_000_000))
        );
        assert_eq!(m.used, Some(12_000_000));
        assert_eq!(m.remaining, Some(3_000_000));
        assert_eq!(m.used_pct, Some(80));
        assert_eq!(m.source, Some(UsageSource::Estimated));
        assert_eq!(m.state, QuotaState::Warning);
        assert_eq!(m.alerts, vec![75]);
        assert_eq!(
            m.cap_locked_by.as_deref(),
            Some("ODM__VENDORS__ALCHEMY__CAP__MONTHLY_CREDITS")
        );
        assert_eq!(m.limit_locked_by, None);
        assert!(a.estimated_only);
        assert_eq!(a.series.as_ref().unwrap().len(), 30);
        // keyless vendors get a card too
        assert!(report.iter().any(|v| v.vendor == "public"));
        assert!(!report.iter().any(|v| v.vendor == "rpc"));
        let csv = QuotaEngine::to_csv(&report);
        assert!(csv.contains("alchemy,monthly,credits,30000000,15000000,15000000,12000000"));
    }

    #[tokio::test]
    async fn vendor_api_is_most_pessimistic_and_reconciles_guard() {
        let (e, store) = engine(&[], "", Some(9_500));
        let now = Utc::now();
        store.add("coingecko", &WindowKey::month(now), 100, &Dims::default());
        e.poll().await;
        let report = e.report(now, false).await;
        let cg = report.iter().find(|v| v.vendor == "coingecko").unwrap();
        assert!(!cg.estimated_only);
        assert_eq!(cg.plan.as_deref(), Some("Demo"));
        let m = cg.windows.iter().find(|w| w.kind == "monthly").unwrap();
        assert_eq!(m.used, Some(9_500));
        assert_eq!(m.source, Some(UsageSource::VendorApi));
        // local counters were raised to the vendor's number, so routing's guard sees it
        assert_eq!(store.total("coingecko", &WindowKey::month(now)), 9_500);
        assert!(m.state >= QuotaState::Reserve, "{m:?}");
    }
}
