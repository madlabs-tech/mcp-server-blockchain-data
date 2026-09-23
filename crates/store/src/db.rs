//! sqlite persistence on a dedicated thread, with in-memory mirrors for hot-path reads.

use bdm_config::ClientLimits;
use bdm_domain::DomainError;
use bdm_routing::{CounterStore, Dims, WindowKey};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rand::RngCore;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::Path,
    sync::{mpsc, Arc, Mutex},
    thread::JoinHandle,
    time::Duration,
};
use tokio::sync::broadcast;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError(pub String);

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "store: {}", self.0)
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self(e.to_string())
    }
}

type Result<T> = std::result::Result<T, StoreError>;

/// Ordered schema migrations; `schema_migrations` records the applied versions.
const MIGRATIONS: &[&str] = &[r#"
CREATE TABLE usage (
    vendor TEXT NOT NULL,
    window TEXT NOT NULL,
    method TEXT NOT NULL,
    tool   TEXT NOT NULL DEFAULT '',
    chain  TEXT NOT NULL DEFAULT '',
    client TEXT NOT NULL DEFAULT '',
    amount INTEGER NOT NULL,
    PRIMARY KEY (vendor, window, method, tool, chain, client)
);
CREATE INDEX usage_client ON usage (client, window);
CREATE TABLE calls (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    ts         TEXT NOT NULL,
    op         TEXT NOT NULL,
    chain      TEXT,
    provider   TEXT,
    fallback   INTEGER NOT NULL,
    cached     INTEGER NOT NULL,
    latency_ms INTEGER NOT NULL,
    client     TEXT,
    ok         INTEGER NOT NULL,
    error_code TEXT
);
CREATE TABLE clients (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    key_hash   TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    revoked_at TEXT,
    limits     TEXT
);
CREATE TABLE client_usage (
    client    TEXT NOT NULL,
    window    TEXT NOT NULL,
    requests  INTEGER NOT NULL DEFAULT 0,
    credits   INTEGER NOT NULL DEFAULT 0,
    throttled INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (client, window)
);
"#];

/// Default number of calls kept in the call-log ring.
const DEFAULT_RING: usize = 1_000;

fn wkey(w: &WindowKey) -> String {
    match w {
        WindowKey::Day(d) => format!("D:{d}"),
        WindowKey::Month(m) => format!("M:{m}"),
    }
}

fn parse_wkey(s: &str) -> Option<WindowKey> {
    match s.split_once(':')? {
        ("D", d) => Some(WindowKey::Day(d.to_owned())),
        ("M", m) => Some(WindowKey::Month(m.to_owned())),
        _ => None,
    }
}

/// SHA-256 of a client key, hex-encoded (the only form stored).
pub fn hash_key(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

/// One row of the call log (and one event of the dashboard's live stream).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallRecord {
    pub ts: DateTime<Utc>,
    pub op: String,
    pub chain: Option<String>,
    pub provider: Option<String>,
    pub fallback: bool,
    pub cached: bool,
    pub latency_ms: u64,
    pub client: Option<String>,
    pub ok: bool,
    pub error_code: Option<String>,
}

impl CallRecord {
    /// Build a row from a rendered tool result (`{data, meta}` envelope, or bare legacy data).
    pub fn from_result(
        client: Option<&str>,
        op: &str,
        result: &std::result::Result<Value, DomainError>,
        latency: Duration,
    ) -> Self {
        let meta = result.as_ref().ok().and_then(|v| v.get("meta"));
        let s = |k: &str| meta.and_then(|m| m.get(k)).and_then(Value::as_str);
        Self {
            ts: Utc::now(),
            op: op.to_owned(),
            chain: s("chain").map(str::to_owned),
            provider: s("provider").map(str::to_owned),
            fallback: s("source") == Some("fallback"),
            cached: meta
                .and_then(|m| m.get("cached"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            latency_ms: latency.as_millis() as u64,
            client: client.map(str::to_owned),
            ok: result.is_ok(),
            error_code: result.as_ref().err().map(|e| e.code.to_string()),
        }
    }
}

/// A hosted-mode client. The key itself is never stored, only its SHA-256.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClientRecord {
    pub id: String,
    pub name: String,
    #[serde(skip)]
    pub key_hash: String,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    /// Per-client override; fields fall back to config `clients.overrides.<id>` then `clients.default`.
    pub limits: Option<ClientLimits>,
}

impl ClientRecord {
    pub fn active(&self) -> bool {
        self.revoked_at.is_none()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ClientCounters {
    pub requests: u64,
    /// Vendor credits spent on the client's behalf (sum over vendors, each in its own unit).
    pub credits: u64,
    pub throttled: u64,
}

/// Which client quota was hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exceeded {
    DailyRequests,
    MonthlyCredits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BreakdownRow {
    pub method: String,
    pub tool: Option<String>,
    pub chain: Option<String>,
    pub client: Option<String>,
    pub amount: u64,
}

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// Owns the connection thread; dropping it drains queued writes and joins the thread.
struct Worker {
    tx: Mutex<Option<mpsc::Sender<Job>>>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        drop(self.tx.lock().expect("worker lock").take());
        if let Some(h) = self.handle.lock().expect("worker lock").take() {
            let _ = h.join();
        }
    }
}

struct Inner {
    // Declared first so it drops (flushing writes) before the caches.
    worker: Worker,
    totals: Mutex<HashMap<(String, WindowKey), u64>>,
    clients: Mutex<HashMap<String, ClientRecord>>,
    client_usage: Mutex<HashMap<(String, WindowKey), ClientCounters>>,
    calls: broadcast::Sender<CallRecord>,
    ring: usize,
}

/// Handle to the sqlite store. Cheap to clone.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

impl Store {
    /// Open (or create) the database file. Parent directories are created.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| StoreError(e.to_string()))?;
        }
        Self::start(Connection::open(path)?)
    }

    /// Private in-memory database (tests, ephemeral runs).
    pub fn open_in_memory() -> Result<Self> {
        Self::start(Connection::open_in_memory()?)
    }

    fn start(mut conn: Connection) -> Result<Self> {
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get::<_, String>(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(Duration::from_secs(5))?;
        migrate(&mut conn)?;
        let totals = load_totals(&conn)?;
        let clients = load_clients(&conn)?;
        let client_usage = load_client_usage(&conn)?;

        let (tx, rx) = mpsc::channel::<Job>();
        let handle = std::thread::Builder::new()
            .name("bdm-store".into())
            .spawn(move || {
                let mut conn = conn;
                for job in rx {
                    job(&mut conn);
                }
            })
            .map_err(|e| StoreError(e.to_string()))?;
        Ok(Self {
            inner: Arc::new(Inner {
                worker: Worker {
                    tx: Mutex::new(Some(tx)),
                    handle: Mutex::new(Some(handle)),
                },
                totals: Mutex::new(totals),
                clients: Mutex::new(clients),
                client_usage: Mutex::new(client_usage),
                calls: broadcast::channel(256).0,
                ring: DEFAULT_RING,
            }),
        })
    }

    fn send(&self, job: Job) -> bool {
        let tx = self.inner.worker.tx.lock().expect("worker lock");
        tx.as_ref().is_some_and(|tx| tx.send(job).is_ok())
    }

    /// Fire-and-forget write; errors are logged.
    fn submit(
        &self,
        what: &'static str,
        f: impl FnOnce(&mut Connection) -> Result<()> + Send + 'static,
    ) {
        let queued = self.send(Box::new(move |c| {
            if let Err(e) = f(c) {
                tracing::error!(what, "sqlite write failed: {e}");
            }
        }));
        if !queued {
            tracing::error!(what, "store thread is gone; write dropped");
        }
    }

    /// Run `f` on the store thread and wait (blocking). Do not call from an async task without
    /// `spawn_blocking`; use [`Store::call`] there.
    pub fn exec<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> R + Send + 'static,
    ) -> Result<R> {
        let (tx, rx) = mpsc::sync_channel(1);
        if !self.send(Box::new(move |c| {
            let _ = tx.send(f(c));
        })) {
            return Err(StoreError("store thread is gone".into()));
        }
        rx.recv()
            .map_err(|_| StoreError("store thread is gone".into()))
    }

    /// Run `f` on the store thread and await the result without blocking the runtime.
    pub async fn call<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> R + Send + 'static,
    ) -> Result<R> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if !self.send(Box::new(move |c| {
            let _ = tx.send(f(c));
        })) {
            return Err(StoreError("store thread is gone".into()));
        }
        rx.await
            .map_err(|_| StoreError("store thread is gone".into()))
    }

    // ------------------------------------------------------------------ usage history

    /// Per-day totals for `vendor` over the last `days` days ending at `now` (oldest first,
    /// zero-filled): the dashboard's 30-day chart.
    pub async fn daily_series(
        &self,
        vendor: &str,
        days: u32,
        now: DateTime<Utc>,
    ) -> Result<Vec<(String, u64)>> {
        let dates: Vec<String> = (0..days)
            .rev()
            .map(|i| {
                (now - ChronoDuration::days(i64::from(i)))
                    .format("%Y-%m-%d")
                    .to_string()
            })
            .collect();
        let from = format!("D:{}", dates.first().cloned().unwrap_or_default());
        let vendor = vendor.to_owned();
        let rows: HashMap<String, u64> = self
            .call(move |c| -> Result<HashMap<String, u64>> {
                let mut st = c.prepare(
                    "SELECT window, SUM(amount) FROM usage WHERE vendor = ?1 AND window >= ?2 \
                     AND window LIKE 'D:%' GROUP BY window",
                )?;
                let rows = st.query_map(params![vendor, from], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                })?;
                let mut out = HashMap::new();
                for row in rows {
                    let (w, n) = row?;
                    out.insert(w.trim_start_matches("D:").to_owned(), n.max(0) as u64);
                }
                Ok(out)
            })
            .await??;
        Ok(dates
            .into_iter()
            .map(|d| {
                let n = rows.get(&d).copied().unwrap_or(0);
                (d, n)
            })
            .collect())
    }

    pub async fn breakdown_rows(
        &self,
        vendor: &str,
        window: &WindowKey,
    ) -> Result<Vec<BreakdownRow>> {
        let (vendor, window) = (vendor.to_owned(), wkey(window));
        self.call(move |c| query_breakdown(c, &vendor, &window))
            .await?
    }

    // ------------------------------------------------------------------ call log

    /// Append to the call-log ring and publish to live subscribers.
    pub fn log_call(&self, rec: CallRecord) {
        let _ = self.inner.calls.send(rec.clone());
        let ring = self.inner.ring as i64;
        self.submit("call log", move |c| {
            c.execute(
                "INSERT INTO calls (ts, op, chain, provider, fallback, cached, latency_ms, client, ok, error_code) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    rec.ts.to_rfc3339(),
                    rec.op,
                    rec.chain,
                    rec.provider,
                    rec.fallback,
                    rec.cached,
                    rec.latency_ms as i64,
                    rec.client,
                    rec.ok,
                    rec.error_code
                ],
            )?;
            c.execute(
                "DELETE FROM calls WHERE id <= (SELECT MAX(id) FROM calls) - ?1",
                params![ring],
            )?;
            Ok(())
        });
    }

    /// Live call stream (dashboard SSE).
    pub fn subscribe_calls(&self) -> broadcast::Receiver<CallRecord> {
        self.inner.calls.subscribe()
    }

    /// Most recent calls, newest first.
    pub async fn recent_calls(&self, limit: usize) -> Result<Vec<CallRecord>> {
        let limit = limit.min(self.inner.ring) as i64;
        self.call(move |c| -> Result<Vec<CallRecord>> {
            let mut st = c.prepare(
                "SELECT ts, op, chain, provider, fallback, cached, latency_ms, client, ok, error_code \
                 FROM calls ORDER BY id DESC LIMIT ?1",
            )?;
            let rows = st.query_map(params![limit], |r| {
                Ok(CallRecord {
                    ts: parse_ts(&r.get::<_, String>(0)?),
                    op: r.get(1)?,
                    chain: r.get(2)?,
                    provider: r.get(3)?,
                    fallback: r.get(4)?,
                    cached: r.get(5)?,
                    latency_ms: r.get::<_, i64>(6)?.max(0) as u64,
                    client: r.get(7)?,
                    ok: r.get(8)?,
                    error_code: r.get(9)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await?
    }

    // ------------------------------------------------------------------ clients

    /// Create a client and return it with its key. The key is shown once and never stored.
    pub async fn create_client(
        &self,
        name: &str,
        limits: Option<ClientLimits>,
    ) -> Result<(ClientRecord, String)> {
        let name = name.trim();
        if name.is_empty() || name.len() > 100 {
            return Err(StoreError("client name must be 1..=100 characters".into()));
        }
        let key = format!("bdm_{}", random_hex(32));
        let rec = ClientRecord {
            id: format!("c_{}", random_hex(6)),
            name: name.to_owned(),
            key_hash: hash_key(&key),
            created_at: Utc::now(),
            revoked_at: None,
            limits,
        };
        let row = rec.clone();
        self.call(move |c| -> Result<()> {
            c.execute(
                "INSERT INTO clients (id, name, key_hash, created_at, revoked_at, limits) VALUES (?1, ?2, ?3, ?4, NULL, ?5)",
                params![
                    row.id,
                    row.name,
                    row.key_hash,
                    row.created_at.to_rfc3339(),
                    limits_json(&row.limits)
                ],
            )?;
            Ok(())
        })
        .await??;
        self.inner
            .clients
            .lock()
            .expect("clients lock")
            .insert(rec.id.clone(), rec.clone());
        Ok((rec, key))
    }

    /// Look up a client by its key (in memory). Includes revoked clients; check [`ClientRecord::active`].
    // ponytail: linear scan over clients; index by hash if an instance ever has thousands.
    pub fn client_by_key(&self, key: &str) -> Option<ClientRecord> {
        let h = hash_key(key);
        self.inner
            .clients
            .lock()
            .expect("clients lock")
            .values()
            .find(|c| c.key_hash == h)
            .cloned()
    }

    pub fn client(&self, id: &str) -> Option<ClientRecord> {
        self.inner
            .clients
            .lock()
            .expect("clients lock")
            .get(id)
            .cloned()
    }

    /// Every client, oldest first.
    pub fn clients(&self) -> Vec<ClientRecord> {
        let mut v: Vec<_> = self
            .inner
            .clients
            .lock()
            .expect("clients lock")
            .values()
            .cloned()
            .collect();
        v.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        v
    }

    pub fn active_client_count(&self) -> usize {
        self.inner
            .clients
            .lock()
            .expect("clients lock")
            .values()
            .filter(|c| c.active())
            .count()
    }

    /// Revoke a client key. `false` if the id is unknown.
    pub async fn revoke_client(&self, id: &str) -> Result<bool> {
        let Some(mut rec) = self.client(id) else {
            return Ok(false);
        };
        if rec.revoked_at.is_none() {
            rec.revoked_at = Some(Utc::now());
        }
        let (rid, ts) = (rec.id.clone(), rec.revoked_at.map(|t| t.to_rfc3339()));
        self.call(move |c| {
            c.execute(
                "UPDATE clients SET revoked_at = ?2 WHERE id = ?1",
                params![rid, ts],
            )
        })
        .await??;
        self.inner
            .clients
            .lock()
            .expect("clients lock")
            .insert(rec.id.clone(), rec);
        Ok(true)
    }

    /// Set (or clear) a client's limits override. `false` if the id is unknown.
    pub async fn set_client_limits(&self, id: &str, limits: Option<ClientLimits>) -> Result<bool> {
        let Some(mut rec) = self.client(id) else {
            return Ok(false);
        };
        rec.limits = limits;
        let (rid, json) = (rec.id.clone(), limits_json(&rec.limits));
        self.call(move |c| {
            c.execute(
                "UPDATE clients SET limits = ?2 WHERE id = ?1",
                params![rid, json],
            )
        })
        .await??;
        self.inner
            .clients
            .lock()
            .expect("clients lock")
            .insert(rec.id.clone(), rec);
        Ok(true)
    }

    // ------------------------------------------------------------------ per-client counters

    pub fn client_counters(&self, client: &str, window: &WindowKey) -> ClientCounters {
        self.inner
            .client_usage
            .lock()
            .expect("client usage lock")
            .get(&(client.to_owned(), window.clone()))
            .copied()
            .unwrap_or_default()
    }

    /// Atomically check the client's daily-request and monthly-credit quotas and, if both have
    /// room, count one request.
    pub fn admit_client(
        &self,
        client: &str,
        now: DateTime<Utc>,
        daily_requests: Option<u64>,
        monthly_credits: Option<u64>,
    ) -> std::result::Result<(), Exceeded> {
        let (day, month) = (WindowKey::day(now), WindowKey::month(now));
        {
            let mut m = self.inner.client_usage.lock().expect("client usage lock");
            let d = m
                .get(&(client.to_owned(), day.clone()))
                .copied()
                .unwrap_or_default();
            let mo = m
                .get(&(client.to_owned(), month.clone()))
                .copied()
                .unwrap_or_default();
            if daily_requests.is_some_and(|l| d.requests >= l) {
                return Err(Exceeded::DailyRequests);
            }
            if monthly_credits.is_some_and(|l| mo.credits >= l) {
                return Err(Exceeded::MonthlyCredits);
            }
            for w in [&day, &month] {
                m.entry((client.to_owned(), w.clone()))
                    .or_default()
                    .requests += 1;
            }
        }
        self.bump_client(client, &[day, month], 1, 0, 0);
        Ok(())
    }

    /// Count a rejected (throttled) request for the dashboard.
    pub fn record_throttled(&self, client: &str, now: DateTime<Utc>) {
        let windows = [WindowKey::day(now), WindowKey::month(now)];
        {
            let mut m = self.inner.client_usage.lock().expect("client usage lock");
            for w in &windows {
                m.entry((client.to_owned(), w.clone()))
                    .or_default()
                    .throttled += 1;
            }
        }
        self.bump_client(client, &windows, 0, 0, 1);
    }

    fn bump_client(
        &self,
        client: &str,
        windows: &[WindowKey],
        requests: u64,
        credits: u64,
        throttled: u64,
    ) {
        let client = client.to_owned();
        let windows: Vec<String> = windows.iter().map(wkey).collect();
        self.submit("client usage", move |c| {
            for w in windows {
                c.execute(
                    "INSERT INTO client_usage (client, window, requests, credits, throttled) VALUES (?1, ?2, ?3, ?4, ?5) \
                     ON CONFLICT (client, window) DO UPDATE SET requests = requests + excluded.requests, \
                     credits = credits + excluded.credits, throttled = throttled + excluded.throttled",
                    params![client, w, requests as i64, credits as i64, throttled as i64],
                )?;
            }
            Ok(())
        });
    }

    /// Top tools by credits for a client in a window (dashboard).
    pub async fn client_top_tools(
        &self,
        client: &str,
        window: &WindowKey,
    ) -> Result<Vec<(String, u64)>> {
        let (client, window) = (client.to_owned(), wkey(window));
        self.call(move |c| -> Result<Vec<(String, u64)>> {
            let mut st = c.prepare(
                "SELECT tool, SUM(amount) AS n FROM usage WHERE client = ?1 AND window = ?2 \
                 GROUP BY tool ORDER BY n DESC LIMIT 5",
            )?;
            let rows = st.query_map(params![client, window], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as u64))
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await?
    }
}

impl CounterStore for Store {
    fn add(&self, vendor: &str, window: &WindowKey, amount: u64, dims: &Dims) {
        *self
            .inner
            .totals
            .lock()
            .expect("totals lock")
            .entry((vendor.to_owned(), window.clone()))
            .or_default() += amount;
        if let Some(client) = &dims.client {
            self.inner
                .client_usage
                .lock()
                .expect("client usage lock")
                .entry((client.clone(), window.clone()))
                .or_default()
                .credits += amount;
            self.bump_client(client, std::slice::from_ref(window), 0, amount, 0);
        }
        let (vendor, window, dims) = (vendor.to_owned(), wkey(window), dims.clone());
        self.submit("usage", move |c| {
            c.execute(
                "INSERT INTO usage (vendor, window, method, tool, chain, client, amount) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                 ON CONFLICT (vendor, window, method, tool, chain, client) DO UPDATE SET amount = amount + excluded.amount",
                params![
                    vendor,
                    window,
                    dims.method,
                    dims.tool.unwrap_or_default(),
                    dims.chain.unwrap_or_default(),
                    dims.client.unwrap_or_default(),
                    amount as i64
                ],
            )?;
            Ok(())
        });
    }

    fn total(&self, vendor: &str, window: &WindowKey) -> u64 {
        self.inner
            .totals
            .lock()
            .expect("totals lock")
            .get(&(vendor.to_owned(), window.clone()))
            .copied()
            .unwrap_or(0)
    }

    /// Blocking round-trip to the store thread (dashboard only; call via `spawn_blocking`).
    fn breakdown(&self, vendor: &str, window: &WindowKey) -> Vec<(Dims, u64)> {
        let (vendor, window) = (vendor.to_owned(), wkey(window));
        let rows = self
            .exec(move |c| query_breakdown(c, &vendor, &window))
            .and_then(|r| r)
            .unwrap_or_else(|e| {
                tracing::error!("breakdown query failed: {e}");
                Vec::new()
            });
        rows.into_iter()
            .map(|r| {
                (
                    Dims {
                        method: r.method,
                        tool: r.tool,
                        chain: r.chain,
                        client: r.client,
                    },
                    r.amount,
                )
            })
            .collect()
    }
}

fn nonempty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

fn query_breakdown(c: &Connection, vendor: &str, window: &str) -> Result<Vec<BreakdownRow>> {
    let mut st = c.prepare(
        "SELECT method, tool, chain, client, amount FROM usage WHERE vendor = ?1 AND window = ?2 ORDER BY amount DESC",
    )?;
    let rows = st.query_map(params![vendor, window], |r| {
        Ok(BreakdownRow {
            method: r.get(0)?,
            tool: nonempty(r.get(1)?),
            chain: nonempty(r.get(2)?),
            client: nonempty(r.get(3)?),
            amount: r.get::<_, i64>(4)?.max(0) as u64,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn parse_ts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_default()
}

fn limits_json(l: &Option<ClientLimits>) -> Option<String> {
    l.as_ref().and_then(|l| serde_json::to_string(l).ok())
}

fn migrate(conn: &mut Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)",
    )?;
    let current: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get::<_, Option<i64>>(0)
        })?
        .unwrap_or(0);
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let version = i as i64 + 1;
        if version <= current {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            params![version, Utc::now().to_rfc3339()],
        )?;
        tx.commit()?;
    }
    Ok(())
}

fn load_totals(c: &Connection) -> Result<HashMap<(String, WindowKey), u64>> {
    let mut st =
        c.prepare("SELECT vendor, window, SUM(amount) FROM usage GROUP BY vendor, window")?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (v, w, n) = row?;
        if let Some(w) = parse_wkey(&w) {
            out.insert((v, w), n.max(0) as u64);
        }
    }
    Ok(out)
}

fn load_clients(c: &Connection) -> Result<HashMap<String, ClientRecord>> {
    let mut st =
        c.prepare("SELECT id, name, key_hash, created_at, revoked_at, limits FROM clients")?;
    let rows = st.query_map([], |r| {
        Ok(ClientRecord {
            id: r.get(0)?,
            name: r.get(1)?,
            key_hash: r.get(2)?,
            created_at: parse_ts(&r.get::<_, String>(3)?),
            revoked_at: r.get::<_, Option<String>>(4)?.map(|s| parse_ts(&s)),
            limits: r
                .get::<_, Option<String>>(5)?
                .and_then(|s| serde_json::from_str(&s).ok()),
        })
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let rec = row?;
        out.insert(rec.id.clone(), rec);
    }
    Ok(out)
}

fn load_client_usage(c: &Connection) -> Result<HashMap<(String, WindowKey), ClientCounters>> {
    let mut st =
        c.prepare("SELECT client, window, requests, credits, throttled FROM client_usage")?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            ClientCounters {
                requests: r.get::<_, i64>(2)?.max(0) as u64,
                credits: r.get::<_, i64>(3)?.max(0) as u64,
                throttled: r.get::<_, i64>(4)?.max(0) as u64,
            },
        ))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (client, w, n) = row?;
        if let Some(w) = parse_wkey(&w) {
            out.insert((client, w), n);
        }
    }
    Ok(out)
}

/// Every call on every transport (stdio, /mcp, REST) lands in the call log / SSE stream.
impl bdm_app::CallObserver for Store {
    fn on_call(
        &self,
        caller: &bdm_app::Caller,
        op: &str,
        result: &std::result::Result<Value, DomainError>,
        latency: std::time::Duration,
    ) {
        self.log_call(CallRecord::from_result(
            caller.client.as_deref(),
            op,
            result,
            latency,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rusqlite::OptionalExtension;

    fn has_table(c: &Connection, name: &str) -> Result<bool> {
        Ok(c.query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
    }

    fn dims(method: &str, tool: &str, client: Option<&str>) -> Dims {
        Dims {
            method: method.into(),
            tool: Some(tool.into()),
            chain: Some("eip155:1".into()),
            client: client.map(str::to_owned),
        }
    }

    #[tokio::test]
    async fn counters_clients_and_calls_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/bdm.db");
        let now = Utc::now();
        let (month, day) = (WindowKey::month(now), WindowKey::day(now));
        let key = {
            let s = Store::open(&path).unwrap();
            s.add(
                "alchemy",
                &month,
                20,
                &dims("eth_getBalance", "wallet_get_balances", Some("c1")),
            );
            s.add(
                "alchemy",
                &month,
                26,
                &dims("eth_call", "wallet_get_balances", None),
            );
            s.add(
                "alchemy",
                &day,
                46,
                &dims("eth_call", "wallet_get_balances", None),
            );
            assert_eq!(s.total("alchemy", &month), 46);
            let (rec, key) = s.create_client("acme", None).await.unwrap();
            assert!(key.starts_with("bdm_") && key.len() > 40);
            assert_eq!(s.client_by_key(&key).unwrap().id, rec.id);
            s.admit_client("c1", now, Some(10), None).unwrap();
            s.log_call(CallRecord::from_result(
                Some("c1"),
                "wallet_get_balances",
                &Ok(serde_json::json!({"data": {}, "meta": {"chain": "eip155:1", "provider": "quicknode", "source": "fallback", "cached": false}})),
                Duration::from_millis(12),
            ));
            key
        }; // drop flushes queued writes

        let s = Store::open(&path).unwrap();
        assert_eq!(s.total("alchemy", &month), 46);
        assert_eq!(s.total("alchemy", &day), 46);
        let mut rows = s.breakdown("alchemy", &month);
        rows.sort_by_key(|(_, n)| *n);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0.method, "eth_getBalance");
        assert_eq!(rows[0].0.client.as_deref(), Some("c1"));
        let c = s.client_by_key(&key).unwrap();
        assert_eq!(c.name, "acme");
        assert!(s.client_by_key("bdm_wrong").is_none());
        assert_eq!(s.active_client_count(), 1);
        let cu = s.client_counters("c1", &month);
        assert_eq!((cu.requests, cu.credits), (1, 20));
        let calls = s.recent_calls(10).await.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].provider.as_deref(), Some("quicknode"));
        assert!(calls[0].fallback);
        // raw file never contains the key
        drop(s);
        let raw = std::fs::read(&path).unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains(&key));
    }

    #[tokio::test]
    async fn revoke_limits_and_client_quota() {
        let s = Store::open_in_memory().unwrap();
        let (rec, key) = s.create_client("a", None).await.unwrap();
        let limits = ClientLimits {
            daily_requests: Some(2),
            ..Default::default()
        };
        assert!(s
            .set_client_limits(&rec.id, Some(limits.clone()))
            .await
            .unwrap());
        assert_eq!(s.client(&rec.id).unwrap().limits, Some(limits));
        assert!(s.revoke_client(&rec.id).await.unwrap());
        assert!(!s.client_by_key(&key).unwrap().active());
        assert!(!s.revoke_client("nope").await.unwrap());

        let now = Utc.with_ymd_and_hms(2026, 9, 23, 12, 0, 0).unwrap();
        s.admit_client("x", now, Some(2), None).unwrap();
        s.admit_client("x", now, Some(2), None).unwrap();
        assert_eq!(
            s.admit_client("x", now, Some(2), None),
            Err(Exceeded::DailyRequests)
        );
        let tomorrow = now + ChronoDuration::days(1);
        assert!(s.admit_client("x", tomorrow, Some(2), None).is_ok());
        s.add("v", &WindowKey::month(now), 5, &dims("m", "t", Some("y")));
        assert_eq!(
            s.admit_client("y", now, None, Some(5)),
            Err(Exceeded::MonthlyCredits)
        );
    }

    #[tokio::test]
    async fn daily_series_is_zero_filled() {
        let s = Store::open_in_memory().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 9, 23, 12, 0, 0).unwrap();
        s.add("v", &WindowKey::day(now), 7, &dims("m", "t", None));
        s.add(
            "v",
            &WindowKey::day(now - ChronoDuration::days(2)),
            3,
            &dims("m", "t", None),
        );
        let series = s.daily_series("v", 30, now).await.unwrap();
        assert_eq!(series.len(), 30);
        assert_eq!(series[29], ("2026-09-23".to_string(), 7));
        assert_eq!(series[27], ("2026-09-21".to_string(), 3));
        assert_eq!(series[28].1, 0);
    }

    #[test]
    fn migrations_are_idempotent() {
        let mut c = Connection::open_in_memory().unwrap();
        migrate(&mut c).unwrap();
        migrate(&mut c).unwrap();
        assert!(has_table(&c, "usage").unwrap());
    }
}
