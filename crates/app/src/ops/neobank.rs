//! `neobank` tools: `fiat_get_fx_rate`, `neobank_card_funding_status`, `neobank_get_ledger`.
//! See the ownership table in `ops/mod.rs`.

use super::chain::{fiat_value, hex_u64, parse_address, resolve_asset, rpc_meta, stablecoin};
use super::tx::lookup_tx;
use super::wallet::{
    fetch_transfers, is_token_program, sol_account, stablecoin_info, transfer_query,
    StablecoinInfo, TransferFilter,
};
use crate::{Catalog, Ctx, Domain, OpOutput, Operation, Profile};
use alloy_primitives::U256;
use async_trait::async_trait;
use bdm_config::ChainEntry;
use bdm_domain::{
    AccountAddress, Amount, AssetId, AssetRef, ChainId, DomainError, Fiat, Price, SolanaPubkey,
    Transfer,
};
use bdm_ports::{Capability, EvmRpc, FxRate, FxRates, PriceHistory};
use bdm_protocols::{evm::erc20, solana::spl};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashMap, time::Duration};

pub fn register(c: &mut Catalog) {
    c.register(FiatGetFxRate);
    c.register(CardFundingStatus);
    c.register(GetLedger);
}

const NEOBANK: &[Profile] = &[Profile::Neobank];
const FIAT: &[Profile] = &[Profile::Neobank, Profile::Payments];

fn currency(c: &str) -> Result<String, DomainError> {
    let c = c.trim().to_ascii_uppercase();
    if c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic()) {
        Ok(c)
    } else {
        Err(DomainError::invalid(format!(
            "'{c}' is not an ISO 4217 currency code"
        )))
    }
}

// ------------------------------------------------------------------ fiat_get_fx_rate

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FxIn {
    /// Base currency (ISO 4217), e.g. "EUR".
    pub base: String,
    /// Quote currency (ISO 4217), e.g. "USD". Rate = quote units per 1 base unit.
    pub quote: String,
    /// Date YYYY-MM-DD for a historical rate; omit for the latest.
    #[serde(default)]
    pub date: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FxOut {
    pub base: String,
    pub quote: String,
    /// Exact decimal string.
    #[serde(with = "bdm_domain::serde_str")]
    #[schemars(with = "String")]
    pub rate: Decimal,
    /// Business date the rate belongs to (reference rates are published on business days only).
    pub business_date: NaiveDate,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_date: Option<NaiveDate>,
    /// False when the requested date had no fixing (weekend/holiday) and the previous business
    /// day's rate is returned.
    pub is_requested_date: bool,
    pub source: String,
    pub as_of: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Label a rate against the requested date.
pub(crate) fn label_fx(r: FxRate, requested: Option<NaiveDate>) -> FxOut {
    let is_requested_date = requested.is_none_or(|d| d == r.business_date);
    let note = match requested {
        Some(d) if d != r.business_date => Some(format!(
            "no fixing on {d} (weekend or holiday): rate from business date {}",
            r.business_date
        )),
        None => Some(format!(
            "latest published rate (business date {})",
            r.business_date
        )),
        _ => None,
    };
    FxOut {
        base: r.base,
        quote: r.quote,
        rate: r.rate,
        business_date: r.business_date,
        requested_date: requested,
        is_requested_date,
        source: r.source,
        as_of: r.as_of,
        note,
    }
}

pub struct FiatGetFxRate;

#[async_trait]
impl Operation for FiatGetFxRate {
    type Input = FxIn;
    type Output = FxOut;
    const NAME: &'static str = "fiat_get_fx_rate";
    const DOMAIN: Domain = Domain::Neobank;
    const DESCRIPTION: &'static str = "Fiat exchange rate between two ISO 4217 currencies, latest \
        or for a date (YYYY-MM-DD), as an exact decimal. Sources: ECB reference rates via \
        Frankfurter (keyless, business days only), then Open Exchange Rates. Weekend and holiday \
        dates return the previous business day's rate, labeled with `business_date` and \
        `is_requested_date = false`. Use it to show local-currency values.";
    const PROFILES: &'static [Profile] = FIAT;

    fn cache_ttl(&self) -> Option<Duration> {
        Some(Duration::from_secs(300))
    }

    async fn execute(&self, ctx: &Ctx, input: FxIn) -> Result<OpOutput<FxOut>, DomainError> {
        let (base, quote) = (currency(&input.base)?, currency(&input.quote)?);
        let date = input
            .date
            .as_deref()
            .map(|d| {
                NaiveDate::parse_from_str(d.trim(), "%Y-%m-%d")
                    .map_err(|_| DomainError::invalid(format!("'{d}' is not a YYYY-MM-DD date")))
            })
            .transpose()?;
        if date.is_some_and(|d| d > Utc::now().date_naive()) {
            return Err(DomainError::invalid("date is in the future"));
        }
        if base == quote {
            let today = Utc::now();
            return Ok(OpOutput::local(label_fx(
                FxRate {
                    base,
                    quote,
                    rate: Decimal::ONE,
                    business_date: date.unwrap_or(today.date_naive()),
                    source: "identity".into(),
                    as_of: today,
                },
                date,
            )));
        }
        let req = ctx.route(Capability::Fx);
        let (b, q) = (&base, &quote);
        let r = ctx
            .router()
            .failover::<dyn FxRates, _, _, _>(req, |p| async move { p.rate(b, q, date).await })
            .await
            .map_err(|e| e.error)?;
        Ok(OpOutput::from_routed(r, |v| label_fx(v, date)))
    }
}

// ------------------------------------------------------------------ neobank_card_funding_status

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CardFundingIn {
    /// Chain (CAIP-2 id or alias).
    pub chain: String,
    /// Cardholder wallet (Solana: owner wallet, not the token account).
    pub owner: String,
    /// Stablecoin the card spends: symbol ("USDC"), CAIP-19, or contract / mint address.
    pub token: String,
    /// The card issuer's spender address (EVM: approved via ERC-20 `approve`; Solana: the SPL
    /// token-account delegate). Supplied by the card program.
    pub spender: String,
    /// Optional authorization amount to test, in human units ("25.00").
    #[serde(default)]
    pub amount: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeclineReason {
    /// Solana: the cardholder has no token account for this mint.
    NoTokenAccount,
    /// Solana: the token account is frozen by the issuer.
    AccountFrozen,
    /// Solana: the token account's delegate is someone other than the issuer's spender.
    DelegateMismatch,
    /// No approval / delegation for the issuer's spender.
    NoApproval,
    /// Approval is below the authorization amount.
    InsufficientApproval,
    /// Token balance is below the authorization amount.
    InsufficientBalance,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CardFundingOut {
    pub chain: ChainId,
    pub owner: AccountAddress,
    pub token: AssetId,
    pub spender: AccountAddress,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stablecoin: Option<StablecoinInfo>,
    pub balance: Amount,
    /// ERC-20 allowance to the spender, or the SPL `delegatedAmount` when the spender is the delegate.
    pub approved: Amount,
    pub unlimited_approval: bool,
    /// What the issuer can pull right now: min(balance, approved).
    pub spendable: Amount,
    /// Whether `amount` would be fundable (only when `amount` was given).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_authorize: Option<bool>,
    pub decline_reasons: Vec<DeclineReason>,
    /// EVM block both reads were pinned to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<u64>,
    /// Solana: token account checked (the owner's associated token account).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_account: Option<String>,
    /// Solana: the token account's current delegate, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delegate: Option<String>,
}

/// Spendable = min(balance, approved) (0 when frozen) and why an authorization of `required`
/// (default: any amount) would be declined.
pub(crate) fn funding_verdict(
    balance: U256,
    approved: U256,
    required: Option<U256>,
    frozen: bool,
    no_account: bool,
    delegate_mismatch: bool,
) -> (U256, Vec<DeclineReason>) {
    use DeclineReason::*;
    let spendable = if frozen {
        U256::ZERO
    } else {
        balance.min(approved)
    };
    let need = required.unwrap_or(U256::from(1u8)).max(U256::from(1u8));
    let mut reasons = Vec::new();
    if no_account {
        return (U256::ZERO, vec![NoTokenAccount]);
    }
    if frozen {
        reasons.push(AccountFrozen);
    }
    if delegate_mismatch {
        reasons.push(DelegateMismatch);
    } else if approved.is_zero() {
        reasons.push(NoApproval);
    } else if approved < need {
        reasons.push(InsufficientApproval);
    }
    if balance < need {
        reasons.push(InsufficientBalance);
    }
    (spendable, reasons)
}

pub struct CardFundingStatus;

#[async_trait]
impl Operation for CardFundingStatus {
    type Input = CardFundingIn;
    type Output = CardFundingOut;
    const NAME: &'static str = "neobank_card_funding_status";
    const DOMAIN: Domain = Domain::Neobank;
    const DESCRIPTION: &'static str = "Explain whether a non-custodial stablecoin card (Bridge / \
        Stripe, Baanx and similar just-in-time programs) can be funded, and why an authorization \
        was declined. Reads the cardholder's stablecoin balance and the approval to the issuer's \
        spender (EVM ERC-20 allowance, pinned to one block; Solana SPL delegate + \
        delegatedAmount on the owner's token account). spendable = min(balance, approval). Pass \
        `amount` to test a specific authorization. The approval can be revoked at any time, so \
        this is a point-in-time answer.";
    const PROFILES: &'static [Profile] = NEOBANK;

    async fn execute(
        &self,
        ctx: &Ctx,
        input: CardFundingIn,
    ) -> Result<OpOutput<CardFundingOut>, DomainError> {
        let chain = ctx.chain(&input.chain)?;
        let owner = parse_address(chain, &input.owner)?;
        let spender = parse_address(chain, &input.spender)?;
        let token = resolve_asset(chain, Some(&input.token))?;
        let reads = match (&token.asset, owner, spender) {
            (AssetRef::Erc20(t), AccountAddress::Evm(o), AccountAddress::Evm(s)) => {
                evm_funding(ctx, chain, &token, *t, o, s).await?
            }
            (AssetRef::SplToken(m), AccountAddress::Solana(o), AccountAddress::Solana(s)) => {
                sol_funding(ctx, chain, *m, o, s).await?
            }
            _ => {
                return Err(DomainError::invalid(
                    "token must be an ERC-20 (EVM) or SPL mint (Solana) on this chain",
                ))
            }
        };
        let required = input
            .amount
            .as_deref()
            .map(|a| Amount::parse_units(a, reads.decimals).map(|a| a.raw))
            .transpose()?;
        let (spendable, decline_reasons) = funding_verdict(
            reads.balance,
            reads.approved,
            required,
            reads.frozen,
            reads.no_account,
            reads.delegate_mismatch,
        );
        let amt = |raw| Amount::new(raw, reads.decimals);
        Ok(OpOutput::new(
            CardFundingOut {
                chain: chain.id.clone(),
                owner,
                stablecoin: stablecoin_info(&token),
                token,
                spender,
                balance: amt(reads.balance),
                approved: amt(reads.approved),
                unlimited_approval: reads.approved >= U256::from(1u8) << 255,
                spendable: amt(spendable),
                can_authorize: required.map(|_| decline_reasons.is_empty()),
                decline_reasons,
                block: reads.block,
                token_account: reads.token_account,
                delegate: reads.delegate,
            },
            rpc_meta(&chain.id),
        ))
    }
}

#[derive(Default)]
struct FundingReads {
    decimals: u8,
    balance: U256,
    approved: U256,
    frozen: bool,
    no_account: bool,
    delegate_mismatch: bool,
    block: Option<u64>,
    token_account: Option<String>,
    delegate: Option<String>,
}

async fn evm_funding(
    ctx: &Ctx,
    chain: &ChainEntry,
    asset: &AssetId,
    token: alloy_primitives::Address,
    owner: alloy_primitives::Address,
    spender: alloy_primitives::Address,
) -> Result<FundingReads, DomainError> {
    let rpc = ctx.evm_rpc(chain)?;
    let block = hex_u64(&rpc.request("eth_blockNumber", json!([])).await?)
        .ok_or_else(|| DomainError::internal("eth_blockNumber returned non-hex"))?;
    let tag = format!("{block:#x}");
    let decimals = async {
        match stablecoin(asset) {
            Some(e) => Ok(e.decimals),
            None => erc20::decimals(&rpc, token).await,
        }
    };
    let (balance, approved, decimals) = tokio::join!(
        erc20::balance_of(&rpc, token, owner, &tag),
        erc20::allowance(&rpc, token, owner, spender, &tag),
        decimals
    );
    Ok(FundingReads {
        decimals: decimals?,
        balance: balance?,
        approved: approved?,
        block: Some(block),
        ..Default::default()
    })
}

fn raw_amount(v: &Value) -> U256 {
    v["amount"]
        .as_str()
        .and_then(|s| U256::from_str_radix(s, 10).ok())
        .unwrap_or_default()
}

async fn sol_funding(
    ctx: &Ctx,
    chain: &ChainEntry,
    mint: SolanaPubkey,
    owner: SolanaPubkey,
    spender: SolanaPubkey,
) -> Result<FundingReads, DomainError> {
    let rpc = ctx.solana_rpc(chain)?;
    let m = sol_account(&rpc, &mint.to_string()).await?;
    let program = m.get("owner").and_then(Value::as_str).unwrap_or_default();
    let kind = m.pointer("/data/parsed/type").and_then(Value::as_str);
    if !is_token_program(program) || kind != Some("mint") {
        return Err(DomainError::invalid(format!(
            "{mint} is not a token mint on {}",
            chain.id
        )));
    }
    let decimals = m
        .pointer("/data/parsed/info/decimals")
        .and_then(Value::as_u64)
        .and_then(|d| u8::try_from(d).ok())
        .ok_or_else(|| DomainError::internal("mint has no decimals"))?;
    let ata = spl::associated_token_address(&owner, &mint, &program.parse()?)?;
    let acct = sol_account(&rpc, &ata.to_string()).await?;
    let mut r = FundingReads {
        decimals,
        token_account: Some(ata.to_string()),
        ..Default::default()
    };
    if acct.is_null() {
        r.no_account = true;
        return Ok(r);
    }
    let info = acct.pointer("/data/parsed/info").unwrap_or(&Value::Null);
    r.balance = raw_amount(&info["tokenAmount"]);
    r.frozen = info["state"] == "frozen";
    r.delegate = info["delegate"].as_str().map(str::to_owned);
    match r.delegate.as_deref() {
        Some(d) if d == spender.to_string() => r.approved = raw_amount(&info["delegatedAmount"]),
        Some(_) => r.delegate_mismatch = true,
        None => {}
    }
    Ok(r)
}

// ------------------------------------------------------------------ neobank_get_ledger

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LedgerIn {
    /// Account wallet (0x… or Solana owner wallet).
    pub address: String,
    /// Chain (CAIP-2 id or alias).
    pub chain: String,
    /// Valuation currency (ISO 4217, default "USD").
    #[serde(default)]
    pub currency: Option<String>,
    /// Look up the network fee of outgoing transactions (one extra lookup per transaction).
    #[serde(default = "yes")]
    pub include_fees: bool,
    #[serde(flatten)]
    pub filter: TransferFilter,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LedgerDirection {
    Credit,
    Debit,
    /// Sent from the account to itself.
    SelfTransfer,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LedgerRow {
    pub tx_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_index: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<u64>,
    /// Block time: the valuation timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_time: Option<DateTime<Utc>>,
    pub direction: LedgerDirection,
    pub asset: AssetId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// Exact amount (raw base units + decimals).
    pub amount: Amount,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counterparty: Option<AccountAddress>,
    /// Network fee paid by the account (native coin), on the first row of its transaction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fee: Option<Amount>,
    /// Market value at block time from a historical price source (absent = unknown, never 0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub market_value: Option<Fiat>,
    /// Stablecoins pegged to the valuation currency: face value (1 token = 1 unit).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub par_value: Option<Fiat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valuation_note: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LedgerOut {
    pub owner: AccountAddress,
    pub chain: ChainId,
    pub currency: String,
    pub rows: Vec<LedgerRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// See wallet_get_transfers: the page restarted on another provider.
    pub cursor_reset: bool,
}

/// Peg currency of a registry stablecoin.
// ponytail: inferred from the symbol until the registry carries a peg currency field.
fn peg_currency(symbol: &str) -> &'static str {
    if symbol.to_ascii_uppercase().starts_with("EUR") {
        "EUR"
    } else {
        "USD"
    }
}

pub(crate) fn ledger_row(
    owner: &AccountAddress,
    t: Transfer,
    currency: &str,
    price: Option<&Price>,
) -> LedgerRow {
    let direction = match (t.to == *owner, t.from.as_ref() == Some(owner)) {
        (true, true) => LedgerDirection::SelfTransfer,
        (false, true) => LedgerDirection::Debit,
        _ => LedgerDirection::Credit,
    };
    let counterparty = match direction {
        LedgerDirection::Credit => t.from,
        LedgerDirection::Debit => Some(t.to),
        LedgerDirection::SelfTransfer => None,
    };
    let block_time = t.block.as_ref().and_then(|b| b.timestamp);
    let sc = stablecoin(&t.asset);
    let par_value = sc
        .filter(|e| peg_currency(&e.symbol) == currency)
        .zip(block_time)
        .and_then(|(_, at)| {
            Some(Fiat {
                amount: t.amount.to_decimal()?,
                currency: currency.to_owned(),
                as_of: at,
                source: "par".into(),
            })
        });
    let market_value = price.and_then(|p| fiat_value(&t.amount, p));
    let valuation_note = match (block_time, &market_value) {
        (None, _) => Some("no block time available: not valued".into()),
        (Some(_), None) => Some("no historical price source answered: market value unknown".into()),
        _ => None,
    };
    LedgerRow {
        tx_hash: t.tx_hash,
        log_index: t.log_index,
        block: t.block.as_ref().map(|b| b.number),
        block_time,
        direction,
        symbol: sc.map(|e| e.symbol.clone()),
        asset: t.asset,
        amount: t.amount,
        counterparty,
        fee: None,
        market_value,
        par_value,
        valuation_note,
    }
}

pub struct GetLedger;

#[async_trait]
impl Operation for GetLedger {
    type Input = LedgerIn;
    type Output = LedgerOut;
    const NAME: &'static str = "neobank_get_ledger";
    const DOMAIN: Domain = Domain::Neobank;
    const DESCRIPTION: &'static str = "Bank-style statement for a wallet on one chain: one credit \
        or debit row per transfer with exact amount, counterparty, network fee, block time, and \
        fiat value AT BLOCK TIME (not now) with its price source and timestamp. Stablecoins also \
        get their par (face) value, since market price is not always exactly 1. A missing price \
        is reported as unknown, never 0. Paginated with `cursor`; defaults to all assets, set \
        `stablecoins_only` for a stablecoin statement.";
    const PROFILES: &'static [Profile] = NEOBANK;

    async fn execute(
        &self,
        ctx: &Ctx,
        input: LedgerIn,
    ) -> Result<OpOutput<LedgerOut>, DomainError> {
        let chain = ctx.chain(&input.chain)?;
        let owner = parse_address(chain, &input.address)?;
        let cur = currency(input.currency.as_deref().unwrap_or("USD"))?;
        let page = fetch_transfers(ctx, chain, transfer_query(chain, owner, input.filter)?).await?;

        // Historical prices, one lookup per (asset, block time).
        let mut keys: Vec<(AssetId, DateTime<Utc>)> = page
            .page
            .items
            .iter()
            .filter_map(|t| Some((t.asset.clone(), t.block.as_ref()?.timestamp?)))
            .collect();
        keys.sort();
        keys.dedup();
        let prices = futures::future::join_all(keys.iter().map(|(asset, at)| {
            let req = ctx.route(Capability::PriceHistory).chain(chain.id.clone());
            let cur = cur.as_str();
            async move {
                ctx.router()
                    .failover::<dyn PriceHistory, _, _, _>(req, |p| async move {
                        p.price_at(asset, cur, *at).await
                    })
                    .await
                    .ok()
                    .map(|r| r.value)
            }
        }))
        .await;
        let prices: HashMap<_, _> = keys.into_iter().zip(prices).collect();

        let mut rows: Vec<LedgerRow> = page
            .page
            .items
            .into_iter()
            .map(|t| {
                let p = t
                    .block
                    .as_ref()
                    .and_then(|b| b.timestamp)
                    .and_then(|at| prices.get(&(t.asset.clone(), at)))
                    .and_then(Option::as_ref);
                ledger_row(&owner, t, &cur, p)
            })
            .collect();

        if input.include_fees {
            let mut hashes: Vec<String> = rows
                .iter()
                .filter(|r| r.direction != LedgerDirection::Credit)
                .map(|r| r.tx_hash.clone())
                .collect();
            hashes.sort();
            hashes.dedup();
            let txs =
                futures::future::join_all(hashes.iter().map(|h| lookup_tx(ctx, chain, h))).await;
            let fees: HashMap<String, Amount> = hashes
                .into_iter()
                .zip(txs)
                .filter_map(|(h, tx)| {
                    let tx = tx.ok()??;
                    (tx.from.as_ref() == Some(&owner)).then_some((h, tx.fee?))
                })
                .collect();
            let mut seen = std::collections::HashSet::new();
            for r in &mut rows {
                if seen.insert(r.tx_hash.clone()) {
                    r.fee = fees.get(&r.tx_hash).copied();
                }
            }
        }

        Ok(OpOutput::new(
            LedgerOut {
                owner,
                chain: chain.id.clone(),
                currency: cur,
                rows,
                next_cursor: page.page.next_cursor,
                cursor_reset: page.cursor_reset,
            },
            page.meta,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use DeclineReason::*;

    fn u(n: u64) -> U256 {
        U256::from(n)
    }

    #[test]
    fn funding_verdicts() {
        // healthy: spendable = min(balance, allowance)
        assert_eq!(
            funding_verdict(u(100), u(40), None, false, false, false),
            (u(40), vec![])
        );
        assert_eq!(
            funding_verdict(u(30), u(40), Some(u(30)), false, false, false),
            (u(30), vec![])
        );
        // declines
        assert_eq!(
            funding_verdict(u(100), u(0), None, false, false, false).1,
            vec![NoApproval]
        );
        assert_eq!(
            funding_verdict(u(100), u(10), Some(u(25)), false, false, false).1,
            vec![InsufficientApproval]
        );
        assert_eq!(
            funding_verdict(u(10), u(100), Some(u(25)), false, false, false),
            (u(10), vec![InsufficientBalance])
        );
        assert_eq!(
            funding_verdict(u(0), u(0), None, false, false, false).1,
            vec![NoApproval, InsufficientBalance]
        );
        assert_eq!(
            funding_verdict(u(100), u(0), None, false, false, true).1,
            vec![DelegateMismatch]
        );
        assert_eq!(
            funding_verdict(u(100), u(100), None, true, false, false),
            (u(0), vec![AccountFrozen])
        );
        assert_eq!(
            funding_verdict(u(0), u(0), None, false, true, false),
            (u(0), vec![NoTokenAccount])
        );
    }

    #[test]
    fn fx_labels_business_date() {
        let d = |s| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let rate = FxRate {
            base: "EUR".into(),
            quote: "USD".into(),
            rate: Decimal::from_str("1.0956").unwrap(),
            business_date: d("2026-09-18"),
            source: "frankfurter".into(),
            as_of: Utc::now(),
        };
        let weekend = label_fx(rate.clone(), Some(d("2026-09-20")));
        assert!(!weekend.is_requested_date);
        assert!(weekend.note.unwrap().contains("2026-09-18"));
        let exact = label_fx(rate.clone(), Some(d("2026-09-18")));
        assert!(exact.is_requested_date && exact.note.is_none());
        assert!(label_fx(rate, None).is_requested_date);
    }

    #[test]
    fn ledger_direction_and_valuation() {
        let owner: AccountAddress = "0x00000000000000000000000000000000000000aa"
            .parse()
            .unwrap();
        let other: AccountAddress = "0x00000000000000000000000000000000000000bb"
            .parse()
            .unwrap();
        let asset: AssetId = "eip155:1/slip44:60".parse().unwrap();
        let at = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let t = Transfer {
            chain: ChainId::evm(1),
            tx_hash: "0x01".into(),
            log_index: None,
            kind: bdm_domain::TransferKind::Native,
            asset: asset.clone(),
            from: Some(other),
            to: owner,
            amount: Amount::from_u128(1_500_000_000_000_000_000, 18),
            block: Some(bdm_domain::BlockRef {
                number: 7,
                hash: None,
                timestamp: Some(at),
            }),
        };
        let price = Price {
            asset,
            currency: "USD".into(),
            value: Decimal::from_str("2000").unwrap(),
            as_of: at,
            source: "coingecko".into(),
            liquidity_usd: None,
        };
        let r = ledger_row(&owner, t.clone(), "USD", Some(&price));
        assert_eq!(r.direction, LedgerDirection::Credit);
        assert_eq!(r.counterparty, Some(other));
        let mv = r.market_value.unwrap();
        assert_eq!(mv.amount, Decimal::from(3000));
        assert_eq!((mv.as_of, mv.source.as_str()), (at, "coingecko"));
        assert!(r.par_value.is_none(), "ETH is not a stablecoin");

        let out = Transfer {
            from: Some(owner),
            to: other,
            ..t
        };
        let r = ledger_row(&owner, out, "USD", None);
        assert_eq!(r.direction, LedgerDirection::Debit);
        assert_eq!(r.counterparty, Some(other));
        assert!(r.market_value.is_none());
        assert!(r.valuation_note.unwrap().contains("unknown"));
        assert_eq!(peg_currency("EURC"), "EUR");
        assert_eq!(peg_currency("USDC"), "USD");
    }
}
