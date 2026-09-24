#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
//! market / trade / rwa tools end to end through `App` with mock ports (T1.M1–T1.M4).

use alloy_primitives::U256;
use async_trait::async_trait;
use bdm_app::{App, Caller, Catalog};
use bdm_config::{ConfigDir, ConfigLoader, EnvSource};
use bdm_domain::{Amount, AssetId, ChainId, Price, RiskFlag, Severity, SwapQuote, UnsignedTx};
use bdm_ports::{
    PortHandle, PortResult, ProviderError, Registration, RiskAssessment, SwapQuoter, SwapRequest,
    TokenInfo, TokenMetadata, TokenRisk, VendorMeta,
};
use bdm_routing::{InMemoryCounterStore, ProviderRegistry, Router, RouterOptions, RoutingTable};
use bdm_testkit::mocks::{MockEvmRpc, MockPriceFeed, Scripted};
use chrono::{Duration, Utc};
use serde_json::{json, Value};
use std::sync::Arc;

fn app(cfg: &str, regs: Vec<Registration>) -> App {
    let loader = ConfigLoader::new(ConfigDir::new("/nonexistent"), EnvSource::default()).unwrap();
    let config = Arc::new(
        loader
            .load_texts(&format!("[server]\ntool_profile = \"all\"\n{cfg}"), "")
            .unwrap(),
    );
    let router = Router::new(
        RoutingTable {
            config,
            registry: ProviderRegistry::new(regs),
        },
        Arc::new(InMemoryCounterStore::default()),
        RouterOptions::default(),
    );
    let mut catalog = Catalog::new();
    bdm_app::ops::register_all(&mut catalog);
    App::new(catalog, router, 1000)
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

fn global(id: &str, port: PortHandle) -> Registration {
    Registration::new(meta(id)).global_port(port)
}

async fn call(app: &App, tool: &str, input: Value) -> Result<Value, bdm_domain::DomainError> {
    app.call(tool, input, Caller::local()).await
}

// ------------------------------------------------------------------ mocks

#[derive(Default)]
struct Risk(Scripted<RiskAssessment>);
#[async_trait]
impl TokenRisk for Risk {
    async fn assess(&self, _: &AssetId) -> PortResult<RiskAssessment> {
        self.0.next().await
    }
}

#[derive(Default)]
struct Quoter {
    quote: Scripted<SwapQuote>,
    build: Scripted<SwapQuote>,
}
#[async_trait]
impl SwapQuoter for Quoter {
    async fn quote(&self, _: &SwapRequest) -> PortResult<SwapQuote> {
        self.quote.next().await
    }
    async fn build(&self, _: &SwapRequest) -> PortResult<SwapQuote> {
        self.build.next().await
    }
}

#[derive(Default)]
struct Meta(Scripted<TokenInfo>);
#[async_trait]
impl TokenMetadata for Meta {
    async fn metadata(&self, _: &AssetId) -> PortResult<TokenInfo> {
        self.0.next().await
    }
}

const USDC_BASE: &str = "eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
const ETH_BASE: &str = "eip155:8453/slip44:60";
const TAKER: &str = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";

fn price(v: &str, source: &str) -> Price {
    Price {
        asset: USDC_BASE.parse().unwrap(),
        currency: "USD".into(),
        value: v.parse().unwrap(),
        as_of: Utc::now(),
        source: source.into(),
        liquidity_usd: None,
    }
}

fn feed(v: PortResult<Price>) -> Arc<MockPriceFeed> {
    let m = Arc::new(MockPriceFeed::default());
    m.script.always(v);
    m
}

// ------------------------------------------------------------------ market_get_price

#[tokio::test]
async fn price_median_spread_and_divergence() {
    let regs = vec![
        global(
            "defillama",
            PortHandle::Price(feed(Ok(price("1.00", "defillama")))),
        ),
        global(
            "geckoterminal",
            PortHandle::Price(feed(Ok(price("1.01", "geckoterminal")))),
        ),
        global(
            "dexscreener",
            PortHandle::Price(feed(Ok(price("1.30", "dexscreener")))),
        ),
    ];
    let a = app("", regs);
    let out = call(&a, "market_get_price", json!({"asset": USDC_BASE}))
        .await
        .unwrap();
    let d = &out["data"];
    assert_eq!(d["median"], json!("1.01"));
    assert_eq!(d["spread_bps"], json!(2970));
    assert_eq!(d["status"], json!("divergent"));
    assert_eq!(d["sources"].as_array().unwrap().len(), 3);
    assert_eq!(out["meta"]["source"], json!("aggregate"));
    // coingecko has no key → listed as skipped in the trail.
    let tried = out["meta"]["providersTried"].as_array().unwrap();
    assert!(tried
        .iter()
        .any(|t| t["vendor"] == "coingecko" && t["outcome"] == "skipped"));

    let ok = call(
        &a,
        "market_get_price",
        json!({"asset": USDC_BASE, "max_spread_bps": 5000}),
    )
    .await
    .unwrap();
    assert_eq!(ok["data"]["status"], json!("ok"));
}

#[tokio::test]
async fn missing_price_is_unknown_never_zero() {
    let regs = vec![
        global(
            "defillama",
            PortHandle::Price(feed(Err(ProviderError::NotFound))),
        ),
        global(
            "geckoterminal",
            PortHandle::Price(feed(Err(ProviderError::Unsupported("no".into())))),
        ),
    ];
    let a = app("", regs);
    let out = call(&a, "market_get_price", json!({"asset": USDC_BASE}))
        .await
        .unwrap();
    assert_eq!(out["data"]["status"], json!("unknown"));
    assert!(out["data"].get("median").is_none_or(Value::is_null));
    assert_eq!(out["data"]["sources"], json!([]));

    // No vendor at all is an error with a hint, not "unknown".
    let none = app("[routing.defaults]\nprice = [\"coingecko\"]\n", vec![]);
    let err = call(&none, "market_get_price", json!({"asset": USDC_BASE}))
        .await
        .unwrap_err();
    assert_eq!(err.code, bdm_domain::ErrorCode::UnsupportedCapability);
    assert!(err.hint.unwrap().contains("COINGECKO_API_KEY"));
}

// ------------------------------------------------------------------ token_get_metadata

#[tokio::test]
async fn metadata_on_chain_decimals_first() {
    let info = |d: u8, sym: Option<&str>, src: &str| TokenInfo {
        asset: USDC_BASE.parse().unwrap(),
        decimals: d,
        symbol: sym.map(str::to_owned),
        name: None,
        logo_url: None,
        verified: None,
        source: src.into(),
    };
    let rpc = Arc::new(Meta::default());
    rpc.0.always(Ok(info(6, None, "rpc")));
    let gt = Arc::new(Meta::default());
    gt.0.always(Ok(info(6, Some("USDC"), "geckoterminal")));
    let a = app(
        "[routing.defaults]\ntoken_metadata = [\"rpc\", \"geckoterminal\"]\n",
        vec![
            global("rpc", PortHandle::TokenMetadata(rpc)),
            global("geckoterminal", PortHandle::TokenMetadata(gt)),
        ],
    );
    let out = call(&a, "token_get_metadata", json!({"asset": USDC_BASE}))
        .await
        .unwrap();
    let d = &out["data"];
    assert_eq!(
        (d["decimals"].clone(), d["decimals_source"].clone()),
        (json!(6), json!("rpc"))
    );
    assert_eq!(d["symbol"], json!("USDC"));

    let native = call(&a, "token_get_metadata", json!({"asset": ETH_BASE}))
        .await
        .unwrap();
    assert_eq!(native["data"]["decimals"], json!(18));
    assert_eq!(native["data"]["symbol"], json!("ETH"));
}

// ------------------------------------------------------------------ token_check_risk

#[tokio::test]
async fn risk_merges_sources_and_attributes_flags() {
    let goplus = Arc::new(Risk::default());
    goplus.0.always(Ok(RiskAssessment {
        source: "goplus".into(),
        flags: vec![RiskFlag {
            code: "honeypot".into(),
            severity: Severity::Critical,
            source: "goplus".into(),
            detail: None,
        }],
    }));
    let hp = Arc::new(Risk::default());
    hp.0.always(Ok(RiskAssessment {
        source: "honeypot_is".into(),
        flags: vec![],
    }));
    let evm = Arc::new(MockEvmRpc {
        chain_id: 8453,
        ..Default::default()
    });
    // EIP-1967 implementation slot set, admin slot empty.
    evm.script
        .push_ok(json!(
            "0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
        ))
        .push_ok(json!(format!("0x{}", "0".repeat(64))));
    let a = app(
        "",
        vec![
            global("goplus", PortHandle::TokenRisk(goplus)),
            global("honeypot_is", PortHandle::TokenRisk(hp)),
            Registration::new(meta("public"))
                .chain_port(ChainId::evm(8453), PortHandle::EvmRpc(evm)),
        ],
    );
    let out = call(&a, "token_check_risk", json!({"asset": USDC_BASE}))
        .await
        .unwrap();
    let d = &out["data"];
    assert_eq!(d["level"], json!("critical"));
    assert_eq!(d["sources"], json!(["goplus", "honeypot_is", "rpc"]));
    let flags = d["flags"].as_array().unwrap();
    assert_eq!(flags[0]["code"], json!("honeypot"));
    assert!(flags
        .iter()
        .any(|f| f["code"] == "proxy_upgradeable" && f["source"] == "rpc"));
}

#[tokio::test]
async fn risk_with_no_answers_is_unknown() {
    let a = app("[routing.defaults]\ntoken_risk = [\"goplus\"]\n", vec![]);
    let out = call(&a, "token_check_risk", json!({"asset": USDC_BASE}))
        .await
        .unwrap();
    assert_eq!(out["data"]["level"], json!("unknown"));
}

// ------------------------------------------------------------------ trade

fn quote(source: &str, buy: u64, tx: bool) -> SwapQuote {
    let min = buy / 10_000 * 9_950;
    SwapQuote {
        chain: ChainId::evm(8453),
        sell_asset: USDC_BASE.parse().unwrap(),
        sell_amount: Amount::from_u128(1_000_000_000, 6),
        buy_asset: ETH_BASE.parse().unwrap(),
        buy_amount: Amount::new(U256::from(buy), 0), // decimals re-stamped by the op
        min_buy_amount: Amount::new(U256::from(min), 0),
        price_impact_bps: None,
        source: source.into(),
        expires_at: Utc::now() + Duration::seconds(30),
        tx: tx.then(|| UnsignedTx::Evm {
            chain_id: 8453,
            to: "0x6a000f20005980200259b80c5102003040001068".into(),
            data: "0xe3ead59e".into(),
            value: "0".into(),
            gas_limit: None,
            max_fee_per_gas: None,
            max_priority_fee_per_gas: None,
            nonce: None,
        }),
        required_approvals: if tx {
            vec![UnsignedTx::Evm {
                chain_id: 8453,
                to: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
                data: format!(
                    "0x095ea7b30000000000000000000000006a000f20005980200259b80c5102003040001068{:064x}",
                    1_000_000_000u64
                ),
                value: "0".into(),
                gas_limit: None,
                max_fee_per_gas: None,
                max_priority_fee_per_gas: None,
                nonce: None,
            }]
        } else {
            vec![]
        },
    }
}

fn swap_app() -> App {
    let velora = Arc::new(Quoter::default());
    velora
        .quote
        .always(Ok(quote("velora", 382_000_000_000_000_000, false)));
    velora
        .build
        .always(Ok(quote("velora", 381_900_000_000_000_000, true)));
    let cow = Arc::new(Quoter::default());
    cow.quote
        .always(Ok(quote("cow", 380_000_000_000_000_000, false)));
    cow.build
        .always(Err(ProviderError::Unsupported("intents only".into())));
    let rpc_meta = Arc::new(Meta::default());
    rpc_meta.0.always(Ok(TokenInfo {
        asset: USDC_BASE.parse().unwrap(),
        decimals: 6,
        symbol: None,
        name: None,
        logo_url: None,
        verified: None,
        source: "rpc".into(),
    }));
    app(
        "",
        vec![
            global("velora", PortHandle::SwapQuote(velora)),
            global("cow", PortHandle::SwapQuote(cow)),
            global("rpc", PortHandle::TokenMetadata(rpc_meta)),
        ],
    )
}

#[tokio::test]
async fn parallel_quotes_best_and_spread() {
    let a = swap_app();
    let out = call(
        &a,
        "trade_get_swap_quote",
        json!({"sell_asset": USDC_BASE, "buy_asset": ETH_BASE, "sell_amount": "1000000000"}),
    )
    .await
    .unwrap();
    let d = &out["data"];
    assert_eq!(d["best"]["source"], json!("velora"));
    assert_eq!(
        d["best"]["buy_amount"]["decimals"],
        json!(18),
        "re-stamped from the chain registry"
    );
    assert_eq!(d["best"]["buy_amount"]["formatted"], json!("0.382"));
    assert_eq!(d["quotes"].as_array().unwrap().len(), 2);
    // (0.382 − 0.380) / 0.382 = 52 bps
    assert_eq!(d["spread_bps"], json!(52));

    let err = call(
        &a,
        "trade_get_swap_quote",
        json!({"sell_asset": USDC_BASE, "buy_asset": "eip155:1/slip44:60", "sell_amount": "1"}),
    )
    .await
    .unwrap_err();
    assert!(err.message.contains("same chain"));
    let err = call(
        &a,
        "trade_get_swap_quote",
        json!({"sell_asset": USDC_BASE, "buy_asset": ETH_BASE, "sell_amount": "1.5"}),
    )
    .await
    .unwrap_err();
    assert!(err.message.contains("base units"));
}

#[tokio::test]
async fn build_requires_taker_enforces_min_out_and_lists_approvals() {
    let a = swap_app();
    let base = json!({"sell_asset": USDC_BASE, "buy_asset": ETH_BASE, "sell_amount": "1000000000"});
    let err = call(&a, "trade_build_swap_tx", base.clone())
        .await
        .unwrap_err();
    assert!(err.message.contains("taker"));

    let mut input = base.clone();
    input["taker"] = json!(TAKER);
    let out = call(&a, "trade_build_swap_tx", input.clone())
        .await
        .unwrap();
    let q = &out["data"]["quote"];
    assert_eq!(
        q["source"],
        json!("velora"),
        "cow cannot build; velora wins"
    );
    assert!(q["tx"].is_object());
    // Allowance reader is not available yet (evm T1.E5), so the approval is kept with a warning.
    assert_eq!(q["required_approvals"].as_array().unwrap().len(), 1);
    assert!(out["data"]["warnings"][0]
        .as_str()
        .unwrap()
        .contains("allowance"));

    input["min_buy_amount"] = json!("381000000000000000"); // above the fresh min_buy
    let err = call(&a, "trade_build_swap_tx", input).await.unwrap_err();
    assert_eq!(err.code, bdm_domain::ErrorCode::StaleData);
}

// ------------------------------------------------------------------ rwa

const TSLA: &str = "eip155:4663/erc20:0x322F0929c4625eD5bAd873c95208D54E1c003b2d";

fn robinhood_rpc(results: &[bdm_ports::PortResult<Value>]) -> Registration {
    let evm = Arc::new(MockEvmRpc {
        chain_id: 4663,
        ..Default::default()
    });
    for r in results {
        match r {
            Ok(v) => evm.script.push_ok(v.clone()),
            Err(e) => evm.script.push_err(e.clone()),
        };
    }
    Registration::new(meta("public")).chain_port(ChainId::evm(4663), PortHandle::EvmRpc(evm))
}

#[tokio::test]
async fn rwa_token_info_official_list_and_lookalike() {
    let paused = json!(format!("0x{}1", "0".repeat(63)));
    // The tool reads the ERC-8056 multiplier (one Multicall3 eth_call) before oraclePaused;
    // the mock pops answers in order, so script the multiplier read as unavailable first.
    let no_multiplier =
        bdm_ports::ProviderError::Unsupported("multicall unavailable in test".into());
    let a = app("", vec![robinhood_rpc(&[Err(no_multiplier), Ok(paused)])]);
    let out = call(&a, "rwa_token_info", json!({"asset": TSLA}))
        .await
        .unwrap();
    let d = &out["data"];
    assert_eq!(d["on_official_list"], json!(true));
    assert_eq!(d["underlying_ticker"], json!("TSLA"));
    assert_eq!(d["issuer"]["id"], json!("robinhood"));
    assert_eq!(d["decimals"], json!(18));
    assert_eq!(d["oracle_paused"], json!(true));

    let fake = "eip155:4663/erc20:0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";
    let out = call(&a, "rwa_token_info", json!({"asset": fake}))
        .await
        .unwrap();
    assert_eq!(out["data"]["on_official_list"], json!(false));
    assert!(out["data"]["warnings"][0]
        .as_str()
        .unwrap()
        .contains("not on the official list"));
}

#[tokio::test]
async fn rwa_price_needs_official_token() {
    let a = app("", vec![robinhood_rpc(&[])]);
    let fake = "eip155:4663/erc20:0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";
    let err = call(&a, "rwa_price", json!({"asset": fake}))
        .await
        .unwrap_err();
    assert_eq!(err.code, bdm_domain::ErrorCode::NotFound);
}
