#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
//! App executor tests (T0.9): envelope, cache, profiles, guard, metering, legacy aliases.

use async_trait::async_trait;
use bdm_app::{
    App, CallGuard, Caller, Catalog, Ctx, Domain, OpOutput, Operation, Profile, ProfileSelection,
};
use bdm_domain::{ChainId, DomainError, ErrorCode};
use bdm_ports::{metering, PortHandle, Registration};
use bdm_routing::WindowKey;
use bdm_testkit::mocks::MockEvmRpc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

mod common;

#[derive(Deserialize, JsonSchema)]
struct EchoIn {
    msg: String,
}

#[derive(Serialize, JsonSchema)]
struct EchoOut {
    msg: String,
    n: usize,
}

struct Echo(Arc<AtomicUsize>);

#[async_trait]
impl Operation for Echo {
    type Input = EchoIn;
    type Output = EchoOut;
    const NAME: &'static str = "test_echo";
    const DOMAIN: Domain = Domain::Chain;
    const DESCRIPTION: &'static str = "echo";
    const PROFILES: &'static [Profile] = &[Profile::Payments];

    fn cache_ttl(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }

    async fn execute(&self, _ctx: &Ctx, input: EchoIn) -> Result<OpOutput<EchoOut>, DomainError> {
        let n = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        metering::record_request("defillama", "/echo");
        Ok(OpOutput::local(EchoOut { msg: input.msg, n }))
    }
}

struct TradingOnly;

#[async_trait]
impl Operation for TradingOnly {
    type Input = EchoIn;
    type Output = EchoOut;
    const NAME: &'static str = "test_trading";
    const DOMAIN: Domain = Domain::Trade;
    const DESCRIPTION: &'static str = "trading only";
    const PROFILES: &'static [Profile] = &[Profile::Trading];

    async fn execute(&self, _ctx: &Ctx, input: EchoIn) -> Result<OpOutput<EchoOut>, DomainError> {
        Ok(OpOutput::local(EchoOut {
            msg: input.msg,
            n: 0,
        }))
    }
}

struct Panicking;

#[async_trait]
impl Operation for Panicking {
    type Input = EchoIn;
    type Output = EchoOut;
    const NAME: &'static str = "test_panic";
    const DOMAIN: Domain = Domain::Chain;
    const DESCRIPTION: &'static str = "panics";
    const PROFILES: &'static [Profile] = &[Profile::Payments];

    async fn execute(&self, _ctx: &Ctx, _input: EchoIn) -> Result<OpOutput<EchoOut>, DomainError> {
        panic!("deliberate test panic")
    }
}

fn app(cfg: &str, regs: Vec<Registration>) -> (App, Arc<AtomicUsize>) {
    app_with_cache(cfg, regs, 1000)
}

fn app_with_cache(cfg: &str, regs: Vec<Registration>, cache_max: u64) -> (App, Arc<AtomicUsize>) {
    let router = common::router(&[], cfg, regs);
    let count = Arc::new(AtomicUsize::new(0));
    let mut catalog = Catalog::new();
    bdm_app::ops::register_all(&mut catalog);
    catalog.register(Echo(count.clone()));
    catalog.register(TradingOnly);
    catalog.register(Panicking);
    (App::new(catalog, router, cache_max), count)
}

#[tokio::test]
async fn panicking_operation_is_an_internal_error_not_a_crash() {
    let rec = Arc::new(Recorder(Default::default()));
    let (a, _) = app("", vec![]);
    let a = a.with_observer(rec.clone());
    let err = a
        .call("test_panic", json!({"msg": "x"}), Caller::local())
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Internal);
    assert_eq!(err.message, "internal error; see server log");
    assert_eq!(a.metrics().snapshot()["test_panic"].errors, 1);
    assert_eq!(
        *rec.0.lock().unwrap(),
        vec![("test_panic".to_string(), false)]
    );
    // The process and the App keep serving.
    a.call("test_echo", json!({"msg": "x"}), Caller::local())
        .await
        .unwrap();
}

#[tokio::test]
async fn envelope_cache_and_metering() {
    let (app, count) = app("[server]\ntool_profile = \"payments\"\n", vec![]);
    let a = app
        .call("test_echo", json!({"msg": "hi"}), Caller::local())
        .await
        .unwrap();
    assert_eq!(a["data"], json!({"msg": "hi", "n": 1}));
    assert_eq!(a["meta"]["cached"], json!(false));
    assert!(a.get("__ems_cache_ttl_ms").is_none());
    let b = app
        .call("test_echo", json!({"msg": "hi"}), Caller::local())
        .await
        .unwrap();
    assert_eq!(b["data"]["n"], json!(1), "served from cache");
    assert_eq!(b["meta"]["cached"], json!(true));
    assert_eq!(b["meta"]["source"], json!("cache"));
    assert_eq!(count.load(Ordering::SeqCst), 1);
    app.call("test_echo", json!({"msg": "other"}), Caller::local())
        .await
        .unwrap();
    assert_eq!(
        count.load(Ordering::SeqCst),
        2,
        "different input is a different cache key"
    );

    let day = WindowKey::day(chrono::Utc::now());
    let rows = app.router().counters().breakdown("defillama", &day);
    assert!(
        rows.iter()
            .any(|(d, _)| d.tool.as_deref() == Some("test_echo")),
        "{rows:?}"
    );
    assert_eq!(app.metrics().snapshot()["test_echo"].cache_hits, 1);
}

#[tokio::test]
async fn config_cache_ttl_zero_disables_cache() {
    let (app, count) = app("[operations.test_echo]\ncache_ttl_secs = 0\n", vec![]);
    app.call("test_echo", json!({"msg": "x"}), Caller::local())
        .await
        .unwrap();
    app.call("test_echo", json!({"msg": "x"}), Caller::local())
        .await
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

async fn echo(app: &App, msg: &str) -> Value {
    app.call("test_echo", json!({ "msg": msg }), Caller::local())
        .await
        .unwrap()
}

#[tokio::test]
async fn cache_entry_expires_after_its_ttl() {
    let (app, count) = app("[operations.test_echo]\ncache_ttl_secs = 1\n", vec![]);
    echo(&app, "x").await;
    assert_eq!(echo(&app, "x").await["meta"]["cached"], json!(true));
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(echo(&app, "x").await["meta"]["cached"], json!(false));
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn cache_capacity_zero_caches_nothing() {
    let (app, count) = app_with_cache("", vec![], 0);
    echo(&app, "x").await;
    assert_eq!(echo(&app, "x").await["meta"]["cached"], json!(false));
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn cache_holds_at_most_cache_max_entries() {
    let (app, count) = app_with_cache("", vec![], 2);
    for i in 0..10 {
        echo(&app, &i.to_string()).await;
    }
    let before = count.load(Ordering::SeqCst);
    let mut hits = 0;
    for i in 0..10 {
        hits += usize::from(echo(&app, &i.to_string()).await["meta"]["cached"] == json!(true));
    }
    assert!(hits <= 2, "{hits} hits with room for 2");
    assert_eq!(count.load(Ordering::SeqCst), before + 10 - hits);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cache_is_consistent_under_concurrent_calls() {
    let (app, _) = app("", vec![]);
    let app = Arc::new(app);
    let tasks: Vec<_> = (0..64)
        .map(|i| {
            let app = app.clone();
            tokio::spawn(async move {
                let msg = (i % 8).to_string();
                (echo(&app, &msg).await, msg)
            })
        })
        .collect();
    for t in tasks {
        let (out, msg) = t.await.unwrap();
        assert_eq!(out["data"]["msg"], json!(msg));
    }
    for i in 0..8 {
        assert_eq!(
            echo(&app, &i.to_string()).await["meta"]["cached"],
            json!(true)
        );
    }
}

#[tokio::test]
async fn profiles_filter_visible_tools() {
    let names = |app: &App, caller: &Caller| -> Vec<&'static str> {
        app.visible(caller).iter().map(|o| o.name()).collect()
    };

    let (a, _) = app("[server]\ntool_profile = \"payments\"\n", vec![]);
    let v = names(&a, &Caller::local());
    assert!(v.contains(&"test_echo") && v.contains(&"eth_get_balance"));
    assert!(!v.contains(&"test_trading"));
    let err = a
        .call("test_trading", json!({"msg": "x"}), Caller::local())
        .await
        .unwrap_err();
    assert!(err.message.contains("unknown tool"));

    let (a, _) = app(
        "[server]\ntool_profile = \"custom\"\nenabled_tools = [\"test_trading\"]\n",
        vec![],
    );
    assert_eq!(names(&a, &Caller::local()), ["test_trading"]);

    let (a, _) = app("[server]\ntool_profile = \"all\"\ndisabled_tools = [\"test_echo\"]\n[operations.eth_gas_price]\nenabled = false\n", vec![]);
    let v = names(&a, &Caller::local());
    assert!(
        !v.contains(&"test_echo") && !v.contains(&"eth_gas_price") && v.contains(&"test_trading")
    );

    // hosted client restricted to trading
    let (a, _) = app("", vec![]);
    let client = Caller {
        client: Some("c1".into()),
        profile: Some(ProfileSelection::Profile(Profile::Trading)),
    };
    let v = names(&a, &client);
    assert!(v.contains(&"test_trading") && !v.contains(&"test_echo"));
}

#[tokio::test]
async fn invalid_input_is_reported() {
    let (a, _) = app("", vec![]);
    let err = a
        .call("test_echo", json!({"nope": 1}), Caller::local())
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
    assert!(err.message.contains("test_echo"));
}

struct DenyAll;

#[async_trait]
impl CallGuard for DenyAll {
    async fn admit(&self, _: &Caller, _: &str) -> Result<(), DomainError> {
        Err(DomainError::new(ErrorCode::QuotaExceeded, "daily limit reached").with_retry_after(60))
    }
}

#[tokio::test]
async fn guard_can_reject() {
    let (a, count) = app("", vec![]);
    let a = a.with_guard(Arc::new(DenyAll));
    let err = a
        .call("test_echo", json!({"msg": "x"}), Caller::local())
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::QuotaExceeded);
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn schemas_are_generated() {
    let (a, _) = app("", vec![]);
    let op = a.catalog().get("test_echo").unwrap();
    let input: Value = Value::Object(op.input_schema());
    assert_eq!(input["type"], json!("object"));
    assert!(input["properties"]["msg"].is_object());
    let output = Value::Object(op.output_schema());
    assert!(output["properties"]["data"].is_object() && output["properties"]["meta"].is_object());
    let legacy = Value::Object(a.catalog().get("eth_get_balance").unwrap().output_schema());
    assert!(
        legacy["properties"]["balanceWei"].is_object(),
        "legacy output has no envelope"
    );
}

fn public_eth(mock: Arc<MockEvmRpc>) -> Registration {
    Registration::new(common::meta("public")).chain_port(ChainId::evm(1), PortHandle::EvmRpc(mock))
}

#[tokio::test]
async fn legacy_balance_on_new_stack() {
    let mock = Arc::new(MockEvmRpc {
        chain_id: 1,
        ..Default::default()
    });
    mock.script.push_ok(json!("0xde0b6b3a7640000"));
    let (a, _) = app("", vec![public_eth(mock.clone())]);
    let out = a
        .call(
            "eth_get_balance",
            json!({"address": "0xd8da6bf26964af9d7eed9e03e53415d37aa96045", "chain": "ethereum"}),
            Caller::local(),
        )
        .await
        .unwrap();
    assert_eq!(
        out,
        json!({"address": "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045", "chain": "Ethereum", "balanceWei": "1000000000000000000", "symbol": "ETH", "decimals": 18})
    );
    let err = a
        .call(
            "eth_get_balance",
            json!({"address": "nope", "chain": "ethereum"}),
            Caller::local(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
    let err = a
        .call("eth_gas_price", json!({"chain": "solana"}), Caller::local())
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
}

struct Recorder(std::sync::Mutex<Vec<(String, bool)>>);

impl bdm_app::CallObserver for Recorder {
    fn on_call(&self, _c: &Caller, op: &str, r: &Result<Value, DomainError>, _l: Duration) {
        self.0.lock().unwrap().push((op.to_owned(), r.is_ok()));
    }
}

struct MetaRecorder(std::sync::Mutex<Vec<Value>>);

impl bdm_app::CallObserver for MetaRecorder {
    fn on_call(&self, _c: &Caller, _op: &str, r: &Result<Value, DomainError>, _l: Duration) {
        let meta = r.as_ref().ok().and_then(|v| v.get("meta")).cloned();
        self.0.lock().unwrap().push(meta.unwrap_or(Value::Null));
    }
}

/// Legacy aliases return bare data, but the call log still needs their chain and provider.
#[tokio::test]
async fn observer_sees_legacy_chain_and_provider() {
    let mock = Arc::new(MockEvmRpc {
        chain_id: 1,
        ..Default::default()
    });
    mock.script.push_ok(json!("0x4a817c800"));
    let rec = Arc::new(MetaRecorder(Default::default()));
    let (a, _) = app("", vec![public_eth(mock)]);
    let a = a.with_observer(rec.clone());
    let out = a
        .call(
            "eth_gas_price",
            json!({"chain": "ethereum"}),
            Caller::local(),
        )
        .await
        .unwrap();
    assert_eq!(out["gasPriceGwei"], "20.00", "bare legacy output: {out}");
    assert!(out.get("meta").is_none());
    let meta = rec.0.lock().unwrap()[0].clone();
    assert_eq!(meta["chain"], "eip155:1");
    assert_eq!(meta["provider"], "rpc");
}

#[tokio::test]
async fn observer_sees_every_call_including_cache_hits_and_rejections() {
    let rec = Arc::new(Recorder(Default::default()));
    let (a, _) = app("", vec![]);
    let a = a.with_observer(rec.clone());
    a.call("test_echo", json!({"msg": "x"}), Caller::local())
        .await
        .unwrap();
    a.call("test_echo", json!({"msg": "x"}), Caller::local())
        .await
        .unwrap(); // cache hit
    let _ = a.call("nope", json!({}), Caller::local()).await;
    assert_eq!(
        *rec.0.lock().unwrap(),
        vec![
            ("test_echo".to_string(), true),
            ("test_echo".to_string(), true),
            ("nope".to_string(), false)
        ]
    );
}
