//! HTTP transport: REST (`POST /v1/<domain>/<tool>`), OpenAPI, MCP streamable HTTP (`/mcp`),
//! health and metrics on the public router; admin API + dashboard ([`admin_router`]) on a
//! separate router that the server binds to `admin_bind` in hosted mode (localhost otherwise).
//!
//! REST and MCP return byte-identical JSON for the same input (both call [`App::call`]).
//!
//! Call log: if the server layers an `axum::Extension<ems_store::Store>` onto the public router,
//! every REST call is appended to the store's call log (dashboard live stream).

mod admin;

pub use admin::{admin_router, ensure_admin_token, AdminState, Rebuild, ADMIN_BODY_LIMIT};

use axum::{
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use ems_app::{App, Caller, ClientAuth};
use ems_domain::{DomainError, ErrorCode};
use ems_transport_mcp::McpServer;
use serde_json::{json, Map, Value};
use std::{sync::Arc, time::Instant};
use tower_http::{limit::RequestBodyLimitLayer, trace::TraceLayer};

/// Max request body for REST / MCP calls.
pub const BODY_LIMIT: usize = 1024 * 1024;

#[derive(Clone)]
pub struct HttpState {
    pub app: Arc<App>,
    /// `Some` in hosted mode: every public request must carry a valid client key.
    pub auth: Option<Arc<dyn ClientAuth>>,
}

/// Public router: REST, OpenAPI, `/mcp`, `/healthz`, `/metrics`.
pub fn public_router(state: HttpState) -> Router {
    let mcp = McpServer::new(state.app.clone()).http_service();
    Router::new()
        .route("/v1/tools", get(list_tools))
        .route("/v1/tools/{name}", post(call_by_name))
        .route("/v1/{domain}/{tool}", post(call_by_domain))
        .route("/openapi.json", get(openapi))
        .nest_service("/mcp", mcp)
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .route("/healthz", get(|| async { "ok" }))
        .route("/metrics", get(metrics))
        .layer(RequestBodyLimitLayer::new(BODY_LIMIT))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Hosted mode: resolve the bearer token into a `Caller` and attach it for REST and MCP.
async fn authenticate(State(s): State<HttpState>, mut req: Request, next: Next) -> Response {
    let caller = match &s.auth {
        None => Caller::local(),
        Some(auth) => {
            let bearer = req
                .headers()
                .get(http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "));
            match auth.authenticate(bearer).await {
                Ok(c) => c,
                Err(e) => return error_response(e),
            }
        }
    };
    req.extensions_mut().insert(caller);
    next.run(req).await
}

fn caller_of(headers_ext: &http::Extensions) -> Caller {
    headers_ext.get::<Caller>().cloned().unwrap_or_default()
}

async fn list_tools(State(s): State<HttpState>, req: Request) -> Json<Value> {
    let caller = caller_of(req.extensions());
    let tools: Vec<Value> = s
        .app
        .visible(&caller)
        .iter()
        .map(|op| {
            json!({
                "name": op.name(),
                "domain": op.domain(),
                "description": op.description(),
                "readOnly": op.read_only(),
                "legacy": op.legacy(),
                "path": rest_path(op.domain().as_str(), op.name()),
                "inputSchema": Value::Object(op.input_schema()),
            })
        })
        .collect();
    Json(json!({ "tools": tools }))
}

/// `/v1/<domain>/<tool>` where `<tool>` drops the domain prefix: `wallet_get_balances` →
/// `/v1/wallet/get_balances`. Tools without the prefix keep their full name.
fn rest_path(domain: &str, name: &str) -> String {
    let short = name.strip_prefix(&format!("{domain}_")).unwrap_or(name);
    format!("/v1/{domain}/{short}")
}

async fn call_by_domain(
    State(s): State<HttpState>,
    Path((domain, tool)): Path<(String, String)>,
    req: Request,
) -> Response {
    let full = format!("{domain}_{tool}");
    let name = s
        .app
        .catalog()
        .iter()
        .find(|op| op.domain().as_str() == domain && (op.name() == full || op.name() == tool))
        .map(|op| op.name().to_owned());
    match name {
        Some(n) => call(s, n, req).await,
        None => error_response(DomainError::new(
            ErrorCode::InvalidInput,
            format!("unknown tool '/v1/{domain}/{tool}'"),
        )),
    }
}

async fn call_by_name(
    State(s): State<HttpState>,
    Path(name): Path<String>,
    req: Request,
) -> Response {
    call(s, name, req).await
}

async fn call(s: HttpState, name: String, req: Request) -> Response {
    let started = Instant::now();
    let caller = caller_of(req.extensions());
    let call_log = req.extensions().get::<ems_store::Store>().cloned();
    let bytes = match axum::body::to_bytes(req.into_body(), BODY_LIMIT).await {
        Ok(b) => b,
        Err(_) => {
            return error_response(DomainError::invalid("request body too large or unreadable"))
        }
    };
    let input: Value = if bytes.is_empty() {
        Value::Object(Map::new())
    } else {
        match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => {
                return error_response(DomainError::invalid(format!("body is not valid JSON: {e}")))
            }
        }
    };
    let result = s.app.call(&name, input, caller.clone()).await;
    if let Some(store) = call_log {
        store.log_call(ems_store::CallRecord::from_result(
            caller.client.as_deref(),
            &name,
            &result,
            started.elapsed(),
        ));
    }
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => error_response(e),
    }
}

pub fn status_of(code: ErrorCode) -> StatusCode {
    match code {
        ErrorCode::InvalidInput | ErrorCode::UnsupportedChain => StatusCode::BAD_REQUEST,
        ErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
        ErrorCode::NotFound => StatusCode::NOT_FOUND,
        ErrorCode::Conflict => StatusCode::CONFLICT,
        ErrorCode::Unverifiable => StatusCode::UNPROCESSABLE_ENTITY,
        ErrorCode::RateLimited | ErrorCode::QuotaExceeded => StatusCode::TOO_MANY_REQUESTS,
        ErrorCode::UnsupportedCapability => StatusCode::NOT_IMPLEMENTED,
        ErrorCode::AllProvidersFailed => StatusCode::BAD_GATEWAY,
        ErrorCode::StaleData => StatusCode::SERVICE_UNAVAILABLE,
        ErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub fn error_response(e: DomainError) -> Response {
    let mut headers = HeaderMap::new();
    if let Some(secs) = e.retry_after_secs {
        if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
            headers.insert(http::header::RETRY_AFTER, v);
        }
    }
    (status_of(e.code), headers, Json(json!({ "error": e }))).into_response()
}

async fn openapi(State(s): State<HttpState>, req: Request) -> Json<Value> {
    let caller = caller_of(req.extensions());
    let mut paths = Map::new();
    for op in s.app.visible(&caller) {
        let path = rest_path(op.domain().as_str(), op.name());
        paths.insert(
            path,
            json!({ "post": {
                "operationId": op.name(),
                "summary": op.description(),
                "tags": [op.domain()],
                "requestBody": { "required": true, "content": { "application/json": { "schema": Value::Object(op.input_schema()) } } },
                "responses": {
                    "200": { "description": "OK", "content": { "application/json": { "schema": Value::Object(op.output_schema()) } } },
                    "default": { "description": "Error: {\"error\": {code, message, hint?, retry_after_secs?}}" }
                }
            }}),
        );
    }
    Json(json!({
        "openapi": "3.1.0",
        "info": { "title": "Blockchain data aggregator", "version": env!("CARGO_PKG_VERSION") },
        "paths": paths,
        "components": { "securitySchemes": { "bearer": { "type": "http", "scheme": "bearer" } } }
    }))
}

async fn metrics(State(s): State<HttpState>) -> Response {
    let mut out = s.app.metrics().render_prometheus();
    out += "# TYPE ems_vendor_ok_total counter\n# TYPE ems_vendor_failed_total counter\n# TYPE ems_vendor_month_used gauge\n";
    for h in s.app.router().health() {
        out += &format!(
            "ems_vendor_ok_total{{vendor=\"{v}\"}} {}\nems_vendor_failed_total{{vendor=\"{v}\"}} {}\nems_vendor_month_used{{vendor=\"{v}\"}} {}\n",
            h.ok,
            h.failed,
            h.usage.month_used,
            v = h.vendor
        );
    }
    (
        [(http::header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        out,
    )
        .into_response()
}
