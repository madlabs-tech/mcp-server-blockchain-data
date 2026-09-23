//! Transport conformance checks every chain RPC adapter must pass (T0.11, reused in Phase 1).
//!
//! Call from an adapter test with a constructor that builds the adapter for a URL:
//! ```ignore
//! port_conformance::evm_rpc_transport(|url, chain| Arc::new(MyClient::new(url, chain, timeout)), timeout).await;
//! ```
//! The URL passed to the constructor embeds a fake secret in its path; errors must never echo it.

use crate::{CountingSink, Failure, FakeJsonRpc};
use bdm_ports::{metering, EvmRpc, ProviderError, SolanaRpc};
use serde_json::{json, Value};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

/// Fake secret embedded in the URL path handed to adapter constructors.
pub const SECRET: &str = "SECRETKEY0123456789";

type Call<'a> = Box<
    dyn Fn(String, Value) -> Pin<Box<dyn Future<Output = Result<Value, ProviderError>> + Send + 'a>>
        + 'a,
>;

async fn run_suite(server: &FakeJsonRpc, method: &str, call: Call<'_>, client_timeout: Duration) {
    server.on(method, json!("0x10"));

    // success
    assert_eq!(
        call(method.to_owned(), json!([])).await.unwrap(),
        json!("0x10"),
        "success"
    );

    // HTTP 429 + Retry-After → RateLimited
    server.fail_next(1, Failure::Http(429, Some(5)));
    assert_eq!(
        call(method.to_owned(), json!([])).await.unwrap_err(),
        ProviderError::RateLimited {
            retry_after: Some(Duration::from_secs(5))
        },
        "429"
    );

    // HTTP 503 → Transient (and no secret in the message)
    server.fail_next(1, Failure::Http(503, None));
    let e = call(method.to_owned(), json!([])).await.unwrap_err();
    assert!(matches!(e, ProviderError::Transient(_)), "503 → {e:?}");
    assert!(!e.to_string().contains(SECRET), "secret leaked: {e}");

    // JSON-RPC invalid params → Invalid
    server.fail_next(1, Failure::JsonRpc(-32602, "invalid params".into()));
    let e = call(method.to_owned(), json!([])).await.unwrap_err();
    assert!(matches!(e, ProviderError::Invalid(_)), "-32602 → {e:?}");

    // unknown method (-32601) → Unsupported
    let e = call("no_such_method".to_owned(), json!([]))
        .await
        .unwrap_err();
    assert!(matches!(e, ProviderError::Unsupported(_)), "-32601 → {e:?}");

    // vendor error echoing the key → scrubbed
    server.fail_next(
        1,
        Failure::JsonRpc(-32000, format!("invalid api key {SECRET}")),
    );
    let e = call(method.to_owned(), json!([])).await.unwrap_err();
    assert!(!e.to_string().contains(SECRET), "secret leaked: {e}");

    // timeout → Transient
    server.fail_next(
        1,
        Failure::Timeout(client_timeout + Duration::from_millis(500)),
    );
    let e = call(method.to_owned(), json!([])).await.unwrap_err();
    assert!(matches!(e, ProviderError::Transient(_)), "timeout → {e:?}");
    assert!(!e.to_string().contains(SECRET), "secret leaked: {e}");

    // metering inside a scope
    let sink = Arc::new(CountingSink::default());
    let ctx = metering::CallContext {
        tool: Some("conformance".into()),
        client: None,
        chain: None,
        sink: sink.clone(),
    };
    metering::scope(ctx, call(method.to_owned(), json!([])))
        .await
        .unwrap();
    let reqs = sink.requests();
    assert!(
        reqs.iter()
            .any(|(v, m, t)| !v.is_empty() && m == method && t.as_deref() == Some("conformance")),
        "metering not recorded: {reqs:?}"
    );
}

/// Conformance for [`EvmRpc`] transports. `make(url, chain_id)` must build the adapter with a
/// request timeout of `client_timeout`.
pub async fn evm_rpc_transport<F>(make: F, client_timeout: Duration)
where
    F: Fn(String, u64) -> Arc<dyn EvmRpc>,
{
    let server = FakeJsonRpc::start().await;
    let rpc = make(format!("{}/v2/{SECRET}", server.url()), 8453);
    assert_eq!(rpc.chain_id(), 8453);
    let rpc_ref = &rpc;
    run_suite(
        &server,
        "eth_blockNumber",
        Box::new(move |m, p| Box::pin(async move { rpc_ref.request(&m, p).await })),
        client_timeout,
    )
    .await;
}

/// Conformance for [`SolanaRpc`] transports.
pub async fn solana_rpc_transport<F>(make: F, client_timeout: Duration)
where
    F: Fn(String) -> Arc<dyn SolanaRpc>,
{
    let server = FakeJsonRpc::start().await;
    let rpc = make(format!("{}/?api-key={SECRET}", server.url()));
    let rpc_ref = &rpc;
    run_suite(
        &server,
        "getSlot",
        Box::new(move |m, p| Box::pin(async move { rpc_ref.request(&m, p).await })),
        client_timeout,
    )
    .await;
}
