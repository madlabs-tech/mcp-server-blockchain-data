//! EVM protocol tests against an in-process fake node (no network). Golden cases from
//! plan/PLAN.md "Pitfall golden tests": spoofed token log, 4-topic log, removed log, BSC 18 decimals.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use async_trait::async_trait;
use ems_config::{ChainEntry, ConfigDir, ConfigLoader, EnvSource, Loaded, Registry};
use ems_domain::{AccountAddress, AssetId, AssetRef, Finality, TxStatus, UnsignedTx};
use ems_ports::{
    Capability, Direction, EvmRpc, PortHandle, PortKind, PortResult, ProviderError, Registration,
    Simulator, TokenBalances, TokenMetadata, TransferHistory, TransferQuery, VendorMeta,
};
use ems_protocols::evm::{
    chainlink, erc20, erc8056, fees,
    multicall3::{self, IMulticall3},
    tx,
};
use ems_routing::{InMemoryCounterStore, ProviderRegistry, Router, RouterOptions, RoutingTable};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

// ------------------------------------------------------------------ fake node

type MethodFn = Box<dyn Fn(&Value) -> PortResult<Value> + Send + Sync>;
/// Contract handler: full calldata in, return data out; `None` = revert.
type ContractFn = Box<dyn Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync>;

/// JSON-RPC methods answer from `methods`; `eth_call` executes `contracts`, including Multicall3
/// `aggregate3` (dispatching each sub-call). Unknown contracts behave like an EOA (empty data).
struct FakeEvm {
    chain_id: u64,
    methods: Mutex<HashMap<String, MethodFn>>,
    contracts: Mutex<HashMap<(Address, [u8; 4]), ContractFn>>,
    calls: Mutex<Vec<(String, Value)>>,
}

impl FakeEvm {
    fn new(chain_id: u64) -> Arc<Self> {
        Arc::new(Self {
            chain_id,
            methods: Mutex::default(),
            contracts: Mutex::default(),
            calls: Mutex::default(),
        })
    }

    fn on(&self, method: &str, v: Value) {
        self.on_fn(method, move |_| Ok(v.clone()));
    }

    fn on_fn(&self, method: &str, f: impl Fn(&Value) -> PortResult<Value> + Send + Sync + 'static) {
        self.methods
            .lock()
            .unwrap()
            .insert(method.into(), Box::new(f));
    }

    /// `f` answers calls to `to` with selector `C::SELECTOR`.
    fn contract<C: SolCall>(
        &self,
        to: Address,
        f: impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static,
    ) {
        self.contracts
            .lock()
            .unwrap()
            .insert((to, C::SELECTOR), Box::new(f));
    }

    fn returns<C: SolCall>(&self, to: Address, ret: Vec<u8>) {
        self.contract::<C>(to, move |_| Some(ret.clone()));
    }

    fn params(&self, method: &str) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }

    fn exec(&self, to: Address, data: &[u8]) -> Option<Vec<u8>> {
        if to == multicall3::ADDRESS && data.starts_with(&IMulticall3::aggregate3Call::SELECTOR) {
            let req = IMulticall3::aggregate3Call::abi_decode(data).ok()?;
            let results: Vec<IMulticall3::Result> = req
                .calls
                .iter()
                .map(|c| {
                    let r = self.exec(c.target, &c.callData);
                    IMulticall3::Result {
                        success: r.is_some(),
                        returnData: Bytes::from(r.unwrap_or_default()),
                    }
                })
                .collect();
            return Some(IMulticall3::aggregate3Call::abi_encode_returns(&results));
        }
        let sel: [u8; 4] = data.get(..4)?.try_into().ok()?;
        match self.contracts.lock().unwrap().get(&(to, sel)) {
            Some(f) => f(data),
            None => Some(Vec::new()),
        }
    }
}

#[async_trait]
impl EvmRpc for FakeEvm {
    fn chain_id(&self) -> u64 {
        self.chain_id
    }

    async fn request(&self, method: &str, params: Value) -> PortResult<Value> {
        self.calls
            .lock()
            .unwrap()
            .push((method.into(), params.clone()));
        if let Some(f) = self.methods.lock().unwrap().get(method) {
            return f(&params);
        }
        if method == "eth_call" {
            let to: Address = params[0]["to"].as_str().unwrap().parse().unwrap();
            let data =
                hex::decode(params[0]["data"].as_str().unwrap().trim_start_matches("0x")).unwrap();
            return match self.exec(to, &data) {
                Some(out) => Ok(json!(format!("0x{}", hex::encode(out)))),
                None => Err(ProviderError::Invalid("execution reverted".into())),
            };
        }
        Err(ProviderError::Unsupported(format!(
            "the method {method} does not exist"
        )))
    }
}

// ------------------------------------------------------------------ helpers

fn chain(alias: &str) -> ChainEntry {
    Registry::builtin()
        .unwrap()
        .chains
        .resolve(alias)
        .unwrap()
        .clone()
}

fn addr(byte: u8) -> Address {
    Address::repeat_byte(byte)
}

fn word(a: Address) -> String {
    format!("{:#x}", a.into_word())
}

fn hex_amount(v: u128) -> String {
    format!("{:#x}", alloy_primitives::B256::from(U256::from(v)))
}

fn transfer_log(token: Address, from: Address, to: Address, value: u128, idx: u64) -> Value {
    json!({
        "address": token,
        "topics": [format!("{:#x}", erc20::IERC20::Transfer::SIGNATURE_HASH), word(from), word(to)],
        "data": hex_amount(value),
        "transactionHash": TX,
        "logIndex": format!("0x{idx:x}"),
        "blockNumber": "0x64",
        "blockHash": BLOCK_HASH,
        "removed": false,
    })
}

fn decimals(fake: &FakeEvm, token: Address, d: u8) {
    fake.returns::<erc20::IERC20::decimalsCall>(token, U256::from(d).abi_encode());
}

const TX: &str = "0x00000000000000000000000000000000000000000000000000000000000000aa";
const BLOCK_HASH: &str = "0x00000000000000000000000000000000000000000000000000000000000000bb";

fn mined_tx(fake: &FakeEvm, logs: Vec<Value>, extra_receipt: Value) {
    fake.on(
        "eth_getTransactionByHash",
        json!({"hash": TX, "from": addr(0xf1), "to": addr(0x10), "blockNumber": "0x64", "gasPrice": "0x1"}),
    );
    let mut receipt = json!({
        "status": "0x1", "blockNumber": "0x64", "blockHash": BLOCK_HASH,
        "gasUsed": "0x5208", "effectiveGasPrice": "0x3b9aca00", "logs": logs,
    });
    for (k, v) in extra_receipt.as_object().unwrap() {
        receipt[k] = v.clone();
    }
    fake.on("eth_getTransactionReceipt", receipt);
    fake.on("eth_getBlockByNumber", Value::Null);
    fake.on("eth_blockNumber", json!("0x6e")); // head 110
}

// ------------------------------------------------------------------ tx::get_tx golden tests

#[tokio::test]
async fn get_tx_golden_spoof_nft_removed_and_summed_deltas() {
    let fake = FakeEvm::new(8453);
    let canonical = addr(0x10);
    let spoof = addr(0x20); // a fake "USDC": same symbol, different contract
    let nft = addr(0x30);
    let recipient = addr(0xee);
    let payer = addr(0xf1);
    decimals(&fake, canonical, 6);
    decimals(&fake, spoof, 6);
    fake.returns::<erc20::IERC20::symbolCall>(spoof, "USDC".to_string().abi_encode());

    let mut nft_log = transfer_log(nft, payer, recipient, 0, 2);
    nft_log["topics"]
        .as_array_mut()
        .unwrap()
        .push(json!(hex_amount(7)));
    nft_log["data"] = json!("0x");
    let mut removed = transfer_log(canonical, payer, recipient, 999_000_000, 4);
    removed["removed"] = json!(true);
    let logs = vec![
        transfer_log(canonical, payer, recipient, 100_000_000, 0),
        transfer_log(spoof, payer, recipient, 1_000_000_000_000, 1),
        nft_log,
        transfer_log(canonical, payer, recipient, 50_000_000, 3),
        removed,
    ];
    // Base receipt with the OP-stack l1Fee field.
    mined_tx(&fake, logs, json!({"l1Fee": "0x64"}));

    let t = tx::get_tx(fake.as_ref(), &chain("base"), TX)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(t.status, TxStatus::Success);
    let block = t.block.as_ref().unwrap();
    assert_eq!(
        (block.number, block.hash.as_deref()),
        (100, Some(BLOCK_HASH))
    );
    // 21000 × 1 gwei + 100 wei L1 fee.
    assert_eq!(t.fee.unwrap().raw, U256::from(21_000_000_000_000u64 + 100));
    assert_eq!(t.fee.unwrap().decimals, 18);

    // NFT (4 topics) and removed logs dropped; spoofed token kept under ITS OWN contract.
    assert_eq!(t.transfers.len(), 3);
    let asset = |a: Address| AssetId {
        chain: "eip155:8453".parse().unwrap(),
        asset: AssetRef::Erc20(a),
    };
    assert!(t.transfers.iter().all(|x| x.asset != asset(nft)));
    assert_eq!(t.transfers[1].asset, asset(spoof));

    // Two canonical transfers to the same recipient summed; the spoof is a separate asset row.
    assert_eq!(t.balance_deltas.len(), 2);
    let d = |a: Address| {
        t.balance_deltas
            .iter()
            .find(|d| d.asset == asset(a))
            .unwrap()
    };
    assert_eq!(d(canonical).owner, AccountAddress::Evm(recipient));
    assert_eq!(d(canonical).received().unwrap().format_units(), "150");
    assert_eq!(d(spoof).received().unwrap().format_units(), "1000000");

    // Tags unsupported (null) → confirmations from head: 110 − 100 + 1.
    assert_eq!(t.finality, Finality::Confirmed { confirmations: 11 });
    // decimals read at the tx block, not "latest".
    let call = fake.params("eth_call");
    assert_eq!(call[0][1], json!("0x64"));
}

#[tokio::test]
async fn get_tx_bsc_18_decimal_stablecoin() {
    let fake = FakeEvm::new(56);
    let token = addr(0x55);
    decimals(&fake, token, 18);
    mined_tx(
        &fake,
        vec![transfer_log(
            token,
            addr(1),
            addr(2),
            25 * 10u128.pow(18),
            0,
        )],
        json!({}),
    );
    let t = tx::get_tx(fake.as_ref(), &chain("bsc"), TX)
        .await
        .unwrap()
        .unwrap();
    let amount = t.transfers[0].amount;
    assert_eq!(
        (amount.decimals, amount.format_units().as_str()),
        (18, "25")
    );
    assert_eq!(t.balance_deltas[0].after.format_units(), "25");
}

#[tokio::test]
async fn get_tx_unknown_pending_and_failed() {
    let fake = FakeEvm::new(1);
    fake.on("eth_getTransactionByHash", Value::Null);
    assert!(tx::get_tx(fake.as_ref(), &chain("ethereum"), TX)
        .await
        .unwrap()
        .is_none());

    fake.on(
        "eth_getTransactionByHash",
        json!({"hash": TX, "from": addr(1), "to": null, "blockNumber": null}),
    );
    let t = tx::get_tx(fake.as_ref(), &chain("ethereum"), TX)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (t.status, t.finality),
        (TxStatus::Pending, Finality::Pending)
    );
    assert!(t.to.is_none());

    mined_tx(&fake, vec![], json!({"status": "0x0"}));
    let t = tx::get_tx(fake.as_ref(), &chain("ethereum"), TX)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(t.status, TxStatus::Failed);
    assert!(t.fee.is_some());
}

#[tokio::test]
async fn finality_uses_safe_and_finalized_tags() {
    let fake = FakeEvm::new(8453);
    fake.on_fn("eth_getBlockByNumber", |p| {
        Ok(match p[0].as_str().unwrap() {
            "finalized" => json!({"number": "0x50"}), // 80
            "safe" => json!({"number": "0x5a"}),      // 90
            _ => Value::Null,
        })
    });
    fake.on("eth_blockNumber", json!("0x64")); // 100
    let base = chain("base");
    assert_eq!(
        tx::finality_of(fake.as_ref(), &base, 80).await.unwrap(),
        Finality::Finalized
    );
    assert_eq!(
        tx::finality_of(fake.as_ref(), &base, 85).await.unwrap(),
        Finality::Safe
    );
    // 99 has 2 blocks on top but is above `safe`: only the sequencer's word.
    assert_eq!(
        tx::finality_of(fake.as_ref(), &base, 99).await.unwrap(),
        Finality::Confirmed { confirmations: 2 }
    );
    // A node rejecting the tag (Invalid) falls back to confirmations.
    fake.on_fn("eth_getBlockByNumber", |_| {
        Err(ProviderError::Invalid("unknown block tag".into()))
    });
    assert_eq!(
        tx::finality_of(fake.as_ref(), &base, 80).await.unwrap(),
        Finality::Confirmed { confirmations: 21 }
    );
}

// ------------------------------------------------------------------ logs::scan_transfers

fn range(p: &Value) -> (u64, u64) {
    let n = |k: &str| {
        u64::from_str_radix(p[0][k].as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
    };
    (n("fromBlock"), n("toBlock"))
}

fn query(owner: Address, dir: Direction) -> TransferQuery {
    TransferQuery {
        owner: AccountAddress::Evm(owner),
        direction: dir,
        assets: None,
        from_block: Some(0),
        to_block: Some(1_000), // beyond head: must be capped
        cursor: None,
        limit: 100,
    }
}

#[tokio::test]
async fn scan_caps_at_head_chunks_and_splits_on_range_errors() {
    let fake = FakeEvm::new(1);
    let owner = addr(0xee);
    let token = addr(0x10);
    decimals(&fake, token, 6);
    fake.on("eth_blockNumber", json!("0x23")); // head 35
    fake.on_fn("eth_getLogs", move |p| {
        let (lo, hi) = range(p);
        if hi - lo + 1 > 6 {
            return Err(ProviderError::Invalid("block range too large".into()));
        }
        // One incoming transfer in block 30; a removed one in block 3.
        let mut logs = vec![];
        if (lo..=hi).contains(&30) {
            let mut l = transfer_log(token, addr(1), owner, 5_000_000, 0);
            l["blockNumber"] = json!("0x1e");
            logs.push(l);
        }
        if (lo..=hi).contains(&3) {
            let mut l = transfer_log(token, addr(1), owner, 1, 1);
            l["blockNumber"] = json!("0x3");
            l["removed"] = json!(true);
            logs.push(l);
        }
        Ok(json!(logs))
    });

    let page = ems_protocols::evm::logs::scan_transfers(
        fake.as_ref(),
        &chain("ethereum"),
        &query(owner, Direction::In),
        Some(10),
    )
    .await
    .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].amount.format_units(), "5");
    assert_eq!(page.items[0].block.as_ref().unwrap().number, 30);
    assert!(page.next_cursor.is_none(), "scanned down to from_block");

    let ranges: Vec<_> = fake.params("eth_getLogs").iter().map(range).collect();
    assert_eq!(
        ranges[0],
        (26, 35),
        "starts at max_range, toBlock capped at head 35"
    );
    assert_eq!(ranges[1], (31, 35), "split after the range error");
    assert!(ranges.iter().all(|(lo, hi)| hi <= &35 && lo <= hi));
    // Contiguous coverage of 0..=35 by the successful calls, newest first.
    let ok: Vec<_> = ranges.iter().filter(|(lo, hi)| hi - lo < 6).collect();
    assert_eq!(ok.first().unwrap().1, 35);
    assert_eq!(ok.last().unwrap().0, 0);
    for w in ok.windows(2) {
        assert_eq!(w[1].1 + 1, w[0].0);
    }
    // Incoming filter: topic1 (from) is null, topic2 is the owner.
    let topics = &fake.params("eth_getLogs")[0][0]["topics"];
    assert!(topics[1].is_null());
    assert_eq!(topics[2], json!(word(owner)));
}

#[tokio::test]
async fn scan_pages_with_cursor_and_dedupes_self_transfers() {
    let fake = FakeEvm::new(1);
    let owner = addr(0xee);
    let token = addr(0x10);
    decimals(&fake, token, 6);
    fake.on("eth_blockNumber", json!("0x3e8")); // head 1000
                                                // A self-transfer in every range: matches both the in and out filters.
    fake.on_fn("eth_getLogs", move |p| {
        let (_, hi) = range(p);
        let mut l = transfer_log(token, owner, owner, 1, 0);
        l["blockNumber"] = json!(format!("0x{hi:x}"));
        l["transactionHash"] = json!(format!("0x{hi:064x}"));
        Ok(json!([l]))
    });
    let mut q = query(owner, Direction::Both);
    let page =
        ems_protocols::evm::logs::scan_transfers(fake.as_ref(), &chain("ethereum"), &q, Some(5))
            .await
            .unwrap();
    // 20 calls per page, 2 per chunk (in + out) → 10 chunks of 5 blocks: 1000 down to 951.
    assert_eq!(fake.params("eth_getLogs").len(), 20);
    assert_eq!(page.items.len(), 10, "self-transfer counted once per chunk");
    assert_eq!(page.next_cursor.as_deref(), Some("evm-logs:950"));

    q.cursor = page.next_cursor;
    q.limit = 1;
    let next =
        ems_protocols::evm::logs::scan_transfers(fake.as_ref(), &chain("ethereum"), &q, Some(5))
            .await
            .unwrap();
    assert_eq!(next.items[0].block.as_ref().unwrap().number, 950);
    assert_eq!(next.next_cursor.as_deref(), Some("evm-logs:945"));

    q.cursor = Some("garbage".into());
    assert!(matches!(
        ems_protocols::evm::logs::scan_transfers(fake.as_ref(), &chain("ethereum"), &q, None).await,
        Err(ProviderError::Invalid(_))
    ));
}

// ------------------------------------------------------------------ fees (recorded fixtures)

/// Recorded with `cargo test -p ems-protocols --test evm -- --ignored record_fee_fixtures`.
fn fixture(name: &str) -> Value {
    let path = format!("{}/fixtures/fees/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).expect(&path)).unwrap()
}

fn replay(chain_id: u64, fx: &Value) -> Arc<FakeEvm> {
    let fake = FakeEvm::new(chain_id);
    for (method, result) in fx["responses"].as_object().unwrap() {
        fake.on(method, result.clone());
    }
    fake
}

#[tokio::test]
async fn fee_estimate_fixtures_base_arbitrum_ethereum() {
    for (alias, id, has_l1) in [
        ("base", 8453, true),
        ("arbitrum", 42161, true),
        ("ethereum", 1, false),
    ] {
        let fx = fixture(alias);
        let fake = replay(id, &fx);
        let est = fees::fee_estimate(fake.as_ref(), &chain(alias))
            .await
            .unwrap();
        assert_eq!(est.tiers.len(), 3, "{alias}");
        assert_eq!(est.l1_data_fee.is_some(), has_l1, "{alias}");
        let l1 = est.l1_data_fee.map(|a| a.raw).unwrap_or_default();
        if has_l1 {
            assert!(!l1.is_zero(), "{alias}: recorded L1 fee is non-zero");
        }
        let base = {
            let h = &fx["responses"]["eth_feeHistory"]["baseFeePerGas"];
            let last = h.as_array().unwrap().last().unwrap().as_str().unwrap();
            U256::from_str_radix(last.trim_start_matches("0x"), 16).unwrap()
        };
        let mut prev = U256::ZERO;
        for t in &est.tiers {
            let prio = t.max_priority_fee_per_gas.unwrap();
            assert!(prio >= prev, "{alias}: tiers ordered");
            prev = prio;
            assert_eq!(t.max_fee_per_gas.unwrap(), base * U256::from(2) + prio);
            let total = t.estimated_total.unwrap();
            assert_eq!(total.decimals, 18);
            // l1_data_fee is included in the total.
            assert_eq!(
                total.raw,
                (base + prio) * U256::from(21_000) + l1,
                "{alias}"
            );
        }
        // The L1 fee call hits the right system contract.
        let to: Vec<String> = fake
            .params("eth_call")
            .iter()
            .map(|p| p[0]["to"].as_str().unwrap().to_lowercase())
            .collect();
        match alias {
            "base" => assert_eq!(to, [format!("{:#x}", multicall3::ADDRESS)]),
            "arbitrum" => assert_eq!(to, [format!("{:#x}", fees::NODE_INTERFACE)]),
            _ => assert!(to.is_empty()),
        }
    }
}

/// Records `fixtures/fees/*.json` from public RPCs. Live network: run manually.
#[tokio::test]
#[ignore]
async fn record_fee_fixtures() {
    use ems_adapters::{EvmRpcClient, HttpClient};
    struct Recorder(EvmRpcClient, Mutex<serde_json::Map<String, Value>>);
    #[async_trait]
    impl EvmRpc for Recorder {
        fn chain_id(&self) -> u64 {
            self.0.chain_id()
        }
        async fn request(&self, m: &str, p: Value) -> PortResult<Value> {
            let v = self.0.request(m, p).await?;
            self.1.lock().unwrap().insert(m.into(), v.clone());
            Ok(v)
        }
    }
    for alias in ["base", "arbitrum", "ethereum"] {
        let c = chain(alias);
        let url = c.public_rpc[0].clone();
        let http = HttpClient::new("public", std::time::Duration::from_secs(20));
        let rec = Recorder(
            EvmRpcClient::new(
                http,
                c.id.evm_chain_id().unwrap(),
                ems_config::Redacted::new(url.clone()),
            ),
            Mutex::default(),
        );
        fees::fee_estimate(&rec, &c).await.unwrap();
        let out = json!({
            "_meta": {
                "recorded_at": chrono::Utc::now().to_rfc3339(),
                "rpc": url,
                "request": "evm::fees::fee_estimate (eth_feeHistory 10 blocks [25,50,75]; L1 fee eth_call)",
            },
            "responses": Value::Object(rec.1.into_inner().unwrap()),
        });
        let path = format!("{}/fixtures/fees/{alias}.json", env!("CARGO_MANIFEST_DIR"));
        std::fs::create_dir_all(std::path::Path::new(&path).parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&out).unwrap() + "\n").unwrap();
    }
}

// ------------------------------------------------------------------ readers

#[tokio::test]
async fn erc20_readers_and_symbol_fallbacks() {
    let fake = FakeEvm::new(1);
    let token = addr(0x10);
    let owner = addr(0xee);
    fake.returns::<erc20::IERC20::balanceOfCall>(token, U256::from(42u8).abi_encode());
    fake.returns::<erc20::IERC20::allowanceCall>(token, U256::MAX.abi_encode());
    decimals(&fake, token, 6);
    fake.contract::<erc20::IERC20::symbolCall>(token, |_| None); // reverts
    let rpc = fake.as_ref();
    assert_eq!(
        erc20::balance_of(rpc, token, owner, "0x10").await.unwrap(),
        U256::from(42u8)
    );
    assert_eq!(fake.params("eth_call")[0][1], json!("0x10"));
    assert_eq!(
        erc20::allowance(rpc, token, owner, addr(0x77), "latest")
            .await
            .unwrap(),
        U256::MAX
    );
    assert_eq!(erc20::decimals(rpc, token).await.unwrap(), 6);
    assert_eq!(erc20::symbol(rpc, token).await.unwrap(), None);
    // Not a contract: empty return data.
    assert_eq!(
        erc20::decimals(rpc, addr(0x99)).await,
        Err(ProviderError::NotFound)
    );
}

#[tokio::test]
async fn erc8056_current_and_pending_multiplier() {
    let fake = FakeEvm::new(4663);
    let token = addr(0x42);
    let one = U256::from(10u128.pow(18));
    fake.returns::<erc8056::IERC8056::uiMultiplierCall>(token, one.abi_encode());
    fake.returns::<erc8056::IERC8056::newUIMultiplierCall>(
        token,
        (one * U256::from(2)).abi_encode(),
    );
    fake.returns::<erc8056::IERC8056::effectiveAtCall>(
        token,
        U256::from(1_800_000_000u64).abi_encode(),
    );
    let m = erc8056::ui_multiplier(fake.as_ref(), token).await.unwrap();
    assert_eq!(m.current, one);
    assert_eq!(m.pending, Some((one * U256::from(2), 1_800_000_000)));
    // Change already applied: new == current → nothing pending.
    fake.returns::<erc8056::IERC8056::newUIMultiplierCall>(token, one.abi_encode());
    assert_eq!(
        erc8056::ui_multiplier(fake.as_ref(), token)
            .await
            .unwrap()
            .pending,
        None
    );
    // A plain ERC-20 without the extension.
    fake.contract::<erc8056::IERC8056::uiMultiplierCall>(token, |_| None);
    assert!(matches!(
        erc8056::ui_multiplier(fake.as_ref(), token).await,
        Err(ProviderError::Unsupported(_))
    ));
    // All three reads went through one pinned multicall.
    assert_eq!(fake.params("eth_call").len(), 3);
}

#[tokio::test]
async fn chainlink_latest_round() {
    let fake = FakeEvm::new(1);
    let feed = addr(0x5f);
    let answer = alloy_primitives::I256::try_from(123_456_789i64).unwrap();
    let ret = chainlink::IAggregatorV3::latestRoundDataReturn {
        roundId: alloy_primitives::Uint::from(7u8),
        answer,
        startedAt: U256::from(1u8),
        updatedAt: U256::from(1_700_000_000u64),
        answeredInRound: alloy_primitives::Uint::from(7u8),
    };
    fake.returns::<chainlink::IAggregatorV3::latestRoundDataCall>(
        feed,
        chainlink::IAggregatorV3::latestRoundDataCall::abi_encode_returns(&ret),
    );
    fake.returns::<chainlink::IAggregatorV3::decimalsCall>(feed, U256::from(8u8).abi_encode());
    let r = chainlink::latest_round(fake.as_ref(), feed).await.unwrap();
    assert_eq!(
        (r.round_id, r.answer, r.decimals, r.updated_at),
        (7, answer, 8, 1_700_000_000)
    );
    assert!(matches!(
        chainlink::latest_round(fake.as_ref(), addr(0x01)).await,
        Err(ProviderError::Unsupported(_))
    ));
}

// ------------------------------------------------------------------ rpc pseudo-vendor

fn load(env: &[(&str, &str)]) -> Loaded {
    // Lift the public RPC's rate limit so tests don't wait on the limiter.
    ConfigLoader::new(
        ConfigDir::new("/nonexistent"),
        EnvSource::from_pairs(env.iter().copied()),
    )
    .unwrap()
    .load_texts("[vendors.public]\nlimit = { rps = 100000 }\n", "")
    .unwrap()
}

fn public_reg(fake: &Arc<FakeEvm>) -> Registration {
    Registration::new(VendorMeta {
        id: "public".into(),
        display_name: "fake".into(),
        requires_key: false,
        signup_url: None,
        rpc_features: Default::default(),
    })
    .chain_port(
        ems_domain::ChainId::evm(fake.chain_id),
        PortHandle::EvmRpc(fake.clone()),
    )
}

/// The `rpc` registrations wired on a router whose only `evm_rpc` vendor is the fake.
fn rpc_port<P: PortKind + ?Sized>(
    fake: &Arc<FakeEvm>,
    env: &[(&str, &str)],
    cap: Capability,
) -> Arc<P> {
    let loaded = Arc::new(load(env));
    let router = Router::new(
        RoutingTable {
            config: loaded.clone(),
            registry: ProviderRegistry::new([public_reg(fake)]),
        },
        Arc::new(InMemoryCounterStore::default()),
        RouterOptions::default(),
    );
    let mut registry = ProviderRegistry::new([public_reg(fake)]);
    for r in ems_protocols::evm::rpc_vendor::registrations(&loaded, &router) {
        registry.add(r);
    }
    let chain = ems_domain::ChainId::evm(fake.chain_id);
    let handle = registry.get(cap, Some(&chain), "rpc").cloned().unwrap();
    router.swap(RoutingTable {
        config: loaded,
        registry,
    });
    P::extract(&handle).unwrap()
}

#[tokio::test]
async fn rpc_registers_every_evm_chain_but_not_solana() {
    let fake = FakeEvm::new(1);
    let loaded = load(&[]);
    let router = Router::new(
        RoutingTable {
            config: Arc::new(load(&[])),
            registry: ProviderRegistry::new([public_reg(&fake)]),
        },
        Arc::new(InMemoryCounterStore::default()),
        RouterOptions::default(),
    );
    let regs = ems_protocols::evm::rpc_vendor::registrations(&loaded, &router);
    assert_eq!(regs.len(), 1);
    assert_eq!(regs[0].vendor.id, "rpc");
    let mut per_cap: HashMap<Capability, usize> = HashMap::new();
    for (chain, h) in &regs[0].ports {
        assert_eq!(chain.as_ref().unwrap().namespace(), "eip155");
        *per_cap.entry(h.capability()).or_default() += 1;
    }
    for cap in [
        Capability::TokenBalances,
        Capability::TransferHistory,
        Capability::FeeEstimate,
        Capability::Simulate,
        Capability::TokenMetadata,
    ] {
        assert_eq!(per_cap[&cap], 8, "{cap}");
    }
}

#[tokio::test]
async fn rpc_balances_pinned_to_one_block_via_multicall() {
    let fake = FakeEvm::new(1);
    let owner = addr(0xee);
    let usdc = addr(0x10);
    let junk = addr(0x99); // not a token: no decimals
    fake.on("eth_blockNumber", json!("0x1234"));
    fake.returns::<IMulticall3::getEthBalanceCall>(
        multicall3::ADDRESS,
        U256::from(5u8).abi_encode(),
    );
    fake.returns::<erc20::IERC20::balanceOfCall>(usdc, U256::from(1_500_000u64).abi_encode());
    decimals(&fake, usdc, 6);
    fake.returns::<erc20::IERC20::symbolCall>(usdc, "USDC".to_string().abi_encode());

    let port: Arc<dyn TokenBalances> = rpc_port(&fake, &[], Capability::TokenBalances);
    let chain = ems_domain::ChainId::evm(1);
    let assets = [
        AssetId::native(chain.clone(), 60),
        AssetId {
            chain: chain.clone(),
            asset: AssetRef::Erc20(usdc),
        },
        AssetId {
            chain: chain.clone(),
            asset: AssetRef::Erc20(junk),
        },
    ];
    let b = port
        .balances(&AccountAddress::Evm(owner), Some(&assets))
        .await
        .unwrap();
    assert_eq!(b.len(), 2, "non-token dropped");
    assert_eq!(
        (b[0].amount.raw, b[0].symbol.as_deref()),
        (U256::from(5u8), Some("ETH"))
    );
    assert_eq!(b[1].amount.format_units(), "1.5");
    assert_eq!(b[1].symbol.as_deref(), Some("USDC"));
    let calls = fake.params("eth_call");
    assert_eq!(calls.len(), 1, "one multicall");
    assert_eq!(calls[0][1], json!("0x1234"), "pinned to the block number");

    let other = AssetId::native(ems_domain::ChainId::evm(8453), 60);
    assert!(matches!(
        port.balances(&AccountAddress::Evm(owner), Some(&[other]))
            .await,
        Err(ProviderError::Invalid(_))
    ));
}

#[tokio::test]
async fn rpc_transfers_use_the_smallest_plan_logs_range() {
    let fake = FakeEvm::new(1);
    fake.on("eth_blockNumber", json!("0x64"));
    fake.on("eth_getLogs", json!([]));
    // Alchemy active (key set) → its free-tier 10-block limit applies to the routed scan.
    let port: Arc<dyn TransferHistory> = rpc_port(
        &fake,
        &[("ALCHEMY_API_KEY", "alc_key_123456")],
        Capability::TransferHistory,
    );
    let mut q = query(addr(0xee), Direction::In);
    q.from_block = Some(91);
    port.transfers(&q).await.unwrap();
    let ranges: Vec<_> = fake.params("eth_getLogs").iter().map(range).collect();
    assert_eq!(ranges, [(91, 100)]);
}

#[tokio::test]
async fn rpc_token_metadata_on_chain() {
    let fake = FakeEvm::new(56);
    let token = addr(0x10);
    decimals(&fake, token, 18);
    fake.returns::<erc20::IERC20::symbolCall>(token, "USDT".to_string().abi_encode());
    fake.returns::<erc20::IERC20::nameCall>(token, "Tether USD".to_string().abi_encode());
    let port: Arc<dyn TokenMetadata> = rpc_port(&fake, &[], Capability::TokenMetadata);
    let chain = ems_domain::ChainId::evm(56);
    let m = port
        .metadata(&AssetId {
            chain: chain.clone(),
            asset: AssetRef::Erc20(token),
        })
        .await
        .unwrap();
    assert_eq!(
        (m.decimals, m.symbol.as_deref(), m.name.as_deref()),
        (18, Some("USDT"), Some("Tether USD"))
    );
    assert_eq!(m.source, "rpc");
    let missing = AssetId {
        chain,
        asset: AssetRef::Erc20(addr(0x99)),
    };
    assert_eq!(port.metadata(&missing).await, Err(ProviderError::NotFound));
}

fn transfer_tx(chain_id: u64, to: Address) -> UnsignedTx {
    UnsignedTx::Evm {
        chain_id,
        to: format!("{to:#x}"),
        data: "0x".into(),
        value: "1000".into(),
        gas_limit: Some(50_000),
        max_fee_per_gas: None,
        max_priority_fee_per_gas: None,
        nonce: None,
    }
}

#[tokio::test]
async fn simulate_v1_reports_balance_changes() {
    let fake = FakeEvm::new(1);
    let from = addr(0xf1);
    let to = addr(0x22);
    let token = addr(0x10);
    fake.on("eth_blockNumber", json!("0x10"));
    let native_log = {
        let mut l = transfer_log(
            "0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE"
                .parse()
                .unwrap(),
            from,
            to,
            1000,
            0,
        );
        l.as_object_mut().unwrap().remove("removed");
        l
    };
    fake.on(
        "eth_simulateV1",
        json!([{"calls": [{
            "status": "0x1", "gasUsed": "0x5208", "returnData": "0x",
            "logs": [native_log, transfer_log(token, from, to, 7, 1)],
        }]}]),
    );
    fake.contract::<IMulticall3::getEthBalanceCall>(multicall3::ADDRESS, move |data| {
        let who = IMulticall3::getEthBalanceCall::abi_decode(data)
            .unwrap()
            .addr;
        Some(U256::from(if who == from { 10_000u64 } else { 0 }).abi_encode())
    });
    fake.contract::<erc20::IERC20::balanceOfCall>(token, move |data| {
        let who = erc20::IERC20::balanceOfCall::abi_decode(data)
            .unwrap()
            .owner;
        Some(U256::from(if who == from { 7u8 } else { 0 }).abi_encode())
    });
    decimals(&fake, token, 6);

    let port: Arc<dyn Simulator> = rpc_port(&fake, &[], Capability::Simulate);
    let r = port
        .simulate(&AccountAddress::Evm(from), &transfer_tx(1, to))
        .await
        .unwrap();
    assert!(r.success);
    assert_eq!(r.units_consumed, Some(21_000));
    let delta = |owner: Address, native: bool| {
        r.balance_changes
            .iter()
            .find(|d| d.owner == AccountAddress::Evm(owner) && d.asset.is_native() == native)
            .unwrap()
    };
    assert_eq!(
        (delta(from, true).before.raw, delta(from, true).after.raw),
        (U256::from(10_000u64), U256::from(9_000u64))
    );
    assert_eq!(delta(to, true).received().unwrap().raw, U256::from(1000u64));
    assert_eq!(
        delta(to, false).received().unwrap().format_units(),
        "0.000007"
    );
    // Simulation and balance reads pinned to the same block.
    assert_eq!(fake.params("eth_simulateV1")[0][1], json!("0x10"));
    assert_eq!(fake.params("eth_call")[0][1], json!("0x10"));
    let sent = &fake.params("eth_simulateV1")[0][0]["blockStateCalls"][0]["calls"][0];
    assert_eq!(
        (sent["value"].as_str(), sent["gas"].as_str()),
        (Some("0x3e8"), Some("0xc350"))
    );
}

#[tokio::test]
async fn simulate_falls_back_to_trace_then_eth_call() {
    let fake = FakeEvm::new(1);
    fake.on("eth_blockNumber", json!("0x10"));
    // eth_simulateV1 unknown (fake answers Unsupported), debug_traceCall reverts in-band.
    fake.on(
        "debug_traceCall",
        json!({"gasUsed": "0x6000", "error": "execution reverted", "revertReason": "ERC20: insufficient"}),
    );
    let port: Arc<dyn Simulator> = rpc_port(&fake, &[], Capability::Simulate);
    let from = AccountAddress::Evm(addr(0xf1));
    let r = port
        .simulate(&from, &transfer_tx(1, addr(0x22)))
        .await
        .unwrap();
    assert!(!r.success);
    assert_eq!(r.error.as_deref(), Some("ERC20: insufficient"));
    assert_eq!(r.units_consumed, Some(0x6000));

    // No trace support either → plain eth_call; the target reverts.
    let fake = FakeEvm::new(1);
    fake.on("eth_blockNumber", json!("0x10"));
    fake.on_fn("eth_call", |_| {
        Err(ProviderError::Invalid("execution reverted".into()))
    });
    let port: Arc<dyn Simulator> = rpc_port(&fake, &[], Capability::Simulate);
    let r = port
        .simulate(&from, &transfer_tx(1, addr(0x22)))
        .await
        .unwrap();
    assert!(!r.success);
    assert_eq!(r.error.as_deref(), Some("execution reverted"));
    let tried: Vec<String> = fake
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|(m, _)| m.clone())
        .filter(|m| m != "eth_blockNumber")
        .collect();
    assert_eq!(tried, ["eth_simulateV1", "debug_traceCall", "eth_call"]);

    // Wrong chain is rejected before any call.
    assert!(matches!(
        port.simulate(&from, &transfer_tx(8453, addr(0x22))).await,
        Err(ProviderError::Invalid(_))
    ));
}
