#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
//! neobank-wallet tools (T1.N1–T1.N4) through the App with mock ports. No network.

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use bdm_app::{App, Caller, Profile, ProfileSelection};
use bdm_domain::{
    AccountAddress, Amount, AssetId, BlockRef, ChainId, ErrorCode, FeeEstimate, FeeSpeed, FeeTier,
    Price, Transfer, TransferKind, UnsignedTx,
};
use bdm_ports::{
    BroadcastReceipt, Broadcaster, EvmRpc, FeeOracle, FxRate, Page, PortHandle, PortResult,
    PriceHistory, ProviderError, Registration, SimulationResult, Simulator, SolanaRpc,
    TokenBalance, TransferHistory, TransferQuery,
};
use bdm_testkit::mocks::{MockFxRates, MockPriceFeed, MockTokenBalances};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::{
    str::FromStr,
    sync::{Arc, Mutex},
};

mod common;
use common::call;

const SOL: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";
const WALLET: &str = "0xd8da6bf26964af9d7eed9e03e53415d37aa96045";
const OTHER: &str = "0x00000000000000000000000000000000000000bb";
const KEY_ENV: &[(&str, &str)] = &[("ALCHEMY_API_KEY", "alc_test_key_123456")];

// ------------------------------------------------------------------ harness

type Handler = Box<dyn Fn(&str, &Value) -> PortResult<Value> + Send + Sync>;

struct Rpc(Handler);

#[async_trait]
impl EvmRpc for Rpc {
    fn chain_id(&self) -> u64 {
        0
    }
    async fn request(&self, method: &str, params: Value) -> PortResult<Value> {
        (self.0)(method, &params)
    }
}

#[async_trait]
impl SolanaRpc for Rpc {
    async fn request(&self, method: &str, params: Value) -> PortResult<Value> {
        (self.0)(method, &params)
    }
}

fn rpc(f: impl Fn(&str, &Value) -> PortResult<Value> + Send + Sync + 'static) -> Arc<Rpc> {
    Arc::new(Rpc(Box::new(f)))
}

struct Fixed<T>(PortResult<T>);

#[async_trait]
impl FeeOracle for Fixed<FeeEstimate> {
    async fn fee_estimate(&self) -> PortResult<FeeEstimate> {
        self.0.clone()
    }
}

#[async_trait]
impl Simulator for Fixed<SimulationResult> {
    async fn simulate(&self, _: &AccountAddress, _: &UnsignedTx) -> PortResult<SimulationResult> {
        self.0.clone()
    }
}

#[async_trait]
impl Broadcaster for Fixed<BroadcastReceipt> {
    async fn send_raw(&self, _: &str) -> PortResult<BroadcastReceipt> {
        self.0.clone()
    }
}

#[async_trait]
impl PriceHistory for Fixed<Price> {
    async fn price_at(&self, _: &AssetId, _: &str, at: DateTime<Utc>) -> PortResult<Price> {
        self.0.clone().map(|mut p| {
            p.as_of = at;
            p
        })
    }
}

/// Transfer history that records the cursor it received.
struct History {
    result: Mutex<PortResult<Page<Transfer>>>,
    seen: Mutex<Vec<Option<String>>>,
}

impl History {
    fn new(r: PortResult<Page<Transfer>>) -> Arc<Self> {
        Arc::new(Self {
            result: Mutex::new(r),
            seen: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl TransferHistory for History {
    async fn transfers(&self, q: &TransferQuery) -> PortResult<Page<Transfer>> {
        self.seen.lock().unwrap().push(q.cursor.clone());
        self.result.lock().unwrap().clone()
    }
}

fn reg(vendor: &str, ports: Vec<(Option<ChainId>, PortHandle)>) -> Registration {
    let mut r = Registration::new(common::meta(vendor));
    r.ports = ports;
    r
}

fn chain(s: &str) -> Option<ChainId> {
    Some(s.parse().unwrap())
}

fn app(env: &[(&str, &str)], regs: Vec<Registration>) -> App {
    common::app(env, "[server]\ntool_profile = \"all\"\n", regs)
}

fn block(n: u64, ts: i64) -> Option<BlockRef> {
    Some(BlockRef {
        number: n,
        hash: None,
        timestamp: DateTime::from_timestamp(ts, 0),
    })
}

// ------------------------------------------------------------------ catalog

#[tokio::test]
async fn tools_are_registered_with_profiles() {
    let a = app(&[], vec![]);
    let names = [
        "chain_list",
        "chain_finality",
        "provider_health",
        "wallet_get_balances",
        "wallet_get_transfers",
        "address_validate",
        "tx_get",
        "tx_status",
        "tx_estimate_fee",
        "tx_simulate",
        "tx_build_transfer",
        "tx_broadcast",
        "fiat_get_fx_rate",
        "neobank_card_funding_status",
        "neobank_get_ledger",
    ];
    for n in names {
        let op = a.catalog().get(n).unwrap_or_else(|| panic!("{n} missing"));
        assert!(
            op.description().len() > 80,
            "{n} needs an LLM-grade description"
        );
        assert_eq!(op.read_only(), n != "tx_broadcast", "{n}");
        assert!(
            op.profiles().contains(&Profile::Neobank) || n.starts_with("neobank"),
            "{n}"
        );
    }
    for n in names
        .iter()
        .filter(|n| n.starts_with("chain_") || **n == "provider_health")
    {
        assert_eq!(a.catalog().get(n).unwrap().profiles(), Profile::ALL);
    }
    let trading = Caller {
        client: None,
        profile: Some(ProfileSelection::Profile(Profile::Trading)),
    };
    let visible: Vec<&str> = a.visible(&trading).iter().map(|o| o.name()).collect();
    assert!(visible.contains(&"tx_broadcast") && visible.contains(&"chain_list"));
    assert!(!visible.contains(&"neobank_get_ledger") && !visible.contains(&"fiat_get_fx_rate"));
}

// ------------------------------------------------------------------ chain

#[tokio::test]
async fn chain_list_matrix_and_provider_health() {
    let balances = Arc::new(MockTokenBalances::default());
    let a = app(
        &[],
        vec![reg(
            "rpc",
            vec![(chain("eip155:8453"), PortHandle::TokenBalances(balances))],
        )],
    );
    let out = call(&a, "chain_list", json!({"chain": "base"}))
        .await
        .unwrap();
    let base = &out["data"]["chains"][0];
    assert_eq!(base["id"], "eip155:8453");
    assert_eq!(base["finality"]["policy"], "tags");
    let caps = base["capabilities"].as_array().unwrap();
    let tb = caps
        .iter()
        .find(|c| c["capability"] == "token_balances")
        .unwrap();
    assert_eq!(tb["order"], json!(["alchemy", "moralis", "rpc"]));
    assert_eq!(tb["usable"], json!(["rpc"]));
    assert_eq!(tb["order_level"], "built_in");
    assert!(!caps.iter().any(|c| c["capability"] == "solana_rpc"));

    let all = call(&a, "chain_list", json!({})).await.unwrap();
    assert_eq!(all["data"]["chains"].as_array().unwrap().len(), 9);

    let h = call(&a, "provider_health", json!({"chain": "solana"}))
        .await
        .unwrap();
    let vendors = h["data"]["vendors"].as_array().unwrap();
    let alchemy = vendors.iter().find(|v| v["vendor"] == "alchemy").unwrap();
    assert_eq!(alchemy["config"]["status"], "missing_key");
    assert_eq!(alchemy["runtime"]["breaker"], "closed");
    assert_eq!(
        h["data"]["orders"][SOL]["token_balances"]["vendors"],
        json!(["helius", "rpc"])
    );
}

#[tokio::test]
async fn chain_finality_evm_tags_and_provider_lag() {
    let node = |head: u64| {
        rpc(move |m, p| match m {
            "eth_blockNumber" => Ok(json!(format!("{head:#x}"))),
            "eth_getBlockByNumber" => {
                let n = match p[0].as_str().unwrap() {
                    "latest" => head,
                    "safe" => head - 30,
                    "finalized" => head - 60,
                    _ => return Err(ProviderError::Invalid("tag".into())),
                };
                Ok(
                    json!({"number": format!("{n:#x}"), "hash": format!("0x{n:064x}"), "timestamp": "0x6553f100"}),
                )
            }
            _ => Err(ProviderError::Unsupported(m.into())),
        })
    };
    let a = app(
        KEY_ENV,
        vec![
            reg(
                "alchemy",
                vec![(chain("eip155:8453"), PortHandle::EvmRpc(node(1000)))],
            ),
            reg(
                "public",
                vec![(chain("eip155:8453"), PortHandle::EvmRpc(node(996)))],
            ),
        ],
    );
    let out = call(
        &a,
        "chain_finality",
        json!({"chain": "base", "per_provider": true}),
    )
    .await
    .unwrap();
    let d = &out["data"];
    assert_eq!(d["levels"][0]["level"], "latest");
    assert_eq!(d["levels"][0]["block"]["number"], 1000);
    assert_eq!(d["levels"][1]["behind_head"], 30);
    assert_eq!(d["levels"][2]["level"], "finalized");
    assert_eq!(d["levels"][2]["behind_head"], 60);
    let heads = d["provider_heads"].as_array().unwrap();
    let public = heads.iter().find(|h| h["vendor"] == "public").unwrap();
    assert_eq!(
        (public["head"].as_u64(), public["lag"].as_u64()),
        (Some(996), Some(4))
    );
}

#[tokio::test]
async fn chain_finality_solana_commitments() {
    let node = rpc(|m, p| match (m, p[0]["commitment"].as_str()) {
        ("getSlot", Some("processed")) => Ok(json!(500)),
        ("getSlot", Some("confirmed")) => Ok(json!(499)),
        ("getSlot", Some("finalized")) => Ok(json!(468)),
        _ => Err(ProviderError::Unsupported(m.into())),
    });
    let a = app(
        &[],
        vec![reg(
            "public",
            vec![(chain(SOL), PortHandle::SolanaRpc(node))],
        )],
    );
    let out = call(&a, "chain_finality", json!({"chain": "solana"}))
        .await
        .unwrap();
    let levels = &out["data"]["levels"];
    assert_eq!(levels[0]["level"], "processed");
    assert_eq!(levels[2]["block"]["number"], 468);
    assert_eq!(levels[2]["behind_head"], 32);
    assert_eq!(out["data"]["policy"]["policy"], "commitment");
}

// ------------------------------------------------------------------ wallet

#[tokio::test]
async fn balances_cross_chain_with_partial_failure() {
    let eth: AssetId = "eip155:8453/slip44:60".parse().unwrap();
    let usdc: AssetId = "eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
        .parse()
        .unwrap();
    let base = Arc::new(MockTokenBalances::default());
    base.script.always(Ok(vec![
        TokenBalance {
            asset: usdc.clone(),
            amount: Amount::from_u128(12_345_678, 6),
            symbol: Some("USDC".into()),
            token_account: None,
        },
        TokenBalance {
            asset: eth,
            amount: Amount::from_u128(1, 18),
            symbol: None,
            token_account: None,
        },
    ]));
    let arb = Arc::new(MockTokenBalances::default());
    arb.script
        .always(Err(ProviderError::Transient("boom".into())));
    let a = app(
        &[],
        vec![reg(
            "rpc",
            vec![
                (chain("eip155:8453"), PortHandle::TokenBalances(base)),
                (chain("eip155:42161"), PortHandle::TokenBalances(arb)),
            ],
        )],
    );
    let out = call(
        &a,
        "wallet_get_balances",
        json!({"address": WALLET, "chains": ["base", "arbitrum"]}),
    )
    .await
    .unwrap();
    let chains = out["data"]["chains"].as_array().unwrap();
    let b = &chains[0]["balances"];
    assert_eq!(b[0]["native"], true, "native first");
    assert_eq!(b[0]["symbol"], "ETH");
    assert_eq!(
        b[1]["amount"],
        json!({"raw": "12345678", "decimals": 6, "formatted": "12.345678"})
    );
    assert_eq!(chains[1]["error"]["code"], "ALL_PROVIDERS_FAILED");
    assert_eq!(
        out["data"]["owner"],
        "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"
    );

    // stablecoins_only keeps native + registry stablecoins (registry empty on this branch)
    let out = call(
        &a,
        "wallet_get_balances",
        json!({"address": WALLET, "chains": ["base"], "stablecoins_only": true}),
    )
    .await
    .unwrap();
    let b = out["data"]["chains"][0]["balances"].as_array().unwrap();
    assert!(b
        .iter()
        .all(|r| r["native"] == true || r["stablecoin"].is_object()));

    // every chain failing is an error; wrong family is rejected
    let err = call(
        &a,
        "wallet_get_balances",
        json!({"address": WALLET, "chains": ["arbitrum"]}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::AllProvidersFailed);
    let err = call(
        &a,
        "wallet_get_balances",
        json!({"address": WALLET, "chains": ["solana"]}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
}

fn transfer(to: &str, from: &str, n: u64) -> Transfer {
    Transfer {
        chain: ChainId::evm(1),
        tx_hash: format!("0x{n:064x}"),
        log_index: Some(0),
        kind: TransferKind::Native,
        asset: "eip155:1/slip44:60".parse().unwrap(),
        from: Some(from.parse().unwrap()),
        to: to.parse().unwrap(),
        amount: Amount::from_u128(2_000_000_000_000_000_000, 18),
        block: block(n, 1_700_000_000),
    }
}

#[tokio::test]
async fn transfers_cursor_is_pinned_to_its_vendor() {
    let page = Page {
        items: vec![transfer(WALLET, OTHER, 1)],
        next_cursor: Some("p2".into()),
    };
    let alchemy = History::new(Ok(page.clone()));
    let fallback = History::new(Ok(page));
    let eth = chain("eip155:1");
    let a = app(
        KEY_ENV,
        vec![
            reg(
                "alchemy",
                vec![(eth.clone(), PortHandle::TransferHistory(alchemy.clone()))],
            ),
            reg(
                "rpc",
                vec![(eth, PortHandle::TransferHistory(fallback.clone()))],
            ),
        ],
    );
    let first = call(
        &a,
        "wallet_get_transfers",
        json!({"address": WALLET, "chain": "ethereum"}),
    )
    .await
    .unwrap();
    assert_eq!(first["data"]["next_cursor"], "alchemy:p2");
    assert_eq!(first["meta"]["provider"], "alchemy");
    let second = call(
        &a,
        "wallet_get_transfers",
        json!({"address": WALLET, "chain": "ethereum", "cursor": "alchemy:p2", "limit": 10}),
    )
    .await
    .unwrap();
    assert_eq!(second["data"]["cursor_reset"], false);
    assert_eq!(
        alchemy.seen.lock().unwrap().clone(),
        vec![None, Some("p2".into())]
    );

    // issuing vendor down → next vendor serves the first page, flagged
    *alchemy.result.lock().unwrap() = Err(ProviderError::Transient("down".into()));
    let third = call(
        &a,
        "wallet_get_transfers",
        json!({"address": WALLET, "chain": "ethereum", "cursor": "alchemy:p2"}),
    )
    .await
    .unwrap();
    assert_eq!(third["data"]["cursor_reset"], true);
    assert_eq!(third["data"]["next_cursor"], "rpc:p2");
    assert_eq!(fallback.seen.lock().unwrap().clone(), vec![None]);
}

// ------------------------------------------------------------------ address_validate

fn code_node(code: &'static str) -> Arc<Rpc> {
    rpc(move |m, _| match m {
        "eth_getCode" => Ok(json!(code)),
        "eth_call" => Err(ProviderError::Invalid("execution reverted".into())),
        _ => Err(ProviderError::Unsupported(m.into())),
    })
}

#[tokio::test]
async fn address_validate_evm() {
    let a = app(
        &[],
        vec![reg(
            "public",
            vec![
                (chain("eip155:8453"), PortHandle::EvmRpc(code_node("0x"))),
                (
                    chain("eip155:1"),
                    PortHandle::EvmRpc(code_node("0x6080604052")),
                ),
                (
                    chain("eip155:10"),
                    PortHandle::EvmRpc(code_node(
                        "0xef01001111111111111111111111111111111111111111",
                    )),
                ),
            ],
        )],
    );
    // EOA on Base, but a contract on Ethereum → wrong-network warning
    let out = call(
        &a,
        "address_validate",
        json!({"address": WALLET, "chain": "base"}),
    )
    .await
    .unwrap();
    let d = &out["data"];
    assert_eq!(
        (d["valid"].as_bool(), d["kind"].as_str()),
        (Some(true), Some("eoa"))
    );
    assert_eq!(d["checksum"], "not_checksummed");
    assert_eq!(
        d["normalized"],
        "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"
    );
    assert_eq!(d["code_on_other_chains"], json!(["eip155:1"]));
    assert!(d["warnings"].to_string().contains("unrecoverable"));

    // EIP-7702 delegation flagged separately
    let out = call(
        &a,
        "address_validate",
        json!({"address": "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045", "chain": "optimism"}),
    )
    .await
    .unwrap();
    assert_eq!(out["data"]["kind"], "eip7702_delegated");
    assert_eq!(out["data"]["checksum"], "valid");
    assert_eq!(
        out["data"]["delegate"],
        "0x1111111111111111111111111111111111111111"
    );

    // plain contract (not Safe / 4337)
    let out = call(
        &a,
        "address_validate",
        json!({"address": WALLET, "chain": "ethereum"}),
    )
    .await
    .unwrap();
    assert_eq!(out["data"]["kind"], "contract");

    // bad checksum and wrong family are reported, not thrown
    let out = call(
        &a,
        "address_validate",
        json!({"address": "0xD8dA6BF26964aF9D7eEd9e03E53415D37aA96045", "chain": "base"}),
    )
    .await
    .unwrap();
    assert_eq!(out["data"]["valid"], false);
    assert!(out["data"]["errors"][0]
        .as_str()
        .unwrap()
        .contains("checksum"));
    let out = call(
        &a,
        "address_validate",
        json!({"address": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", "chain": "base"}),
    )
    .await
    .unwrap();
    assert!(out["data"]["errors"].to_string().contains("Solana address"));
}

#[tokio::test]
async fn address_validate_solana_token_account() {
    let node = rpc(|m, p| match (m, p[0].as_str()) {
        ("getAccountInfo", Some("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v")) => Ok(json!({
            "context": {"slot": 1},
            "value": {
                "owner": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
                "executable": false,
                "data": {"parsed": {"type": "account", "info": {
                    "mint": "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB",
                    "owner": "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM",
                    "state": "initialized"
                }}}
            }
        })),
        ("getAccountInfo", _) => Ok(json!({"context": {"slot": 1}, "value": null})),
        _ => Err(ProviderError::Unsupported(m.into())),
    });
    let a = app(
        &[],
        vec![reg(
            "public",
            vec![(chain(SOL), PortHandle::SolanaRpc(node))],
        )],
    );
    let out = call(
        &a,
        "address_validate",
        json!({"address": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", "chain": "solana"}),
    )
    .await
    .unwrap();
    let d = &out["data"];
    assert_eq!(d["kind"], "token_account");
    assert_eq!(
        d["token_account"]["owner"],
        "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM"
    );
    assert!(d["warnings"][0].as_str().unwrap().contains("owner wallet"));

    let out = call(
        &a,
        "address_validate",
        json!({"address": "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM", "chain": "solana"}),
    )
    .await
    .unwrap();
    assert_eq!(out["data"]["kind"], "unfunded_wallet");
}

// ------------------------------------------------------------------ tx

fn fee_estimate(chain: ChainId) -> FeeEstimate {
    FeeEstimate {
        chain,
        tiers: vec![FeeTier {
            speed: FeeSpeed::Standard,
            max_fee_per_gas: Some(alloy_primitives::U256::from(2_000_000_000u64)),
            max_priority_fee_per_gas: Some(alloy_primitives::U256::from(1_000_000u64)),
            compute_unit_price_micro_lamports: None,
            estimated_total: Some(Amount::from_u128(42_000_000_000_000, 18)),
            estimated_total_fiat: None,
        }],
        l1_data_fee: None,
        tip: None,
        as_of: Utc::now(),
    }
}

fn price(asset: &str, v: &str) -> Price {
    Price {
        asset: asset.parse().unwrap(),
        currency: "USD".into(),
        value: Decimal::from_str(v).unwrap(),
        as_of: Utc::now(),
        source: "coingecko".into(),
        liquidity_usd: None,
    }
}

#[tokio::test]
async fn estimate_fee_with_and_without_fiat() {
    let base = chain("eip155:8453");
    let fee = Arc::new(Fixed(Ok(fee_estimate(base.clone().unwrap()))));
    let prices = Arc::new(MockPriceFeed::default());
    prices
        .script
        .push_ok(price("eip155:8453/slip44:60", "2500"));
    prices
        .script
        .always(Err(ProviderError::Transient("down".into())));
    let a = app(
        &[],
        vec![
            reg("rpc", vec![(base, PortHandle::FeeEstimate(fee))]),
            reg("defillama", vec![(None, PortHandle::Price(prices))]),
        ],
    );
    let out = call(&a, "tx_estimate_fee", json!({"chain": "base"}))
        .await
        .unwrap();
    let tier = &out["data"]["estimate"]["tiers"][0];
    assert_eq!(tier["estimated_total_fiat"]["amount"], "0.105");
    assert_eq!(tier["estimated_total_fiat"]["source"], "coingecko");
    let out = call(
        &a,
        "tx_estimate_fee",
        json!({"chain": "base", "currency": "usd"}),
    )
    .await
    .unwrap();
    assert!(out["data"]["estimate"]["tiers"][0]["estimated_total_fiat"].is_null());
    assert!(out["data"]["native_price"].is_null(), "unknown, never 0");
}

fn sim_ok() -> Arc<Fixed<SimulationResult>> {
    Arc::new(Fixed(Ok(SimulationResult {
        success: true,
        error: None,
        units_consumed: Some(21_000),
        balance_changes: vec![],
        logs: vec![],
    })))
}

#[tokio::test]
async fn build_evm_native_transfer_is_filled_and_simulated() {
    let base = chain("eip155:8453");
    let node = rpc(|m, _| match m {
        "eth_getTransactionCount" => Ok(json!("0x5")),
        "eth_estimateGas" => Ok(json!("0x5208")),
        _ => Err(ProviderError::Unsupported(m.into())),
    });
    let a = app(
        &[],
        vec![
            reg("public", vec![(base.clone(), PortHandle::EvmRpc(node))]),
            reg(
                "rpc",
                vec![
                    (
                        base.clone(),
                        PortHandle::FeeEstimate(Arc::new(Fixed(Ok(fee_estimate(
                            base.clone().unwrap(),
                        ))))),
                    ),
                    (base, PortHandle::Simulate(sim_ok())),
                ],
            ),
        ],
    );
    let out = call(
        &a,
        "tx_build_transfer",
        json!({"chain": "base", "from": WALLET, "to": OTHER, "amount": "0.5"}),
    )
    .await
    .unwrap();
    let d = &out["data"];
    assert_eq!(d["ready"], true);
    let tx = &d["tx"];
    assert_eq!(tx["family"], "evm");
    assert_eq!(tx["chain_id"], 8453);
    assert_eq!(tx["value"], "500000000000000000");
    assert_eq!(tx["data"], "0x");
    assert_eq!(
        (tx["nonce"].as_u64(), tx["gas_limit"].as_u64()),
        (Some(5), Some(21_000))
    );
    assert_eq!(tx["max_fee_per_gas"], "2000000000");
    assert_eq!(d["amount"]["raw"], "500000000000000000");

    // too many decimals is rejected, never rounded
    let err = call(
        &a,
        "tx_build_transfer",
        json!({"chain": "base", "from": WALLET, "to": OTHER, "amount": "0.0000000000000000001"}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
}

#[tokio::test]
async fn build_solana_native_transfer_with_memo() {
    let from = "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM";
    let to = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    let blockhash = bs58::encode([7u8; 32]).into_string();
    let bh = blockhash.clone();
    let node = rpc(move |m, _| match m {
        "getAccountInfo" => Ok(json!({"context": {"slot": 1}, "value": null})),
        "getLatestBlockhash" => Ok(
            json!({"context": {"slot": 1}, "value": {"blockhash": bh, "lastValidBlockHeight": 1234}}),
        ),
        _ => Err(ProviderError::Unsupported(m.into())),
    });
    let a = app(
        &[],
        vec![
            reg("public", vec![(chain(SOL), PortHandle::SolanaRpc(node))]),
            reg("rpc", vec![(chain(SOL), PortHandle::Simulate(sim_ok()))]),
        ],
    );
    let out = call(
        &a,
        "tx_build_transfer",
        json!({"chain": "solana", "from": from, "to": to, "amount": "0.001", "memo": "invoice-42"}),
    )
    .await
    .unwrap();
    let tx = &out["data"]["tx"];
    assert_eq!(tx["family"], "solana");
    assert_eq!(tx["last_valid_block_height"], 1234);
    assert_eq!(tx["recent_blockhash"], blockhash);
    let msg = B64.decode(tx["message_base64"].as_str().unwrap()).unwrap();
    // header: 1 signer, 0 readonly signed, 2 readonly unsigned (system + memo); 4 keys
    assert_eq!(&msg[..4], &[1, 0, 2, 4]);
    assert_eq!(
        &msg[4..36],
        bs58::decode(from).into_vec().unwrap().as_slice()
    );
    assert!(msg.windows(10).any(|w| w == b"invoice-42"));
    let lamports = 1_000_000u64.to_le_bytes();
    assert!(msg
        .windows(12)
        .any(|w| w[..4] == [2, 0, 0, 0] && w[4..] == lamports));
    assert_eq!(out["data"]["ready"], true);
}

#[tokio::test]
async fn broadcast_rejects_secrets_and_fans_out() {
    let raw = format!("0x02f8{}", "ab".repeat(120));
    let receipt = |h: &str| {
        Arc::new(Fixed(Ok(BroadcastReceipt {
            tx_hash: h.into(),
            private: false,
        })))
    };
    let eth = chain("eip155:1");
    let base = chain("eip155:8453");
    let local = format!(
        "{:#x}",
        alloy_primitives::keccak256(
            (0..raw.len() - 2)
                .step_by(2)
                .map(|i| u8::from_str_radix(&raw[2 + i..4 + i], 16).unwrap())
                .collect::<Vec<u8>>()
        )
    );
    let failing = Arc::new(Fixed::<BroadcastReceipt>(Err(ProviderError::Invalid(
        "nonce too low".into(),
    ))));
    let a = app(
        KEY_ENV,
        vec![
            reg(
                "alchemy",
                vec![(base.clone(), PortHandle::Broadcast(receipt(&local)))],
            ),
            reg("public", vec![(base, PortHandle::Broadcast(failing))]),
            reg(
                "flashbots",
                vec![(eth, PortHandle::PrivateRelay(receipt(&local)))],
            ),
        ],
    );
    for secret in [
        "0x4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318",
        "legal winner thank year wave sausage worth useful legal winner thank yellow",
    ] {
        let err = call(
            &a,
            "tx_broadcast",
            json!({"chain": "base", "signed_tx": secret}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        assert!(err.message.contains("private key") && !err.message.contains(secret));
    }
    let out = call(
        &a,
        "tx_broadcast",
        json!({"chain": "base", "signed_tx": raw}),
    )
    .await
    .unwrap();
    let d = &out["data"];
    assert_eq!(d["tx_hash"], local);
    assert_eq!(d["accepted_by"], json!(["alchemy"]));
    assert_eq!(d["rejected"][0]["vendor"], "public");
    assert!(d["hash_mismatch"].is_null());

    let err = call(
        &a,
        "tx_broadcast",
        json!({"chain": "base", "signed_tx": raw, "private": true}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::UnsupportedCapability);
    assert!(err.message.contains("no private mempool"));
    let out = call(
        &a,
        "tx_broadcast",
        json!({"chain": "ethereum", "signed_tx": raw, "private": true}),
    )
    .await
    .unwrap();
    assert_eq!(out["data"]["private"], true);
    assert_eq!(out["data"]["accepted_by"], json!(["flashbots"]));
}

// ------------------------------------------------------------------ neobank

#[tokio::test]
async fn fx_rate_labels_weekend_and_identity() {
    let fx = Arc::new(MockFxRates::default());
    fx.script.always(Ok(FxRate {
        base: "EUR".into(),
        quote: "USD".into(),
        rate: Decimal::from_str("1.1702").unwrap(),
        business_date: NaiveDate::from_ymd_opt(2026, 9, 18).unwrap(),
        source: "frankfurter".into(),
        as_of: Utc::now(),
    }));
    let a = app(
        &[],
        vec![reg("frankfurter", vec![(None, PortHandle::Fx(fx))])],
    );
    let out = call(
        &a,
        "fiat_get_fx_rate",
        json!({"base": "eur", "quote": "USD", "date": "2026-09-20"}),
    )
    .await
    .unwrap();
    let d = &out["data"];
    assert_eq!(d["rate"], "1.1702");
    assert_eq!(d["business_date"], "2026-09-18");
    assert_eq!(d["is_requested_date"], false);
    assert_eq!(out["meta"]["provider"], "frankfurter");
    let same = call(
        &a,
        "fiat_get_fx_rate",
        json!({"base": "USD", "quote": "usd"}),
    )
    .await
    .unwrap();
    assert_eq!(same["data"]["rate"], "1");
    let err = call(
        &a,
        "fiat_get_fx_rate",
        json!({"base": "EURO", "quote": "USD"}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
}

#[tokio::test]
async fn ledger_rows_are_valued_at_block_time() {
    let eth = chain("eip155:1");
    let history = History::new(Ok(Page {
        items: vec![transfer(WALLET, OTHER, 1), transfer(OTHER, WALLET, 2)],
        next_cursor: None,
    }));
    let hist_price = Arc::new(Fixed(Ok(price("eip155:1/slip44:60", "1800.5"))));
    let a = app(
        &[],
        vec![
            reg("rpc", vec![(eth, PortHandle::TransferHistory(history))]),
            reg(
                "defillama",
                vec![(None, PortHandle::PriceHistory(hist_price))],
            ),
        ],
    );
    let out = call(
        &a,
        "neobank_get_ledger",
        json!({"address": WALLET, "chain": "ethereum"}),
    )
    .await
    .unwrap();
    let rows = out["data"]["rows"].as_array().unwrap();
    assert_eq!(rows[0]["direction"], "credit");
    assert_eq!(
        rows[0]["counterparty"],
        "0x00000000000000000000000000000000000000bb"
    );
    assert_eq!(rows[1]["direction"], "debit");
    let mv = &rows[0]["market_value"];
    assert_eq!(mv["amount"], "3601");
    assert_eq!(mv["as_of"], "2023-11-14T22:13:20Z", "valued at block time");
    assert_eq!(mv["source"], "coingecko");
    assert_eq!(rows[0]["block_time"], "2023-11-14T22:13:20Z");
    assert_eq!(out["data"]["currency"], "USD");
}

#[tokio::test]
async fn balances_report_family_mismatch_per_chain() {
    // An EVM address asked on base + solana: solana gets an error row, base still answers.
    let base = Arc::new(MockTokenBalances::default());
    base.script.always(Ok(vec![TokenBalance {
        asset: "eip155:8453/slip44:60".parse().unwrap(),
        amount: Amount::from_u128(1, 18),
        symbol: None,
        token_account: None,
    }]));
    let a = app(
        &[],
        vec![reg(
            "rpc",
            vec![(chain("eip155:8453"), PortHandle::TokenBalances(base))],
        )],
    );
    let v = call(
        &a,
        "wallet_get_balances",
        json!({"chains": ["base", "solana"], "address": "0xd8da6bf26964af9d7eed9e03e53415d37aa96045"}),
    )
    .await
    .expect("one bad chain must not fail the whole request");
    let chains = v["data"]["chains"].as_array().unwrap();
    let sol = chains
        .iter()
        .find(|c| c["chain"].as_str().unwrap().starts_with("solana:"))
        .unwrap();
    assert_eq!(sol["error"]["code"], "INVALID_INPUT");
    let b = chains.iter().find(|c| c["chain"] == "eip155:8453").unwrap();
    assert_eq!(b["balances"].as_array().unwrap().len(), 1);

    // Every chain wrong → still a plain error.
    let err = call(
        &a,
        "wallet_get_balances",
        json!({"chains": ["solana"], "address": "0xd8da6bf26964af9d7eed9e03e53415d37aa96045"}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
}
