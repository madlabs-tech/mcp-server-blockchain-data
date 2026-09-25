//! Admin API (`/admin/api/*`) and the embedded dashboard (`/dashboard`).
//!
//! Security:
//! - every `/admin/api/*` call needs `Authorization: Bearer <dashboard password>` (401 otherwise) and
//!   the custom header `X-BDM-Admin: 1` (403 otherwise; browsers can't send it cross-site
//!   without a CORS preflight, which we never grant, so it blocks CSRF);
//! - secrets are never returned: key fields are write-only (status only), and config responses
//!   are scrubbed of every known secret value;
//! - request bodies are capped at [`ADMIN_BODY_LIMIT`];
//! - the server binds this router to localhost (`http_bind` self-hosted, `admin_bind` hosted).

use crate::error_response;
use axum::{
    extract::{Path, Query, Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use bdm_app::{App, DynOperation, Profile};
use bdm_config::{
    apply_edits, ClientLimits, ConfigLoader, Edit, Issue, Loaded, OnExhausted, VendorStatus,
};
use bdm_domain::{ChainId, DomainError, ErrorCode};
use bdm_ports::{
    metering::{self, CallContext},
    Capability, EvmRpc, FxRates, PortKind, SolanaRpc,
};
use bdm_routing::{ProviderRegistry, Router as EmsRouter, RoutingTable, VendorHealth, WindowKey};
use bdm_store::{effective_client_limits, ClientRecord, QuotaEngine, Store};
use chrono::Utc;
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{Arc, Mutex},
    time::Duration,
};
use tower_http::limit::RequestBodyLimitLayer;

/// Max admin request body.
pub const ADMIN_BODY_LIMIT: usize = 64 * 1024;

/// Rebuilds the provider registry for a new config (vendor factories live in the server).
pub type Rebuild = Arc<dyn Fn(&Loaded) -> ProviderRegistry + Send + Sync>;

const INDEX_HTML: &str = include_str!("dashboard/index.html");
const APP_JS: &str = include_str!("dashboard/app.js");
const APP_CSS: &str = include_str!("dashboard/app.css");
/// Bundled dashboard fonts (SIL OFL 1.1, see `dashboard/fonts/OFL.txt`). Served by name
/// lookup only: no filesystem access, so no path traversal.
const FONTS: &[(&str, &[u8])] = &[
    (
        "orbitron.woff2",
        include_bytes!("dashboard/fonts/orbitron.woff2"),
    ),
    (
        "jetbrains-mono.woff2",
        include_bytes!("dashboard/fonts/jetbrains-mono.woff2"),
    ),
    (
        "share-tech-mono.woff2",
        include_bytes!("dashboard/fonts/share-tech-mono.woff2"),
    ),
];

#[derive(Clone)]
pub struct AdminState {
    pub app: Arc<App>,
    pub store: Store,
    pub quota: Arc<QuotaEngine>,
    pub loader: Arc<ConfigLoader>,
    pub rebuild: Rebuild,
    token: Arc<str>,
    edit_lock: Arc<Mutex<()>>,
}

impl AdminState {
    pub fn new(
        app: Arc<App>,
        store: Store,
        quota: Arc<QuotaEngine>,
        loader: Arc<ConfigLoader>,
        rebuild: Rebuild,
        token: impl Into<Arc<str>>,
    ) -> Self {
        Self {
            app,
            store,
            quota,
            loader,
            rebuild,
            token: token.into(),
            edit_lock: Arc::default(),
        }
    }

    fn router(&self) -> &Arc<EmsRouter> {
        self.app.router()
    }

    fn install(&self, loaded: Loaded) -> Vec<Issue> {
        let warnings = loaded.warnings.clone();
        let registry = (self.rebuild)(&loaded);
        self.router().swap(RoutingTable {
            config: Arc::new(loaded),
            registry,
        });
        warnings
    }

    /// Validate, write atomically and hot-swap the routing table. Returns the new warnings.
    pub fn apply(&self, edits: &[Edit]) -> Result<Vec<Issue>, Vec<Issue>> {
        let _g = self.edit_lock.lock().unwrap_or_else(|e| e.into_inner());
        let current = self.router().table().config.clone();
        let next = apply_edits(&self.loader, &current, edits)?;
        Ok(self.install(next))
    }

    /// Validate edits without writing (applied to a throwaway copy of the config dir).
    pub fn validate(&self, edits: &[Edit]) -> Result<Vec<Issue>, Vec<Issue>> {
        let current = self.router().table().config.clone();
        let tmp = std::env::temp_dir().join(format!("bdm-validate-{}", random_hex(8)));
        let io = |e: std::io::Error| vec![Issue::error("validate", e.to_string())];
        std::fs::create_dir_all(&tmp).map_err(io)?;
        let result = (|| {
            for p in [
                self.loader.dir.config_path(),
                self.loader.dir.secrets_path(),
            ] {
                if let Some(name) = p.file_name().filter(|_| p.exists()) {
                    std::fs::copy(&p, tmp.join(name)).map_err(io)?;
                }
            }
            let loader =
                ConfigLoader::new(bdm_config::ConfigDir::new(&tmp), self.loader.env.clone())?;
            apply_edits(&loader, &current, edits).map(|l| l.warnings)
        })();
        let _ = std::fs::remove_dir_all(&tmp);
        result
    }

    /// Re-read the config files (dashboard "reload" and SIGHUP).
    pub fn reload(&self) -> Result<Vec<Issue>, Vec<Issue>> {
        let _g = self.edit_lock.lock().unwrap_or_else(|e| e.into_inner());
        let next = self.loader.load()?;
        Ok(self.install(next))
    }
}

pub(crate) fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

/// Dashboard + admin API router.
pub fn admin_router(state: AdminState) -> Router {
    let api = Router::new()
        .route("/admin/api/health", get(health))
        .route("/admin/api/connect", get(connect))
        .route("/admin/api/config", get(config).put(config_apply))
        .route("/admin/api/config/validate", post(config_validate))
        .route("/admin/api/reload", post(reload))
        .route("/admin/api/vendors/{id}/test", post(vendor_test))
        .route("/admin/api/vendors/{id}/budget", post(vendor_budget))
        .route("/admin/api/quota", get(quota))
        .route("/admin/api/quota.csv", get(quota_csv))
        .route("/admin/api/quota/refresh", post(quota_refresh))
        .route("/admin/api/clients", get(clients).post(client_create))
        .route(
            "/admin/api/clients/{id}",
            axum::routing::patch(client_update).delete(client_revoke),
        )
        .route("/admin/api/calls", get(calls))
        .route("/admin/api/calls/stream", get(calls_stream))
        .layer(middleware::from_fn_with_state(state.clone(), admin_auth));
    Router::new()
        .merge(api)
        .route(
            "/dashboard",
            get(|| async { asset("text/html; charset=utf-8", INDEX_HTML) }),
        )
        .route(
            "/dashboard/",
            get(|| async { asset("text/html; charset=utf-8", INDEX_HTML) }),
        )
        .route(
            "/dashboard/app.js",
            get(|| async { asset("text/javascript; charset=utf-8", APP_JS) }),
        )
        .route(
            "/dashboard/app.css",
            get(|| async { asset("text/css; charset=utf-8", APP_CSS) }),
        )
        .route("/dashboard/fonts/{name}", get(font))
        .layer(RequestBodyLimitLayer::new(ADMIN_BODY_LIMIT))
        .layer(crate::catch_panic_layer())
        .with_state(state)
}

async fn font(Path(name): Path<String>) -> Response {
    match FONTS.iter().find(|(n, _)| *n == name) {
        Some((_, bytes)) => asset("font/woff2", *bytes),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn asset(content_type: &'static str, body: impl Into<axum::body::Body>) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body.into(),
    )
        .into_response()
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn admin_auth(State(s): State<AdminState>, req: Request, next: Next) -> Response {
    let h = req.headers();
    let bearer = h
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !ct_eq(bearer.trim().as_bytes(), s.token.as_bytes()) {
        return error_response(DomainError::new(
            ErrorCode::Unauthorized,
            "missing or invalid dashboard password (run `onchain-data-mcp password` to see it)",
        ));
    }
    if h.get("x-bdm-admin").and_then(|v| v.to_str().ok()) != Some("1") {
        let e = DomainError::new(
            ErrorCode::Unauthorized,
            "missing X-BDM-Admin: 1 header (CSRF protection)",
        );
        return (StatusCode::FORBIDDEN, Json(json!({ "error": e }))).into_response();
    }
    let mut resp = next.run(req).await;
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

/// JSON response with every known secret value scrubbed (defense in depth).
fn scrubbed(cfg: &Loaded, v: Value) -> Response {
    let body = cfg.scrub(&v.to_string());
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

fn issues_response(status: StatusCode, errors: Vec<Issue>) -> Response {
    (status, Json(json!({ "ok": false, "errors": errors }))).into_response()
}

fn bad_request(msg: impl Into<String>) -> Response {
    error_response(DomainError::invalid(msg))
}

// ------------------------------------------------------------------ health

async fn health(State(s): State<AdminState>) -> Response {
    let cfg = s.router().table().config.clone();
    scrubbed(
        &cfg,
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "mode": cfg.settings.server.mode,
            "tools": s.app.catalog().len(),
            "clients": s.store.active_client_count(),
            "vendors": s.router().health(),
            "ops": s.app.metrics().snapshot(),
        }),
    )
}

// ------------------------------------------------------------------ connect

/// `host:port` bind → base URL a client can use (wildcard hosts become loopback).
fn bind_url(bind: &str) -> String {
    let (host, port) = bind.rsplit_once(':').unwrap_or((bind, ""));
    let host = match host.trim_matches(|c| c == '[' || c == ']') {
        "0.0.0.0" | "::" | "" => "127.0.0.1".to_owned(),
        h if h.contains(':') => format!("[{h}]"),
        h => h.to_owned(),
    };
    if port.is_empty() {
        format!("http://{host}")
    } else {
        format!("http://{host}:{port}")
    }
}

/// Base URL of the dashboard and admin API: `admin_bind` in hosted mode, `http_bind` otherwise.
pub fn dashboard_base(srv: &bdm_config::ServerSettings) -> String {
    if srv.mode == bdm_config::Mode::Hosted {
        bind_url(srv.admin_bind.as_deref().unwrap_or("127.0.0.1:8788"))
    } else {
        bind_url(&srv.http_bind)
    }
}

/// Facts the dashboard needs to generate MCP/REST client snippets. Secret-free by construction.
async fn connect(State(s): State<AdminState>) -> Response {
    let cfg = s.router().table().config.clone();
    let srv = &cfg.settings.server;
    let hosted = srv.mode == bdm_config::Mode::Hosted;
    let http_url = dashboard_base(srv);
    let public_url = hosted.then(|| bind_url(srv.public_bind.as_deref().unwrap_or_default()));
    let mcp_url = format!("{}/mcp", public_url.as_deref().unwrap_or(&http_url));
    let binary_path = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "onchain-data-mcp".into());
    let root = &s.loader.dir.root;
    let config_dir = std::fs::canonicalize(root)
        .unwrap_or_else(|_| root.clone())
        .display()
        .to_string();
    let sample_tool = s
        .app
        .visible(&bdm_app::Caller::local())
        .first()
        .map(|o| o.name().to_owned());
    Json(json!({
        "mode": srv.mode,
        "http_url": http_url,
        "public_url": public_url,
        "mcp_url": mcp_url,
        "binary_path": binary_path,
        "config_dir": config_dir,
        "tool_profile": srv.tool_profile,
        "sample_tool": sample_tool,
    }))
    .into_response()
}

// ------------------------------------------------------------------ config

fn lock_of(cfg: &Loaded, path: &[&str]) -> Option<String> {
    let path: Vec<String> = path.iter().map(|s| s.to_string()).collect();
    cfg.locked_by(&path).map(str::to_owned)
}

fn order_view(
    cfg: &Loaded,
    registry: &ProviderRegistry,
    health: &HashMap<String, VendorHealth>,
    cap: Capability,
    chain: Option<&ChainId>,
    effective: bool,
) -> Value {
    let r = cfg.order(cap, chain, None);
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    if effective {
        for v in &r.vendors {
            let reason = match cfg.vendor_status(v) {
                VendorStatus::Active => None,
                VendorStatus::MissingKey { env_vars } => {
                    Some(format!("missing key ({})", env_vars.join(", ")))
                }
                VendorStatus::Disabled { unverified: true } => {
                    Some("disabled (free tier unverified)".into())
                }
                VendorStatus::Disabled { .. } => Some("disabled".into()),
                VendorStatus::Unknown => Some("unknown vendor".into()),
            }
            .or_else(|| {
                registry
                    .get(cap, chain, v)
                    .is_none()
                    .then(|| "not available for this chain/capability".to_owned())
            })
            .or_else(|| {
                let h = health.get(v)?;
                if h.exhausted_until.is_some() {
                    return Some("quota exhausted".into());
                }
                let overage = cfg
                    .effective_budget(v)
                    .is_some_and(|b| b.on_exhausted == OnExhausted::AllowOverage);
                let u = &h.usage;
                let over = |used: u64, b: Option<u64>| b.is_some_and(|b| used >= b);
                if !overage
                    && (over(u.day_used, u.day_budget) || over(u.month_used, u.month_budget))
                {
                    return Some("quota reserve reached".into());
                }
                (h.breaker == bdm_routing::BreakerState::Open).then(|| "breaker open".into())
            });
            if let Some(why) = &reason {
                if matches!(cfg.vendor_status(v), VendorStatus::Unknown)
                    || why.starts_with("not available")
                {
                    warnings.push(format!("{v}: {why}"));
                }
            }
            rows.push(json!({ "vendor": v, "usable": reason.is_none(), "reason": reason }));
        }
        if !r.vendors.is_empty() && rows.iter().all(|r| r["usable"] == false) {
            warnings.push("no usable vendor: calls needing this capability will fail".into());
        }
    }
    json!({
        "vendors": r.vendors,
        "level": r.level,
        "effective": effective.then_some(rows),
        "registered": registry.registered_for(cap, chain),
        "warnings": warnings,
    })
}

fn op_view(op: &Arc<dyn DynOperation>, cfg: &Loaded, visible: bool) -> Value {
    let os = cfg.operation(op.name());
    json!({
        "name": op.name(),
        "domain": op.domain(),
        "description": op.description(),
        "profiles": op.profiles(),
        "read_only": op.read_only(),
        "legacy": op.legacy(),
        "visible": visible,
        "enabled": os.enabled.unwrap_or(true),
        "strategy": os.strategy,
        "cache_ttl_secs": os.cache_ttl_secs,
        "default_cache_ttl_secs": op.cache_ttl().map(|d| d.as_secs()),
        "locked_by": lock_of(cfg, &["operations", op.name()]),
    })
}

async fn config(State(s): State<AdminState>) -> Response {
    let table = s.router().table();
    let cfg = &table.config;
    let health: HashMap<String, VendorHealth> = s
        .router()
        .health()
        .into_iter()
        .map(|h| (h.vendor.clone(), h))
        .collect();
    let reporters: Vec<String> = table
        .registry
        .quota_reporters()
        .map(|(v, _)| v.clone())
        .collect();

    let mut settings = serde_json::to_value(&cfg.settings).unwrap_or_default();
    if let Some(m) = settings.as_object_mut() {
        m.remove("keys"); // write-only: key status is reported per vendor instead
    }
    let locked: Vec<Value> = cfg
        .locked
        .iter()
        .map(|(p, env)| json!({ "path": p.join("."), "env": env }))
        .collect();

    let vendors: Vec<Value> = cfg
        .registry
        .vendors
        .iter()
        .map(|(id, e)| {
            let keys: Vec<Value> = e
                .keys
                .iter()
                .map(|(field, env)| {
                    json!({
                        "field": field,
                        "env": env,
                        "set": cfg.key(id, field).is_some(),
                        "locked_by": lock_of(cfg, &["keys", id, field]),
                    })
                })
                .collect();
            json!({
                "id": id,
                "display_name": e.display_name,
                "requires_key": e.requires_key,
                "signup_url": e.signup_url,
                "note": e.note,
                "free_tier_verified": e.free_tier_verified,
                "tier": e.tier,
                "unit": e.unit,
                "status": cfg.vendor_status(id),
                "enabled": cfg.settings.vendors.get(id).and_then(|v| v.enabled).or(e.enabled).unwrap_or(e.free_tier_verified),
                "enabled_locked_by": lock_of(cfg, &["vendors", id, "enabled"]),
                "keys": keys,
                "registered": table.registry.vendor(id).is_some(),
                "quota_reporter": reporters.contains(id),
                "budget": cfg.effective_budget(id),
            })
        })
        .collect();

    let chains: Vec<&bdm_config::ChainEntry> = cfg.registry.chains.all().iter().collect();
    let mut orders = serde_json::Map::new();
    for cap in Capability::ALL {
        let mut per_chain = serde_json::Map::new();
        for c in &chains {
            let fits = match cap {
                Capability::EvmRpc => c.family == bdm_domain::ChainFamily::Evm,
                Capability::SolanaRpc => c.family == bdm_domain::ChainFamily::Solana,
                _ => true,
            };
            if fits && c.enabled {
                let mut view = order_view(cfg, &table.registry, &health, *cap, Some(&c.id), true);
                let id = c.id.to_string();
                let locked_by = std::iter::once(&id)
                    .chain(&c.aliases)
                    .find_map(|k| lock_of(cfg, &["routing", "chains", k, cap.as_str()]));
                if let Some(m) = view.as_object_mut() {
                    m.insert("locked_by".into(), json!(locked_by));
                }
                per_chain.insert(id, view);
            }
        }
        orders.insert(
            cap.to_string(),
            json!({
                "default": order_view(cfg, &table.registry, &health, *cap, None, !cap.is_chain_bound()),
                "default_locked_by": lock_of(cfg, &["routing", "defaults", cap.as_str()]),
                "chains": per_chain,
            }),
        );
    }

    let visible: Vec<String> = s
        .app
        .visible(&bdm_app::Caller::local())
        .iter()
        .map(|o| o.name().to_owned())
        .collect();
    let operations: Vec<Value> = s
        .app
        .catalog()
        .iter()
        .map(|op| op_view(op, cfg, visible.iter().any(|v| v == op.name())))
        .collect();

    let chain_rows: Vec<Value> = chains
        .iter()
        .map(|c| {
            let alias = c.aliases.first().cloned().unwrap_or_else(|| c.id.to_string());
            json!({
                "id": c.id,
                "name": c.name,
                "aliases": c.aliases,
                "family": c.family,
                "enabled": c.enabled,
                "finality": c.finality,
                "public_rpc": c.public_rpc,
                "explorer": c.explorer,
                "override_key": alias,
                "locked_by": lock_of(cfg, &["chain_overrides", &alias]).or_else(|| lock_of(cfg, &["chain_overrides", &c.id.to_string()])),
            })
        })
        .collect();

    scrubbed(
        cfg,
        json!({
            "mode": cfg.settings.server.mode,
            "settings": settings,
            "locked": locked,
            "warnings": cfg.warnings,
            "vendors": vendors,
            "capabilities": Capability::ALL,
            "orders": orders,
            "chains": chain_rows,
            "operations": operations,
            "profiles": Profile::ALL,
            "custom_rpc": cfg.settings.custom_rpc.iter().map(|(k, v)| json!({"name": k, "chain": v.chain})).collect::<Vec<_>>(),
        }),
    )
}

#[derive(Deserialize)]
struct EditIn {
    path: Vec<String>,
    #[serde(default)]
    value: Option<Value>,
}

#[derive(Deserialize)]
struct EditsIn {
    edits: Vec<EditIn>,
}

fn to_edits(body: EditsIn) -> Result<Vec<Edit>, &'static str> {
    if body.edits.is_empty() || body.edits.len() > 200 {
        return Err("send 1..=200 edits");
    }
    body.edits
        .into_iter()
        .map(|e| {
            if e.path.is_empty()
                || e.path.len() > 8
                || e.path.iter().any(|p| p.is_empty() || p.len() > 128)
            {
                return Err("invalid edit path");
            }
            Ok(Edit {
                path: e.path,
                value: e.value,
            })
        })
        .collect()
}

async fn config_validate(State(s): State<AdminState>, Json(body): Json<EditsIn>) -> Response {
    let edits = match to_edits(body) {
        Ok(e) => e,
        Err(m) => return bad_request(m),
    };
    let st = s.clone();
    match tokio::task::spawn_blocking(move || st.validate(&edits)).await {
        Ok(Ok(warnings)) => Json(json!({ "ok": true, "warnings": warnings })).into_response(),
        Ok(Err(errors)) => issues_response(StatusCode::UNPROCESSABLE_ENTITY, errors),
        Err(e) => error_response(DomainError::internal(e.to_string())),
    }
}

async fn apply(s: AdminState, edits: Vec<Edit>) -> Response {
    let st = s.clone();
    match tokio::task::spawn_blocking(move || st.apply(&edits)).await {
        Ok(Ok(warnings)) => {
            tracing::info!("config updated from the admin API; routing table swapped");
            Json(json!({ "ok": true, "warnings": warnings })).into_response()
        }
        Ok(Err(errors)) => issues_response(StatusCode::UNPROCESSABLE_ENTITY, errors),
        Err(e) => error_response(DomainError::internal(e.to_string())),
    }
}

async fn config_apply(State(s): State<AdminState>, Json(body): Json<EditsIn>) -> Response {
    match to_edits(body) {
        Ok(edits) => apply(s, edits).await,
        Err(m) => bad_request(m),
    }
}

async fn reload(State(s): State<AdminState>) -> Response {
    let st = s.clone();
    match tokio::task::spawn_blocking(move || st.reload()).await {
        Ok(Ok(warnings)) => Json(json!({ "ok": true, "warnings": warnings })).into_response(),
        Ok(Err(errors)) => issues_response(StatusCode::UNPROCESSABLE_ENTITY, errors),
        Err(e) => error_response(DomainError::internal(e.to_string())),
    }
}

#[derive(Deserialize)]
struct BudgetIn {
    which: String,
    window: String,
    value: Option<u64>,
}

/// Set or clear one `limit` / `cap` window. Alias keys (`monthly_credits`, …) are removed first
/// so the canonical key never collides with them.
async fn vendor_budget(
    State(s): State<AdminState>,
    Path(id): Path<String>,
    Json(b): Json<BudgetIn>,
) -> Response {
    if !s.router().table().config.registry.vendors.contains_key(&id) {
        return bad_request(format!("unknown vendor '{id}'"));
    }
    if !matches!(b.which.as_str(), "limit" | "cap") {
        return bad_request("which must be 'limit' or 'cap'");
    }
    let aliases: &[&str] = match b.window.as_str() {
        "rps" | "per_minute" => &[],
        "daily" => &["daily_credits", "daily_requests"],
        "monthly" => &["monthly_credits", "monthly_requests"],
        _ => return bad_request("window must be rps, per_minute, daily or monthly"),
    };
    let path = |w: &str| {
        vec![
            "vendors".to_owned(),
            id.clone(),
            b.which.clone(),
            w.to_owned(),
        ]
    };
    let mut edits: Vec<Edit> = aliases
        .iter()
        .map(|a| Edit {
            path: path(a),
            value: None,
        })
        .collect();
    edits.push(Edit {
        path: path(&b.window),
        value: b.value.map(Value::from),
    });
    apply(s, edits).await
}

// ------------------------------------------------------------------ vendor test

async fn vendor_test(State(s): State<AdminState>, Path(id): Path<String>) -> Response {
    let table = s.router().table();
    let cfg = table.config.clone();
    let status = cfg.vendor_status(&id);
    if status == VendorStatus::Unknown {
        return bad_request(format!("unknown vendor '{id}'"));
    }
    if status != VendorStatus::Active {
        return Json(json!({ "vendor": id, "ok": false, "status": status, "message": "vendor is not active" }))
            .into_response();
    }
    let registry = &table.registry;
    let reporter = registry
        .quota_reporters()
        .find(|(v, _)| *v == &id)
        .map(|(_, r)| r.clone());
    let evm = cfg.registry.chains.enabled().find_map(|c| {
        registry
            .get(Capability::EvmRpc, Some(&c.id), &id)
            .and_then(<dyn EvmRpc>::extract)
            .map(|p| (c.id.clone(), p))
    });
    let sol = cfg.registry.chains.enabled().find_map(|c| {
        registry
            .get(Capability::SolanaRpc, Some(&c.id), &id)
            .and_then(<dyn SolanaRpc>::extract)
            .map(|p| (c.id.clone(), p))
    });
    let fx = registry
        .get(Capability::Fx, None, &id)
        .and_then(<dyn FxRates>::extract);

    let ctx = CallContext {
        tool: Some("admin_vendor_test".into()),
        client: None,
        chain: None,
        sink: s.router().usage_sink(),
    };
    let started = std::time::Instant::now();
    let fut = async {
        if let Some(r) = reporter {
            return ("usage endpoint", r.usage().await.map(|u| json!(u)));
        }
        if let Some((chain, p)) = evm {
            return (
                "eth_blockNumber",
                p.request("eth_blockNumber", json!([]))
                    .await
                    .map(|v| json!({"chain": chain, "result": v})),
            );
        }
        if let Some((chain, p)) = sol {
            return (
                "getSlot",
                p.request("getSlot", json!([]))
                    .await
                    .map(|v| json!({"chain": chain, "result": v})),
            );
        }
        if let Some(p) = fx {
            return (
                "fx USD/EUR",
                p.rate("USD", "EUR", None).await.map(|r| json!(r)),
            );
        }
        (
            "none",
            Err(bdm_ports::ProviderError::Unsupported(
                "no cheap test for this vendor; real calls exercise it".into(),
            )),
        )
    };
    let (method, result) =
        match tokio::time::timeout(Duration::from_secs(15), metering::scope(ctx, fut)).await {
            Ok(r) => r,
            Err(_) => (
                "timeout",
                Err(bdm_ports::ProviderError::Transient("timed out".into())),
            ),
        };
    let latency_ms = started.elapsed().as_millis() as u64;
    let body = match result {
        Ok(detail) => {
            json!({ "vendor": id, "ok": true, "method": method, "latency_ms": latency_ms, "detail": detail })
        }
        Err(e) => {
            json!({ "vendor": id, "ok": false, "method": method, "latency_ms": latency_ms, "message": e.to_string() })
        }
    };
    scrubbed(&cfg, body)
}

// ------------------------------------------------------------------ quota

async fn quota(State(s): State<AdminState>) -> Response {
    let cfg = s.router().table().config.clone();
    let report = s.quota.report(Utc::now(), true).await;
    scrubbed(
        &cfg,
        json!({ "generated_at": Utc::now(), "vendors": report }),
    )
}

async fn quota_csv(State(s): State<AdminState>) -> Response {
    let report = s.quota.report(Utc::now(), false).await;
    (
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"quota.csv\"",
            ),
        ],
        QuotaEngine::to_csv(&report),
    )
        .into_response()
}

async fn quota_refresh(State(s): State<AdminState>) -> Response {
    s.quota.poll().await;
    quota(State(s)).await
}

// ------------------------------------------------------------------ clients

fn client_view(s: &AdminState, cfg: &Loaded, c: &ClientRecord, top: Vec<(String, u64)>) -> Value {
    let now = Utc::now();
    json!({
        "id": c.id,
        "name": c.name,
        "created_at": c.created_at,
        "revoked_at": c.revoked_at,
        "active": c.active(),
        "limits": c.limits,
        "effective_limits": effective_client_limits(cfg, c),
        "today": s.store.client_counters(&c.id, &WindowKey::day(now)),
        "month": s.store.client_counters(&c.id, &WindowKey::month(now)),
        "top_tools": top,
    })
}

async fn clients(State(s): State<AdminState>) -> Response {
    let cfg = s.router().table().config.clone();
    let month = WindowKey::month(Utc::now());
    let mut rows = Vec::new();
    for c in s.store.clients() {
        let top = s
            .store
            .client_top_tools(&c.id, &month)
            .await
            .unwrap_or_default();
        rows.push(client_view(&s, &cfg, &c, top));
    }
    Json(json!({
        "mode": cfg.settings.server.mode,
        "defaults": cfg.settings.clients.default,
        "defaults_locked_by": lock_of(&cfg, &["clients", "default"]),
        "clients": rows,
    }))
    .into_response()
}

#[derive(Deserialize)]
struct ClientIn {
    name: String,
    #[serde(default)]
    limits: Option<ClientLimits>,
}

async fn client_create(State(s): State<AdminState>, Json(body): Json<ClientIn>) -> Response {
    match s.store.create_client(&body.name, body.limits).await {
        Ok((rec, key)) => {
            tracing::info!(client = %rec.id, "client key created from the admin API");
            let cfg = s.router().table().config.clone();
            let view = client_view(&s, &cfg, &rec, vec![]);
            // The only response that ever carries a client key; it is not stored.
            (
                StatusCode::CREATED,
                Json(json!({ "client": view, "key": key })),
            )
                .into_response()
        }
        Err(e) => bad_request(e.0),
    }
}

#[derive(Deserialize)]
struct ClientPatch {
    limits: Option<ClientLimits>,
}

async fn client_update(
    State(s): State<AdminState>,
    Path(id): Path<String>,
    Json(body): Json<ClientPatch>,
) -> Response {
    match s.store.set_client_limits(&id, body.limits).await {
        Ok(true) => match s.store.client(&id) {
            Some(rec) => {
                let cfg = s.router().table().config.clone();
                Json(json!({ "client": client_view(&s, &cfg, &rec, vec![]) })).into_response()
            }
            None => error_response(DomainError::new(
                ErrorCode::NotFound,
                format!("unknown client '{id}'"),
            )),
        },
        Ok(false) => error_response(DomainError::new(
            ErrorCode::NotFound,
            format!("unknown client '{id}'"),
        )),
        Err(e) => error_response(DomainError::internal(e.0)),
    }
}

async fn client_revoke(State(s): State<AdminState>, Path(id): Path<String>) -> Response {
    match s.store.revoke_client(&id).await {
        Ok(true) => Json(json!({ "ok": true })).into_response(),
        Ok(false) => error_response(DomainError::new(
            ErrorCode::NotFound,
            format!("unknown client '{id}'"),
        )),
        Err(e) => error_response(DomainError::internal(e.0)),
    }
}

// ------------------------------------------------------------------ call log

#[derive(Deserialize)]
struct CallsQuery {
    limit: Option<usize>,
}

async fn calls(State(s): State<AdminState>, Query(q): Query<CallsQuery>) -> Response {
    match s
        .store
        .recent_calls(q.limit.unwrap_or(100).min(1_000))
        .await
    {
        Ok(rows) => Json(json!({ "calls": rows })).into_response(),
        Err(e) => error_response(DomainError::internal(e.0)),
    }
}

async fn calls_stream(State(s): State<AdminState>) -> Response {
    let rx = s.store.subscribe_calls();
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(rec) => {
                    let ev = Event::default()
                        .event("call")
                        .json_data(&rec)
                        .unwrap_or_default();
                    return Some((Ok::<_, Infallible>(ev), rx));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}
