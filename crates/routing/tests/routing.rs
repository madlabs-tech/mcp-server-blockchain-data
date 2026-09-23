//! Routing behavior tests (T0.8): order + filtering, failover per error kind, retries, breaker,
//! quota guard, strategies, hot swap, metering, routed RPC.

use async_trait::async_trait;
use chrono::Utc;
use ems_config::{ConfigDir, ConfigLoader, EnvSource, Loaded};
use ems_domain::{AssetId, AttemptOutcome, ChainId, ErrorCode, Price, SourceKind};
use ems_ports::{
    metering::{self, CallContext},
    Capability, EvmRpc, PortHandle, PortResult, PriceFeed, ProviderError, Registration, VendorMeta,
};
use ems_routing::{InMemoryCounterStore, RouteReq, Router, RouterOptions, RoutingTable, WindowKey};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

// ---------------------------------------------------------------- test doubles

struct ScriptedPrice {
    vendor: &'static str,
    script: Mutex<VecDeque<PortResult<Decimal>>>,
    fallback: PortResult<Decimal>,
    delay: Duration,
    calls: AtomicUsize,
}

impl ScriptedPrice {
    fn new(vendor: &'static str, fallback: PortResult<Decimal>) -> Arc<Self> {
        Arc::new(Self {
            vendor,
            script: Mutex::default(),
            fallback,
            delay: Duration::ZERO,
            calls: AtomicUsize::new(0),
        })
    }
    fn with_script(
        vendor: &'static str,
        script: Vec<PortResult<Decimal>>,
        fallback: PortResult<Decimal>,
    ) -> Arc<Self> {
        Arc::new(Self {
            vendor,
            script: Mutex::new(script.into()),
            fallback,
            delay: Duration::ZERO,
            calls: AtomicUsize::new(0),
        })
    }
    fn slow(vendor: &'static str, value: Decimal, delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            vendor,
            script: Mutex::default(),
            fallback: Ok(value),
            delay,
            calls: AtomicUsize::new(0),
        })
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl PriceFeed for ScriptedPrice {
    async fn price(&self, asset: &AssetId, currency: &str) -> PortResult<Price> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        let next = self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| self.fallback.clone());
        next.map(|value| Price {
            asset: asset.clone(),
            currency: currency.into(),
            value,
            as_of: Utc::now(),
            source: self.vendor.into(),
            liquidity_usd: None,
        })
    }
}

fn meta(id: &str) -> VendorMeta {
    VendorMeta {
        id: id.into(),
        display_name: id.into(),
        requires_key: false,
        signup_url: None,
        rpc_features: Default::default(),
    }
}

fn price_reg(p: &Arc<ScriptedPrice>) -> Registration {
    Registration::new(meta(p.vendor)).global_port(PortHandle::Price(p.clone()))
}

fn load(cfg: &str, env: &[(&str, &str)]) -> Arc<Loaded> {
    let loader = ConfigLoader::new(
        ConfigDir::new("/nonexistent"),
        EnvSource::from_pairs(env.iter().copied()),
    )
    .unwrap();
    Arc::new(loader.load_texts(cfg, "").unwrap())
}

const ORDER3: &str =
    "[routing.defaults]\nprice = [\"defillama\", \"geckoterminal\", \"dexscreener\"]\n";

fn router_with(
    cfg: &str,
    env: &[(&str, &str)],
    regs: Vec<Registration>,
    opts: RouterOptions,
) -> Arc<Router> {
    let table = RoutingTable {
        config: load(cfg, env),
        registry: ems_routing::ProviderRegistry::new(regs),
    };
    Router::new(table, Arc::new(InMemoryCounterStore::default()), opts)
}

fn fast_opts() -> RouterOptions {
    RouterOptions {
        attempt_timeout: Duration::from_secs(2),
        ..Default::default()
    }
}

fn eth_asset() -> AssetId {
    "eip155:1/slip44:60".parse().unwrap()
}

fn req() -> RouteReq {
    RouteReq::new(Capability::Price).op("market_get_price")
}

async fn price_of(
    r: &Router,
    req: RouteReq,
) -> Result<ems_routing::Routed<Price>, ems_routing::RouteError> {
    let asset = eth_asset();
    r.failover::<dyn PriceFeed, _, _, _>(req, |p| {
        let asset = asset.clone();
        async move { p.price(&asset, "USD").await }
    })
    .await
}

fn d(v: i64) -> Decimal {
    Decimal::from(v)
}

// ---------------------------------------------------------------- failover

#[tokio::test]
async fn transient_is_retried_then_failed_over() {
    let a = ScriptedPrice::new("defillama", Err(ProviderError::Transient("503".into())));
    let b = ScriptedPrice::new("geckoterminal", Ok(d(3000)));
    let c = ScriptedPrice::new("dexscreener", Ok(d(1)));
    let r = router_with(
        ORDER3,
        &[],
        vec![price_reg(&a), price_reg(&b), price_reg(&c)],
        fast_opts(),
    );
    let out = price_of(&r, req()).await.unwrap();
    assert_eq!(out.value.value, d(3000));
    assert_eq!(out.provenance.provider.as_deref(), Some("geckoterminal"));
    assert_eq!(out.provenance.source, SourceKind::Fallback);
    assert_eq!(a.calls(), 2, "one retry on the same vendor");
    assert_eq!(c.calls(), 0);
    let trail: Vec<_> = out
        .provenance
        .providers_tried
        .iter()
        .map(|t| (t.vendor.as_str(), t.outcome))
        .collect();
    assert_eq!(
        trail,
        [
            ("defillama", AttemptOutcome::Failed),
            ("geckoterminal", AttemptOutcome::Ok)
        ]
    );
}

#[tokio::test]
async fn invalid_is_returned_without_failover() {
    let a = ScriptedPrice::new("defillama", Err(ProviderError::Invalid("bad asset".into())));
    let b = ScriptedPrice::new("geckoterminal", Ok(d(1)));
    let r = router_with(ORDER3, &[], vec![price_reg(&a), price_reg(&b)], fast_opts());
    let err = price_of(&r, req()).await.unwrap_err();
    assert_eq!(err.error.code, ErrorCode::InvalidInput);
    assert_eq!(b.calls(), 0);
}

#[tokio::test]
async fn not_found_needs_confirmation_only_when_asked() {
    let a = ScriptedPrice::new("defillama", Err(ProviderError::NotFound));
    let b = ScriptedPrice::new("geckoterminal", Ok(d(7)));
    let c = ScriptedPrice::new("dexscreener", Ok(d(8)));
    let r = router_with(
        ORDER3,
        &[],
        vec![price_reg(&a), price_reg(&b), price_reg(&c)],
        fast_opts(),
    );
    assert_eq!(
        price_of(&r, req()).await.unwrap_err().error.code,
        ErrorCode::NotFound
    );
    assert_eq!(b.calls(), 0);
    let ok = price_of(&r, req().confirm_not_found()).await.unwrap();
    assert_eq!(ok.value.value, d(7), "second provider found it");

    let a = ScriptedPrice::new("defillama", Err(ProviderError::NotFound));
    let b = ScriptedPrice::new("geckoterminal", Err(ProviderError::NotFound));
    let c = ScriptedPrice::new("dexscreener", Ok(d(8)));
    let r = router_with(
        ORDER3,
        &[],
        vec![price_reg(&a), price_reg(&b), price_reg(&c)],
        fast_opts(),
    );
    let err = price_of(&r, req().confirm_not_found()).await.unwrap_err();
    assert_eq!(err.error.code, ErrorCode::NotFound);
    assert_eq!(c.calls(), 0, "two providers agreeing on NotFound is enough");
}

#[tokio::test]
async fn inactive_vendors_are_skipped_with_reasons() {
    let cg = ScriptedPrice::new("coingecko", Ok(d(1)));
    let ll = ScriptedPrice::new("defillama", Ok(d(2)));
    let cfg = "[routing.defaults]\nprice = [\"coingecko\", \"ankr\", \"birdeye\", \"defillama\"]\n";
    let r = router_with(cfg, &[], vec![price_reg(&cg), price_reg(&ll)], fast_opts());
    let out = price_of(&r, req()).await.unwrap();
    assert_eq!(out.provenance.provider.as_deref(), Some("defillama"));
    assert_eq!(out.provenance.source, SourceKind::Fallback);
    let reasons: Vec<_> = out
        .provenance
        .providers_tried
        .iter()
        .map(|t| (t.vendor.clone(), t.reason.clone()))
        .collect();
    assert_eq!(
        reasons[0],
        (
            "coingecko".into(),
            Some("missing_key (COINGECKO_API_KEY)".into())
        )
    );
    assert_eq!(
        reasons[1],
        (
            "ankr".into(),
            Some("disabled (free tier unverified)".into())
        )
    );
    assert_eq!(
        reasons[2],
        (
            "birdeye".into(),
            Some("missing_key (BIRDEYE_API_KEY)".into())
        )
    );
    assert_eq!(cg.calls(), 0);

    // with the key present coingecko becomes primary
    let r = router_with(
        cfg,
        &[("COINGECKO_API_KEY", "cg_demo_key_1")],
        vec![price_reg(&cg), price_reg(&ll)],
        fast_opts(),
    );
    let out = price_of(&r, req()).await.unwrap();
    assert_eq!(out.provenance.source, SourceKind::Primary);
}

#[tokio::test]
async fn no_candidates_explains_missing_keys() {
    let cfg = "[routing.defaults]\nprice = [\"coingecko\", \"birdeye\"]\n";
    let r = router_with(cfg, &[], vec![], fast_opts());
    let err = price_of(&r, req()).await.unwrap_err();
    assert_eq!(err.error.code, ErrorCode::UnsupportedCapability);
    let hint = err.error.hint.unwrap();
    assert!(
        hint.contains("COINGECKO_API_KEY") && hint.contains("BIRDEYE_API_KEY"),
        "{hint}"
    );
}

#[tokio::test]
async fn registered_but_not_for_this_chain_is_skipped() {
    let rpc_ok: Arc<dyn EvmRpc> = Arc::new(FixedRpc {
        chain: 1,
        result: Ok(json!("0x1")),
        calls: AtomicUsize::new(0),
    });
    let regs =
        vec![Registration::new(meta("public"))
            .chain_port(ChainId::evm(1), PortHandle::EvmRpc(rpc_ok))];
    let r = router_with("", &[], regs, fast_opts());
    let base = RouteReq::new(Capability::EvmRpc).chain(ChainId::evm(8453));
    let err = r
        .failover::<dyn EvmRpc, _, _, _>(base, |p| async move {
            p.request("eth_chainId", json!([])).await
        })
        .await
        .unwrap_err();
    assert_eq!(err.error.code, ErrorCode::UnsupportedCapability);
    assert!(err
        .attempts
        .iter()
        .any(|a| a.vendor == "public" && a.reason.as_deref() == Some("not_available_for_chain")));
}

// ---------------------------------------------------------------- breaker

#[tokio::test(start_paused = true)]
async fn breaker_opens_then_half_opens_after_cooldown() {
    let a = ScriptedPrice::new("defillama", Err(ProviderError::Transient("down".into())));
    let b = ScriptedPrice::new("geckoterminal", Ok(d(1)));
    let opts = RouterOptions {
        retries: 0,
        breaker_threshold: 2,
        breaker_cooldown: Duration::from_secs(30),
        ..fast_opts()
    };
    let r = router_with(ORDER3, &[], vec![price_reg(&a), price_reg(&b)], opts);
    price_of(&r, req()).await.unwrap();
    price_of(&r, req()).await.unwrap();
    assert_eq!(a.calls(), 2);
    let out = price_of(&r, req()).await.unwrap();
    assert_eq!(a.calls(), 2, "breaker open: defillama not called");
    assert_eq!(
        out.provenance.providers_tried[0].reason.as_deref(),
        Some("breaker_open")
    );
    tokio::time::advance(Duration::from_secs(31)).await;
    price_of(&r, req()).await.unwrap();
    assert_eq!(a.calls(), 3, "half-open probe after cooldown");
}

// ---------------------------------------------------------------- quota guard

#[tokio::test]
async fn quota_reserve_skips_vendor_and_allow_overage_does_not() {
    let a = ScriptedPrice::new("defillama", Ok(d(1)));
    let b = ScriptedPrice::new("geckoterminal", Ok(d(2)));
    let cfg = format!("{ORDER3}[vendors.defillama.cap]\ndaily = 3\n");
    let r = router_with(&cfg, &[], vec![price_reg(&a), price_reg(&b)], fast_opts());
    let ctx = CallContext {
        tool: Some("market_get_price".into()),
        client: None,
        chain: None,
        sink: r.usage_sink(),
    };
    metering::scope(ctx, async {
        for _ in 0..3 {
            metering::record_request("defillama", "/prices");
        }
    })
    .await;
    let out = price_of(&r, req()).await.unwrap();
    assert_eq!(out.provenance.provider.as_deref(), Some("geckoterminal"));
    assert_eq!(
        out.provenance.providers_tried[0].reason.as_deref(),
        Some("quota_reserve")
    );

    let cfg = format!("{cfg}on_exhausted = \"allow_overage\"\n").replace(
        "[vendors.defillama.cap]\ndaily = 3\non_exhausted",
        "[vendors.defillama]\non_exhausted",
    );
    let cfg = format!("{cfg}[vendors.defillama.cap]\ndaily = 3\n");
    let r = router_with(&cfg, &[], vec![price_reg(&a), price_reg(&b)], fast_opts());
    let ctx = CallContext {
        tool: None,
        client: None,
        chain: None,
        sink: r.usage_sink(),
    };
    metering::scope(ctx, async {
        for _ in 0..5 {
            metering::record_request("defillama", "/prices");
        }
    })
    .await;
    assert_eq!(
        price_of(&r, req())
            .await
            .unwrap()
            .provenance
            .provider
            .as_deref(),
        Some("defillama")
    );
}

#[tokio::test]
async fn rate_limited_marks_vendor_exhausted() {
    let a = ScriptedPrice::with_script(
        "defillama",
        vec![Err(ProviderError::RateLimited {
            retry_after: Some(Duration::from_secs(60)),
        })],
        Ok(d(1)),
    );
    let b = ScriptedPrice::new("geckoterminal", Ok(d(2)));
    let r = router_with(ORDER3, &[], vec![price_reg(&a), price_reg(&b)], fast_opts());
    assert_eq!(
        price_of(&r, req())
            .await
            .unwrap()
            .provenance
            .provider
            .as_deref(),
        Some("geckoterminal")
    );
    let out = price_of(&r, req()).await.unwrap();
    assert_eq!(
        out.provenance.providers_tried[0].reason.as_deref(),
        Some("quota_exhausted")
    );
    assert_eq!(a.calls(), 1);
    let h = r
        .health()
        .into_iter()
        .find(|h| h.vendor == "defillama")
        .unwrap();
    assert!(h.exhausted_until.is_some());
}

#[tokio::test]
async fn metering_applies_cost_table_and_dims() {
    let cfg = "[vendors.defillama.costs]\n\"/prices\" = 5\n";
    let r = router_with(cfg, &[], vec![], fast_opts());
    let ctx = CallContext {
        tool: Some("t1".into()),
        client: Some("c1".into()),
        chain: Some(ChainId::evm(1)),
        sink: r.usage_sink(),
    };
    metering::scope(ctx, async {
        metering::record_request("defillama", "/prices");
        metering::record_request("defillama", "/prices");
        metering::record_request("defillama", "/other");
    })
    .await;
    let day = WindowKey::day(Utc::now());
    assert_eq!(r.counters().total("defillama", &day), 11);
    let rows = r.counters().breakdown("defillama", &day);
    assert!(rows.iter().any(|(d, n)| d.method == "/prices"
        && d.tool.as_deref() == Some("t1")
        && d.client.as_deref() == Some("c1")
        && *n == 10));
    let h = r
        .health()
        .into_iter()
        .find(|h| h.vendor == "defillama")
        .unwrap();
    assert_eq!(h.usage.day_used, 11);
}

// ---------------------------------------------------------------- strategies

#[tokio::test]
async fn quorum_agree_disagree_and_short() {
    let key = |p: &Price| p.value.to_string();
    let asset = eth_asset();
    let call = |p: Arc<dyn PriceFeed>| {
        let asset = asset.clone();
        async move { p.price(&asset, "USD").await }
    };

    let a = ScriptedPrice::new("defillama", Ok(d(5)));
    let b = ScriptedPrice::new("geckoterminal", Ok(d(5)));
    let r = router_with(ORDER3, &[], vec![price_reg(&a), price_reg(&b)], fast_opts());
    let out = r
        .quorum::<dyn PriceFeed, _, _, _, _>(req(), 2, key, call)
        .await
        .unwrap();
    assert!(out.value.met());
    assert_eq!(out.provenance.source, SourceKind::Aggregate);

    let b2 = ScriptedPrice::new("geckoterminal", Ok(d(6)));
    let r = router_with(
        ORDER3,
        &[],
        vec![price_reg(&a), price_reg(&b2)],
        fast_opts(),
    );
    let err = r
        .quorum::<dyn PriceFeed, _, _, _, _>(req(), 2, key, call)
        .await
        .unwrap_err();
    assert_eq!(err.error.code, ErrorCode::Conflict);

    let b3 = ScriptedPrice::new("geckoterminal", Err(ProviderError::Transient("x".into())));
    let r = router_with(
        ORDER3,
        &[],
        vec![price_reg(&a), price_reg(&b3)],
        fast_opts(),
    );
    let out = r
        .quorum::<dyn PriceFeed, _, _, _, _>(req(), 2, key, call)
        .await
        .unwrap();
    assert!(!out.value.met());
    assert_eq!((out.value.agreeing, out.value.required), (1, 2));
}

#[tokio::test]
async fn aggregate_backfills_failures() {
    let a = ScriptedPrice::new("defillama", Err(ProviderError::Transient("x".into())));
    let b = ScriptedPrice::new("geckoterminal", Ok(d(10)));
    let c = ScriptedPrice::new("dexscreener", Ok(d(11)));
    let r = router_with(
        ORDER3,
        &[],
        vec![price_reg(&a), price_reg(&b), price_reg(&c)],
        fast_opts(),
    );
    let asset = eth_asset();
    let out = r
        .aggregate::<dyn PriceFeed, _, _, _>(req(), 2, |p| {
            let asset = asset.clone();
            async move { p.price(&asset, "USD").await }
        })
        .await
        .unwrap();
    let vendors: Vec<_> = out.value.iter().map(|(v, _)| v.as_str()).collect();
    assert_eq!(vendors, ["geckoterminal", "dexscreener"]);
    assert_eq!(out.provenance.provider.as_deref(), Some("aggregate"));
}

#[tokio::test]
async fn fan_out_reports_every_vendor() {
    let a = ScriptedPrice::new("defillama", Ok(d(1)));
    let b = ScriptedPrice::new("geckoterminal", Err(ProviderError::Transient("x".into())));
    let c = ScriptedPrice::new("dexscreener", Ok(d(1)));
    let r = router_with(
        ORDER3,
        &[],
        vec![price_reg(&a), price_reg(&b), price_reg(&c)],
        RouterOptions {
            retries: 0,
            ..fast_opts()
        },
    );
    let asset = eth_asset();
    let out = r
        .fan_out::<dyn PriceFeed, _, _, _>(req(), |p| {
            let asset = asset.clone();
            async move { p.price(&asset, "USD").await }
        })
        .await
        .unwrap();
    assert_eq!(out.value.len(), 3);
    assert_eq!(out.value.iter().filter(|(_, r)| r.is_ok()).count(), 2);
}

#[tokio::test]
async fn hedged_takes_the_faster_provider() {
    let a = ScriptedPrice::slow("defillama", d(1), Duration::from_millis(400));
    let b = ScriptedPrice::new("geckoterminal", Ok(d(2)));
    let r = router_with(ORDER3, &[], vec![price_reg(&a), price_reg(&b)], fast_opts());
    let asset = eth_asset();
    let t = std::time::Instant::now();
    let out = r
        .hedged::<dyn PriceFeed, _, _, _>(req(), Duration::from_millis(30), |p| {
            let asset = asset.clone();
            async move { p.price(&asset, "USD").await }
        })
        .await
        .unwrap();
    assert_eq!(out.provenance.provider.as_deref(), Some("geckoterminal"));
    assert!(t.elapsed() < Duration::from_millis(300));
}

// ---------------------------------------------------------------- hot swap

#[tokio::test]
async fn hot_swap_applies_to_new_requests_only() {
    let a = ScriptedPrice::new("defillama", Ok(d(1)));
    let b = ScriptedPrice::new("geckoterminal", Ok(d(2)));
    let regs = || vec![price_reg(&a), price_reg(&b)];
    let r = router_with(ORDER3, &[], regs(), fast_opts());
    let old = r.table();
    let cfg = "[routing.defaults]\nprice = [\"geckoterminal\", \"defillama\"]\n";
    r.swap(RoutingTable {
        config: load(cfg, &[]),
        registry: ems_routing::ProviderRegistry::new(regs()),
    });
    assert_eq!(
        price_of(&r, req())
            .await
            .unwrap()
            .provenance
            .provider
            .as_deref(),
        Some("geckoterminal")
    );
    assert_eq!(
        old.config.order(Capability::Price, None, None).vendors[0],
        "defillama",
        "old snapshot unchanged"
    );
}

// ---------------------------------------------------------------- routed RPC

struct FixedRpc {
    chain: u64,
    result: PortResult<Value>,
    calls: AtomicUsize,
}

#[async_trait]
impl EvmRpc for FixedRpc {
    fn chain_id(&self) -> u64 {
        self.chain
    }
    async fn request(&self, _method: &str, _params: Value) -> PortResult<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.result.clone()
    }
}

#[tokio::test]
async fn routed_evm_rpc_fails_over() {
    let bad = Arc::new(FixedRpc {
        chain: 1,
        result: Err(ProviderError::Transient("503".into())),
        calls: AtomicUsize::new(0),
    });
    let good = Arc::new(FixedRpc {
        chain: 1,
        result: Ok(json!("0xde0b6b3a7640000")),
        calls: AtomicUsize::new(0),
    });
    let regs = vec![
        Registration::new(meta("alchemy"))
            .chain_port(ChainId::evm(1), PortHandle::EvmRpc(bad.clone())),
        Registration::new(meta("public"))
            .chain_port(ChainId::evm(1), PortHandle::EvmRpc(good.clone())),
    ];
    let r = router_with("", &[("ALCHEMY_API_KEY", "alc_key_123")], regs, fast_opts());
    let rpc = ems_routing::RoutedEvmRpc::new(r, ChainId::evm(1)).unwrap();
    assert_eq!(
        rpc.request("eth_getBalance", json!(["0x0", "latest"]))
            .await
            .unwrap(),
        json!("0xde0b6b3a7640000")
    );
    assert_eq!(bad.calls.load(Ordering::SeqCst), 2);
    assert_eq!(good.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn all_unsupported_stays_unsupported() {
    let a = ScriptedPrice::new(
        "defillama",
        Err(ProviderError::Unsupported("no such method".into())),
    );
    let b = ScriptedPrice::new(
        "geckoterminal",
        Err(ProviderError::Unsupported("no such method".into())),
    );
    let r = router_with(ORDER3, &[], vec![price_reg(&a), price_reg(&b)], fast_opts());
    let err = price_of(&r, req()).await.unwrap_err();
    assert_eq!(err.error.code, ErrorCode::UnsupportedCapability);
}
