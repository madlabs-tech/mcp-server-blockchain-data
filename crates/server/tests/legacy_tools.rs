#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
//! Characterization tests (T0.3): lock the observable behavior of the 4 legacy MCP tools.
//! Black-box: spawns the real `onchain-data-mcp` binary over stdio against a fake JSON-RPC node.
//! These tests must pass unchanged after the workspace refactor (T0.13).

use bdm_testkit::FakeJsonRpc;
use rmcp::{
    model::CallToolRequestParam,
    service::{RoleClient, RunningService},
    transport::{ConfigureCommandExt, TokioChildProcess},
    ServiceExt,
};
use serde_json::{json, Value};
use tokio::process::Command;

const WALLET: &str = "0xd8da6bf26964af9d7eed9e03e53415d37aa96045";
const WALLET_CHECKSUM: &str = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";
const CONTRACT: &str = "0xdac17f958d2ee523a2206206994597c13d831ec7";
const TX_HASH: &str = "0x88df016429689c079f3b2f6ad39fa052532c56795b733da78a91ebe6a713944b";

fn fake_transaction() -> Value {
    json!({
        "blockHash": "0x1d59ff54b1eb26b013ce3cb5fc9dab3705b415a67127a003c3e61eb445bb8df2",
        "blockNumber": "0x5daf3b",
        "from": WALLET,
        "gas": "0x5208",
        "gasPrice": "0x4a817c800",
        "maxFeePerGas": "0x4a817c800",
        "maxPriorityFeePerGas": "0x3b9aca00",
        "hash": TX_HASH,
        "input": "0x",
        "nonce": "0x15",
        "to": CONTRACT,
        "transactionIndex": "0x41",
        "value": "0xf3dbb76162000",
        "type": "0x2",
        "accessList": [],
        "chainId": "0x1",
        "v": "0x1",
        "yParity": "0x1",
        "r": "0x1b5e176d927f8e9ab405058b2d2457392da3e20f328b16ddabcebc33eaac5fea",
        "s": "0x4ba69724e8f69de52f0125ad8b3c5c2cef33019bac3249e2c0a2192766d1721c"
    })
}

/// Keeps the fake node and the temp config/data dir alive for the test.
type Keep = (FakeJsonRpc, tempfile::TempDir);

async fn start() -> (RunningService<RoleClient, ()>, Keep) {
    let rpc = FakeJsonRpc::start().await;
    rpc.on("eth_chainId", json!("0x1"))
        .on("eth_blockNumber", json!("0x5daf3b"))
        .on("eth_getBalance", json!("0xde0b6b3a7640000")) // 1 ETH
        .on_fn("eth_getCode", |params| {
            let addr = params[0].as_str().unwrap_or_default().to_lowercase();
            Ok(if addr == CONTRACT {
                json!("0x6080604052")
            } else {
                json!("0x")
            })
        })
        .on("eth_gasPrice", json!("0x4a817c800")) // 20 gwei
        .on("eth_getTransactionByHash", fake_transaction());

    // Temp config + data dir and no dashboard: nothing written to the repo, no fixed port bound.
    let dir = tempfile::tempdir().unwrap();
    let cmd = Command::new(env!("CARGO_BIN_EXE_onchain-data-mcp")).configure(|c| {
        c.arg("--config-dir")
            .arg(dir.path())
            .env("RPC_URL", rpc.url())
            .env("RUST_LOG", "error")
            .env("ODM__SERVER__DASHBOARD", "false")
            .env("ODM__SERVER__DATA_DIR", dir.path())
            .env("ODM__SERVER__WARMUP", "false")
            .env_remove("QN_ENDPOINT_NAME")
            .env_remove("QN_TOKEN_ID");
    });
    let client = ().serve(TokioChildProcess::new(cmd).unwrap()).await.unwrap();
    (client, (rpc, dir))
}

async fn call(
    client: &RunningService<RoleClient, ()>,
    name: &'static str,
    args: Value,
) -> Result<Value, String> {
    let res = client
        .call_tool(CallToolRequestParam {
            name: name.into(),
            arguments: args.as_object().cloned(),
        })
        .await
        .map_err(|e| e.to_string())?;
    let text = &res.content[0].as_text().expect("text content").text;
    Ok(serde_json::from_str(text).expect("tool output is JSON"))
}

#[tokio::test]
async fn lists_legacy_tools() {
    let (client, _keep) = start().await;
    let names: Vec<String> = client
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    for n in [
        "eth_get_balance",
        "eth_get_code",
        "eth_gas_price",
        "eth_get_transaction_by_hash",
    ] {
        assert!(
            names.contains(&n.to_string()),
            "missing tool {n} in {names:?}"
        );
    }
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn eth_get_balance_shape() {
    let (client, _keep) = start().await;
    let out = call(
        &client,
        "eth_get_balance",
        json!({"address": WALLET, "chain": "ethereum"}),
    )
    .await
    .unwrap();
    assert_eq!(
        out,
        json!({"address": WALLET_CHECKSUM, "chain": "Ethereum", "balanceWei": "1000000000000000000", "symbol": "ETH", "decimals": 18})
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn eth_get_code_shape() {
    let (client, _keep) = start().await;
    let contract = call(
        &client,
        "eth_get_code",
        json!({"address": CONTRACT, "chain": "ethereum"}),
    )
    .await
    .unwrap();
    assert_eq!(
        contract,
        json!({"address": "0xdAC17F958D2ee523a2206206994597C13D831ec7", "chain": "Ethereum", "isContract": true, "bytecodeSize": 5})
    );
    let eoa = call(
        &client,
        "eth_get_code",
        json!({"address": WALLET, "chain": "ethereum"}),
    )
    .await
    .unwrap();
    assert_eq!(eoa["isContract"], json!(false));
    assert_eq!(eoa["bytecodeSize"], json!(0));
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn eth_gas_price_shape() {
    let (client, _keep) = start().await;
    let out = call(&client, "eth_gas_price", json!({"chain": "ethereum"}))
        .await
        .unwrap();
    assert_eq!(out["chain"], json!("Ethereum"));
    assert_eq!(out["gasPriceWei"], json!("20000000000"));
    assert_eq!(out["gasPriceGwei"], json!("20.00"));
    let ts = out["timestamp"].as_str().expect("timestamp string");
    chrono::DateTime::parse_from_rfc3339(ts).expect("RFC 3339 timestamp");
    let mut keys: Vec<_> = out.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["chain", "gasPriceGwei", "gasPriceWei", "timestamp"]);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn eth_get_transaction_by_hash_shape() {
    let (client, _keep) = start().await;
    let out = call(
        &client,
        "eth_get_transaction_by_hash",
        json!({"hash": TX_HASH, "chain": "ethereum"}),
    )
    .await
    .unwrap();
    assert_eq!(out["chain"], json!("Ethereum"));
    let tx = &out["transaction"];
    assert_eq!(tx["hash"], json!(TX_HASH));
    assert_eq!(tx["blockNumber"], json!("0x5daf3b"));
    assert_eq!(tx["value"], json!("0xf3dbb76162000"));
    let mut keys: Vec<_> = tx.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, EXPECTED_TX_KEYS);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn invalid_input_is_an_error() {
    let (client, _keep) = start().await;
    assert!(call(
        &client,
        "eth_get_balance",
        json!({"address": "not-an-address", "chain": "ethereum"})
    )
    .await
    .is_err());
    assert!(
        call(&client, "eth_gas_price", json!({"chain": "dogechain"}))
            .await
            .is_err()
    );
    client.cancel().await.unwrap();
}

/// Captured from the pre-refactor binary (alloy 1.0.41 RPC transaction serialization).
const EXPECTED_TX_KEYS: &[&str] = &[
    "accessList",
    "blockHash",
    "blockNumber",
    "chainId",
    "from",
    "gas",
    "gasPrice",
    "hash",
    "input",
    "maxFeePerGas",
    "maxPriorityFeePerGas",
    "nonce",
    "r",
    "s",
    "to",
    "transactionIndex",
    "type",
    "v",
    "value",
    "yParity",
];
