//! `rpc` pseudo-vendor for EVM chains: `token_balances` (Multicall3), `transfer_history` (logs),
//! `fee_estimate`, `simulate` (`eth_simulateV1` → `debug_traceCall` → `eth_call`), `token_metadata`.
//! Owner: `evm` (T1.E1–E3). Every port sits on `bdm_routing::RoutedEvmRpc`, so it inherits the
//! user's `evm_rpc` order, failover, breakers and quota guard.
//!
//! Private relays (Flashbots Protect, MEV Blocker; Ethereum mainnet only) need HTTP, so they live
//! in `bdm-adapters` (`vendors::{flashbots, mev_blocker}`).

use super::{
    block_number, block_tag, decode_transfer_log, erc20, fees, hex_u64, logs,
    multicall3::{self, Call},
    RawTransfer,
};
use crate::stablecoins::StablecoinRegistry;
use alloy_primitives::{address, Address, U256};
use async_trait::async_trait;
use bdm_config::{ChainEntry, Loaded, VendorStatus};
use bdm_domain::{
    AccountAddress, Amount, AssetId, AssetRef, BalanceDelta, ChainFamily, FeeEstimate, Transfer,
    UnsignedTx,
};
use bdm_ports::{
    Capability, EvmRpc, FeeOracle, Page, PortHandle, PortResult, ProviderError, Registration,
    SimulationResult, Simulator, TokenBalance, TokenBalances, TokenInfo, TokenMetadata,
    TransferHistory, TransferQuery, VendorMeta, RPC_VENDOR,
};
use bdm_routing::{RoutedEvmRpc, Router};
use serde_json::{json, Value};
use std::sync::Arc;

/// Emitter of native-value transfer logs in `eth_simulateV1` with `traceTransfers`.
/// Source: https://github.com/ethereum/execution-apis/blob/main/src/eth/execute.yaml
const NATIVE_TRANSFER_EMITTER: Address = address!("EeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE");

pub fn registrations(loaded: &Loaded, router: &Arc<Router>) -> Vec<Registration> {
    let stables = StablecoinRegistry::builtin().unwrap_or_default();
    let meta = VendorMeta {
        id: RPC_VENDOR.to_owned(),
        display_name: loaded.registry.vendors.get(RPC_VENDOR).map_or_else(
            || "On-chain via routed RPC".into(),
            |e| e.display_name.clone(),
        ),
        requires_key: false,
        signup_url: None,
        rpc_features: Default::default(),
    };
    let mut reg = Registration::new(meta);
    for chain in loaded
        .registry
        .chains
        .enabled()
        .filter(|c| c.family == ChainFamily::Evm)
    {
        let Some(rpc) = RoutedEvmRpc::new(router.clone(), chain.id.clone()) else {
            continue;
        };
        let port = Arc::new(ChainRpc {
            rpc,
            chain: chain.clone(),
            router: router.clone(),
            stables: stables
                .for_chain(&chain.id)
                .map(|e| e.asset.clone())
                .collect(),
        });
        let id = chain.id.clone();
        if chain.multicall3.is_some() {
            reg = reg.chain_port(id.clone(), PortHandle::TokenBalances(port.clone()));
        }
        reg = reg
            .chain_port(id.clone(), PortHandle::TransferHistory(port.clone()))
            .chain_port(id.clone(), PortHandle::FeeEstimate(port.clone()))
            .chain_port(id.clone(), PortHandle::Simulate(port.clone()));
        // `token_metadata` is a chain-agnostic capability, but one global `rpc` entry would be
        // shared with Solana's `rpc` registration (last one wins). Per-chain entries don't
        // collide, and the registry looks up chain-bound entries first.
        reg.ports.push((Some(id), PortHandle::TokenMetadata(port)));
    }
    if reg.ports.is_empty() {
        Vec::new()
    } else {
        vec![reg]
    }
}

/// All `rpc` ports for one EVM chain.
struct ChainRpc {
    rpc: RoutedEvmRpc,
    chain: ChainEntry,
    router: Arc<Router>,
    /// Canonical stablecoins on this chain: the default list for `balances(owner, None)`.
    stables: Vec<AssetId>,
}

impl ChainRpc {
    fn native(&self) -> AssetId {
        AssetId::native(self.chain.id.clone(), self.chain.native.slip44)
    }

    /// Smallest `getLogs` range among the active vendors of this chain's `evm_rpc` order: the
    /// routed RPC may land on any of them.
    fn max_logs_range(&self) -> Option<u64> {
        let table = self.router.table();
        let cfg = &table.config;
        cfg.order(Capability::EvmRpc, Some(&self.chain.id), None)
            .vendors
            .iter()
            .filter(|v| cfg.vendor_status(v) == VendorStatus::Active)
            .filter_map(|v| cfg.registry.vendors.get(v)?.rpc_features.get_logs_max_range)
            .min()
    }

    fn check_asset(&self, a: &AssetId) -> PortResult<()> {
        if a.chain != self.chain.id {
            return Err(ProviderError::Invalid(format!(
                "asset {a} is not on {}",
                self.chain.id
            )));
        }
        Ok(())
    }
}

fn evm_owner(owner: &AccountAddress) -> PortResult<Address> {
    match owner {
        AccountAddress::Evm(a) => Ok(*a),
        AccountAddress::Solana(_) => Err(ProviderError::Invalid("expected an EVM address".into())),
    }
}

fn word(d: &Option<Vec<u8>>) -> Option<U256> {
    d.as_deref()
        .filter(|d| d.len() >= 32)
        .map(|d| U256::from_be_slice(&d[..32]))
}

#[async_trait]
impl TokenBalances for ChainRpc {
    /// Native + ERC-20 balances in one Multicall3 call pinned to one block number. `assets =
    /// None` reads native + the chain's canonical stablecoins (zero token balances omitted).
    /// Tokens that don't answer `balanceOf`/`decimals` are left out.
    async fn balances(
        &self,
        owner: &AccountAddress,
        assets: Option<&[AssetId]>,
    ) -> PortResult<Vec<TokenBalance>> {
        let owner = evm_owner(owner)?;
        let explicit = assets.is_some();
        let list: Vec<AssetId> = match assets {
            Some(a) => a.to_vec(),
            None => std::iter::once(self.native())
                .chain(self.stables.iter().cloned())
                .collect(),
        };
        let mut calls = Vec::new();
        for a in &list {
            self.check_asset(a)?;
            match a.asset {
                AssetRef::Native { .. } => calls.push(Call::eth_balance(owner)),
                AssetRef::Erc20(t) => {
                    calls.push(Call::new(t, erc20::IERC20::balanceOfCall { owner }));
                    calls.push(Call::new(t, erc20::IERC20::decimalsCall {}));
                    calls.push(Call::new(t, erc20::IERC20::symbolCall {}));
                }
                AssetRef::SplToken(_) => {
                    return Err(ProviderError::Invalid(format!("{a} is not an EVM asset")))
                }
            }
        }
        let block = block_tag(block_number(&self.rpc).await?);
        let results = multicall3::aggregate3(&self.rpc, &calls, &block).await?;
        let mut r = results.iter();
        let mut out = Vec::new();
        for a in list {
            if a.is_native() {
                let raw = word(r.next().expect("one result per call"))
                    .ok_or_else(|| super::malformed("getEthBalance"))?;
                out.push(TokenBalance {
                    asset: a,
                    amount: Amount::new(raw, self.chain.native.decimals),
                    symbol: Some(self.chain.native.symbol.clone()),
                    token_account: None,
                });
                continue;
            }
            let (bal, dec, sym) = (r.next(), r.next(), r.next());
            let (Some(raw), Some(d)) = (
                bal.and_then(word),
                dec.and_then(|d| d.as_deref().and_then(erc20::decode_decimals)),
            ) else {
                continue;
            };
            if raw.is_zero() && !explicit {
                continue;
            }
            out.push(TokenBalance {
                asset: a,
                amount: Amount::new(raw, d),
                symbol: sym.and_then(|s| s.as_deref().and_then(erc20::decode_str)),
                token_account: None,
            });
        }
        Ok(out)
    }
}

#[async_trait]
impl TransferHistory for ChainRpc {
    async fn transfers(&self, query: &TransferQuery) -> PortResult<Page<Transfer>> {
        logs::scan_transfers(&self.rpc, &self.chain, query, self.max_logs_range()).await
    }
}

#[async_trait]
impl FeeOracle for ChainRpc {
    async fn fee_estimate(&self) -> PortResult<FeeEstimate> {
        fees::fee_estimate(&self.rpc, &self.chain).await
    }
}

#[async_trait]
impl TokenMetadata for ChainRpc {
    /// On-chain `decimals`/`symbol`/`name` (one Multicall3 call). `NotFound` for a non-token.
    async fn metadata(&self, asset: &AssetId) -> PortResult<TokenInfo> {
        self.check_asset(asset)?;
        let token = match asset.asset {
            AssetRef::Native { .. } => {
                return Ok(TokenInfo {
                    asset: asset.clone(),
                    decimals: self.chain.native.decimals,
                    symbol: Some(self.chain.native.symbol.clone()),
                    name: None,
                    logo_url: None,
                    verified: None,
                    source: RPC_VENDOR.into(),
                })
            }
            AssetRef::Erc20(t) => t,
            AssetRef::SplToken(_) => {
                return Err(ProviderError::Invalid(format!(
                    "{asset} is not an EVM asset"
                )))
            }
        };
        let calls = [
            Call::new(token, erc20::IERC20::decimalsCall {}),
            Call::new(token, erc20::IERC20::symbolCall {}),
            Call::new(token, erc20::IERC20::nameCall {}),
        ];
        let r = multicall3::aggregate3(&self.rpc, &calls, "latest").await?;
        let text = |i: usize| r[i].as_deref().and_then(erc20::decode_str);
        let decimals = r[0]
            .as_deref()
            .and_then(erc20::decode_decimals)
            .ok_or(ProviderError::NotFound)?;
        Ok(TokenInfo {
            asset: asset.clone(),
            decimals,
            symbol: text(1),
            name: text(2),
            logo_url: None,
            verified: None,
            source: RPC_VENDOR.into(),
        })
    }
}

#[async_trait]
impl Simulator for ChainRpc {
    /// `eth_simulateV1` (with balance changes) → `debug_traceCall` (callTracer) → `eth_call`.
    /// A later method is tried when the earlier one is unsupported, rejected or failing everywhere.
    async fn simulate(
        &self,
        from: &AccountAddress,
        tx: &UnsignedTx,
    ) -> PortResult<SimulationResult> {
        let from = evm_owner(from)?;
        let UnsignedTx::Evm {
            chain_id,
            to,
            data,
            value,
            gas_limit,
            ..
        } = tx
        else {
            return Err(ProviderError::Invalid("expected an EVM transaction".into()));
        };
        if *chain_id != self.rpc.chain_id() {
            return Err(ProviderError::Invalid(format!(
                "transaction is for chain {chain_id}, not {}",
                self.chain.id
            )));
        }
        let value = U256::from_str_radix(value, 10)
            .map_err(|_| ProviderError::Invalid(format!("value '{value}' is not wei")))?;
        let mut call =
            json!({"from": from, "to": to, "data": data, "value": format!("{value:#x}")});
        if let Some(g) = gas_limit {
            call["gas"] = json!(format!("0x{g:x}"));
        }
        let block = block_tag(block_number(&self.rpc).await?);

        // `Transient` too: the routed RPC reports "unsupported by every vendor" as
        // all-providers-failed. Rate/quota errors stop here (the next method costs the same).
        let next = |e: &ProviderError| {
            matches!(
                e,
                ProviderError::Unsupported(_)
                    | ProviderError::Invalid(_)
                    | ProviderError::Transient(_)
            )
        };
        match self.simulate_v1(&call, &block).await {
            Err(e) if next(&e) => {}
            other => return other,
        }
        match self.trace_call(&call, &block).await {
            Err(e) if next(&e) => {}
            other => return other,
        }
        match self.rpc.request("eth_call", json!([call, block])).await {
            Ok(_) => Ok(sim(true, None, None)),
            Err(ProviderError::Invalid(msg)) => Ok(sim(false, Some(msg), None)),
            Err(e) => Err(e),
        }
    }
}

fn sim(success: bool, error: Option<String>, units: Option<u64>) -> SimulationResult {
    SimulationResult {
        success,
        error,
        units_consumed: units,
        balance_changes: Vec::new(),
        logs: Vec::new(),
    }
}

impl ChainRpc {
    async fn simulate_v1(&self, call: &Value, block: &str) -> PortResult<SimulationResult> {
        let params = json!([{
            "blockStateCalls": [{"calls": [call]}],
            "traceTransfers": true,
            "validation": false,
        }, block]);
        let v = self.rpc.request("eth_simulateV1", params).await?;
        let c = &v[0]["calls"][0];
        if c.is_null() {
            return Err(super::malformed("eth_simulateV1 result"));
        }
        let success = c["status"].as_str() == Some("0x1");
        let mut out = sim(
            success,
            c["error"]["message"].as_str().map(str::to_owned),
            hex_u64(&c["gasUsed"]).ok(),
        );
        if success {
            let raw: Vec<RawTransfer> = c["logs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|l| {
                    // Simulated logs carry no tx hash; the decoder only needs a placeholder.
                    let mut l = l.clone();
                    l["transactionHash"] = json!("0x");
                    decode_transfer_log(&l)
                })
                .collect();
            out.balance_changes = self.balance_changes(raw, block).await?;
        }
        Ok(out)
    }

    async fn trace_call(&self, call: &Value, block: &str) -> PortResult<SimulationResult> {
        let v = self
            .rpc
            .request(
                "debug_traceCall",
                json!([call, block, {"tracer": "callTracer"}]),
            )
            .await?;
        let error = v["revertReason"]
            .as_str()
            .or(v["error"].as_str())
            .map(str::to_owned);
        Ok(sim(error.is_none(), error, hex_u64(&v["gasUsed"]).ok()))
    }

    /// Before = balances at the simulation's base block; after = before + in − out.
    async fn balance_changes(
        &self,
        raw: Vec<RawTransfer>,
        block: &str,
    ) -> PortResult<Vec<BalanceDelta>> {
        // (owner, token) → (in, out); the native pseudo-emitter stands for the native asset.
        let mut net: Vec<((Address, Address), (U256, U256))> = Vec::new();
        let mut add = |key: (Address, Address), inc: U256, out: U256| match net
            .iter_mut()
            .find(|(k, _)| *k == key)
        {
            Some((_, (i, o))) => {
                *i = i.saturating_add(inc);
                *o = o.saturating_add(out);
            }
            None => net.push((key, (inc, out))),
        };
        for t in &raw {
            add((t.to, t.token), t.value, U256::ZERO);
            add((t.from, t.token), U256::ZERO, t.value);
        }
        let mut calls = Vec::new();
        for ((owner, token), _) in &net {
            if *token == NATIVE_TRANSFER_EMITTER {
                calls.push(Call::eth_balance(*owner));
            } else {
                calls.push(Call::new(
                    *token,
                    erc20::IERC20::balanceOfCall { owner: *owner },
                ));
                calls.push(Call::new(*token, erc20::IERC20::decimalsCall {}));
            }
        }
        let results = multicall3::aggregate3(&self.rpc, &calls, block).await?;
        let mut r = results.iter();
        let mut out = Vec::new();
        for ((owner, token), (inc, dec)) in net {
            let (before, decimals, asset) = if token == NATIVE_TRANSFER_EMITTER {
                (
                    r.next().and_then(word),
                    Some(self.chain.native.decimals),
                    self.native(),
                )
            } else {
                let b = r.next().and_then(word);
                let d = r
                    .next()
                    .and_then(|d| d.as_deref().and_then(erc20::decode_decimals));
                let asset = AssetId {
                    chain: self.chain.id.clone(),
                    asset: AssetRef::Erc20(token),
                };
                (b, d, asset)
            };
            let (Some(before), Some(d)) = (before, decimals) else {
                continue;
            };
            let Some(after) = before.saturating_add(inc).checked_sub(dec) else {
                continue;
            };
            out.push(BalanceDelta {
                owner: AccountAddress::Evm(owner),
                asset,
                before: Amount::new(before, d),
                after: Amount::new(after, d),
                withheld_fee: None,
            });
        }
        Ok(out)
    }
}
