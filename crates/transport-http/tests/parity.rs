#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
//! Transport tests (T0.10): REST vs MCP (streamable HTTP) parity, hosted-mode auth, OpenAPI,
//! error mapping.

use async_trait::async_trait;
use bdm_app::{
    App, Caller, Catalog, ClientAuth, Ctx, Domain, OpOutput, Operation, Profile, ProfileSelection,
};
use bdm_config::{ConfigDir, ConfigLoader, EnvSource};
use bdm_domain::{DomainError, ErrorCode};
use bdm_routing::{InMemoryCounterStore, ProviderRegistry, Router, RouterOptions, RoutingTable};
use bdm_transport_http::{public_router, HttpState};
use rmcp::{
    model::CallToolRequestParam,
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
    },
    ServiceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Deserialize, JsonSchema)]
struct EchoIn {
    msg: String,
}

#[derive(Serialize, JsonSchema)]
struct EchoOut {
    echoed: String,
}

struct Echo;

#[async_trait]
impl Operation for Echo {
    type Input = EchoIn;
    type Output = EchoOut;
    const NAME: &'static str = "chain_echo";
    const DOMAIN: Domain = Domain::Chain;
    const DESCRIPTION: &'static str = "Echo a message.";
    const PROFILES: &'static [Profile] = &[Profile::Payments];

    async fn execute(&self, _ctx: &Ctx, input: EchoIn) -> Result<OpOutput<EchoOut>, DomainError> {
        if input.msg == "fail" {
            return Err(
                DomainError::new(ErrorCode::AllProvidersFailed, "boom").with_hint("try later")
            );
        }
        Ok(OpOutput::local(EchoOut { echoed: input.msg }))
    }
}

struct Boom;

#[async_trait]
impl Operation for Boom {
    type Input = EchoIn;
    type Output = EchoOut;
    const NAME: &'static str = "chain_boom";
    const DOMAIN: Domain = Domain::Chain;
    const DESCRIPTION: &'static str = "Panics.";
    const PROFILES: &'static [Profile] = &[Profile::Payments];

    async fn execute(&self, _ctx: &Ctx, _input: EchoIn) -> Result<OpOutput<EchoOut>, DomainError> {
        panic!("deliberate test panic")
    }
}

struct StaticAuth;

#[async_trait]
impl ClientAuth for StaticAuth {
    async fn authenticate(&self, bearer: Option<&str>) -> Result<Caller, DomainError> {
        match bearer {
            Some("good-key") => Ok(Caller {
                client: Some("c1".into()),
                profile: Some(ProfileSelection::Profile(Profile::Payments)),
            }),
            _ => Err(DomainError::new(
                ErrorCode::Unauthorized,
                "missing or invalid client key",
            )),
        }
    }
}

async fn serve(auth: Option<Arc<dyn ClientAuth>>) -> String {
    let loader = ConfigLoader::new(ConfigDir::new("/nonexistent"), EnvSource::default()).unwrap();
    let config = Arc::new(loader.load_texts("", "").unwrap());
    let router = Router::new(
        RoutingTable {
            config,
            registry: ProviderRegistry::new(vec![]),
        },
        Arc::new(InMemoryCounterStore::default()),
        RouterOptions::default(),
    );
    let mut catalog = Catalog::new();
    bdm_app::ops::register_all(&mut catalog);
    catalog.register(Echo);
    catalog.register(Boom);
    let state = HttpState {
        app: Arc::new(App::new(catalog, router, 100)),
        auth,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, public_router(state)).await });
    url
}

async fn mcp_call(
    url: &str,
    auth: Option<&str>,
    name: &'static str,
    args: Value,
) -> rmcp::model::CallToolResult {
    let mut cfg = StreamableHttpClientTransportConfig::with_uri(format!("{url}/mcp"));
    cfg.auth_header = auth.map(str::to_owned);
    let transport = StreamableHttpClientTransport::from_config(cfg);
    let client = ().serve(transport).await.unwrap();
    let r = client
        .call_tool(CallToolRequestParam {
            name: name.into(),
            arguments: args.as_object().cloned(),
        })
        .await
        .unwrap();
    client.cancel().await.unwrap();
    r
}

#[tokio::test]
async fn rest_and_mcp_return_the_same_json() {
    let url = serve(None).await;
    let http = reqwest::Client::new();
    let rest: Value = http
        .post(format!("{url}/v1/chain/echo"))
        .json(&json!({"msg": "hi"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let by_name: Value = http
        .post(format!("{url}/v1/tools/chain_echo"))
        .json(&json!({"msg": "hi"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mcp = mcp_call(&url, None, "chain_echo", json!({"msg": "hi"})).await;
    let mcp = mcp.structured_content.expect("structured content");
    assert_eq!(rest["data"], json!({"echoed": "hi"}));
    assert_eq!(rest["data"], mcp["data"]);
    assert_eq!(rest["data"], by_name["data"]);
    let keys = |v: &Value| {
        v["meta"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(keys(&rest), keys(&mcp));
    assert_eq!(rest["meta"]["provider"], mcp["meta"]["provider"]);
}

#[tokio::test]
async fn errors_map_to_http_status_and_mcp_is_error() {
    let url = serve(None).await;
    let http = reqwest::Client::new();
    let r = http
        .post(format!("{url}/v1/chain/echo"))
        .json(&json!({"msg": "fail"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["error"]["code"], json!("ALL_PROVIDERS_FAILED"));
    assert_eq!(body["error"]["hint"], json!("try later"));
    assert_eq!(
        http.post(format!("{url}/v1/chain/nope"))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        http.post(format!("{url}/v1/chain/echo"))
            .body("{not json")
            .send()
            .await
            .unwrap()
            .status(),
        400
    );

    let mcp = mcp_call(&url, None, "chain_echo", json!({"msg": "fail"})).await;
    assert_eq!(mcp.is_error, Some(true));
    assert_eq!(
        mcp.structured_content.unwrap()["error"]["code"],
        json!("ALL_PROVIDERS_FAILED")
    );
}

#[tokio::test]
async fn tool_panics_are_internal_errors_on_rest_and_mcp() {
    let url = serve(None).await;
    let r = reqwest::Client::new()
        .post(format!("{url}/v1/chain/boom"))
        .json(&json!({"msg": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 500);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["error"]["code"], json!("INTERNAL"));

    // MCP: caught at the App layer; the client gets `isError` and the session stays usable.
    let cfg = StreamableHttpClientTransportConfig::with_uri(format!("{url}/mcp"));
    let client = ().serve(StreamableHttpClientTransport::from_config(cfg)).await.unwrap();
    let param = |name: &'static str, args: Value| CallToolRequestParam {
        name: name.into(),
        arguments: args.as_object().cloned(),
    };
    let r = client
        .call_tool(param("chain_boom", json!({"msg": "x"})))
        .await
        .unwrap();
    assert_eq!(r.is_error, Some(true));
    assert_eq!(
        r.structured_content.unwrap()["error"]["code"],
        json!("INTERNAL")
    );
    let r = client
        .call_tool(param("chain_echo", json!({"msg": "still alive"})))
        .await
        .unwrap();
    assert_eq!(r.is_error, Some(false));
    client.cancel().await.unwrap();
}

async fn boom_handler() -> &'static str {
    panic!("deliberate handler panic")
}

#[tokio::test]
async fn http_layer_turns_handler_panics_into_internal_json() {
    let app = axum::Router::new()
        .route("/boom", axum::routing::get(boom_handler))
        .layer(bdm_transport_http::catch_panic_layer());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    let r = reqwest::get(format!("{url}/boom")).await.unwrap();
    assert_eq!(r.status(), 500);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["error"]["code"], json!("INTERNAL"));
    assert_eq!(
        body["error"]["message"],
        json!("internal error; see server log")
    );
}

#[tokio::test]
async fn hosted_mode_requires_client_key() {
    let url = serve(Some(Arc::new(StaticAuth))).await;
    let http = reqwest::Client::new();
    let r = http
        .post(format!("{url}/v1/chain/echo"))
        .json(&json!({"msg": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let r = http
        .post(format!("{url}/v1/chain/echo"))
        .bearer_auth("wrong")
        .json(&json!({"msg": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let r = http
        .post(format!("{url}/v1/chain/echo"))
        .bearer_auth("good-key")
        .json(&json!({"msg": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(
        http.get(format!("{url}/healthz"))
            .send()
            .await
            .unwrap()
            .status(),
        200,
        "health stays public"
    );

    let mcp = mcp_call(&url, Some("good-key"), "chain_echo", json!({"msg": "x"})).await;
    assert_eq!(
        mcp.structured_content.unwrap()["data"]["echoed"],
        json!("x")
    );
}

#[tokio::test]
async fn openapi_and_tool_list() {
    let url = serve(None).await;
    let http = reqwest::Client::new();
    let spec: Value = http
        .get(format!("{url}/openapi.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(spec["openapi"], json!("3.1.0"));
    assert!(spec["paths"]["/v1/chain/echo"]["post"].is_object());
    assert!(spec["paths"]["/v1/legacy/eth_get_balance"]["post"].is_object());
    let tools: Value = http
        .get(format!("{url}/v1/tools"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["name"] == "chain_echo" && t["path"] == "/v1/chain/echo"));
    let metrics = http
        .get(format!("{url}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(metrics.contains("bdm_vendor_ok_total"));
}
