//! Normalized Solana transactions. Owner: `solana` (T1.S1). Used by `tx_get`, `payments_verify_transfer`.
//!
//! Amounts come from `pre/postTokenBalances` deltas per owner (not instruction amounts): handles
//! inner instructions, multiple transfers, Token-2022 transfer fees (net + `withheld_fee`);
//! confidential transfers → `Finality::Unverifiable`.
//!
//! ## Broadcasting and resends (for `tx_broadcast`, owned by `neobank-wallet`)
//! The tx id is the first signature, so [`signature_of`] computes it locally before sending:
//! resending the same signed bytes is idempotent. Keep resending (every ~2 s) until the
//! signature shows up in `getSignatureStatuses` or `getBlockHeight` passes the message's
//! `lastValidBlockHeight`; only then report `TxStatus::Dropped` (expired blockhash).
//!
//! ## Wire format
//! A small decoder for the transaction wire format (compact-u16 arrays; legacy and v0
//! messages) backs [`signature_of`], [`check_tip`] and [`transaction_for_simulation`].
//! Spec: <https://solana.com/docs/core/transactions>.

use super::{
    finality_from_status, native_asset,
    spl::{TOKEN_2022_PROGRAM, TOKEN_PROGRAMS},
    token_asset,
};
use alloy_primitives::U256;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use bdm_config::ChainEntry;
use bdm_domain::{
    AccountAddress, Amount, BalanceDelta, BlockRef, DomainError, Finality, SolanaPubkey, Transfer,
    TransferKind, Tx, TxStatus,
};
use bdm_ports::{PortResult, ProviderError, SolanaRpc};
use chrono::DateTime;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

/// System program (native SOL transfers). <https://solana.com/docs/core/programs>
pub const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";
/// Compute Budget program. <https://solana.com/docs/core/fees#compute-unit-price>
pub const COMPUTE_BUDGET_PROGRAM: &str = "ComputeBudget111111111111111111111111111111";

/// `getTransaction` config every reader uses (versioned txs, lookup-table keys resolved).
pub fn get_transaction_config(commitment: &str) -> Value {
    json!({"encoding": "jsonParsed", "maxSupportedTransactionVersion": 0, "commitment": commitment})
}

/// Look up one transaction. `Ok(None)` if no node knows the signature. Finality comes from
/// `getSignatureStatuses` mapped through the chain policy (see the `solana` module docs).
#[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
pub async fn get_tx(
    rpc: &dyn SolanaRpc,
    chain: &ChainEntry,
    signature: &str,
) -> PortResult<Option<Tx>> {
    let sig_ok = bs58::decode(signature)
        .into_vec()
        .is_ok_and(|b| b.len() == 64);
    if !sig_ok {
        return Err(ProviderError::Invalid(format!(
            "'{signature}' is not a Solana transaction signature"
        )));
    }
    let st = rpc
        .request(
            "getSignatureStatuses",
            json!([[signature], {"searchTransactionHistory": true}]),
        )
        .await?;
    let status = &st["value"][0];
    if status.is_null() {
        return Ok(None);
    }
    let finality = finality_from_status(
        chain,
        status["confirmationStatus"].as_str(),
        status["confirmations"].as_u64(),
    );
    let v = rpc
        .request(
            "getTransaction",
            json!([signature, get_transaction_config("confirmed")]),
        )
        .await?;
    if v.is_null() {
        // Seen at `processed` only: getTransaction does not serve that commitment.
        return Ok(Some(Tx {
            chain: chain.id.clone(),
            hash: signature.to_owned(),
            status: TxStatus::Pending,
            finality: Finality::Pending,
            block: status["slot"].as_u64().map(|number| BlockRef {
                number,
                hash: None,
                timestamp: None,
            }),
            from: None,
            to: None,
            fee: None,
            transfers: Vec::new(),
            balance_deltas: Vec::new(),
            raw: None,
        }));
    }
    parse_tx_with_finality(chain, &v, finality)
        .map(Some)
        .map_err(|e| ProviderError::Transient(e.to_string()))
}

/// Pure parser over a `getTransaction` (jsonParsed, maxSupportedTransactionVersion 0) result.
/// The result carries no commitment, so the caller supplies `finality` (e.g. from
/// `getSignatureStatuses`). Confidential transfers always yield `Unverifiable`.
#[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
pub fn parse_tx_with_finality(
    chain: &ChainEntry,
    tx: &Value,
    finality: Finality,
) -> Result<Tx, DomainError> {
    let meta = &tx["meta"];
    if meta.is_null() {
        return Err(DomainError::invalid("transaction has no meta"));
    }
    let msg = &tx["transaction"]["message"];
    let keys = account_keys(&msg["accountKeys"]);
    let hash = tx["transaction"]["signatures"][0]
        .as_str()
        .ok_or_else(|| DomainError::invalid("transaction has no signature"))?
        .to_owned();
    let slot = tx["slot"]
        .as_u64()
        .ok_or_else(|| DomainError::invalid("transaction has no slot"))?;

    let ixs = instructions(msg, meta);
    let confidential = ixs.iter().any(|(_, _, ix)| is_confidential(ix));
    let raw: Vec<RawTransfer> = ixs
        .iter()
        .filter_map(|(pos, inner, ix)| raw_transfer(ix, *pos, *inner))
        .collect();
    let accounts = token_accounts(&keys, meta);

    let block = BlockRef {
        number: slot,
        hash: None,
        timestamp: tx["blockTime"]
            .as_i64()
            .and_then(|t| DateTime::from_timestamp(t, 0)),
    };
    let transfers = raw
        .iter()
        .filter_map(|r| to_transfer(chain, &hash, &block, r, &accounts))
        .collect();
    Ok(Tx {
        chain: chain.id.clone(),
        hash,
        status: if meta["err"].is_null() {
            TxStatus::Success
        } else {
            TxStatus::Failed
        },
        finality: if confidential {
            Finality::Unverifiable
        } else {
            finality
        },
        from: keys.first().and_then(|k| k.parse().ok()),
        to: None,
        fee: meta["fee"]
            .as_u64()
            .map(|f| Amount::from_u128(f.into(), chain.native.decimals)),
        balance_deltas: balance_deltas(chain, &keys, meta, &raw)?,
        block: Some(block),
        transfers,
        raw: None,
    })
}

/// `accountKeys` in jsonParsed are objects (`{pubkey, signer, writable, source}`); in other
/// encodings they are strings. Lookup-table keys are already appended by the node.
fn account_keys(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|k| k.as_str().or_else(|| k["pubkey"].as_str()))
        .map(str::to_owned)
        .collect()
}

/// Outer instructions then their CPIs: `(position, is_inner, ix)`. Position encodes
/// `(outer << 16) | inner` with inner = 0 for the outer instruction, 1.. for CPIs.
fn instructions<'a>(msg: &'a Value, meta: &'a Value) -> Vec<(u64, bool, &'a Value)> {
    let mut out = Vec::new();
    for (i, ix) in msg["instructions"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        out.push(((i as u64) << 16, false, ix));
        for group in meta["innerInstructions"].as_array().into_iter().flatten() {
            if group["index"].as_u64() == Some(i as u64) {
                for (j, inner) in group["instructions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    out.push((((i as u64) << 16) | (j as u64 + 1), true, inner));
                }
            }
        }
    }
    out
}

/// Token-2022 confidential-transfer instruction: parsed types contain "confidential"
/// (`confidentialTransfer`, `depositConfidentialTransfer`, …); unparsed ones start with the
/// `ConfidentialTransferExtension` (27) or `ConfidentialTransferFeeExtension` (37) tag.
/// Source: spl-token-2022 `TokenInstruction` enum (program/src/instruction.rs).
#[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
fn is_confidential(ix: &Value) -> bool {
    if ix["programId"].as_str() != Some(TOKEN_2022_PROGRAM) {
        return false;
    }
    if let Some(t) = ix["parsed"]["type"].as_str() {
        return t.to_ascii_lowercase().contains("confidential");
    }
    ix["data"]
        .as_str()
        .and_then(|d| bs58::decode(d).into_vec().ok())
        .and_then(|b| b.first().copied())
        .is_some_and(|tag| matches!(tag, 27 | 37))
}

#[derive(Debug)]
struct RawTransfer {
    pos: u64,
    inner: bool,
    native: bool,
    source: String,
    destination: String,
    authority: Option<String>,
    amount: U256,
    mint: Option<String>,
    decimals: Option<u8>,
}

#[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
fn raw_transfer(ix: &Value, pos: u64, inner: bool) -> Option<RawTransfer> {
    let program = ix["programId"].as_str()?;
    let kind = ix["parsed"]["type"].as_str()?;
    let info = &ix["parsed"]["info"];
    let s = |k: &str| info[k].as_str().map(str::to_owned);
    if program == SYSTEM_PROGRAM && matches!(kind, "transfer" | "transferWithSeed") {
        return Some(RawTransfer {
            pos,
            inner,
            native: true,
            source: s("source")?,
            destination: s("destination")?,
            authority: None,
            amount: U256::from(info["lamports"].as_u64()?),
            mint: None,
            decimals: None,
        });
    }
    if TOKEN_PROGRAMS.contains(&program)
        && matches!(
            kind,
            "transfer" | "transferChecked" | "transferCheckedWithFee"
        )
    {
        let amount = info["amount"]
            .as_str()
            .or_else(|| info["tokenAmount"]["amount"].as_str())?;
        return Some(RawTransfer {
            pos,
            inner,
            native: false,
            source: s("source")?,
            destination: s("destination")?,
            authority: s("authority").or_else(|| s("multisigAuthority")),
            amount: U256::from_str_radix(amount, 10).ok()?,
            mint: s("mint"),
            decimals: info["tokenAmount"]["decimals"]
                .as_u64()
                .and_then(|d| u8::try_from(d).ok()),
        });
    }
    None
}

/// One token account as seen in `pre/postTokenBalances`.
#[derive(Debug, Default)]
struct TokenAcct {
    owner: String,
    mint: String,
    program: String,
    decimals: u8,
    pre: U256,
    post: U256,
}

/// Token accounts keyed by address. A missing pre (or post) entry counts as 0.
#[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
fn token_accounts(keys: &[String], meta: &Value) -> BTreeMap<String, TokenAcct> {
    let mut out: BTreeMap<String, TokenAcct> = BTreeMap::new();
    for (field, post) in [("preTokenBalances", false), ("postTokenBalances", true)] {
        for b in meta[field].as_array().into_iter().flatten() {
            let Some(addr) = b["accountIndex"]
                .as_u64()
                .and_then(|i| keys.get(i as usize))
            else {
                continue;
            };
            let amount = b["uiTokenAmount"]["amount"]
                .as_str()
                .and_then(|a| U256::from_str_radix(a, 10).ok())
                .unwrap_or_default();
            let a = out.entry(addr.clone()).or_default();
            // Old transactions lack `owner`: fall back to the token account itself.
            a.owner = b["owner"].as_str().unwrap_or(addr).to_owned();
            a.mint = b["mint"].as_str().unwrap_or_default().to_owned();
            a.program = b["programId"].as_str().unwrap_or_default().to_owned();
            a.decimals = b["uiTokenAmount"]["decimals"].as_u64().unwrap_or(0) as u8;
            if post {
                a.post = amount;
            } else {
                a.pre = amount;
            }
        }
    }
    out
}

fn to_transfer(
    chain: &ChainEntry,
    hash: &str,
    block: &BlockRef,
    r: &RawTransfer,
    accounts: &BTreeMap<String, TokenAcct>,
) -> Option<Transfer> {
    let (kind, asset, from, to, decimals) = if r.native {
        let kind = if r.inner {
            TransferKind::Internal
        } else {
            TransferKind::Native
        };
        let (from, to) = (r.source.clone(), r.destination.clone());
        (kind, native_asset(chain), from, to, chain.native.decimals)
    } else {
        let src = accounts.get(&r.source);
        let dst = accounts.get(&r.destination);
        let mint = r
            .mint
            .clone()
            .or_else(|| dst.or(src).map(|a| a.mint.clone()))?;
        let decimals = r.decimals.or_else(|| dst.or(src).map(|a| a.decimals))?;
        let from = src
            .map(|a| a.owner.clone())
            .or_else(|| r.authority.clone())
            .unwrap_or_else(|| r.source.clone());
        let to = dst
            .map(|a| a.owner.clone())
            .unwrap_or_else(|| r.destination.clone());
        let asset = token_asset(chain, mint.parse().ok()?);
        (TransferKind::Token, asset, from, to, decimals)
    };
    Some(Transfer {
        chain: chain.id.clone(),
        tx_hash: hash.to_owned(),
        log_index: Some(r.pos),
        kind,
        asset,
        from: from.parse().ok(),
        to: to.parse().ok()?,
        amount: Amount::new(r.amount, decimals),
        block: Some(block.clone()),
    })
}

/// Per-owner balance changes: tokens grouped by (owner, mint) across all of the owner's token
/// accounts; native SOL per account (token accounts' rent lamports excluded). Unchanged
/// balances are omitted.
///
/// Token-2022 transfer fee: a receiving Token-2022 account (no outflow in this tx) whose
/// balance grew by less than the gross amount transferred in has the difference withheld
/// (unspendable until harvested) → `withheld_fee`. `after - before` is the net received.
fn balance_deltas(
    chain: &ChainEntry,
    keys: &[String],
    meta: &Value,
    raw: &[RawTransfer],
) -> Result<Vec<BalanceDelta>, DomainError> {
    let accounts = token_accounts(keys, meta);
    #[derive(Default)]
    struct Agg {
        decimals: u8,
        before: U256,
        after: U256,
        withheld: U256,
    }
    let mut by_owner: BTreeMap<(String, String), Agg> = BTreeMap::new();
    for (addr, a) in &accounts {
        let g = by_owner
            .entry((a.owner.clone(), a.mint.clone()))
            .or_default();
        g.decimals = a.decimals;
        g.before += a.pre;
        g.after += a.post;
        let outflow = raw.iter().any(|r| !r.native && &r.source == addr);
        if a.program == TOKEN_2022_PROGRAM && !outflow {
            let gross: U256 = raw
                .iter()
                .filter(|r| !r.native && &r.destination == addr)
                .map(|r| r.amount)
                .sum();
            let net = a.post.saturating_sub(a.pre);
            g.withheld += gross.saturating_sub(net);
        }
    }

    let addr = |s: &str| -> Result<AccountAddress, DomainError> {
        s.parse::<SolanaPubkey>().map(AccountAddress::Solana)
    };
    let mut out = Vec::new();
    for ((owner, mint), g) in by_owner {
        if g.before == g.after && g.withheld.is_zero() {
            continue;
        }
        out.push(BalanceDelta {
            owner: addr(&owner)?,
            asset: token_asset(chain, mint.parse()?),
            before: Amount::new(g.before, g.decimals),
            after: Amount::new(g.after, g.decimals),
            withheld_fee: (!g.withheld.is_zero()).then(|| Amount::new(g.withheld, g.decimals)),
        });
    }
    let lamports = |field: &str| -> Vec<u64> {
        meta[field]
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| v.as_u64().unwrap_or(0))
            .collect()
    };
    let (pre, post) = (lamports("preBalances"), lamports("postBalances"));
    for (i, key) in keys.iter().enumerate() {
        let (Some(&b), Some(&a)) = (pre.get(i), post.get(i)) else {
            continue;
        };
        if a == b || accounts.contains_key(key) {
            continue;
        }
        out.push(BalanceDelta {
            owner: addr(key)?,
            asset: native_asset(chain),
            before: Amount::from_u128(b.into(), chain.native.decimals),
            after: Amount::from_u128(a.into(), chain.native.decimals),
            withheld_fee: None,
        });
    }
    Ok(out)
}

/// Balance changes from a `simulateTransaction` value (Agave returns `pre/postBalances`,
/// `pre/postTokenBalances` and `loadedAddresses`). Empty if the node omits them.
#[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
pub(crate) fn simulated_deltas(
    chain: &ChainEntry,
    message: &[u8],
    value: &Value,
) -> Result<Vec<BalanceDelta>, DomainError> {
    if value["preBalances"].is_null() {
        return Ok(Vec::new());
    }
    let msg = decode_message(message).map_err(DomainError::invalid)?;
    let mut keys: Vec<String> = msg
        .keys
        .iter()
        .map(|k| bs58::encode(k).into_string())
        .collect();
    for part in ["writable", "readonly"] {
        keys.extend(
            value["loadedAddresses"][part]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|k| k.as_str().map(str::to_owned)),
        );
    }
    balance_deltas(chain, &keys, value, &[])
}

// ------------------------------------------------------------------ wire format

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).ok_or("truncated transaction")?;
        let s = self.b.get(self.pos..end).ok_or("truncated transaction")?;
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        self.take(1)?
            .first()
            .copied()
            .ok_or_else(|| "truncated transaction".into())
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        self.take(N)?
            .try_into()
            .map_err(|_| "truncated transaction".into())
    }
    /// compact-u16: 7 bits per byte, little-endian, high bit = continuation (max 3 bytes).
    fn compact(&mut self) -> Result<usize, String> {
        let mut v = 0usize;
        for i in 0..3 {
            let b = self.u8()?;
            v |= ((b & 0x7f) as usize) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err("bad compact-u16".into())
    }
}

fn encode_compact(mut n: usize, out: &mut Vec<u8>) {
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

pub(crate) struct WireIx {
    pub program_index: u8,
    pub accounts: Vec<u8>,
    pub data: Vec<u8>,
}

pub(crate) struct WireMessage {
    pub num_required_signatures: u8,
    /// Static account keys only (v0 lookup-table keys are not resolved here).
    pub keys: Vec<[u8; 32]>,
    pub recent_blockhash: [u8; 32],
    pub instructions: Vec<WireIx>,
}

pub(crate) fn decode_message(b: &[u8]) -> Result<WireMessage, String> {
    let mut r = Reader { b, pos: 0 };
    let first = r.u8()?;
    let num_required_signatures = if first & 0x80 != 0 {
        if first & 0x7f != 0 {
            return Err(format!("unsupported transaction version {}", first & 0x7f));
        }
        r.u8()?
    } else {
        first
    };
    r.take(2)?; // read-only signed / unsigned counts
    let keys = (0..r.compact()?)
        .map(|_| r.array::<32>())
        .collect::<Result<Vec<[u8; 32]>, _>>()?;
    let recent_blockhash = r.array::<32>()?;
    let mut instructions = Vec::new();
    for _ in 0..r.compact()? {
        let program_index = r.u8()?;
        let n = r.compact()?;
        let accounts = r.take(n)?.to_vec();
        let n = r.compact()?;
        let data = r.take(n)?.to_vec();
        instructions.push(WireIx {
            program_index,
            accounts,
            data,
        });
    }
    Ok(WireMessage {
        num_required_signatures,
        keys,
        recent_blockhash,
        instructions,
    })
}

/// Split a signed transaction into (signatures, message bytes).
fn decode_tx(b: &[u8]) -> Result<(Vec<[u8; 64]>, &[u8]), String> {
    let mut r = Reader { b, pos: 0 };
    let sigs = (0..r.compact()?)
        .map(|_| r.array::<64>())
        .collect::<Result<Vec<[u8; 64]>, _>>()?;
    let msg = b.get(r.pos..).ok_or("truncated transaction")?;
    Ok((sigs, msg))
}

fn decode_signed(signed_base64: &str) -> Result<(Vec<[u8; 64]>, WireMessage), DomainError> {
    let bytes = B64
        .decode(signed_base64.trim())
        .map_err(|_| DomainError::invalid("signed transaction is not valid base64"))?;
    let (sigs, msg) = decode_tx(&bytes).map_err(DomainError::invalid)?;
    let msg = decode_message(msg).map_err(DomainError::invalid)?;
    if sigs.is_empty() || sigs.len() != msg.num_required_signatures as usize {
        return Err(DomainError::invalid(format!(
            "transaction has {} signatures, message requires {}",
            sigs.len(),
            msg.num_required_signatures
        )));
    }
    Ok((sigs, msg))
}

/// Transaction id of a signed base64 transaction: its first signature, base58. Computed
/// locally, identical on every provider, so resends are safe. Rejects unsigned payloads.
pub fn signature_of(signed_base64: &str) -> Result<String, DomainError> {
    let (sigs, _) = decode_signed(signed_base64)?;
    let first = sigs.first().filter(|s| **s != [0u8; 64]);
    let first = first.ok_or_else(|| DomainError::invalid("transaction is not signed"))?;
    Ok(bs58::encode(first).into_string())
}

/// Relay precondition check (Helius Sender, Jito `bundleOnly`): the signed tx must contain a
/// Compute Budget `SetComputeUnitPrice` instruction (tag 3) when `require_compute_price`, and
/// System `Transfer`s (tag 2) totalling ≥ `min_tip_lamports` to one of `tip_accounts`.
/// Tip destinations must be static keys (a tip routed through a lookup table is not seen).
/// Instruction tags: solana compute-budget-interface / system-interface instruction enums.
pub fn check_tip(
    signed_base64: &str,
    tip_accounts: &[&str],
    min_tip_lamports: u64,
    require_compute_price: bool,
) -> Result<(), DomainError> {
    let (_, msg) = decode_signed(signed_base64)?;
    let key = |i: u8| {
        msg.keys
            .get(i as usize)
            .map(|k| bs58::encode(k).into_string())
    };
    let (mut price, mut tip) = (false, 0u64);
    for ix in &msg.instructions {
        match key(ix.program_index).as_deref() {
            Some(COMPUTE_BUDGET_PROGRAM) if ix.data.len() == 9 && ix.data.first() == Some(&3) => {
                price = true;
            }
            Some(SYSTEM_PROGRAM) if ix.data.len() == 12 && ix.data.starts_with(&[2, 0, 0, 0]) => {
                let dest = ix.accounts.get(1).and_then(|&i| key(i));
                let lamports = ix
                    .data
                    .get(4..12)
                    .and_then(|b| <[u8; 8]>::try_from(b).ok())
                    .map(u64::from_le_bytes);
                if let (Some(lamports), true) = (
                    lamports,
                    dest.is_some_and(|d| tip_accounts.contains(&d.as_str())),
                ) {
                    tip = tip.saturating_add(lamports);
                }
            }
            _ => {}
        }
    }
    if require_compute_price && !price {
        return Err(DomainError::invalid(
            "transaction has no SetComputeUnitPrice instruction",
        ));
    }
    if tip < min_tip_lamports {
        return Err(DomainError::invalid(format!(
            "tip of {tip} lamports to a relay tip account is below the minimum {min_tip_lamports}"
        )));
    }
    Ok(())
}

/// Message of an UNSIGNED serialized transaction (e.g. from a swap API): `(message_base64,
/// recent_blockhash)`. Fails if any signature slot is filled, since a partially signed
/// transaction (e.g. an RFQ maker's signature) cannot travel as a bare message.
pub fn unsigned_message_of(tx_base64: &str) -> Result<(String, String), DomainError> {
    let bytes = B64
        .decode(tx_base64.trim())
        .map_err(|_| DomainError::invalid("transaction is not valid base64"))?;
    let (sigs, msg) = decode_tx(&bytes).map_err(DomainError::invalid)?;
    if sigs.iter().any(|s| *s != [0u8; 64]) {
        return Err(DomainError::invalid(
            "transaction is already partially signed",
        ));
    }
    let m = decode_message(msg).map_err(DomainError::invalid)?;
    Ok((
        B64.encode(msg),
        bs58::encode(m.recent_blockhash).into_string(),
    ))
}

/// Wrap an unsigned base64 message into a transaction with zeroed signatures, for
/// `simulateTransaction` with `sigVerify: false`.
pub fn transaction_for_simulation(message_base64: &str) -> Result<String, DomainError> {
    let msg = B64
        .decode(message_base64.trim())
        .map_err(|_| DomainError::invalid("message is not valid base64"))?;
    let n = decode_message(&msg)
        .map_err(DomainError::invalid)?
        .num_required_signatures as usize;
    let mut out = Vec::with_capacity(3 + n * 64 + msg.len());
    encode_compact(n, &mut out);
    out.resize(out.len() + n * 64, 0);
    out.extend_from_slice(&msg);
    Ok(B64.encode(out))
}

/// Parse one `getSignaturesForAddress` page entry map (used by the history scanner).
pub(crate) fn status_map(v: &Value) -> HashMap<String, (u64, bool, Option<String>)> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| {
            Some((
                e["signature"].as_str()?.to_owned(),
                (
                    e["slot"].as_u64()?,
                    !e["err"].is_null(),
                    e["confirmationStatus"].as_str().map(str::to_owned),
                ),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solana::testutil::{fixture, mainnet, FnRpc};
    use alloy_primitives::U256;

    /// Finality is the conservative `Confirmed { confirmations: 0 }` (getTransaction never
    /// serves `processed`).
    fn parse_tx(chain: &ChainEntry, tx: &Value) -> Result<Tx, DomainError> {
        parse_tx_with_finality(chain, tx, Finality::Confirmed { confirmations: 0 })
    }

    const PYUSD: &str = "2b1kV6DkPAnxd5ixfnxCpjxmKwqjjaYmCZfHsFu24GXo";
    const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

    fn delta<'a>(tx: &'a Tx, owner: &str, mint: Option<&str>) -> &'a BalanceDelta {
        tx.balance_deltas
            .iter()
            .find(|d| {
                d.owner.to_string() == owner
                    && match mint {
                        Some(m) => d.asset.to_string().ends_with(&format!("token:{m}")),
                        None => d.asset.is_native(),
                    }
            })
            .unwrap_or_else(|| panic!("no delta for {owner} {mint:?}: {:#?}", tx.balance_deltas))
    }

    fn signed(n: i128, d: &BalanceDelta) -> i128 {
        let to_i = |a: &Amount| i128::try_from(a.raw).unwrap();
        assert_eq!(to_i(&d.after) - to_i(&d.before), n);
        n
    }

    #[test]
    fn transfer_into_new_ata_counts_missing_pre_as_zero() {
        let tx = parse_tx(&mainnet(), &fixture("usdc_transfer_to_new_ata")).unwrap();
        assert_eq!(tx.status, TxStatus::Success);
        assert_eq!(tx.fee.unwrap().raw, U256::from(5000u64));
        assert_eq!(tx.block.as_ref().unwrap().number, 320_000_000);
        assert!(tx.block.as_ref().unwrap().timestamp.is_some());
        let recipient = "Go5EVXxV3ob4CJVaq6YiGGUKqPmEfDo32kSmbWsYevsF";
        let d = delta(&tx, recipient, Some(USDC));
        assert_eq!(d.received().unwrap().raw, U256::from(25_000_000u64));
        assert_eq!(d.before.raw, U256::ZERO);
        assert_eq!(d.withheld_fee, None);
        // One transfer, attributed to wallets (not token accounts).
        assert_eq!(tx.transfers.len(), 1);
        assert_eq!(tx.transfers[0].to.to_string(), recipient);
        assert_eq!(tx.transfers[0].kind, TransferKind::Token);
        // Sender paid fee + ATA rent in SOL; the new ATA's rent lamports are not a delta.
        let payer = tx.from.unwrap().to_string();
        signed(-(5000 + 2_039_280), delta(&tx, &payer, None));
        assert!(!tx
            .balance_deltas
            .iter()
            .any(|d| d.owner.to_string() == "5MjBG96YjNJWEL687DuGroThtGN5GPd9dFgQXg8o22TV"));
    }

    #[test]
    fn multiple_transfers_incl_cpi_sum_per_owner() {
        let tx = parse_tx(&mainnet(), &fixture("multi_transfer_cpi")).unwrap();
        // Two payouts to the same owner (one outer, one inner via CPI) into two different
        // token accounts of that owner, plus one to another owner and a native SOL tip.
        let alice = "Go5EVXxV3ob4CJVaq6YiGGUKqPmEfDo32kSmbWsYevsF";
        let bob = "DXK4yMpigbTSqv33nJk1tJucXB4E3rDmTXn3yZmiFAXt";
        signed(1_500_000 + 2_000_000, delta(&tx, alice, Some(USDC)));
        signed(700_000, delta(&tx, bob, Some(USDC)));
        assert_eq!(tx.transfers.len(), 4);
        let inner: Vec<_> = tx
            .transfers
            .iter()
            .filter(|t| t.log_index.unwrap() & 0xffff != 0)
            .collect();
        assert_eq!(inner.len(), 3);
        assert!(inner.iter().any(|t| t.kind == TransferKind::Internal));
        // Unchecked `transfer` has no mint: resolved from the destination's token balance.
        assert!(tx
            .transfers
            .iter()
            .any(|t| t.amount.raw == U256::from(2_000_000u64) && t.to.to_string() == alice));
        signed(10_000, delta(&tx, bob, None));
    }

    #[test]
    fn token_2022_transfer_fee_is_net_plus_withheld() {
        let tx = parse_tx(&mainnet(), &fixture("token2022_transfer_fee")).unwrap();
        let recipient = "DXK4yMpigbTSqv33nJk1tJucXB4E3rDmTXn3yZmiFAXt";
        let d = delta(&tx, recipient, Some(PYUSD));
        assert_eq!(d.received().unwrap().raw, U256::from(9_990_000u64));
        assert_eq!(d.withheld_fee.unwrap().raw, U256::from(10_000u64));
        // The instruction (gross) amount stays on the transfer.
        assert_eq!(tx.transfers[0].amount.raw, U256::from(10_000_000u64));
        let sender = delta(
            &tx,
            "Go5EVXxV3ob4CJVaq6YiGGUKqPmEfDo32kSmbWsYevsF",
            Some(PYUSD),
        );
        assert_eq!(sender.withheld_fee, None);
        assert_ne!(tx.finality, Finality::Unverifiable);
    }

    #[test]
    fn pyusd_confidential_transfer_is_unverifiable_never_zero() {
        let tx = parse_tx(&mainnet(), &fixture("pyusd_confidential_transfer")).unwrap();
        assert_eq!(tx.finality, Finality::Unverifiable);
        assert!(
            !tx.balance_deltas.iter().any(|d| !d.asset.is_native()),
            "no token delta may be reported (least of all 0): {:#?}",
            tx.balance_deltas
        );
        assert!(tx.transfers.is_empty());
        // Unparsed variant: raw data tagged ConfidentialTransferExtension (27).
        let mut raw = fixture("pyusd_confidential_transfer");
        let ix = &mut raw["transaction"]["message"]["instructions"][0];
        *ix = json!({"programId": TOKEN_2022_PROGRAM, "accounts": [],
                     "data": bs58::encode([27u8, 7, 0, 1]).into_string(), "stackHeight": 1});
        assert_eq!(
            parse_tx(&mainnet(), &raw).unwrap().finality,
            Finality::Unverifiable
        );
        // Even a known-finalized lookup cannot upgrade it.
        let t = parse_tx_with_finality(
            &mainnet(),
            &fixture("pyusd_confidential_transfer"),
            Finality::Finalized,
        )
        .unwrap();
        assert_eq!(t.finality, Finality::Unverifiable);
    }

    #[test]
    fn real_jupiter_route_through_token_2022_and_legacy() {
        let tx = parse_tx(&mainnet(), &fixture("mainnet_jupiter_pyusd_usdc")).unwrap();
        let owner = "Go5EVXxV3ob4CJVaq6YiGGUKqPmEfDo32kSmbWsYevsF";
        signed(-89_098_536, delta(&tx, owner, Some(PYUSD)));
        signed(88_208_817, delta(&tx, owner, Some(USDC)));
        assert_eq!(tx.fee.unwrap().raw, U256::from(66_017u64));
        assert!(tx
            .transfers
            .iter()
            .all(|t| t.log_index.unwrap() & 0xffff != 0));
    }

    #[test]
    fn failed_tx_reports_only_the_fee() {
        let mut v = fixture("usdc_transfer_to_new_ata");
        v["meta"]["err"] = json!({"InstructionError": [0, "Custom"]});
        v["meta"]["postTokenBalances"] = json!([]);
        v["meta"]["preTokenBalances"] = json!([]);
        let tx = parse_tx(&mainnet(), &v).unwrap();
        assert_eq!(tx.status, TxStatus::Failed);
    }

    #[tokio::test]
    async fn get_tx_uses_status_for_finality() {
        let tx = fixture("usdc_transfer_to_new_ata");
        let sig = tx["transaction"]["signatures"][0]
            .as_str()
            .unwrap()
            .to_owned();
        let rpc = FnRpc::new(move |m, p| match m {
            "getSignatureStatuses" => Some(json!({"context": {"slot": 1}, "value": [
                {"slot": 320000000, "confirmations": null, "err": null, "confirmationStatus": "finalized"}]})),
            "getTransaction" => {
                assert_eq!(p[1]["maxSupportedTransactionVersion"], 0);
                assert_eq!(p[1]["encoding"], "jsonParsed");
                Some(tx.clone())
            }
            _ => None,
        });
        let got = get_tx(&rpc, &mainnet(), &sig).await.unwrap().unwrap();
        assert_eq!(got.finality, Finality::Finalized);
        assert_eq!(got.hash, sig);

        let missing = FnRpc::new(|_, _| Some(json!({"value": [null]})));
        assert_eq!(get_tx(&missing, &mainnet(), &sig).await.unwrap(), None);
        assert!(matches!(
            get_tx(&missing, &mainnet(), "nope").await,
            Err(ProviderError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn malformed_get_transaction_is_an_error_never_a_panic() {
        let full = fixture("usdc_transfer_to_new_ata");
        let sig = full["transaction"]["signatures"][0]
            .as_str()
            .unwrap()
            .to_owned();
        let serve = |tx: Value| {
            FnRpc::new(move |m, _| match m {
                "getSignatureStatuses" => Some(json!({"context": {"slot": 1}, "value": [
                    {"slot": 320000000, "confirmations": null, "err": null, "confirmationStatus": "finalized"}]})),
                "getTransaction" => Some(tx.clone()),
                _ => None,
            })
        };
        // (1) no meta
        let mut no_meta = full.clone();
        no_meta.as_object_mut().unwrap().remove("meta");
        assert!(matches!(
            get_tx(&serve(no_meta), &mainnet(), &sig).await,
            Err(ProviderError::Transient(_))
        ));
        // (2) meta without balances: parses, no deltas
        let mut bare = full.clone();
        let meta = bare["meta"].as_object_mut().unwrap();
        for k in [
            "preTokenBalances",
            "postTokenBalances",
            "preBalances",
            "postBalances",
        ] {
            meta.remove(k);
        }
        let tx = get_tx(&serve(bare), &mainnet(), &sig)
            .await
            .unwrap()
            .unwrap();
        assert!(tx.balance_deltas.is_empty());
        // (3) no signatures
        let mut unsigned = full.clone();
        unsigned["transaction"]["signatures"] = json!([]);
        assert!(matches!(
            get_tx(&serve(unsigned), &mainnet(), &sig).await,
            Err(ProviderError::Transient(_))
        ));
        // (4) not even an object
        assert!(get_tx(&serve(json!("garbage")), &mainnet(), &sig)
            .await
            .is_err());
    }

    #[test]
    fn truncated_wire_bytes_are_errors_never_panics() {
        let t = B64.decode(sign(&message(1, true), [5u8; 64])).unwrap();
        for n in 0..t.len() {
            let cut = B64.encode(&t[..n]);
            assert!(signature_of(&cut).is_err(), "prefix of {n} bytes");
            assert!(
                check_tip(&cut, &[], 0, false).is_err(),
                "prefix of {n} bytes"
            );
            assert!(
                unsigned_message_of(&cut).is_err() || n == 0,
                "prefix of {n} bytes"
            );
        }
        let m = message(1, true);
        for n in 0..m.len() {
            assert!(decode_message(&m[..n]).is_err(), "prefix of {n} bytes");
        }
        // Compact-u16 claiming more items than there are bytes.
        assert!(decode_message(&[1, 0, 0, 0xff, 0xff, 0x7f]).is_err());
    }

    // --- wire ---

    /// Legacy message: payer + tip account + system + compute budget; ixs: SetComputeUnitPrice
    /// and a System transfer of `tip` lamports to key #1.
    fn message(tip: u64, with_price: bool) -> Vec<u8> {
        let mut m = vec![1, 0, 2]; // 1 signer; 2 read-only unsigned (programs)
        encode_compact(4, &mut m);
        m.extend([7u8; 32]); // payer
        m.extend(
            bs58::decode("4ACfpUFoaSD9bfPdeu6DBt89gB6ENTeHBXCAi87NhDEE")
                .into_vec()
                .unwrap(),
        );
        m.extend([0u8; 32]); // system program = all zeros
        m.extend(bs58::decode(COMPUTE_BUDGET_PROGRAM).into_vec().unwrap());
        m.extend([9u8; 32]); // blockhash
        let mut ixs: Vec<Vec<u8>> = Vec::new();
        if with_price {
            let mut d = vec![3];
            d.extend(200_000u64.to_le_bytes());
            ixs.push([vec![3, 0, d.len() as u8], d].concat());
        }
        let mut d = vec![2, 0, 0, 0];
        d.extend(tip.to_le_bytes());
        ixs.push([vec![2, 2, 0, 1, d.len() as u8], d].concat());
        encode_compact(ixs.len(), &mut m);
        for ix in ixs {
            m.extend(ix);
        }
        m
    }

    fn sign(msg: &[u8], sig: [u8; 64]) -> String {
        let mut t = vec![1];
        t.extend(sig);
        t.extend(msg);
        B64.encode(t)
    }

    #[test]
    fn signature_is_first_signature_base58() {
        let s = sign(&message(1, true), [5u8; 64]);
        assert_eq!(
            signature_of(&s).unwrap(),
            bs58::encode([5u8; 64]).into_string()
        );
        assert!(signature_of(&sign(&message(1, true), [0u8; 64])).is_err());
        assert!(signature_of("not base64!").is_err());
        let mut t = vec![2]; // 2 sigs but message requires 1
        t.extend([5u8; 128]);
        t.extend(message(1, true));
        assert!(signature_of(&B64.encode(t)).is_err());
    }

    #[test]
    fn tip_and_compute_price_checks() {
        let tips = ["4ACfpUFoaSD9bfPdeu6DBt89gB6ENTeHBXCAi87NhDEE"];
        let ok = sign(&message(1_000_000, true), [5u8; 64]);
        check_tip(&ok, &tips, 1_000_000, true).unwrap();
        let low = sign(&message(999, true), [5u8; 64]);
        assert!(check_tip(&low, &tips, 1_000, true).is_err());
        let no_price = sign(&message(1_000_000, false), [5u8; 64]);
        assert!(check_tip(&no_price, &tips, 1_000_000, true).is_err());
        check_tip(&no_price, &tips, 1_000_000, false).unwrap();
        assert!(check_tip(
            &ok,
            &["96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5"],
            1,
            false
        )
        .is_err());
    }

    #[test]
    fn simulation_wrapper_and_v0_prefix() {
        let msg = message(1, true);
        let tx = B64
            .decode(transaction_for_simulation(&B64.encode(&msg)).unwrap())
            .unwrap();
        assert_eq!(tx[0], 1);
        assert!(tx[1..65].iter().all(|&b| b == 0));
        assert_eq!(&tx[65..], &msg[..]);
        // v0: 0x80 prefix, same body (+ empty lookup table list).
        let mut v0 = vec![0x80];
        v0.extend(&msg);
        v0.push(0);
        assert_eq!(decode_message(&v0).unwrap().keys.len(), 4);
        v0[0] = 0x81;
        assert!(decode_message(&v0).is_err());
    }

    #[test]
    fn unsigned_message_round_trip() {
        let msg = message(1, true);
        let tx = transaction_for_simulation(&B64.encode(&msg)).unwrap();
        let (m, blockhash) = unsigned_message_of(&tx).unwrap();
        assert_eq!(B64.decode(m).unwrap(), msg);
        assert_eq!(blockhash, bs58::encode([9u8; 32]).into_string());
        assert!(unsigned_message_of(&sign(&msg, [5u8; 64])).is_err());
    }

    #[test]
    fn compute_budget_tags_match_a_real_transaction() {
        // Data of the two ComputeBudget ixs in mainnet tx 2dyb2hr9…pmM4K.
        let limit = bs58::decode("H4hrMd").into_vec().unwrap();
        let price = bs58::decode("3NugsQuXabTD").into_vec().unwrap();
        assert_eq!((limit[0], limit.len()), (2, 5)); // SetComputeUnitLimit(u32)
        assert_eq!((price[0], price.len()), (3, 9)); // SetComputeUnitPrice(u64)
    }
}
