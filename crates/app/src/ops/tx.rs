//! `tx` tools: `tx_get`, `tx_status`, `tx_estimate_fee`, `tx_simulate`, `tx_build_transfer`,
//! `tx_broadcast`. EVM and Solana behind the same Operation. See `ops/mod.rs`.

use super::chain::{
    fiat_value, hex_u64, merge_meta, native_asset, parse_address, resolve_asset, rpc_meta,
    stablecoin,
};
use super::wallet::{hex_decode, is_token_program, sol_account, SYSTEM_PROGRAM};
use crate::{Catalog, Ctx, Domain, OpOutput, Operation, Profile};
use alloy_primitives::{keccak256, Address, U256};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use ems_config::ChainEntry;
use ems_domain::{
    AccountAddress, Amount, AssetId, AssetRef, BlockRef, ChainFamily, ChainId, DomainError,
    ErrorCode, FeeEstimate, FeeSpeed, Finality, Price, SolanaPubkey, Tx, TxStatus, UnsignedTx,
};
use ems_ports::{
    BroadcastReceipt, Broadcaster, Capability, EvmRpc, FeeOracle, PriceFeed, ProviderError,
    SimulationResult, Simulator, SolanaRpc,
};
use ems_protocols::{evm, solana, solana::spl};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;

pub fn register(c: &mut Catalog) {
    c.register(TxGet);
    c.register(TxStatusOp);
    c.register(TxEstimateFee);
    c.register(TxSimulate);
    c.register(TxBuildTransfer);
    c.register(TxBroadcast);
}

const ALL: &[Profile] = Profile::ALL;
const SENDERS: &[Profile] = &[Profile::Payments, Profile::Neobank, Profile::Trading];

/// SPL Memo program, the widely deployed "MemoSq…" id (source:
/// https://github.com/solana-program/memo/blob/main/interface/src/lib.rs).
const MEMO_PROGRAM: &str = "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr";

// ------------------------------------------------------------------ shared

fn validate_hash(chain: &ChainEntry, hash: &str) -> Result<String, DomainError> {
    let h = hash.trim();
    let ok = match chain.family {
        ChainFamily::Evm => {
            h.len() == 66 && h.starts_with("0x") && h[2..].bytes().all(|b| b.is_ascii_hexdigit())
        }
        ChainFamily::Solana => bs58::decode(h).into_vec().is_ok_and(|v| v.len() == 64),
    };
    if ok {
        Ok(h.to_owned())
    } else {
        Err(DomainError::invalid(format!(
            "'{h}' is not a {} transaction {}",
            chain.name,
            if chain.family == ChainFamily::Evm {
                "hash (0x + 64 hex)"
            } else {
                "signature (base58, 64 bytes)"
            }
        )))
    }
}

pub(crate) async fn lookup_tx(
    ctx: &Ctx,
    chain: &ChainEntry,
    hash: &str,
) -> Result<Option<Tx>, DomainError> {
    Ok(match chain.family {
        ChainFamily::Evm => evm::tx::get_tx(&ctx.evm_rpc(chain)?, chain, hash).await?,
        ChainFamily::Solana => solana::tx::get_tx(&ctx.solana_rpc(chain)?, chain, hash).await?,
    })
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TxRef {
    /// Chain (CAIP-2 id or alias).
    pub chain: String,
    /// EVM tx hash (0x…) or Solana signature (base58).
    pub hash: String,
    /// Include the vendor-neutral raw RPC object.
    #[serde(default)]
    pub include_raw: bool,
}

// ------------------------------------------------------------------ tx_get

pub struct TxGet;

#[async_trait]
impl Operation for TxGet {
    type Input = TxRef;
    type Output = Tx;
    const NAME: &'static str = "tx_get";
    const DOMAIN: Domain = Domain::Tx;
    const DESCRIPTION: &'static str = "Fetch one transaction, normalized across EVM and Solana: \
        status, finality (pending / confirmed(n) / safe / finalized), block with hash, fee paid, \
        decoded token transfers, and per-owner balance deltas (the source of truth for amounts \
        received, which can differ from the transfer amount with fee-on-transfer or Token-2022 \
        fees). NOT_FOUND means no provider knows the hash yet (still pending, dropped, or wrong \
        chain).";
    const PROFILES: &'static [Profile] = ALL;

    async fn execute(&self, ctx: &Ctx, input: TxRef) -> Result<OpOutput<Tx>, DomainError> {
        let chain = ctx.chain(&input.chain)?;
        let hash = validate_hash(chain, &input.hash)?;
        let mut tx = lookup_tx(ctx, chain, &hash).await?.ok_or_else(|| {
            DomainError::new(
                ErrorCode::NotFound,
                format!("{hash} not found on {}", chain.id),
            )
            .with_hint("it may still be pending, have been dropped, or be on another chain")
        })?;
        if !input.include_raw {
            tx.raw = None;
        }
        let mut meta = rpc_meta(&chain.id);
        meta.block = tx.block.clone();
        meta.finality = Some(tx.finality);
        Ok(OpOutput::new(tx, meta))
    }
}

// ------------------------------------------------------------------ tx_status

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StatusIn {
    /// Chain (CAIP-2 id or alias).
    pub chain: String,
    /// EVM tx hash (0x…) or Solana signature (base58).
    pub hash: String,
    /// Solana: `last_valid_block_height` from tx_build_transfer; lets an unseen transaction be
    /// reported as `dropped` (expired) once the chain passes it, instead of `not_found`.
    #[serde(default)]
    pub last_valid_block_height: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct StatusOut {
    pub chain: ChainId,
    pub hash: String,
    /// pending | success | failed | dropped (Solana blockhash expired) | not_found.
    pub status: TxStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finality: Option<Finality>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<BlockRef>,
    /// Success and finalized: safe to treat funds as settled.
    pub settled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_block_height: Option<u64>,
}

/// Status of a Solana tx no provider has seen: expired only once the finalized block height has
/// passed `last_valid_block_height`.
pub(crate) fn unseen_solana_status(lvbh: Option<u64>, current: Option<u64>) -> TxStatus {
    match (lvbh, current) {
        (Some(l), Some(c)) if c > l => TxStatus::Dropped,
        (Some(_), _) => TxStatus::Pending,
        (None, _) => TxStatus::NotFound,
    }
}

pub struct TxStatusOp;

#[async_trait]
impl Operation for TxStatusOp {
    type Input = StatusIn;
    type Output = StatusOut;
    const NAME: &'static str = "tx_status";
    const DOMAIN: Domain = Domain::Tx;
    const DESCRIPTION: &'static str = "Lightweight status of a transaction: pending / success / \
        failed / dropped / not_found, its finality level, and `settled` (success + finalized). \
        Poll this after tx_broadcast. For Solana pass `last_valid_block_height` from \
        tx_build_transfer so an expired transaction is reported as `dropped` (only after the \
        chain has passed that height; before that it is still `pending`).";
    const PROFILES: &'static [Profile] = ALL;

    async fn execute(
        &self,
        ctx: &Ctx,
        input: StatusIn,
    ) -> Result<OpOutput<StatusOut>, DomainError> {
        let chain = ctx.chain(&input.chain)?;
        let hash = validate_hash(chain, &input.hash)?;
        let tx = lookup_tx(ctx, chain, &hash).await?;
        let mut current_block_height = None;
        let (status, finality, block) = match tx {
            Some(t) => (t.status, Some(t.finality), t.block),
            None if chain.family == ChainFamily::Solana
                && input.last_valid_block_height.is_some() =>
            {
                current_block_height = ctx
                    .solana_rpc(chain)?
                    .request("getBlockHeight", json!([{ "commitment": "finalized" }]))
                    .await?
                    .as_u64();
                (
                    unseen_solana_status(input.last_valid_block_height, current_block_height),
                    None,
                    None,
                )
            }
            None => (TxStatus::NotFound, None, None),
        };
        let mut meta = rpc_meta(&chain.id);
        meta.block = block.clone();
        meta.finality = finality;
        Ok(OpOutput::new(
            StatusOut {
                chain: chain.id.clone(),
                hash,
                settled: status == TxStatus::Success && finality == Some(Finality::Finalized),
                status,
                finality,
                block,
                current_block_height,
            },
            meta,
        ))
    }
}

// ------------------------------------------------------------------ tx_estimate_fee

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FeeIn {
    /// Chain (CAIP-2 id or alias).
    pub chain: String,
    /// Fiat currency for the estimate (ISO 4217, default "USD").
    #[serde(default)]
    pub currency: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FeeOut {
    /// Tiers (slow / standard / fast). EVM: wei per gas, totals include the L2 L1-data fee.
    /// Solana: micro-lamports per compute unit + suggested Jito tip.
    pub estimate: FeeEstimate,
    /// Native-coin price used for `estimated_total_fiat`; absent = fiat value unknown (never 0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_price: Option<Price>,
}

pub(crate) async fn native_price(
    ctx: &Ctx,
    chain: &ChainEntry,
    currency: &str,
) -> Option<ems_routing::Routed<Price>> {
    let asset = native_asset(chain);
    let req = ctx.route(Capability::Price).chain(chain.id.clone());
    let (asset, currency) = (&asset, currency);
    ctx.router()
        .failover::<dyn PriceFeed, _, _, _>(req, |p| async move { p.price(asset, currency).await })
        .await
        .ok()
}

pub(crate) async fn fee_estimate(
    ctx: &Ctx,
    chain: &ChainEntry,
) -> Result<ems_routing::Routed<FeeEstimate>, DomainError> {
    let req = ctx.route(Capability::FeeEstimate).chain(chain.id.clone());
    ctx.router()
        .failover::<dyn FeeOracle, _, _, _>(req, |p| async move { p.fee_estimate().await })
        .await
        .map_err(|e| e.error)
}

fn currency_code(c: Option<&str>) -> Result<String, DomainError> {
    let c = c.unwrap_or("USD").trim().to_ascii_uppercase();
    if c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic()) {
        Ok(c)
    } else {
        Err(DomainError::invalid(format!(
            "'{c}' is not an ISO 4217 currency code"
        )))
    }
}

pub struct TxEstimateFee;

#[async_trait]
impl Operation for TxEstimateFee {
    type Input = FeeIn;
    type Output = FeeOut;
    const NAME: &'static str = "tx_estimate_fee";
    const DOMAIN: Domain = Domain::Tx;
    const DESCRIPTION: &'static str = "Current network fee tiers (slow / standard / fast) for a \
        simple transfer on a chain, with the estimated total in the native coin and in fiat. On \
        OP-stack and Arbitrum L2s the total includes the L1 data fee; on Solana it is the \
        priority fee per compute unit plus a suggested Jito tip. The fiat value is omitted \
        (unknown) when no price source answers.";
    const PROFILES: &'static [Profile] = ALL;

    fn cache_ttl(&self) -> Option<Duration> {
        Some(Duration::from_secs(10))
    }

    async fn execute(&self, ctx: &Ctx, input: FeeIn) -> Result<OpOutput<FeeOut>, DomainError> {
        let chain = ctx.chain(&input.chain)?;
        let currency = currency_code(input.currency.as_deref())?;
        let (fee, price) = tokio::join!(
            fee_estimate(ctx, chain),
            native_price(ctx, chain, &currency)
        );
        let fee = fee?;
        let mut estimate = fee.value;
        let mut metas = vec![fee.provenance];
        if let Some(p) = &price {
            for t in &mut estimate.tiers {
                if t.estimated_total_fiat.is_none() {
                    t.estimated_total_fiat = t
                        .estimated_total
                        .as_ref()
                        .and_then(|a| fiat_value(a, &p.value));
                }
            }
            metas.push(p.provenance.clone());
        }
        Ok(OpOutput::new(
            FeeOut {
                estimate,
                native_price: price.map(|p| p.value),
            },
            merge_meta(Some(chain.id.clone()), metas),
        ))
    }
}

// ------------------------------------------------------------------ tx_simulate

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SimulateIn {
    /// Chain (CAIP-2 id or alias).
    pub chain: String,
    /// Sender / fee payer.
    pub from: String,
    /// Unsigned transaction, e.g. the `tx` returned by tx_build_transfer or trade_build_swap_tx.
    pub tx: UnsignedTx,
}

fn check_tx_chain(chain: &ChainEntry, tx: &UnsignedTx) -> Result<(), DomainError> {
    match (chain.family, tx) {
        (ChainFamily::Evm, UnsignedTx::Evm { chain_id, .. }) => {
            if Some(*chain_id) == chain.id.evm_chain_id() {
                Ok(())
            } else {
                Err(DomainError::invalid(format!(
                    "tx chain_id {chain_id} does not match {}",
                    chain.id
                )))
            }
        }
        (ChainFamily::Solana, UnsignedTx::Solana { .. }) => Ok(()),
        _ => Err(DomainError::invalid(format!(
            "transaction family does not match {} ({:?})",
            chain.id, chain.family
        ))),
    }
}

async fn simulate(
    ctx: &Ctx,
    chain: &ChainEntry,
    from: &AccountAddress,
    tx: &UnsignedTx,
) -> Result<ems_routing::Routed<SimulationResult>, DomainError> {
    let req = ctx.route(Capability::Simulate).chain(chain.id.clone());
    ctx.router()
        .failover::<dyn Simulator, _, _, _>(req, |p| async move { p.simulate(from, tx).await })
        .await
        .map_err(|e| e.error)
}

pub struct TxSimulate;

#[async_trait]
impl Operation for TxSimulate {
    type Input = SimulateIn;
    type Output = SimulationResult;
    const NAME: &'static str = "tx_simulate";
    const DOMAIN: Domain = Domain::Tx;
    const DESCRIPTION: &'static str = "Dry-run an unsigned transaction without broadcasting it: \
        success or the revert/program error, gas or compute units used, and balance changes per \
        owner. EVM uses eth_simulateV1, then debug_traceCall, then eth_call; Solana uses \
        simulateTransaction. Always simulate before asking a user to sign.";
    const PROFILES: &'static [Profile] = ALL;

    async fn execute(
        &self,
        ctx: &Ctx,
        input: SimulateIn,
    ) -> Result<OpOutput<SimulationResult>, DomainError> {
        let chain = ctx.chain(&input.chain)?;
        let from = parse_address(chain, &input.from)?;
        check_tx_chain(chain, &input.tx)?;
        Ok(simulate(ctx, chain, &from, &input.tx).await?.into())
    }
}

// ------------------------------------------------------------------ tx_build_transfer

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BuildIn {
    /// Chain (CAIP-2 id or alias).
    pub chain: String,
    /// Sender (EVM address or Solana owner wallet; pays the fee).
    pub from: String,
    /// Recipient OWNER wallet (on Solana never a token account).
    pub to: String,
    /// Asset: omit or "native" for the native coin; else CAIP-19, token contract / mint, or a
    /// canonical stablecoin symbol like "USDC".
    #[serde(default)]
    pub asset: Option<String>,
    /// Amount in human units as an exact decimal string, e.g. "12.5" (never a float).
    pub amount: String,
    /// Solana: optional memo (SPL Memo), max 256 bytes.
    #[serde(default)]
    pub memo: Option<String>,
    /// Solana: create the recipient's associated token account if missing (sender pays rent).
    /// Default true.
    #[serde(default = "yes")]
    pub create_recipient_account: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct BuildOut {
    /// Unsigned transaction for the user's own signer. This server never holds keys.
    pub tx: UnsignedTx,
    pub asset: AssetId,
    pub amount: Amount,
    pub from: AccountAddress,
    pub to: AccountAddress,
    /// Solana token transfers: the recipient token account credited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient_token_account: Option<String>,
    /// Solana: the tx creates the recipient's associated token account.
    pub creates_recipient_account: bool,
    /// Simulation of this exact transaction (absent if no simulator was available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub simulation: Option<SimulationResult>,
    /// True only when the simulation succeeded.
    pub ready: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

pub struct TxBuildTransfer;

#[async_trait]
impl Operation for TxBuildTransfer {
    type Input = BuildIn;
    type Output = BuildOut;
    const NAME: &'static str = "tx_build_transfer";
    const DOMAIN: Domain = Domain::Tx;
    const DESCRIPTION: &'static str = "Build an UNSIGNED transfer of the native coin or a token \
        (ERC-20 on EVM; SPL / Token-2022 on Solana) for the user to sign with their own wallet, \
        then simulate it. EVM: EIP-1559 fields, nonce and gas limit filled. Solana: recent \
        blockhash, optional memo, and creation of the recipient's associated token account when \
        missing. Check `ready` and `warnings` before asking for a signature; after signing, send \
        with tx_broadcast. Amounts are exact decimal strings in human units.";
    const PROFILES: &'static [Profile] = SENDERS;

    async fn execute(&self, ctx: &Ctx, input: BuildIn) -> Result<OpOutput<BuildOut>, DomainError> {
        let chain = ctx.chain(&input.chain)?;
        let from = parse_address(chain, &input.from)?;
        let to = parse_address(chain, &input.to)?;
        let asset = resolve_asset(chain, input.asset.as_deref())?;
        let mut warnings = Vec::new();
        let built = match (from, to) {
            (AccountAddress::Evm(f), AccountAddress::Evm(t)) => {
                build_evm(ctx, chain, f, t, &asset, &input.amount, &mut warnings).await?
            }
            (AccountAddress::Solana(f), AccountAddress::Solana(t)) => {
                build_solana(ctx, chain, f, t, &asset, &input, &mut warnings).await?
            }
            _ => {
                return Err(DomainError::invalid(
                    "from/to belong to different chain families",
                ))
            }
        };
        let (simulation, sim_meta) = match simulate(ctx, chain, &from, &built.tx).await {
            Ok(r) => (Some(r.value), Some(r.provenance)),
            Err(e) => {
                warnings.push(format!("not simulated: {}", e.message));
                (None, None)
            }
        };
        if let Some(s) = simulation.as_ref().filter(|s| !s.success) {
            warnings.push(format!(
                "simulation failed: {}",
                s.error.as_deref().unwrap_or("unknown error")
            ));
        }
        let meta = merge_meta(
            Some(chain.id.clone()),
            std::iter::once(rpc_meta(&chain.id))
                .chain(sim_meta)
                .collect(),
        );
        Ok(OpOutput::new(
            BuildOut {
                ready: simulation.as_ref().is_some_and(|s| s.success),
                tx: built.tx,
                asset,
                amount: built.amount,
                from,
                to,
                recipient_token_account: built.recipient_token_account,
                creates_recipient_account: built.creates_account,
                simulation,
                warnings,
            },
            meta,
        ))
    }
}

struct Built {
    tx: UnsignedTx,
    amount: Amount,
    recipient_token_account: Option<String>,
    creates_account: bool,
}

/// ERC-20 `transfer(address,uint256)` calldata.
pub(crate) fn erc20_transfer_data(to: Address, amount: U256) -> String {
    format!("0xa9059cbb{:0>64}{amount:064x}", hex_str(to.as_slice()))
}

fn hex_str(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

async fn build_evm(
    ctx: &Ctx,
    chain: &ChainEntry,
    from: Address,
    to: Address,
    asset: &AssetId,
    amount: &str,
    warnings: &mut Vec<String>,
) -> Result<Built, DomainError> {
    let rpc = ctx.evm_rpc(chain)?;
    let (decimals, target, value, data) = match &asset.asset {
        AssetRef::Native { .. } => (chain.native.decimals, to, None, "0x".to_owned()),
        AssetRef::Erc20(token) => {
            let d = match stablecoin(asset) {
                Some(e) => e.decimals,
                None => evm::erc20::decimals(&rpc, *token).await?,
            };
            (d, *token, Some(()), String::new())
        }
        AssetRef::SplToken(_) => return Err(DomainError::invalid("SPL token on an EVM chain")),
    };
    let amount = Amount::parse_units(amount, decimals)?;
    if amount.is_zero() {
        return Err(DomainError::invalid("amount must be greater than zero"));
    }
    let (value, data) = match value {
        None => (amount.raw, data),
        Some(()) => (U256::ZERO, erc20_transfer_data(to, amount.raw)),
    };
    if stablecoin(asset).is_none() && !asset.is_native() {
        warnings.push(
            "token is not a canonical stablecoin in the registry: verify the contract".into(),
        );
    }
    let call = json!({ "from": from, "to": target, "value": format!("{value:#x}"), "data": data });
    let (nonce, gas, fee) = tokio::join!(
        rpc.request("eth_getTransactionCount", json!([from, "pending"])),
        rpc.request("eth_estimateGas", json!([call])),
        fee_estimate(ctx, chain)
    );
    let nonce = hex_u64(&nonce?);
    let gas_limit = match gas {
        // 21000 is exact for a plain transfer; pad contract calls by 20%.
        Ok(g) => hex_u64(&g).map(|g| if g == 21_000 { g } else { g + g / 5 }),
        Err(e) => {
            warnings.push(format!("gas estimation failed (likely to revert): {e}"));
            None
        }
    };
    let tier = match fee {
        Ok(f) => {
            let t = f.value.tiers;
            t.iter()
                .find(|t| t.speed == FeeSpeed::Standard)
                .or(t.first())
                .cloned()
        }
        Err(e) => {
            warnings.push(format!(
                "no fee estimate ({}); the signer must set fees",
                e.message
            ));
            None
        }
    };
    Ok(Built {
        tx: UnsignedTx::Evm {
            chain_id: rpc_chain_id(chain)?,
            to: target.to_checksum(None),
            data,
            value: value.to_string(),
            gas_limit,
            max_fee_per_gas: tier
                .as_ref()
                .and_then(|t| t.max_fee_per_gas)
                .map(|v| v.to_string()),
            max_priority_fee_per_gas: tier
                .as_ref()
                .and_then(|t| t.max_priority_fee_per_gas)
                .map(|v| v.to_string()),
            nonce,
        },
        amount,
        recipient_token_account: None,
        creates_account: false,
    })
}

fn rpc_chain_id(chain: &ChainEntry) -> Result<u64, DomainError> {
    chain
        .id
        .evm_chain_id()
        .ok_or_else(|| DomainError::internal(format!("{} has no EIP-155 id", chain.id)))
}

fn pubkey(s: &str) -> Result<SolanaPubkey, DomainError> {
    s.parse()
}

async fn build_solana(
    ctx: &Ctx,
    chain: &ChainEntry,
    from: SolanaPubkey,
    to: SolanaPubkey,
    asset: &AssetId,
    input: &BuildIn,
    warnings: &mut Vec<String>,
) -> Result<Built, DomainError> {
    let rpc = ctx.solana_rpc(chain)?;
    let recipient = sol_account(&rpc, &to.to_string()).await?;
    let rowner = recipient["owner"].as_str().unwrap_or_default();
    if is_token_program(rowner) {
        return Err(DomainError::invalid(format!(
            "{to} is a token account or mint, not a wallet: pass the owner wallet"
        )));
    }
    if !recipient.is_null() && rowner != SYSTEM_PROGRAM {
        warnings.push(format!(
            "recipient is owned by program {rowner}, not a wallet key"
        ));
    }
    let system = pubkey(SYSTEM_PROGRAM)?;
    let mut ixs = Vec::new();
    let mut recipient_token_account = None;
    let mut creates_account = false;
    let memo_ix = input
        .memo
        .as_deref()
        .map(|m| {
            if m.len() > 256 {
                return Err(DomainError::invalid("memo is longer than 256 bytes"));
            }
            Ok(SolIx {
                program: pubkey(MEMO_PROGRAM)?,
                accounts: vec![],
                data: m.as_bytes().to_vec(),
            })
        })
        .transpose()?;

    let amount = match &asset.asset {
        AssetRef::Native { .. } => {
            let amount = Amount::parse_units(&input.amount, chain.native.decimals)?;
            let lamports = to_u64(&amount)?;
            ixs.extend(memo_ix);
            let mut data = vec![2, 0, 0, 0];
            data.extend(lamports.to_le_bytes());
            ixs.push(SolIx {
                program: system,
                accounts: vec![AcctMeta::signer(from), AcctMeta::writable(to)],
                data,
            });
            amount
        }
        AssetRef::SplToken(mint) => {
            let m = sol_account(&rpc, &mint.to_string()).await?;
            let program_s = m["owner"].as_str().unwrap_or_default();
            let info = &m["data"]["parsed"]["info"];
            if !is_token_program(program_s) || m["data"]["parsed"]["type"] != "mint" {
                return Err(DomainError::invalid(format!(
                    "{mint} is not a token mint on {}",
                    chain.id
                )));
            }
            let decimals = info["decimals"]
                .as_u64()
                .and_then(|d| u8::try_from(d).ok())
                .ok_or_else(|| DomainError::internal("mint has no decimals"))?;
            let amount = Amount::parse_units(&input.amount, decimals)?;
            let raw = to_u64(&amount)?;
            let program = pubkey(program_s)?;
            let src = spl::associated_token_address(&from, mint, &program)?;
            let dst = spl::associated_token_address(&to, mint, &program)?;
            recipient_token_account = Some(dst.to_string());
            if sol_account(&rpc, &dst.to_string()).await?.is_null() {
                if !input.create_recipient_account {
                    return Err(DomainError::invalid(format!(
                        "recipient has no token account for {mint}; set create_recipient_account"
                    )));
                }
                creates_account = true;
                ixs.push(SolIx {
                    program: pubkey(spl::ASSOCIATED_TOKEN_PROGRAM)?,
                    accounts: vec![
                        AcctMeta::signer(from),
                        AcctMeta::writable(dst),
                        AcctMeta::readonly(to),
                        AcctMeta::readonly(*mint),
                        AcctMeta::readonly(system),
                        AcctMeta::readonly(program),
                    ],
                    data: vec![1], // CreateIdempotent
                });
            }
            ixs.extend(memo_ix);
            let mut data = vec![12]; // TransferChecked
            data.extend(raw.to_le_bytes());
            data.push(decimals);
            ixs.push(SolIx {
                program,
                accounts: vec![
                    AcctMeta::writable(src),
                    AcctMeta::readonly(*mint),
                    AcctMeta::writable(dst),
                    AcctMeta {
                        key: from,
                        signer: true,
                        writable: false,
                    },
                ],
                data,
            });
            if program_s == spl::TOKEN_2022_PROGRAM {
                warnings.push(
                    "Token-2022 mint: extensions (transfer fee, hook, memo-required) are checked \
                     only by the simulation"
                        .into(),
                );
            }
            amount
        }
        AssetRef::Erc20(_) => return Err(DomainError::invalid("ERC-20 token on Solana")),
    };
    if amount.is_zero() {
        return Err(DomainError::invalid("amount must be greater than zero"));
    }
    // ponytail: no compute-budget instructions; add a priority fee when landing rate matters.
    let bh = rpc
        .request("getLatestBlockhash", json!([{ "commitment": "confirmed" }]))
        .await?;
    let blockhash = bh["value"]["blockhash"]
        .as_str()
        .ok_or_else(|| DomainError::internal("getLatestBlockhash: no blockhash"))?;
    let last_valid_block_height = bh["value"]["lastValidBlockHeight"]
        .as_u64()
        .ok_or_else(|| DomainError::internal("getLatestBlockhash: no lastValidBlockHeight"))?;
    let message = compile_message(from, &ixs, pubkey(blockhash)?.0);
    Ok(Built {
        tx: UnsignedTx::Solana {
            message_base64: B64.encode(message),
            recent_blockhash: blockhash.to_owned(),
            last_valid_block_height,
        },
        amount,
        recipient_token_account,
        creates_account,
    })
}

fn to_u64(a: &Amount) -> Result<u64, DomainError> {
    u64::try_from(a.raw).map_err(|_| DomainError::invalid("amount exceeds u64 base units"))
}

#[derive(Debug, Clone, Copy)]
struct AcctMeta {
    key: SolanaPubkey,
    signer: bool,
    writable: bool,
}

impl AcctMeta {
    fn signer(key: SolanaPubkey) -> Self {
        Self {
            key,
            signer: true,
            writable: true,
        }
    }
    fn writable(key: SolanaPubkey) -> Self {
        Self {
            key,
            signer: false,
            writable: true,
        }
    }
    fn readonly(key: SolanaPubkey) -> Self {
        Self {
            key,
            signer: false,
            writable: false,
        }
    }
}

struct SolIx {
    program: SolanaPubkey,
    accounts: Vec<AcctMeta>,
    data: Vec<u8>,
}

fn compact_u16(mut n: usize, out: &mut Vec<u8>) {
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

/// Serialize a legacy Solana message (header, account keys, blockhash, instructions).
fn compile_message(payer: SolanaPubkey, ixs: &[SolIx], blockhash: [u8; 32]) -> Vec<u8> {
    let mut keys = vec![AcctMeta::signer(payer)];
    let mut add = |m: AcctMeta| match keys.iter_mut().find(|k| k.key == m.key) {
        Some(k) => {
            k.signer |= m.signer;
            k.writable |= m.writable;
        }
        None => keys.push(m),
    };
    for ix in ixs {
        ix.accounts.iter().for_each(|a| add(*a));
        add(AcctMeta::readonly(ix.program));
    }
    // Stable sort keeps the fee payer first: writable signers, readonly signers, writable, readonly.
    keys.sort_by_key(|k| (!k.signer, !k.writable));
    let idx = |key: &SolanaPubkey| keys.iter().position(|k| &k.key == key).expect("key") as u8;
    let count = |s: bool, w: bool| {
        keys.iter()
            .filter(|k| k.signer == s && k.writable == w)
            .count() as u8
    };
    let mut out = vec![
        keys.iter().filter(|k| k.signer).count() as u8,
        count(true, false),
        count(false, false),
    ];
    compact_u16(keys.len(), &mut out);
    keys.iter().for_each(|k| out.extend(k.key.0));
    out.extend(blockhash);
    compact_u16(ixs.len(), &mut out);
    for ix in ixs {
        out.push(idx(&ix.program));
        compact_u16(ix.accounts.len(), &mut out);
        out.extend(ix.accounts.iter().map(|a| idx(&a.key)));
        compact_u16(ix.data.len(), &mut out);
        out.extend(&ix.data);
    }
    out
}

// ------------------------------------------------------------------ tx_broadcast

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BroadcastIn {
    /// Chain (CAIP-2 id or alias).
    pub chain: String,
    /// The SIGNED transaction: 0x-hex raw transaction (EVM) or base64 wire transaction (Solana).
    /// Never pass a private key or seed phrase; they are rejected.
    pub signed_tx: String,
    /// Send through a private relay (Flashbots / MEV Blocker on Ethereum, Jito on Solana) to
    /// avoid front-running. Fails on chains without a private mempool instead of going public.
    #[serde(default)]
    pub private: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Rejection {
    pub vendor: String,
    pub error: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct BroadcastOut {
    /// Hash / signature computed locally from the signed payload (identical everywhere).
    pub tx_hash: String,
    pub private: bool,
    pub accepted_by: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rejected: Vec<Rejection>,
    /// Vendors that reported a different hash than the local one (should never happen).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hash_mismatch: Vec<String>,
}

/// True for inputs that look like key material: 32-byte hex (EVM private key), a 64-byte key
/// (Solana keypair as base58 or JSON bytes), an extended private key, or a BIP-39-style phrase.
pub(crate) fn looks_like_secret(s: &str) -> bool {
    let t = s.trim();
    let hex = t.strip_prefix("0x").unwrap_or(t);
    let words: Vec<&str> = t.split_whitespace().collect();
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        || bs58::decode(t).into_vec().is_ok_and(|v| v.len() == 64)
        || (t.starts_with('[') && t.split(',').count() == 64)
        || ["xprv", "tprv", "yprv", "zprv"]
            .iter()
            .any(|p| t.starts_with(p))
        || (matches!(words.len(), 12 | 15 | 18 | 21 | 24)
            && words
                .iter()
                .all(|w| (3..=8).contains(&w.len()) && w.bytes().all(|b| b.is_ascii_lowercase())))
}

/// Tx hash / signature computed from the signed payload, so resends are safe and verifiable.
pub(crate) fn local_tx_hash(family: ChainFamily, signed: &str) -> Result<String, DomainError> {
    let t = signed.trim();
    match family {
        ChainFamily::Evm => {
            let bytes = t
                .strip_prefix("0x")
                .and_then(hex_decode)
                .filter(|b| b.len() > 64)
                .ok_or_else(|| {
                    DomainError::invalid("signed_tx must be 0x-prefixed raw transaction hex")
                })?;
            Ok(format!("{:#x}", keccak256(&bytes)))
        }
        ChainFamily::Solana => {
            let bytes = B64.decode(t).map_err(|_| {
                DomainError::invalid("signed_tx must be a base64 Solana transaction")
            })?;
            // compact-u16 signature count (< 128 signatures fits one byte), then 64-byte signatures
            let sig = match bytes.as_slice() {
                [n, rest @ ..] if *n > 0 && *n < 0x80 && rest.len() >= 64 => &rest[..64],
                _ => return Err(DomainError::invalid("signed_tx has no signature")),
            };
            if sig.iter().all(|b| *b == 0) {
                return Err(DomainError::invalid(
                    "transaction is not signed (empty signature)",
                ));
            }
            Ok(bs58::encode(sig).into_string())
        }
    }
}

pub struct TxBroadcast;

#[async_trait]
impl Operation for TxBroadcast {
    type Input = BroadcastIn;
    type Output = BroadcastOut;
    const NAME: &'static str = "tx_broadcast";
    const DOMAIN: Domain = Domain::Tx;
    const DESCRIPTION: &'static str =
        "Broadcast an already-SIGNED transaction to every configured \
        provider at once (the hash is computed locally, so duplicate sends are safe) and return \
        the hash plus which providers accepted it. Set `private` to use a private relay against \
        front-running; chains without one return an error rather than going public. Then poll \
        tx_status. This has side effects: it sends money. It never accepts private keys or seed \
        phrases; sign in the user's wallet.";
    const PROFILES: &'static [Profile] = ALL;
    const READ_ONLY: bool = false;

    async fn execute(
        &self,
        ctx: &Ctx,
        input: BroadcastIn,
    ) -> Result<OpOutput<BroadcastOut>, DomainError> {
        if looks_like_secret(&input.signed_tx) {
            return Err(DomainError::invalid(
                "input looks like a private key or seed phrase. This server never accepts \
                 secrets: sign in the user's wallet and pass the signed transaction",
            ));
        }
        let chain = ctx.chain(&input.chain)?;
        let tx_hash = local_tx_hash(chain.family, &input.signed_tx)?;
        let cap = if input.private {
            Capability::PrivateRelay
        } else {
            Capability::Broadcast
        };
        let req = ctx.route(cap).chain(chain.id.clone());
        if input.private
            && ctx
                .router()
                .candidates::<dyn Broadcaster>(ctx.table(), &req)
                .list
                .is_empty()
        {
            return Err(DomainError::new(
                ErrorCode::UnsupportedCapability,
                format!(
                    "no private relay available on {} (no private mempool)",
                    chain.id
                ),
            )
            .with_hint("send with private=false to use the public mempool"));
        }
        let signed = input.signed_tx.trim();
        let r = ctx
            .router()
            .fan_out::<dyn Broadcaster, _, _, _>(req, |p| async move { p.send_raw(signed).await })
            .await
            .map_err(|e| e.error)?;
        let out = summarize(ctx, &tx_hash, input.private, r.value);
        Ok(OpOutput::new(out, r.provenance))
    }
}

fn summarize(
    ctx: &Ctx,
    tx_hash: &str,
    private: bool,
    results: Vec<(String, Result<BroadcastReceipt, ProviderError>)>,
) -> BroadcastOut {
    let mut out = BroadcastOut {
        tx_hash: tx_hash.to_owned(),
        private,
        accepted_by: Vec::new(),
        rejected: Vec::new(),
        hash_mismatch: Vec::new(),
    };
    for (vendor, r) in results {
        match r {
            Ok(rc) => {
                if !rc.tx_hash.eq_ignore_ascii_case(tx_hash) {
                    out.hash_mismatch.push(vendor.clone());
                }
                out.accepted_by.push(vendor);
            }
            Err(e) => out.rejected.push(Rejection {
                vendor,
                error: ctx.config().scrub(&e.to_string()),
            }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_detected() {
        let key = "0x4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";
        assert!(looks_like_secret(key));
        assert!(looks_like_secret(&key[2..]));
        assert!(looks_like_secret(
            "legal winner thank year wave sausage worth useful legal winner thank yellow"
        ));
        assert!(looks_like_secret(&format!("[{}]", vec!["1"; 64].join(","))));
        assert!(looks_like_secret(&bs58::encode([7u8; 64]).into_string()));
        assert!(looks_like_secret("xprv9s21ZrQH143K3QTDL4LXw2F7HEK3wJUD2nW2nRk4stbPy6cq3jPPqjiChkVvvNKmPGJxWUtg6LnF5kejMRNNU3TGtRBeJgk33yuGBxrMPHi"));
        // a signed EIP-1559 tx is not a secret
        assert!(!looks_like_secret(&format!("0x02f8{}", "ab".repeat(100))));
        assert!(!looks_like_secret(&B64.encode([1u8; 200])));
        assert!(!looks_like_secret("Hello there general kenobi"));
    }

    #[test]
    fn local_hashes() {
        let raw = format!("0x02f8{}", "ab".repeat(100));
        let h = local_tx_hash(ChainFamily::Evm, &raw).unwrap();
        assert_eq!(
            h,
            format!("{:#x}", keccak256(hex_decode(&raw[2..]).unwrap()))
        );
        assert!(local_tx_hash(ChainFamily::Evm, "0x1234").is_err());

        let mut sol = vec![1u8];
        sol.extend([9u8; 64]);
        sol.extend([0u8; 40]);
        assert_eq!(
            local_tx_hash(ChainFamily::Solana, &B64.encode(&sol)).unwrap(),
            bs58::encode([9u8; 64]).into_string()
        );
        sol[1..65].fill(0);
        assert!(local_tx_hash(ChainFamily::Solana, &B64.encode(&sol)).is_err());
    }

    #[test]
    fn erc20_calldata() {
        let to: Address = "0x00000000000000000000000000000000000000aa"
            .parse()
            .unwrap();
        let d = erc20_transfer_data(to, U256::from(1_500_000u64));
        assert_eq!(d.len(), 2 + 8 + 128);
        assert!(d.starts_with("0xa9059cbb000000000000000000000000"));
        assert!(d.ends_with("16e360"));
        assert!(d[10..74].ends_with("aa"));
    }

    #[test]
    fn solana_status_expires_only_after_last_valid_height() {
        assert_eq!(
            unseen_solana_status(Some(100), Some(101)),
            TxStatus::Dropped
        );
        assert_eq!(
            unseen_solana_status(Some(100), Some(100)),
            TxStatus::Pending
        );
        assert_eq!(unseen_solana_status(Some(100), None), TxStatus::Pending);
        assert_eq!(unseen_solana_status(None, Some(5)), TxStatus::NotFound);
    }

    #[test]
    fn sol_transfer_message_layout() {
        let from = SolanaPubkey([1; 32]);
        let to = SolanaPubkey([2; 32]);
        let mut data = vec![2, 0, 0, 0];
        data.extend(5u64.to_le_bytes());
        let ix = SolIx {
            program: SolanaPubkey([0; 32]),
            accounts: vec![AcctMeta::signer(from), AcctMeta::writable(to)],
            data,
        };
        let m = compile_message(from, &[ix], [3; 32]);
        assert_eq!(&m[..4], &[1, 0, 1, 3], "header + 3 keys");
        assert_eq!(&m[4..36], &[1; 32], "fee payer first");
        assert_eq!(&m[36..68], &[2; 32]);
        assert_eq!(&m[68..100], &[0; 32], "system program last (readonly)");
        assert_eq!(&m[100..132], &[3; 32], "blockhash");
        assert_eq!(
            &m[132..137],
            &[1, 2, 2, 0, 1],
            "1 ix, program idx 2, accounts [0,1]"
        );
        assert_eq!(m[137], 12, "data len");
        assert_eq!(m.len(), 138 + 12);
    }

    #[test]
    fn compact_u16_encoding() {
        let enc = |n| {
            let mut v = Vec::new();
            compact_u16(n, &mut v);
            v
        };
        assert_eq!(enc(0), [0]);
        assert_eq!(enc(0x7f), [0x7f]);
        assert_eq!(enc(0x80), [0x80, 0x01]);
        assert_eq!(enc(0x3fff), [0xff, 0x7f]);
    }
}
