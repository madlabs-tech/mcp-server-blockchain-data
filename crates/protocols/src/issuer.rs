//! Issuer controls (blacklist / freeze / pause / deprecated).
//!
//! EVM reads are pinned to one block (`eth_getBlockByNumber("latest")`) and the block is reported,
//! because an address can be frozen between the check and the payment landing. Solana checks the
//! owner's token accounts for the mint (`state == frozen`) plus mint-level controls
//! (permanent delegate, Token-2022 `pausable`, frozen default account state).

use crate::stablecoins::{FreezeCheck, PauseCheck, StablecoinEntry};
use alloy_primitives::{hex, Address, B256};
use alloy_sol_types::{sol, SolCall};
use bdm_domain::{AccountAddress, AssetId, AssetRef, BlockRef};
use bdm_ports::{EvmRpc, PortResult, ProviderError, SolanaRpc};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{json, Value};

sol! {
    function isBlacklisted(address account) external view returns (bool);
    function isBlackListed(address account) external view returns (bool);
    function isBlocked(address account) external view returns (bool);
    function isFrozen(address addr) external view returns (bool);
    function accountPaused(address account) external view returns (bool);
    function paused() external view returns (bool);
    function deprecated() external view returns (bool);
    function upgradedAddress() external view returns (address);
}

/// The chain transport matching the entry's family.
#[derive(Clone, Copy)]
pub enum ChainRpc<'a> {
    Evm(&'a dyn EvmRpc),
    Solana(&'a dyn SolanaRpc),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    /// The address is blacklisted / blocked / frozen by the issuer.
    AddressFrozen,
    /// The whole token is paused (no transfers at all).
    TokenPaused,
    /// Legacy Tether: calls are forwarded to `upgradedAddress`.
    TokenDeprecated,
    /// Solana: new token accounts start frozen (issuer must thaw them before they can receive).
    DefaultFrozen,
    /// Solana: issuer can move funds out of any account. Informational, not a block.
    PermanentDelegate,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct RestrictionCheck {
    pub kind: CheckKind,
    /// Method or field read, e.g. `isBlacklisted(address)`, `tokenAccount.state`.
    pub method: String,
    pub restricted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Restrictions {
    pub asset: AssetId,
    pub symbol: String,
    pub address: AccountAddress,
    /// True when any check blocks sending to / from this address.
    pub restricted: bool,
    /// Block (EVM) or slot (Solana) every read was pinned to.
    pub block: BlockRef,
    pub checks: Vec<RestrictionCheck>,
}

/// Read every issuer control the registry lists for `entry` against `address`.
pub async fn check_restrictions(
    rpc: ChainRpc<'_>,
    entry: &StablecoinEntry,
    address: &AccountAddress,
) -> PortResult<Restrictions> {
    let (block, checks) = match (rpc, &entry.asset.asset, address) {
        (ChainRpc::Evm(rpc), AssetRef::Erc20(token), AccountAddress::Evm(who)) => {
            evm_checks(rpc, entry, *token, *who).await?
        }
        (ChainRpc::Solana(rpc), AssetRef::SplToken(mint), AccountAddress::Solana(owner)) => {
            solana_checks(rpc, &mint.to_string(), &owner.to_string()).await?
        }
        _ => {
            return Err(ProviderError::Invalid(format!(
                "address {address} / transport do not match {}",
                entry.asset
            )))
        }
    };
    Ok(Restrictions {
        asset: entry.asset.clone(),
        symbol: entry.symbol.clone(),
        address: *address,
        restricted: checks.iter().any(|c| c.restricted),
        block,
        checks,
    })
}

// ------------------------------------------------------------------ EVM

/// `eth_call` returning raw bytes.
pub async fn eth_call(
    rpc: &dyn EvmRpc,
    to: Address,
    data: &[u8],
    block: &str,
) -> PortResult<Vec<u8>> {
    let v = rpc
        .request(
            "eth_call",
            json!([{"to": to, "data": format!("0x{}", hex::encode(data))}, block]),
        )
        .await?;
    let s = v
        .as_str()
        .ok_or_else(|| ProviderError::Fatal(format!("eth_call: expected hex, got {v}")))?;
    hex::decode(s.trim_start_matches("0x"))
        .map_err(|e| ProviderError::Fatal(format!("eth_call: bad hex: {e}")))
}

/// ABI word → bool, strictly (anything but 0/1 in a 32-byte word is an error).
pub fn decode_bool(b: &[u8]) -> PortResult<bool> {
    match b {
        [zeros @ .., last] if b.len() == 32 && zeros.iter().all(|&z| z == 0) && *last <= 1 => {
            Ok(*last == 1)
        }
        _ => Err(ProviderError::Fatal(format!(
            "expected ABI bool, got 0x{}",
            hex::encode(b)
        ))),
    }
}

pub async fn eth_call_bool(
    rpc: &dyn EvmRpc,
    to: Address,
    data: &[u8],
    block: &str,
) -> PortResult<bool> {
    decode_bool(&eth_call(rpc, to, data, block).await?)
}

/// Latest block number + hash (the pin for a set of reads).
pub async fn latest_block(rpc: &dyn EvmRpc) -> PortResult<BlockRef> {
    let b = rpc
        .request("eth_getBlockByNumber", json!(["latest", false]))
        .await?;
    let number = b
        .get("number")
        .and_then(Value::as_str)
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .ok_or_else(|| ProviderError::Fatal("eth_getBlockByNumber: no block number".into()))?;
    Ok(BlockRef {
        number,
        hash: b.get("hash").and_then(Value::as_str).map(str::to_owned),
        timestamp: None,
    })
}

async fn evm_checks(
    rpc: &dyn EvmRpc,
    entry: &StablecoinEntry,
    token: Address,
    who: Address,
) -> PortResult<(BlockRef, Vec<RestrictionCheck>)> {
    let block = latest_block(rpc).await?;
    let tag = format!("0x{:x}", block.number);
    let mut checks = Vec::new();

    let freeze = match entry.freeze_check {
        FreezeCheck::IsBlacklisted => Some(isBlacklistedCall { account: who }.abi_encode()),
        FreezeCheck::IsBlackListed => Some(isBlackListedCall { account: who }.abi_encode()),
        FreezeCheck::IsBlocked => Some(isBlockedCall { account: who }.abi_encode()),
        FreezeCheck::IsFrozen => Some(isFrozenCall { addr: who }.abi_encode()),
        FreezeCheck::AccountPaused => Some(accountPausedCall { account: who }.abi_encode()),
        FreezeCheck::TokenAccountState | FreezeCheck::None => None,
    };
    if let Some(data) = freeze {
        let hit = eth_call_bool(rpc, token, &data, &tag).await?;
        checks.push(RestrictionCheck {
            kind: CheckKind::AddressFrozen,
            method: entry.freeze_check.method().into(),
            restricted: hit,
            detail: hit.then(|| format!("{who} is frozen/blacklisted by {}", entry.issuer_entity)),
        });
    }
    if entry.pause_check == PauseCheck::Paused {
        let hit = eth_call_bool(rpc, token, &pausedCall {}.abi_encode(), &tag).await?;
        checks.push(RestrictionCheck {
            kind: CheckKind::TokenPaused,
            method: "paused()".into(),
            restricted: hit,
            detail: hit.then(|| format!("{} transfers are paused", entry.symbol)),
        });
    }
    if entry.deprecated_check {
        let hit = eth_call_bool(rpc, token, &deprecatedCall {}.abi_encode(), &tag).await?;
        let detail = if hit {
            let w = eth_call(rpc, token, &upgradedAddressCall {}.abi_encode(), &tag).await?;
            let to = (w.len() == 32).then(|| Address::from_word(B256::from_slice(&w)));
            Some(format!(
                "deprecated; calls forward to {}",
                to.map_or("unknown".into(), |a| a.to_checksum(None))
            ))
        } else {
            None
        };
        checks.push(RestrictionCheck {
            kind: CheckKind::TokenDeprecated,
            method: "deprecated()".into(),
            restricted: false,
            detail,
        });
    }
    Ok((block, checks))
}

// ------------------------------------------------------------------ Solana

/// Mint-level controls read from a `getAccountInfo(mint, jsonParsed)` result.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MintControls {
    pub permanent_delegate: Option<String>,
    pub paused: bool,
    pub default_frozen: bool,
}

pub fn parse_mint(v: &Value) -> MintControls {
    let mut m = MintControls::default();
    let exts = v
        .pointer("/value/data/parsed/info/extensions")
        .and_then(Value::as_array);
    for e in exts.into_iter().flatten() {
        let state = e.get("state").cloned().unwrap_or(Value::Null);
        match e.get("extension").and_then(Value::as_str) {
            Some("permanentDelegate") => {
                m.permanent_delegate = state
                    .get("delegate")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            }
            Some("pausableConfig") => {
                m.paused = state.get("paused").and_then(Value::as_bool) == Some(true)
            }
            Some("defaultAccountState") => {
                m.default_frozen =
                    state.get("accountState").and_then(Value::as_str) == Some("frozen")
            }
            _ => {}
        }
    }
    m
}

/// `(slot, [(token_account, state)])` from a `getTokenAccountsByOwner(jsonParsed)` result.
pub fn parse_token_accounts(v: &Value) -> PortResult<(u64, Vec<(String, String)>)> {
    let slot = v
        .pointer("/context/slot")
        .and_then(Value::as_u64)
        .ok_or_else(|| ProviderError::Fatal("getTokenAccountsByOwner: no context.slot".into()))?;
    let accounts = v
        .get("value")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|a| {
            let key = a.get("pubkey").and_then(Value::as_str).unwrap_or("?");
            let state = a
                .pointer("/account/data/parsed/info/state")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            (key.to_owned(), state.to_owned())
        })
        .collect();
    Ok((slot, accounts))
}

async fn solana_checks(
    rpc: &dyn SolanaRpc,
    mint: &str,
    owner: &str,
) -> PortResult<(BlockRef, Vec<RestrictionCheck>)> {
    let cfg = json!({"encoding": "jsonParsed", "commitment": "confirmed"});
    let accts = rpc
        .request(
            "getTokenAccountsByOwner",
            json!([owner, {"mint": mint}, cfg]),
        )
        .await?;
    let (slot, accounts) = parse_token_accounts(&accts)?;
    let mint_info = rpc.request("getAccountInfo", json!([mint, cfg])).await?;
    let controls = parse_mint(&mint_info);

    let frozen: Vec<&str> = accounts
        .iter()
        .filter(|(_, s)| s == "frozen")
        .map(|(k, _)| k.as_str())
        .collect();
    let mut checks = vec![RestrictionCheck {
        kind: CheckKind::AddressFrozen,
        method: FreezeCheck::TokenAccountState.method().into(),
        restricted: !frozen.is_empty(),
        detail: Some(if accounts.is_empty() {
            "owner has no token account for this mint (pass the wallet, not a token account)".into()
        } else if frozen.is_empty() {
            format!("{} token account(s), none frozen", accounts.len())
        } else {
            format!("frozen token account(s): {}", frozen.join(", "))
        }),
    }];
    if controls.paused {
        checks.push(RestrictionCheck {
            kind: CheckKind::TokenPaused,
            method: "mint.pausableConfig.paused".into(),
            restricted: true,
            detail: Some("Token-2022 mint is paused".into()),
        });
    }
    if controls.default_frozen {
        checks.push(RestrictionCheck {
            kind: CheckKind::DefaultFrozen,
            method: "mint.defaultAccountState".into(),
            restricted: accounts.is_empty(),
            detail: Some("new token accounts start frozen until the issuer thaws them".into()),
        });
    }
    if let Some(d) = controls.permanent_delegate {
        checks.push(RestrictionCheck {
            kind: CheckKind::PermanentDelegate,
            method: "mint.permanentDelegate".into(),
            restricted: false,
            detail: Some(format!(
                "issuer delegate {d} can move funds from any account"
            )),
        });
    }
    Ok((
        BlockRef {
            number: slot,
            hash: None,
            timestamp: None,
        },
        checks,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stablecoins::StablecoinRegistry;
    use async_trait::async_trait;
    use bdm_domain::ChainId;
    use bdm_testkit::mocks::MockEvmRpc;
    use std::sync::Mutex;

    fn word(n: u8) -> Value {
        json!(format!("0x{n:064x}"))
    }

    fn block() -> Value {
        json!({"number": "0x10", "hash": "0xabc"})
    }

    fn reg() -> StablecoinRegistry {
        StablecoinRegistry::builtin().unwrap()
    }

    const WHO: &str = "0xd8da6bf26964af9d7eed9e03e53415d37aa96045";

    #[test]
    fn selectors_match_known_abis() {
        assert_eq!(isBlacklistedCall::SELECTOR, [0xfe, 0x57, 0x5a, 0x87]);
        assert_eq!(isBlackListedCall::SELECTOR, [0xe4, 0x7d, 0x60, 0x60]);
        assert_eq!(isFrozenCall::SELECTOR, [0xe5, 0x83, 0x98, 0x36]);
        assert_eq!(isBlockedCall::SELECTOR, [0xfb, 0xac, 0x39, 0x51]);
        assert_eq!(pausedCall::SELECTOR, [0x5c, 0x97, 0x5a, 0xbb]);
        assert_eq!(accountPausedCall::SELECTOR, [0xbc, 0x8c, 0x4b, 0x4f]);
    }

    #[test]
    fn strict_bool_decoding() {
        let mut w = [0u8; 32];
        assert!(!decode_bool(&w).unwrap());
        w[31] = 1;
        assert!(decode_bool(&w).unwrap());
        w[31] = 2;
        assert!(decode_bool(&w).is_err());
        assert!(decode_bool(&[1]).is_err());
    }

    #[tokio::test]
    async fn usdc_blacklisted_address_is_restricted_and_block_reported() {
        let r = reg();
        let usdc = r.by_symbol(&ChainId::evm(8453), "USDC").unwrap();
        let mock = MockEvmRpc {
            chain_id: 8453,
            ..Default::default()
        };
        mock.script
            .push_ok(block())
            .push_ok(word(1))
            .push_ok(word(0));
        let who: AccountAddress = WHO.parse().unwrap();
        let out = check_restrictions(ChainRpc::Evm(&mock), usdc, &who)
            .await
            .unwrap();
        assert!(out.restricted);
        assert_eq!(out.block.number, 16);
        assert_eq!(out.block.hash.as_deref(), Some("0xabc"));
        assert_eq!(out.checks.len(), 2);
        assert_eq!(out.checks[0].method, "isBlacklisted(address)");
        assert!(out.checks[0].restricted && !out.checks[1].restricted);
    }

    #[tokio::test]
    async fn legacy_usdt_reports_deprecation_target() {
        let r = reg();
        let usdt = r.by_symbol(&ChainId::evm(1), "USDT").unwrap();
        let mock = MockEvmRpc {
            chain_id: 1,
            ..Default::default()
        };
        let target = format!("0x{:0>64}", "1111111111111111111111111111111111111111");
        mock.script
            .push_ok(block())
            .push_ok(word(0)) // isBlackListed
            .push_ok(word(0)) // paused
            .push_ok(word(1)) // deprecated
            .push_ok(json!(target));
        let who: AccountAddress = WHO.parse().unwrap();
        let out = check_restrictions(ChainRpc::Evm(&mock), usdt, &who)
            .await
            .unwrap();
        assert!(!out.restricted);
        let dep = &out.checks[2];
        assert_eq!(dep.kind, CheckKind::TokenDeprecated);
        assert!(dep
            .detail
            .as_ref()
            .unwrap()
            .contains("0x1111111111111111111111111111111111111111"));
    }

    #[tokio::test]
    async fn usdt0_has_no_pause_call() {
        let r = reg();
        let usdt0 = r.by_symbol(&ChainId::evm(42161), "USDT0").unwrap();
        let mock = MockEvmRpc {
            chain_id: 42161,
            ..Default::default()
        };
        mock.script.push_ok(block()).push_ok(word(0));
        let who: AccountAddress = WHO.parse().unwrap();
        let out = check_restrictions(ChainRpc::Evm(&mock), usdt0, &who)
            .await
            .unwrap();
        assert_eq!(out.checks.len(), 1);
        assert_eq!(out.checks[0].method, "isBlocked(address)");
        assert_eq!(mock.script.calls(), 2);
    }

    /// Solana mock keyed by method.
    struct SolMock(Mutex<Vec<(&'static str, Value)>>);

    #[async_trait]
    impl SolanaRpc for SolMock {
        async fn request(&self, method: &str, _p: Value) -> PortResult<Value> {
            let v = self.0.lock().unwrap();
            Ok(v.iter().find(|(m, _)| *m == method).unwrap().1.clone())
        }
    }

    #[tokio::test]
    async fn solana_frozen_account_and_permanent_delegate() {
        let r = reg();
        let sol: ChainId = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp".parse().unwrap();
        let pyusd = r.by_symbol(&sol, "PYUSD").unwrap();
        let rpc = SolMock(Mutex::new(vec![
            (
                "getTokenAccountsByOwner",
                json!({"context": {"slot": 99}, "value": [
                    {"pubkey": "Acc1", "account": {"data": {"parsed": {"info": {"state": "frozen"}}}}},
                    {"pubkey": "Acc2", "account": {"data": {"parsed": {"info": {"state": "initialized"}}}}}
                ]}),
            ),
            (
                "getAccountInfo",
                json!({"value": {"data": {"parsed": {"info": {"extensions": [
                    {"extension": "permanentDelegate", "state": {"delegate": "Del1"}},
                    {"extension": "transferFeeConfig", "state": {}}
                ]}}}}}),
            ),
        ]));
        let owner: AccountAddress = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
            .parse()
            .unwrap();
        let out = check_restrictions(ChainRpc::Solana(&rpc), pyusd, &owner)
            .await
            .unwrap();
        assert!(out.restricted);
        assert_eq!(out.block.number, 99);
        assert!(out.checks[0].detail.as_ref().unwrap().contains("Acc1"));
        let pd = out
            .checks
            .iter()
            .find(|c| c.kind == CheckKind::PermanentDelegate)
            .unwrap();
        assert!(!pd.restricted && pd.detail.as_ref().unwrap().contains("Del1"));
    }

    #[test]
    fn mint_default_frozen_and_paused() {
        let m = parse_mint(
            &json!({"value": {"data": {"parsed": {"info": {"extensions": [
                {"extension": "defaultAccountState", "state": {"accountState": "frozen"}},
                {"extension": "pausableConfig", "state": {"paused": true}}
            ]}}}}}),
        );
        assert!(m.default_frozen && m.paused && m.permanent_delegate.is_none());
        assert_eq!(parse_mint(&json!({"value": null})), MintControls::default());
    }

    #[tokio::test]
    async fn family_mismatch_is_invalid() {
        let r = reg();
        let usdc = r.by_symbol(&ChainId::evm(1), "USDC").unwrap();
        let mock = MockEvmRpc::default();
        let sol_addr: AccountAddress = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
            .parse()
            .unwrap();
        assert!(matches!(
            check_restrictions(ChainRpc::Evm(&mock), usdc, &sol_addr).await,
            Err(ProviderError::Invalid(_))
        ));
    }
}
