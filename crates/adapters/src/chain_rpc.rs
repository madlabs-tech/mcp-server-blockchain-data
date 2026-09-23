//! Chain RPC transports: [`EvmRpcClient`] and [`SolanaRpcClient`]. Each also implements
//! [`Broadcaster`] (`eth_sendRawTransaction` / `sendTransaction`) so the same instance is
//! registered for the `broadcast` capability.

use crate::{http::HttpClient, jsonrpc::JsonRpcClient};
use async_trait::async_trait;
use ems_config::Redacted;
use ems_ports::{BroadcastReceipt, Broadcaster, EvmRpc, PortResult, ProviderError, SolanaRpc};
use serde_json::{json, Value};

/// EVM JSON-RPC transport for one (vendor, chain, URL).
pub struct EvmRpcClient {
    chain_id: u64,
    rpc: JsonRpcClient,
}

impl EvmRpcClient {
    pub fn new(http: HttpClient, chain_id: u64, url: Redacted<String>) -> Self {
        Self {
            chain_id,
            rpc: JsonRpcClient::new(http, url),
        }
    }

    pub fn vendor(&self) -> &str {
        self.rpc.vendor()
    }

    /// Assert the endpoint serves the configured chain (`eth_chainId`).
    pub async fn verify_chain_id(&self) -> Result<(), ProviderError> {
        let v = self.rpc.request("eth_chainId", json!([])).await?;
        let got = v
            .as_str()
            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .ok_or_else(|| ProviderError::Fatal(format!("unparseable eth_chainId {v}")))?;
        if got != self.chain_id {
            return Err(ProviderError::Fatal(format!(
                "{} endpoint serves chain {got}, expected {}",
                self.vendor(),
                self.chain_id
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl EvmRpc for EvmRpcClient {
    fn chain_id(&self) -> u64 {
        self.chain_id
    }

    async fn request(&self, method: &str, params: Value) -> PortResult<Value> {
        self.rpc.request(method, params).await
    }
}

#[async_trait]
impl Broadcaster for EvmRpcClient {
    async fn send_raw(&self, signed: &str) -> PortResult<BroadcastReceipt> {
        let hash = self
            .rpc
            .request("eth_sendRawTransaction", json!([signed]))
            .await?;
        let tx_hash = hash
            .as_str()
            .ok_or_else(|| {
                ProviderError::Transient("eth_sendRawTransaction returned no hash".into())
            })?
            .to_owned();
        Ok(BroadcastReceipt {
            tx_hash,
            private: false,
        })
    }
}

/// Solana JSON-RPC transport for one (vendor, URL).
pub struct SolanaRpcClient {
    rpc: JsonRpcClient,
}

impl SolanaRpcClient {
    pub fn new(http: HttpClient, url: Redacted<String>) -> Self {
        Self {
            rpc: JsonRpcClient::new(http, url),
        }
    }

    pub fn vendor(&self) -> &str {
        self.rpc.vendor()
    }
}

#[async_trait]
impl SolanaRpc for SolanaRpcClient {
    async fn request(&self, method: &str, params: Value) -> PortResult<Value> {
        self.rpc.request(method, params).await
    }
}

#[async_trait]
impl Broadcaster for SolanaRpcClient {
    /// `signed` is a base64-encoded signed transaction.
    async fn send_raw(&self, signed: &str) -> PortResult<BroadcastReceipt> {
        let sig = self
            .rpc
            .request("sendTransaction", json!([signed, {"encoding": "base64"}]))
            .await?;
        let tx_hash = sig
            .as_str()
            .ok_or_else(|| {
                ProviderError::Transient("sendTransaction returned no signature".into())
            })?
            .to_owned();
        Ok(BroadcastReceipt {
            tx_hash,
            private: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::HttpClient;
    use ems_testkit::{port_conformance, FakeJsonRpc};
    use std::{sync::Arc, time::Duration};

    const TIMEOUT: Duration = Duration::from_millis(500);

    fn http() -> HttpClient {
        HttpClient::new("test", TIMEOUT)
    }

    #[tokio::test]
    async fn evm_conformance() {
        port_conformance::evm_rpc_transport(
            |url, chain| Arc::new(EvmRpcClient::new(http(), chain, Redacted::new(url))),
            TIMEOUT,
        )
        .await;
    }

    #[tokio::test]
    async fn solana_conformance() {
        port_conformance::solana_rpc_transport(
            |url| Arc::new(SolanaRpcClient::new(http(), Redacted::new(url))),
            TIMEOUT,
        )
        .await;
    }

    #[tokio::test]
    async fn verify_chain_id_match_and_mismatch() {
        let server = FakeJsonRpc::start().await;
        server.on("eth_chainId", json!("0x2105"));
        let ok = EvmRpcClient::new(http(), 8453, Redacted::new(server.url()));
        ok.verify_chain_id().await.unwrap();
        let wrong = EvmRpcClient::new(http(), 1, Redacted::new(server.url()));
        let e = wrong.verify_chain_id().await.unwrap_err();
        assert!(
            matches!(e, ProviderError::Fatal(ref m) if m.contains("8453")),
            "{e:?}"
        );
    }

    #[tokio::test]
    async fn broadcasts() {
        let server = FakeJsonRpc::start().await;
        server.on("eth_sendRawTransaction", json!("0xabc"));
        server.on("sendTransaction", json!("5sig"));
        let evm = EvmRpcClient::new(http(), 1, Redacted::new(server.url()));
        assert_eq!(evm.send_raw("0x02f8").await.unwrap().tx_hash, "0xabc");
        let sol = SolanaRpcClient::new(http(), Redacted::new(server.url()));
        assert_eq!(sol.send_raw("AQID").await.unwrap().tx_hash, "5sig");
        let calls = server.calls();
        assert_eq!(calls[1].1, json!(["AQID", {"encoding": "base64"}]));
    }

    #[tokio::test]
    async fn batch_orders_results_and_meters_each_method() {
        let server = FakeJsonRpc::start().await;
        server.on("eth_blockNumber", json!("0x1"));
        server.on("eth_chainId", json!("0x1"));
        let rpc = JsonRpcClient::new(http(), Redacted::new(server.url()));
        let sink = Arc::new(ems_testkit::CountingSink::default());
        let ctx = ems_ports::metering::CallContext {
            tool: None,
            client: None,
            chain: None,
            sink: sink.clone(),
        };
        let out = ems_ports::metering::scope(
            ctx,
            rpc.batch(&[
                ("eth_chainId", json!([])),
                ("eth_blockNumber", json!([])),
                ("nope", json!([])),
            ]),
        )
        .await
        .unwrap();
        assert_eq!(out[0].as_ref().unwrap(), &json!("0x1"));
        assert!(matches!(out[2], Err(ProviderError::Unsupported(_))));
        let mut methods = sink.methods("test");
        methods.sort();
        assert_eq!(methods, ["eth_blockNumber", "eth_chainId", "nope"]);
    }
}
