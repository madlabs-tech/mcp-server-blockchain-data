#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )
)]
//! MCP transport. `list_tools` / `call_tool` are generated from the Operation catalog, so every
//! tool added in `bdm-app` appears here (and in REST) without transport code.
//!
//! - Non-legacy tools return `structuredContent` (`{data, meta}`) plus a pretty JSON text block,
//!   and failures as `isError` results the model can read and act on.
//! - Legacy aliases return pretty JSON text only and fail with protocol errors, exactly like the
//!   pre-refactor server.
//! - Over streamable HTTP, a hosted-mode auth layer puts a [`Caller`] into the HTTP request
//!   extensions; it is read back from the forwarded `http::request::Parts`.

use bdm_app::{App, Caller};
use bdm_domain::{DomainError, ErrorCode};
use rmcp::{
    model::{
        CallToolRequestParam, CallToolResult, Content, Implementation, ListToolsResult,
        PaginatedRequestParam, ProtocolVersion, ServerCapabilities, ServerInfo, Tool,
        ToolAnnotations,
    },
    service::RequestContext,
    transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    },
    ErrorData as McpError, RoleServer, ServerHandler, ServiceExt,
};
use serde_json::Value;
use std::sync::Arc;

pub const SERVER_NAME: &str = "blockchain-data-mcp";

const INSTRUCTIONS: &str = "Chain- and provider-agnostic blockchain data for payments, stablecoin, \
neobank and trading agents (EVM chains incl. Robinhood Chain, and Solana). Chains accept CAIP-2 ids \
or aliases (ethereum, base, arbitrum, optimism, polygon, avalanche, bsc, robinhood, solana). Amounts \
are exact integer base units with decimals. Every response carries `meta` (provider, fallbacks tried, \
block, finality). Non-custodial: never send private keys; build unsigned transactions and broadcast \
signed ones.";

#[derive(Clone)]
pub struct McpServer {
    app: Arc<App>,
    /// Caller used when the request carries none (stdio / self-hosted).
    default_caller: Caller,
}

impl McpServer {
    pub fn new(app: Arc<App>) -> Self {
        Self {
            app,
            default_caller: Caller::local(),
        }
    }

    fn caller(&self, ctx: &RequestContext<RoleServer>) -> Caller {
        ctx.extensions
            .get::<http::request::Parts>()
            .and_then(|p| p.extensions.get::<Caller>().cloned())
            .unwrap_or_else(|| self.default_caller.clone())
    }

    fn tool(op: &dyn bdm_app::DynOperation) -> Tool {
        Tool {
            name: op.name().into(),
            title: None,
            description: Some(op.description().into()),
            input_schema: Arc::new(compact(op.input_schema())),
            // No output schema on purpose: clients put the whole tool list into the model's
            // context, and output schemas were 86% of it (~250 KB for 34 tools). The result JSON
            // is self-describing and REST still serves them in /openapi.json.
            output_schema: None,
            annotations: Some(ToolAnnotations {
                title: None,
                read_only_hint: Some(op.read_only()),
                destructive_hint: Some(false),
                idempotent_hint: Some(op.read_only()),
                open_world_hint: Some(true),
            }),
            icons: None,
        }
    }

    /// Serve over stdio until the client disconnects.
    pub async fn serve_stdio(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let running = self.serve(rmcp::transport::stdio()).await?;
        running.waiting().await?;
        Ok(())
    }

    /// Tower service for streamable HTTP; mount it at `/mcp`.
    pub fn http_service(self) -> StreamableHttpService<McpServer, LocalSessionManager> {
        StreamableHttpService::new(
            move || Ok(self.clone()),
            LocalSessionManager::default().into(),
            StreamableHttpServerConfig::default(),
        )
    }
}

/// Drop JSON-Schema metadata the model doesn't need (`$schema`, `title`) to keep the tool list small.
fn compact(mut schema: serde_json::Map<String, Value>) -> serde_json::Map<String, Value> {
    schema.remove("$schema");
    schema.remove("title");
    schema
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

fn protocol_error(e: &DomainError) -> McpError {
    match e.code {
        ErrorCode::InvalidInput | ErrorCode::UnsupportedChain => {
            McpError::invalid_params(e.message.clone(), None)
        }
        _ => McpError::internal_error(e.message.clone(), None),
    }
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2025_06_18,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: SERVER_NAME.into(),
                title: Some("Blockchain data aggregator".into()),
                version: env!("CARGO_PKG_VERSION").into(),
                icons: None,
                website_url: None,
            },
            instructions: Some(INSTRUCTIONS.into()),
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let caller = self.caller(&ctx);
        let tools = self
            .app
            .visible(&caller)
            .iter()
            .map(|op| Self::tool(op.as_ref()))
            .collect();
        Ok(ListToolsResult {
            tools,
            next_cursor: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let caller = self.caller(&ctx);
        let legacy = self
            .app
            .catalog()
            .get(&request.name)
            .is_some_and(|op| op.legacy());
        let input = Value::Object(request.arguments.unwrap_or_default());
        match self.app.call(&request.name, input, caller).await {
            Ok(v) if legacy => Ok(CallToolResult::success(vec![Content::text(pretty(&v))])),
            Ok(v) => Ok(CallToolResult {
                content: vec![Content::text(pretty(&v))],
                structured_content: Some(v),
                is_error: Some(false),
                meta: None,
            }),
            Err(e) if legacy || e.message.starts_with("unknown tool") => Err(protocol_error(&e)),
            Err(e) => {
                let body = serde_json::json!({ "error": e });
                Ok(CallToolResult::structured_error(body))
            }
        }
    }
}
