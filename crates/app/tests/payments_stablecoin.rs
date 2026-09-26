#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
//! payments / stablecoin / compliance operations through the App (mock ports, no network).

use async_trait::async_trait;
use bdm_app::{App, Caller, Profile, ProfileSelection};
use bdm_domain::{AccountId, ChainId, ErrorCode};
use bdm_ports::{PortHandle, PortResult, Registration, SanctionsScreener, ScreenResult};
use bdm_testkit::mocks::MockEvmRpc;
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;

mod common;
use common::meta;

fn app(regs: Vec<Registration>) -> App {
    common::app(&[], "", regs)
}

struct FixedScreen(bool, &'static str);

#[async_trait]
impl SanctionsScreener for FixedScreen {
    async fn screen(&self, _a: &AccountId) -> PortResult<ScreenResult> {
        Ok(ScreenResult {
            sanctioned: self.0,
            source: self.1.into(),
            detail: None,
            as_of: Utc::now(),
        })
    }
}

fn screener(vendor: &'static str, hit: bool) -> Registration {
    Registration::new(meta(vendor))
        .global_port(PortHandle::Sanctions(Arc::new(FixedScreen(hit, vendor))))
}

const WHO: &str = "0xd8da6bf26964af9d7eed9e03e53415d37aa96045";

async fn call(app: &App, tool: &str, input: Value) -> Result<Value, bdm_domain::DomainError> {
    app.call(tool, input, Caller::local()).await
}

#[tokio::test]
async fn profiles_expose_the_tools() {
    let a = app(vec![]);
    let names = |p| -> Vec<&'static str> {
        let c = Caller {
            client: None,
            profile: Some(ProfileSelection::Profile(p)),
        };
        a.visible(&c).iter().map(|o| o.name()).collect()
    };
    for p in [Profile::Payments, Profile::Neobank] {
        let v = names(p);
        for t in [
            "payments_verify_transfer",
            "payments_list_deposits",
            "payments_build_request",
            "stablecoin_resolve",
            "stablecoin_check_restrictions",
            "stablecoin_peg",
            "compliance_screen_address",
        ] {
            assert!(v.contains(&t), "{t} missing from {p:?}");
        }
    }
    let trading = names(Profile::Trading);
    assert!(trading.contains(&"stablecoin_peg"));
    assert!(!trading.contains(&"payments_verify_transfer"));
}

#[tokio::test]
async fn build_request_x402_and_family_checks() {
    let a = app(vec![]);
    let out = call(
        &a,
        "payments_build_request",
        json!({"chain": "base", "token": "USDC", "recipient": WHO, "amount": "0.01",
               "format": "x402", "resource_url": "https://api.example.com/x"}),
    )
    .await
    .unwrap();
    let r = &out["data"]["x402"]["accepts"][0];
    assert_eq!(r["amount"], "10000");
    assert_eq!(r["asset"], "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
    assert_eq!(out["meta"]["provider"], "local");

    let sol = call(
        &a,
        "payments_build_request",
        json!({"chain": "solana", "token": "USDC", "recipient": "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM",
               "amount": "1", "format": "solana_pay"}),
    )
    .await
    .unwrap();
    let reference = sol["data"]["reference"].as_str().unwrap();
    assert!(sol["data"]["uri"].as_str().unwrap().contains(reference));

    let err = call(
        &a,
        "payments_build_request",
        json!({"chain": "solana", "token": "USDC", "recipient": "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM",
               "amount": "1", "format": "eip681"}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
}

#[tokio::test]
async fn unknown_token_is_rejected_with_hint() {
    let a = app(vec![]);
    let err = call(
        &a,
        "payments_verify_transfer",
        json!({"chain": "base", "tx": "0xabc", "token": "0x2222222222222222222222222222222222222222",
               "recipient": WHO, "amount": "1"}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
    assert!(err.hint.unwrap().contains("USDC"));
}

#[tokio::test]
async fn resolve_symbol_across_chains() {
    let a = app(vec![]);
    let out = call(&a, "stablecoin_resolve", json!({"token": "USDG"}))
        .await
        .unwrap();
    assert_eq!(out["data"]["canonical"], true);
    let chains: Vec<&str> = out["data"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["asset"].as_str().unwrap())
        .collect();
    assert!(
        chains.iter().any(|c| c.starts_with("eip155:4663/")),
        "{chains:?}"
    );
}

#[tokio::test]
async fn check_restrictions_reports_block_and_hit() {
    let mock = Arc::new(MockEvmRpc {
        chain_id: 8453,
        ..Default::default()
    });
    mock.script
        .push_ok(json!({"number": "0x2a", "hash": "0xbeef"}))
        .push_ok(json!(format!("0x{:064x}", 1)))
        .push_ok(json!(format!("0x{:064x}", 0)));
    let a =
        app(vec![Registration::new(meta("public"))
            .chain_port(ChainId::evm(8453), PortHandle::EvmRpc(mock))]);
    let out = call(
        &a,
        "stablecoin_check_restrictions",
        json!({"chain": "base", "address": WHO, "token": "USDC"}),
    )
    .await
    .unwrap();
    assert_eq!(out["data"]["restricted"], true);
    assert_eq!(out["data"]["results"][0]["block"]["number"], 42);
    assert_eq!(out["meta"]["block"]["hash"], "0xbeef");
}

#[tokio::test]
async fn screen_combines_sources() {
    let a = app(vec![
        screener("chainalysis_oracle", false),
        screener("trm", true),
    ]);
    let out = call(
        &a,
        "compliance_screen_address",
        json!({"chain": "ethereum", "address": WHO, "include_issuer_freeze": false}),
    )
    .await
    .unwrap();
    assert_eq!(out["data"]["verdict"], "blocked");
    let sources: Vec<(&str, &str)> = out["data"]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["source"].as_str().unwrap(), s["status"].as_str().unwrap()))
        .collect();
    assert_eq!(sources, [("chainalysis_oracle", "clear"), ("trm", "hit")]);

    // Solana: default order is TRM only.
    let a = app(vec![
        screener("chainalysis_oracle", true),
        screener("trm", false),
    ]);
    let out = call(
        &a,
        "compliance_screen_address",
        json!({"chain": "solana", "address": "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM",
               "include_issuer_freeze": false}),
    )
    .await
    .unwrap();
    assert_eq!(out["data"]["verdict"], "clear");
    assert_eq!(out["data"]["sources"].as_array().unwrap().len(), 1);

    // No sanctions vendor registered → unknown, never clear.
    let a = app(vec![]);
    let out = call(
        &a,
        "compliance_screen_address",
        json!({"chain": "ethereum", "address": WHO, "include_issuer_freeze": false}),
    )
    .await
    .unwrap();
    assert_eq!(out["data"]["verdict"], "unknown");
}
