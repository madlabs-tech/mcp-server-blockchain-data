#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
//! T0.13: the legacy `RPC_URL` now applies to Ethereum only (it used to hijack every chain).

use bdm_testkit::FakeJsonRpc;
use rmcp::{
    model::CallToolRequestParam,
    transport::{ConfigureCommandExt, TokioChildProcess},
    ServiceExt,
};
use serde_json::json;
use tokio::process::Command;

#[tokio::test]
async fn rpc_url_is_ethereum_only() {
    let rpc = FakeJsonRpc::start().await;
    rpc.on("eth_chainId", json!("0x1"))
        .on("eth_blockNumber", json!("0x2a"))
        .on("eth_getBalance", json!("0x2a"));

    // Base's only reachable endpoint is a closed local port, so nothing leaves the machine.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "[chain_overrides.base]\npublic_rpc = [\"http://127.0.0.1:9\"]\n",
    )
    .unwrap();
    let cmd = Command::new(env!("CARGO_BIN_EXE_onchain-data-mcp")).configure(|c| {
        c.arg("--config-dir")
            .arg(dir.path())
            .env("RPC_URL", rpc.url())
            .env("ODM__SERVER__DASHBOARD", "false")
            .env("ODM__SERVER__DATA_DIR", dir.path())
            .env("ODM__SERVER__WARMUP", "false")
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
    let before = rpc.calls().len();

    assert!(
        client.call_tool(call("base")).await.is_err(),
        "base must not use RPC_URL"
    );
    assert_eq!(
        rpc.calls().len(),
        before,
        "RPC_URL node received a Base request"
    );
    client.cancel().await.unwrap();
}
