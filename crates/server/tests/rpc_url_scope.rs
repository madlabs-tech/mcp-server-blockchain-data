//! T0.13: the legacy `RPC_URL` now applies to Ethereum only (it used to hijack every chain).

use axum::{routing::post, Json, Router};
use rmcp::{
    model::CallToolRequestParam,
    transport::{ConfigureCommandExt, TokioChildProcess},
    ServiceExt,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::process::Command;

#[tokio::test]
async fn rpc_url_is_ethereum_only() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = seen.clone();
    let app = Router::new().route(
        "/",
        post(move |Json(req): Json<Value>| {
            let log = log.clone();
            async move {
                let method = req["method"].as_str().unwrap_or_default().to_owned();
                log.lock().unwrap().push(method.clone());
                let result = match method.as_str() {
                    "eth_chainId" => json!("0x1"),
                    _ => json!("0x2a"),
                };
                Json(json!({"jsonrpc": "2.0", "id": req["id"], "result": result}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });

    // Base's only reachable endpoint is a closed local port, so nothing leaves the machine.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "[chain_overrides.base]\npublic_rpc = [\"http://127.0.0.1:9\"]\n",
    )
    .unwrap();
    let cmd = Command::new(env!("CARGO_BIN_EXE_evm-mcp-server")).configure(|c| {
        c.arg("--config-dir")
            .arg(dir.path())
            .env("RPC_URL", &url)
            .env("EMS__SERVER__DASHBOARD", "false")
            .env("RUST_LOG", "error")
            .env_remove("ALCHEMY_API_KEY")
            .env_remove("QN_ENDPOINT_NAME")
            .env_remove("QN_TOKEN_ID");
    });
    let client = ().serve(TokioChildProcess::new(cmd).unwrap()).await.unwrap();
    let call = |chain: &str| CallToolRequestParam {
        name: "eth_get_balance".into(),
        arguments: json!({"address": "0xd8da6bf26964af9d7eed9e03e53415d37aa96045", "chain": chain})
            .as_object()
            .cloned(),
    };

    let eth = client.call_tool(call("ethereum")).await.unwrap();
    assert!(eth.content[0]
        .as_text()
        .unwrap()
        .text
        .contains("\"balanceWei\": \"42\""));
    let before = seen.lock().unwrap().len();

    assert!(
        client.call_tool(call("base")).await.is_err(),
        "base must not use RPC_URL"
    );
    assert_eq!(
        seen.lock().unwrap().len(),
        before,
        "RPC_URL node received a Base request"
    );
    client.cancel().await.unwrap();
}
