//! `rpc` pseudo-vendor for Solana: balances (both token programs), transfers (wallet + every token
//! account), fee_estimate, simulate, token_metadata. Owner: `solana` (T1.S1–S2).
//!
//! Everything runs on [`RoutedSolanaRpc`], so it inherits the user's `solana_rpc` order,
//! breakers and quota guard.
//!
//! **`token_metadata` scope:** the port model registers `token_metadata` chain-agnostic, but the
//! EVM `rpc` registration uses the same `(token_metadata, None, "rpc")` slot. This one is
//! registered chain-scoped instead (`(token_metadata, Some(solana), "rpc")`), which the registry
//! looks up first, so operations must set `RouteReq.chain` for token metadata.

use super::{native_asset, spl, token_asset, tx};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use bdm_config::{ChainEntry, Loaded};
use bdm_domain::{
    AccountAddress, Amount, AssetId, AssetRef, ChainFamily, SolanaPubkey, Transfer, UnsignedTx,
};
use bdm_ports::{
    Direction, FeeOracle, Page, PortHandle, PortResult, ProviderError, Registration,
    SimulationResult, Simulator, SolanaRpc, TokenBalance, TokenBalances, TokenInfo, TokenMetadata,
    TransferHistory, TransferQuery, RPC_VENDOR,
};
use bdm_routing::{RoutedSolanaRpc, Router};
use serde_json::{json, Map};
use std::{collections::BTreeMap, sync::Arc};

/// Commitment for balance reads and history scans (`getSignaturesForAddress` has no
/// `processed`). Per-tx finality is reported from each signature's `confirmationStatus`.
const READ_COMMITMENT: &str = "confirmed";
/// Max signatures (→ one `getTransaction` each) per history page.
// ponytail: one getTransaction per signature; switch to JSON-RPC batches if pages get large.
const MAX_PAGE: u32 = 100;

pub fn registrations(loaded: &Loaded, router: &Arc<Router>) -> Vec<Registration> {
    let mut reg = Registration::new(loaded.vendor_meta(RPC_VENDOR));
    for chain in loaded
        .registry
        .chains
        .enabled()
        .filter(|c| c.family == ChainFamily::Solana)
    {
        let rpc = Arc::new(RoutedSolanaRpc::new(router.clone(), chain.id.clone()));
        let v = Arc::new(SolanaRpcVendor::new(rpc, chain.clone()));
        let id = chain.id.clone();
        reg = reg
            .chain_port(id.clone(), PortHandle::TokenBalances(v.clone()))
            .chain_port(id.clone(), PortHandle::TransferHistory(v.clone()))
            .chain_port(id.clone(), PortHandle::FeeEstimate(v.clone()))
            .chain_port(id.clone(), PortHandle::Simulate(v.clone()));
        reg.ports.push((Some(id), PortHandle::TokenMetadata(v)));
    }
    if reg.ports.is_empty() {
        Vec::new()
    } else {
        vec![reg]
    }
}

/// Every generic Solana port over one `SolanaRpc` (routed in production, fake in tests).
pub struct SolanaRpcVendor {
    rpc: Arc<dyn SolanaRpc>,
    chain: ChainEntry,
}

impl SolanaRpcVendor {
    pub fn new(rpc: Arc<dyn SolanaRpc>, chain: ChainEntry) -> Self {
        Self { rpc, chain }
    }
}

fn solana_owner(a: &AccountAddress) -> PortResult<SolanaPubkey> {
    match a {
        AccountAddress::Solana(p) => Ok(*p),
        AccountAddress::Evm(_) => Err(ProviderError::Invalid(format!(
            "{a} is not a Solana address"
        ))),
    }
}

#[async_trait]
impl TokenBalances for SolanaRpcVendor {
    /// SOL plus one row per mint, summed over every token account of the owner (ATA and
    /// non-ATA, both programs). `token_account` is the ATA when the owner has one.
    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn balances(
        &self,
        owner: &AccountAddress,
        assets: Option<&[AssetId]>,
    ) -> PortResult<Vec<TokenBalance>> {
        let owner = solana_owner(owner)?;
        let wanted = |a: &AssetId| assets.is_none_or(|l| l.contains(a));
        let mut out = Vec::new();
        let native = native_asset(&self.chain);
        if wanted(&native) {
            let v = self
                .rpc
                .request(
                    "getBalance",
                    json!([owner.to_string(), {"commitment": READ_COMMITMENT}]),
                )
                .await?;
            let lamports = v["value"]
                .as_u64()
                .ok_or_else(|| ProviderError::Transient("getBalance without value".into()))?;
            out.push(TokenBalance {
                asset: native,
                amount: Amount::from_u128(lamports.into(), self.chain.native.decimals),
                symbol: Some(self.chain.native.symbol.clone()),
                token_account: None,
            });
        }
        if assets.is_some_and(|l| l.iter().all(AssetId::is_native)) {
            return Ok(out);
        }
        let mut by_mint: BTreeMap<SolanaPubkey, Vec<spl::TokenAccount>> = BTreeMap::new();
        for a in spl::token_accounts(self.rpc.as_ref(), &owner, READ_COMMITMENT).await? {
            by_mint.entry(a.mint).or_default().push(a);
        }
        for (mint, accts) in by_mint {
            let asset = token_asset(&self.chain, mint);
            let total: u128 = accts.iter().map(|a| a.amount as u128).sum();
            let Some(first) = accts.first() else {
                continue;
            };
            if !wanted(&asset) || (total == 0 && assets.is_none()) {
                continue;
            }
            let ata = spl::associated_token_address(&owner, &mint, &first.program).ok();
            let holder = accts
                .iter()
                .find(|a| Some(a.address) == ata)
                .unwrap_or(first);
            out.push(TokenBalance {
                asset,
                amount: Amount::from_u128(total, first.decimals),
                symbol: None,
                token_account: Some(AccountAddress::Solana(holder.address)),
            });
        }
        Ok(out)
    }
}

#[async_trait]
impl TransferHistory for SolanaRpcVendor {
    /// Scan `getSignaturesForAddress` over the wallet AND every token account (both
    /// programs; incoming SPL transfers are indexed on the token account, not the wallet),
    /// newest first, skipping failed txs. `cursor` is the last signature examined; it is
    /// passed as `before` to every address on the next page.
    async fn transfers(&self, q: &TransferQuery) -> PortResult<Page<Transfer>> {
        let owner = solana_owner(&q.owner)?;
        let limit = q.limit.clamp(1, MAX_PAGE) as usize;
        let mints: Option<Vec<SolanaPubkey>> = q.assets.as_ref().map(|l| {
            l.iter()
                .filter_map(|a| match a.asset {
                    AssetRef::SplToken(m) => Some(m),
                    _ => None,
                })
                .collect()
        });
        let mut addresses = vec![owner];
        for a in spl::token_accounts(self.rpc.as_ref(), &owner, READ_COMMITMENT).await? {
            if mints.as_ref().is_none_or(|m| m.contains(&a.mint)) {
                addresses.push(a.address);
            }
        }

        // signature → (slot, failed, confirmationStatus)
        let mut seen = BTreeMap::new();
        let mut more = false;
        for addr in &addresses {
            let mut cfg = Map::new();
            cfg.insert("limit".into(), json!(limit));
            cfg.insert("commitment".into(), json!(READ_COMMITMENT));
            if let Some(c) = &q.cursor {
                cfg.insert("before".into(), json!(c));
            }
            let page = self
                .rpc
                .request("getSignaturesForAddress", json!([addr.to_string(), cfg]))
                .await?;
            let entries = tx::status_map(&page);
            more |= entries.len() >= limit;
            seen.extend(entries);
        }
        let mut sigs: Vec<(String, (u64, bool, Option<String>))> = seen
            .into_iter()
            .filter(|(_, (slot, ..))| q.to_block.is_none_or(|to| *slot <= to))
            .collect();
        // ponytail: ties within one slot are ordered by signature, not by tx index.
        sigs.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then(a.0.cmp(&b.0)));
        more |= sigs.len() > limit;
        sigs.truncate(limit);
        if let Some(from) = q.from_block {
            if sigs.last().is_some_and(|(_, (slot, ..))| *slot < from) {
                more = false;
            }
            sigs.retain(|(_, (slot, ..))| *slot >= from);
        }
        let next_cursor = more.then(|| sigs.last().map(|s| s.0.clone())).flatten();

        let mut items = Vec::new();
        for (sig, (_, failed, status)) in &sigs {
            if *failed {
                continue;
            }
            let v = self
                .rpc
                .request(
                    "getTransaction",
                    json!([sig, tx::get_transaction_config(READ_COMMITMENT)]),
                )
                .await?;
            if v.is_null() {
                continue;
            }
            let finality = super::finality_from_status(&self.chain, status.as_deref(), None);
            let parsed = tx::parse_tx_with_finality(&self.chain, &v, finality)
                .map_err(|e| ProviderError::Transient(e.to_string()))?;
            items.extend(parsed.transfers.into_iter().filter(|t| {
                let dir_ok = match q.direction {
                    Direction::In => t.to == q.owner,
                    Direction::Out => t.from.as_ref() == Some(&q.owner),
                    Direction::Both => t.to == q.owner || t.from.as_ref() == Some(&q.owner),
                };
                dir_ok && q.assets.as_ref().is_none_or(|l| l.contains(&t.asset))
            }));
        }
        Ok(Page { items, next_cursor })
    }
}

#[async_trait]
impl FeeOracle for SolanaRpcVendor {
    async fn fee_estimate(&self) -> PortResult<bdm_domain::FeeEstimate> {
        super::fees::fee_estimate(self.rpc.as_ref(), &self.chain, &[]).await
    }
}

#[async_trait]
impl Simulator for SolanaRpcVendor {
    /// `simulateTransaction` with `sigVerify: false`, `replaceRecentBlockhash: true`,
    /// `innerInstructions: true`. The fee payer is the message's first key (`from` unused).
    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn simulate(
        &self,
        _from: &AccountAddress,
        unsigned: &UnsignedTx,
    ) -> PortResult<SimulationResult> {
        let UnsignedTx::Solana { message_base64, .. } = unsigned else {
            return Err(ProviderError::Invalid("not a Solana transaction".into()));
        };
        let invalid = |e: bdm_domain::DomainError| ProviderError::Invalid(e.to_string());
        let wire = tx::transaction_for_simulation(message_base64).map_err(invalid)?;
        let v = self
            .rpc
            .request(
                "simulateTransaction",
                json!([wire, {"encoding": "base64", "sigVerify": false,
                       "replaceRecentBlockhash": true, "innerInstructions": true,
                       "commitment": READ_COMMITMENT}]),
            )
            .await?;
        let val = &v["value"];
        let message = B64
            .decode(message_base64.trim())
            .map_err(|_| ProviderError::Invalid("message is not base64".into()))?;
        Ok(SimulationResult {
            success: val["err"].is_null(),
            error: (!val["err"].is_null()).then(|| val["err"].to_string()),
            units_consumed: val["unitsConsumed"].as_u64(),
            balance_changes: tx::simulated_deltas(&self.chain, &message, val)
                .map_err(|e| ProviderError::Transient(e.to_string()))?,
            logs: val["logs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|l| l.as_str().map(str::to_owned))
                .collect(),
        })
    }
}

#[async_trait]
impl TokenMetadata for SolanaRpcVendor {
    /// Decimals from the mint account (source of truth); name/symbol from the Token-2022
    /// `tokenMetadata` extension when present.
    async fn metadata(&self, asset: &AssetId) -> PortResult<TokenInfo> {
        if asset.chain != self.chain.id {
            return Err(ProviderError::Unsupported(format!(
                "{} is not on {}",
                asset, self.chain.id
            )));
        }
        let (decimals, symbol, name) = match asset.asset {
            AssetRef::Native { .. } => {
                let n = &self.chain.native;
                (
                    n.decimals,
                    Some(n.symbol.clone()),
                    Some(self.chain.name.clone()),
                )
            }
            AssetRef::SplToken(mint) => {
                let m = spl::mint_info(self.rpc.as_ref(), &mint).await?;
                (m.decimals, m.symbol, m.name)
            }
            AssetRef::Erc20(_) => return Err(ProviderError::Unsupported("ERC-20".into())),
        };
        Ok(TokenInfo {
            asset: asset.clone(),
            decimals,
            symbol,
            name,
            logo_url: None,
            verified: None,
            source: RPC_VENDOR.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solana::testutil::{fixture, mainnet, FnRpc};
    use alloy_primitives::U256;
    use bdm_domain::TransferKind;
    use serde_json::Value;

    const OWNER: &str = "Go5EVXxV3ob4CJVaq6YiGGUKqPmEfDo32kSmbWsYevsF";
    const ATA: &str = "5MjBG96YjNJWEL687DuGroThtGN5GPd9dFgQXg8o22TV";
    const OTHER: &str = "4SMRzaLXsuvTi2B5cSVs4LuCUUqQhB2uByTWUvbixjfF";
    const PYUSD_ATA: &str = "3Rvy7A1hViAyQh8sCJ2NhDMUVERpDv77tpLLHz7fGaaB";
    const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    const PYUSD: &str = "2b1kV6DkPAnxd5ixfnxCpjxmKwqjjaYmCZfHsFu24GXo";

    fn acct(pubkey: &str, mint: &str, amount: u64) -> Value {
        json!({"pubkey": pubkey, "account": {"data": {"program": "spl-token", "parsed": {"type": "account",
            "info": {"mint": mint, "owner": OWNER, "state": "initialized",
                     "tokenAmount": {"amount": amount.to_string(), "decimals": 6}}}}}})
    }

    fn token_accounts(p: &Value) -> Value {
        match p[1]["programId"].as_str().unwrap() {
            spl::TOKEN_PROGRAM => {
                json!({"value": [acct(OTHER, USDC, 2_000_000), acct(ATA, USDC, 25_000_000)]})
            }
            spl::TOKEN_2022_PROGRAM => json!({"value": [acct(PYUSD_ATA, PYUSD, 0)]}),
            other => panic!("unexpected program {other}"),
        }
    }

    fn vendor(rpc: FnRpc) -> (Arc<FnRpc>, SolanaRpcVendor) {
        let rpc = Arc::new(rpc);
        (rpc.clone(), SolanaRpcVendor::new(rpc, mainnet()))
    }

    fn owner() -> AccountAddress {
        OWNER.parse().unwrap()
    }

    #[tokio::test]
    async fn balances_sum_ata_and_non_ata_over_both_programs() {
        let (rpc, v) = vendor(FnRpc::new(|m, p| match m {
            "getBalance" => Some(json!({"context": {"slot": 1}, "value": 1_500_000_000u64})),
            "getTokenAccountsByOwner" => Some(token_accounts(p)),
            _ => None,
        }));
        let b = v.balances(&owner(), None).await.unwrap();
        assert_eq!(b.len(), 2, "{b:#?}"); // SOL + USDC; zero PYUSD omitted
        assert_eq!(b[0].amount.format_units(), "1.5");
        assert_eq!(b[1].amount.raw, U256::from(27_000_000u64));
        assert_eq!(b[1].token_account.unwrap().to_string(), ATA);
        assert_eq!(
            rpc.methods()
                .iter()
                .filter(|m| *m == "getTokenAccountsByOwner")
                .count(),
            2
        );

        // Explicit asset list keeps zero balances and skips SOL.
        let pyusd: AssetId = format!("{}/token:{PYUSD}", mainnet().id).parse().unwrap();
        let b = v
            .balances(&owner(), Some(std::slice::from_ref(&pyusd)))
            .await
            .unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!((b[0].asset.clone(), b[0].amount.is_zero()), (pyusd, true));
    }

    fn history_rpc() -> FnRpc {
        let tx = fixture("usdc_transfer_to_new_ata");
        let sig = tx["transaction"]["signatures"][0]
            .as_str()
            .unwrap()
            .to_owned();
        FnRpc::new(move |m, p| match m {
            "getTokenAccountsByOwner" => Some(token_accounts(p)),
            "getSignaturesForAddress" => Some(match p[0].as_str().unwrap() {
                // The wallet itself never appears in an incoming SPL transfer.
                OWNER => json!([]),
                ATA if p[1]["before"].is_null() => json!([
                    {"signature": sig, "slot": 320000000, "err": null, "confirmationStatus": "finalized", "blockTime": 1},
                    {"signature": "failedSig", "slot": 319999999, "err": {"InstructionError": [0, "Custom"]},
                     "confirmationStatus": "finalized", "blockTime": 1},
                ]),
                _ => json!([]),
            }),
            "getTransaction" => {
                assert_eq!(p[0], json!(sig), "failed txs must not be fetched");
                Some(tx.clone())
            }
            _ => None,
        })
    }

    fn query(direction: Direction, limit: u32) -> TransferQuery {
        TransferQuery {
            owner: owner(),
            direction,
            assets: None,
            from_block: None,
            to_block: None,
            cursor: None,
            limit,
        }
    }

    #[tokio::test]
    async fn incoming_transfer_to_ata_is_found_when_querying_the_wallet() {
        let (rpc, v) = vendor(history_rpc());
        let page = v.transfers(&query(Direction::In, 10)).await.unwrap();
        assert_eq!(page.items.len(), 1, "{page:#?}");
        let t = &page.items[0];
        assert_eq!(t.to, owner());
        assert_eq!(t.kind, TransferKind::Token);
        assert_eq!(t.amount.raw, U256::from(25_000_000u64));
        assert_eq!(page.next_cursor, None);
        // Wallet + 3 token accounts (both programs) were scanned.
        let scanned: Vec<String> = rpc
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.0 == "getSignaturesForAddress")
            .map(|c| c.1[0].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(scanned.len(), 4);
        assert!(scanned.contains(&PYUSD_ATA.to_owned()));

        let out = v.transfers(&query(Direction::Out, 10)).await.unwrap();
        assert!(out.items.is_empty());
    }

    #[tokio::test]
    async fn pagination_uses_before_cursor() {
        let (rpc, v) = vendor(history_rpc());
        let page = v.transfers(&query(Direction::Both, 1)).await.unwrap();
        let cursor = page.next_cursor.clone().expect("more pages");
        assert_eq!(page.items.len(), 1);
        let mut q = query(Direction::Both, 1);
        q.cursor = Some(cursor.clone());
        let next = v.transfers(&q).await.unwrap();
        assert!(next.items.is_empty());
        assert_eq!(next.next_cursor, None);
        let last = rpc.calls.lock().unwrap().last().cloned().unwrap();
        assert_eq!(last.1[1]["before"], json!(cursor));
    }

    #[tokio::test]
    async fn simulate_reports_units_logs_and_deltas() {
        // Legacy message: 1 signer, keys [payer, recipient, system], one System transfer.
        let mut m = vec![1u8, 0, 1, 3];
        m.extend(bs58::decode(OWNER).into_vec().unwrap());
        m.extend(bs58::decode(ATA).into_vec().unwrap());
        m.extend([0u8; 32]);
        m.extend([9u8; 32]);
        m.extend([1u8, 2, 2, 0, 1, 12, 2, 0, 0, 0]);
        m.extend(1000u64.to_le_bytes());
        let msg = B64.encode(&m);
        let (rpc, v) = vendor(FnRpc::new(|m, p| {
            assert_eq!(m, "simulateTransaction");
            assert_eq!(p[1]["sigVerify"], false);
            assert_eq!(p[1]["replaceRecentBlockhash"], true);
            assert_eq!(p[1]["innerInstructions"], true);
            Some(json!({"context": {"slot": 5}, "value": {
                "err": null, "logs": ["Program 11111111111111111111111111111111 success"],
                "unitsConsumed": 150, "preBalances": [10_000, 0, 1], "postBalances": [4_000, 1_000, 1],
                "preTokenBalances": [], "postTokenBalances": [],
                "loadedAddresses": {"writable": [], "readonly": []}}}))
        }));
        let tx = UnsignedTx::Solana {
            message_base64: msg,
            recent_blockhash: "x".into(),
            last_valid_block_height: 1,
        };
        let r = v.simulate(&owner(), &tx).await.unwrap();
        assert!(r.success);
        assert_eq!(r.units_consumed, Some(150));
        assert_eq!(r.logs.len(), 1);
        assert_eq!(r.balance_changes.len(), 2);
        assert_eq!(
            r.balance_changes[1].received().unwrap().raw,
            U256::from(1000u64)
        );
        // Wire payload: 1 zero signature + message.
        let sent = B64
            .decode(rpc.calls.lock().unwrap()[0].1[0].as_str().unwrap())
            .unwrap();
        assert_eq!((sent[0], sent.len()), (1, 1 + 64 + m.len()));
    }

    #[tokio::test]
    async fn metadata_from_token_2022_mint() {
        let (_, v) = vendor(FnRpc::new(|m, _| {
            assert_eq!(m, "getAccountInfo");
            Some(
                json!({"value": {"owner": spl::TOKEN_2022_PROGRAM, "data": {"program": "spl-token-2022",
                "parsed": {"type": "mint", "info": {"decimals": 6, "extensions": [
                    {"extension": "transferFeeConfig", "state": {}},
                    {"extension": "tokenMetadata", "state": {"name": "PayPal USD", "symbol": "PYUSD", "uri": ""}}]}}}}}),
            )
        }));
        let asset: AssetId = format!("{}/token:{PYUSD}", mainnet().id).parse().unwrap();
        let info = v.metadata(&asset).await.unwrap();
        assert_eq!(
            (info.decimals, info.symbol.as_deref(), info.name.as_deref()),
            (6, Some("PYUSD"), Some("PayPal USD"))
        );
        let evm: AssetId = "eip155:1/slip44:60".parse().unwrap();
        assert!(matches!(
            v.metadata(&evm).await,
            Err(ProviderError::Unsupported(_))
        ));
    }

    #[test]
    fn registers_every_solana_port() {
        use bdm_config::{ConfigDir, ConfigLoader, EnvSource};
        use bdm_ports::Capability;
        use bdm_routing::{InMemoryCounterStore, ProviderRegistry, RouterOptions, RoutingTable};
        let loaded = ConfigLoader::new(
            ConfigDir::new("/nonexistent"),
            EnvSource::from_pairs(Vec::<(String, String)>::new()),
        )
        .unwrap()
        .load_texts("", "")
        .unwrap();
        let router = Router::new(
            RoutingTable {
                config: Arc::new(loaded.clone()),
                registry: ProviderRegistry::default(),
            },
            Arc::new(InMemoryCounterStore::default()),
            RouterOptions::default(),
        );
        let regs = registrations(&loaded, &router);
        assert_eq!(regs.len(), 1);
        let mut caps: Vec<_> = regs[0]
            .ports
            .iter()
            .map(|(c, h)| {
                assert_eq!(c.as_ref(), Some(&mainnet().id));
                h.capability()
            })
            .collect();
        caps.sort();
        assert_eq!(
            caps,
            [
                Capability::TokenBalances,
                Capability::TransferHistory,
                Capability::FeeEstimate,
                Capability::Simulate,
                Capability::TokenMetadata
            ]
        );
    }
}
