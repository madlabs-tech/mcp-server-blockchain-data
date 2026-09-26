//! `payments` tools.
//!
//! Matching rules (PLAN.md "Cross-cutting rules → Payments"):
//! - amounts come from the recipient's **balance change**, never the instruction/event amount;
//! - tokens are matched by contract/mint through the stablecoin registry, never by symbol;
//! - the idempotency key is `(chain, tx, logIndex)`.
//!
//! The decision logic lives in pure functions ([`check_payment`], [`canonical_deposits`],
//! [`eip681_uri`], [`solana_pay_url`], [`x402_payment_required`]) so it is unit-tested with
//! hand-built domain values; the operations only fetch data and call them.

use super::stablecoin::{registry, resolve_canonical};
use crate::{Catalog, Ctx, Domain, OpOutput, Operation, Profile};
use alloy_primitives::U256;
use async_trait::async_trait;
use bdm_config::{ChainEntry, Strategy};
use bdm_domain::{
    AccountAddress, Amount, AssetId, BlockRef, ChainFamily, ChainId, DomainError, ErrorCode,
    Finality, Provenance, SolanaPubkey, Transfer, TransferKind, Tx, TxStatus,
};
use bdm_ports::{
    Capability, Direction, EvmRpc, PortKind, PortResult, ProviderError, SolanaRpc, TransferHistory,
    TransferQuery,
};
use bdm_protocols::{
    evm::tx as evm_tx,
    solana::tx as sol_tx,
    stablecoins::{StablecoinEntry, StablecoinRegistry},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, future::Future, sync::Arc};

pub fn register(c: &mut Catalog) {
    c.register(VerifyTransfer);
    c.register(ListDeposits);
    c.register(BuildRequest);
}

const PROFILES: &[Profile] = &[Profile::Payments, Profile::Neobank];

// ------------------------------------------------------------------ pure matching

/// Minimum finality a payment must reach before it counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MinFinality {
    /// Included in a block/slot (L2: sequencer only; Solana: `confirmed`). Can still reorg.
    Confirmed,
    /// EVM `safe` tag (L2: batch posted to L1). Solana has no `safe`: waits for `finalized`.
    Safe,
    /// Irreversible under the chain's rules.
    #[default]
    Finalized,
}

impl MinFinality {
    pub fn floor(self) -> Finality {
        match self {
            Self::Confirmed => Finality::Confirmed { confirmations: 0 },
            Self::Safe => Finality::Safe,
            Self::Finalized => Finality::Finalized,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaymentStatus {
    /// Not yet at `min_finality` (or still in the mempool). Check again later.
    Pending,
    /// Exact amount received and `min_finality` reached, but not yet irreversible.
    Confirmed,
    /// Exact amount received and irreversible.
    Finalized,
    /// The transaction reverted or was dropped: nothing was paid.
    Failed,
    /// Recipient received less of the expected token than requested (`net_received` says how much).
    Underpaid,
    /// Recipient received more than requested.
    Overpaid,
    /// Recipient received a different token (other stablecoin, bridged copy or a spoof), not the
    /// expected contract/mint.
    WrongToken,
    /// Chain data cannot prove the amount (e.g. confidential transfer). Never treat as paid.
    Unverifiable,
}

/// What the caller expects to have been paid.
#[derive(Debug, Clone, PartialEq)]
pub struct ExpectedPayment {
    pub asset: AssetId,
    pub recipient: AccountAddress,
    pub amount: Amount,
    pub min_finality: Finality,
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct QuorumInfo {
    /// Providers that returned the same block hash.
    pub agreeing: usize,
    pub required: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct PaymentCheck {
    pub status: PaymentStatus,
    pub asset: AssetId,
    pub recipient: AccountAddress,
    pub expected: Amount,
    /// Net balance increase of the recipient in the expected token (0 if none).
    pub net_received: Amount,
    /// Token-2022 transfer fee withheld at the recipient.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub withheld_fee: Option<Amount>,
    /// Sender of the matching transfer (else the tx signer). Refund here, never to a caller claim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payer: Option<AccountAddress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<BlockRef>,
    pub finality: Finality,
    pub meets_min_finality: bool,
    /// `chain:tx[:logIndex]`: store it to credit a payment exactly once.
    pub idempotency_key: String,
    /// Whether the reference/memo appears in the transaction (null when it can't be checked).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_found: Option<bool>,
    /// Other assets the recipient received in this tx (explains `wrong_token`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub other_assets_received: Vec<AssetId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quorum: Option<QuorumInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Decide a payment from a normalized transaction. Pure: no I/O.
pub fn check_payment(
    expected: &ExpectedPayment,
    tx: &Tx,
    registry: &StablecoinRegistry,
) -> PaymentCheck {
    let decimals = expected.amount.decimals;
    let mut notes = Vec::new();
    let recipient = &expected.recipient;

    // Balance change of the recipient in the expected asset (the only amount we trust).
    let deltas: Vec<_> = tx
        .balance_deltas
        .iter()
        .filter(|d| &d.owner == recipient && d.asset == expected.asset)
        .collect();
    let mut net = Amount::zero(decimals);
    let mut withheld: Option<Amount> = None;
    let mut scale_mismatch = false;
    for d in &deltas {
        if d.after.decimals != decimals {
            scale_mismatch = true;
            continue;
        }
        if let Some(r) = d.received() {
            net = net.checked_add(&r).unwrap_or(net);
        }
        if let Some(f) = d.withheld_fee {
            withheld = Some(withheld.map_or(f, |w| w.checked_add(&f).unwrap_or(w)));
        }
    }

    let matched = tx
        .transfers
        .iter()
        .find(|t| &t.to == recipient && t.asset == expected.asset);
    let mut others: Vec<AssetId> = tx
        .balance_deltas
        .iter()
        .filter(|d| &d.owner == recipient && d.asset != expected.asset)
        .filter(|d| d.received().is_some_and(|r| !r.is_zero()))
        .map(|d| d.asset.clone())
        .chain(
            tx.transfers
                .iter()
                .filter(|t| &t.to == recipient && t.asset != expected.asset && !t.amount.is_zero())
                .map(|t| t.asset.clone()),
        )
        .collect();
    others.sort();
    others.dedup();
    for a in &others {
        notes.push(match registry.by_asset(a) {
            Some(e) => format!(
                "received canonical {} ({a}), not the expected token",
                e.symbol
            ),
            None if a.is_native() => format!("received the native asset ({a})"),
            None => format!("received {a}, which is not a registry stablecoin (possible spoof)"),
        });
    }

    let payer = matched.and_then(|t| t.from).or(tx.from);
    let idempotency_key = match matched.and_then(|t| t.log_index) {
        Some(i) => format!("{}:{}:{i}", tx.chain, tx.hash),
        None => format!("{}:{}", tx.chain, tx.hash),
    };
    // ponytail: substring search over the raw tx (account keys / memo logs); replace with typed
    // memo + Solana Pay reference fields if the tx parsers start exposing them.
    let reference_found = expected.reference.as_ref().and_then(|r| {
        tx.raw
            .as_ref()
            .map(|raw| raw.to_string().contains(r.as_str()))
    });
    if reference_found == Some(false) {
        notes.push("reference/memo not found in this transaction".into());
    }
    let meets = tx.finality >= expected.min_finality;

    let status = if matches!(tx.status, TxStatus::Failed | TxStatus::Dropped) {
        PaymentStatus::Failed
    } else if tx.finality == Finality::Unverifiable {
        notes.push("amounts are hidden (e.g. confidential transfer)".into());
        PaymentStatus::Unverifiable
    } else if matches!(tx.status, TxStatus::Pending | TxStatus::NotFound) || !meets {
        PaymentStatus::Pending
    } else if scale_mismatch {
        notes.push("balance change decimals differ from the registry".into());
        PaymentStatus::Unverifiable
    } else if deltas.is_empty() && matched.is_some() {
        notes.push("transfer found but no balance change data for the recipient".into());
        PaymentStatus::Unverifiable
    } else if net.is_zero() && !others.is_empty() {
        PaymentStatus::WrongToken
    } else {
        match net.raw.cmp(&expected.amount.raw) {
            std::cmp::Ordering::Less => PaymentStatus::Underpaid,
            std::cmp::Ordering::Greater => PaymentStatus::Overpaid,
            std::cmp::Ordering::Equal if tx.finality == Finality::Finalized => {
                PaymentStatus::Finalized
            }
            std::cmp::Ordering::Equal => PaymentStatus::Confirmed,
        }
    };

    PaymentCheck {
        status,
        asset: expected.asset.clone(),
        recipient: *recipient,
        expected: expected.amount,
        net_received: net,
        withheld_fee: withheld,
        payer,
        block: tx.block.clone(),
        finality: tx.finality,
        meets_min_finality: meets,
        idempotency_key,
        reference_found,
        other_assets_received: others,
        quorum: None,
        notes,
    }
}

/// Amount from exactly one of `amount` (decimal, token units) / `amount_raw` (base units).
fn parse_amount(
    amount: Option<&str>,
    raw: Option<&str>,
    decimals: u8,
) -> Result<Amount, DomainError> {
    let a = match (amount, raw) {
        (Some(a), None) => Amount::parse_units(a, decimals)?,
        (None, Some(r)) => Amount::new(
            U256::from_str_radix(r.trim(), 10)
                .map_err(|_| DomainError::invalid(format!("amount_raw '{r}' is not an integer")))?,
            decimals,
        ),
        _ => {
            return Err(DomainError::invalid(
                "give exactly one of `amount` (e.g. \"10.5\") or `amount_raw` (base units)",
            ))
        }
    };
    if a.is_zero() {
        return Err(DomainError::invalid("amount must be greater than zero"));
    }
    Ok(a)
}

// ------------------------------------------------------------------ payments_verify_transfer

#[derive(Debug, Deserialize, JsonSchema)]
pub struct VerifyTransferIn {
    /// Chain alias or CAIP-2 id, e.g. "base", "solana", "eip155:8453".
    pub chain: String,
    /// Transaction hash (EVM, 0x…) or signature (Solana, base58).
    pub tx: String,
    /// Expected token: symbol ("USDC"), contract/mint address, or CAIP-19 id. Must be a canonical
    /// registry stablecoin on `chain`.
    pub token: String,
    /// Expected recipient: EVM address, or Solana owner wallet (not the token account).
    pub recipient: String,
    /// Expected amount in token units, e.g. "10.5". Use this or `amount_raw`.
    #[serde(default)]
    pub amount: Option<String>,
    /// Expected amount in base units, e.g. "10500000". Use this or `amount`.
    #[serde(default)]
    pub amount_raw: Option<String>,
    /// Optional reference / memo (e.g. Solana Pay reference key) to look for in the transaction.
    #[serde(default)]
    pub reference: Option<String>,
    /// Finality required before the payment counts (default `finalized`).
    #[serde(default)]
    pub min_finality: MinFinality,
    /// Ask two RPC vendors and require the same block hash (default from config
    /// `operations.payments_verify_transfer.strategy = "quorum"`).
    #[serde(default)]
    pub quorum: Option<bool>,
}

pub struct VerifyTransfer;

type Fetched = (Tx, Provenance, Option<QuorumInfo>);

async fn fetch_tx<P, F, Fut>(
    ctx: &Ctx,
    cap: Capability,
    chain: &ChainId,
    quorum: Option<usize>,
    f: F,
) -> Result<Fetched, DomainError>
where
    P: PortKind + ?Sized,
    F: Fn(Arc<P>) -> Fut,
    Fut: Future<Output = PortResult<Tx>>,
{
    let req = ctx.route(cap).chain(chain.clone()).confirm_not_found();
    let not_found = |mut e: DomainError| {
        if e.code == ErrorCode::NotFound {
            e = e.with_hint(
                "not found by two providers: it may not be broadcast yet, or is on another chain",
            );
        }
        e
    };
    match quorum {
        Some(n) => {
            let block_hash = |t: &Tx| {
                t.block
                    .as_ref()
                    .and_then(|b| b.hash.clone())
                    .unwrap_or_default()
            };
            let r = ctx
                .router()
                .quorum::<P, _, _, _, _>(req, n, block_hash, f)
                .await
                .map_err(|e| not_found(e.error))?;
            let q = QuorumInfo {
                agreeing: r.value.agreeing,
                required: r.value.required,
            };
            Ok((r.value.value, r.provenance, Some(q)))
        }
        None => {
            let r = ctx
                .router()
                .failover::<P, _, _, _>(req, f)
                .await
                .map_err(|e| not_found(e.error))?;
            Ok((r.value, r.provenance, None))
        }
    }
}

/// Look the tx up on one vendor per attempt (consistent reads), failover or quorum.
async fn lookup_tx(
    ctx: &Ctx,
    chain: &ChainEntry,
    hash: &str,
    quorum: Option<usize>,
) -> Result<Fetched, DomainError> {
    let hash = hash.trim();
    match chain.family {
        ChainFamily::Evm => {
            fetch_tx::<dyn EvmRpc, _, _>(
                ctx,
                Capability::EvmRpc,
                &chain.id,
                quorum,
                |p| async move {
                    evm_tx::get_tx(p.as_ref(), chain, hash)
                        .await?
                        .ok_or(ProviderError::NotFound)
                },
            )
            .await
        }
        ChainFamily::Solana => {
            fetch_tx::<dyn SolanaRpc, _, _>(
                ctx,
                Capability::SolanaRpc,
                &chain.id,
                quorum,
                |p| async move {
                    sol_tx::get_tx(p.as_ref(), chain, hash)
                        .await?
                        .ok_or(ProviderError::NotFound)
                },
            )
            .await
        }
    }
}

#[async_trait]
impl Operation for VerifyTransfer {
    type Input = VerifyTransferIn;
    type Output = PaymentCheck;
    const NAME: &'static str = "payments_verify_transfer";
    const DOMAIN: Domain = Domain::Payments;
    const DESCRIPTION: &'static str = "Verify that one on-chain transaction paid an expected amount of a canonical stablecoin to a recipient (EVM or Solana). \
Use it before crediting an order or releasing goods, and after an x402 facilitator reports success. \
Returns status pending|confirmed|finalized|failed|underpaid|overpaid|wrong_token|unverifiable, the recipient's NET balance change (never the instruction amount), any withheld transfer fee, the payer, the block hash, and an idempotency key (chain:tx:logIndex) to credit exactly once. \
Tokens are matched by contract/mint from the verified registry, so look-alike or bridged tokens come back as wrong_token. \
Caveats: only `confirmed`/`finalized` with meets_min_finality=true mean paid; default min_finality is `finalized` (Ethereum ~15 min; pass `confirmed` for a soft accept). \
Solana recipient must be the owner wallet. A NOT_FOUND error means two providers could not see the tx. Set quorum=true to require two RPC vendors to agree on the block hash.";
    const PROFILES: &'static [Profile] = PROFILES;

    async fn execute(
        &self,
        ctx: &Ctx,
        input: VerifyTransferIn,
    ) -> Result<OpOutput<PaymentCheck>, DomainError> {
        let chain = ctx.chain(&input.chain)?;
        let entry = resolve_canonical(chain, &input.token)?;
        let recipient = AccountAddress::parse(chain.family, &input.recipient)?;
        let amount = parse_amount(
            input.amount.as_deref(),
            input.amount_raw.as_deref(),
            entry.decimals,
        )?;
        let op_cfg = ctx.config().operation(Self::NAME);
        let quorum = input
            .quorum
            .unwrap_or(op_cfg.strategy == Some(Strategy::Quorum))
            .then(|| op_cfg.quorum.unwrap_or(2).max(2) as usize);

        let (tx, mut meta, q) = lookup_tx(ctx, chain, &input.tx, quorum).await?;
        let expected = ExpectedPayment {
            asset: entry.asset.clone(),
            recipient,
            amount,
            min_finality: input.min_finality.floor(),
            reference: input.reference,
        };
        let mut check = check_payment(&expected, &tx, registry()?);
        check.quorum = q;
        if let Some(q) = q.filter(|q| q.agreeing < q.required) {
            check.notes.push(format!(
                "quorum not met: {}/{} providers answered",
                q.agreeing, q.required
            ));
        }
        meta.chain = Some(chain.id.clone());
        meta.block = tx.block.clone();
        meta.finality = Some(tx.finality);
        Ok(OpOutput::new(check, meta))
    }
}

// ------------------------------------------------------------------ payments_list_deposits

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListDepositsIn {
    /// Chain alias or CAIP-2 id.
    pub chain: String,
    /// Deposit addresses to scan (1–20). Solana: owner wallets.
    pub addresses: Vec<String>,
    /// Restrict to these tokens (symbols, addresses or CAIP-19). Default: every canonical
    /// stablecoin registered on the chain.
    #[serde(default)]
    pub tokens: Option<Vec<String>>,
    /// Per-address cursor from a previous call's `pages[].next_cursor`.
    #[serde(default)]
    pub cursors: BTreeMap<String, String>,
    /// Only transfers at or after this block/slot.
    #[serde(default)]
    pub from_block: Option<u64>,
    /// Max transfers per address (default 50, max 200).
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Deposit {
    pub symbol: String,
    /// `chain:tx[:logIndex]`: credit each deposit once.
    pub idempotency_key: String,
    pub transfer: Transfer,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct AddressPage {
    pub address: String,
    pub deposits: Vec<Deposit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct DepositsOut {
    pub pages: Vec<AddressPage>,
}

/// Incoming canonical-token transfers to `recipient` (drops spoofed/unknown tokens and outgoing).
pub fn canonical_deposits(
    transfers: Vec<Transfer>,
    recipient: &AccountAddress,
    tokens: &[&StablecoinEntry],
) -> Vec<Deposit> {
    transfers
        .into_iter()
        .filter(|t| &t.to == recipient && t.kind == TransferKind::Token && !t.amount.is_zero())
        .filter_map(|t| {
            let e = tokens.iter().find(|e| e.asset == t.asset)?;
            let idempotency_key = match t.log_index {
                Some(i) => format!("{}:{}:{i}", t.chain, t.tx_hash),
                None => format!("{}:{}", t.chain, t.tx_hash),
            };
            Some(Deposit {
                symbol: e.symbol.clone(),
                idempotency_key,
                transfer: t,
            })
        })
        .collect()
}

pub struct ListDeposits;

#[async_trait]
impl Operation for ListDeposits {
    type Input = ListDepositsIn;
    type Output = DepositsOut;
    const NAME: &'static str = "payments_list_deposits";
    const DOMAIN: Domain = Domain::Payments;
    const DESCRIPTION: &'static str = "List incoming canonical-stablecoin deposits to up to 20 addresses on one chain, with per-address cursors for polling. \
Use it to detect deposits (poll with the returned next_cursor). Only verified registry tokens are returned, so spoofed look-alike tokens are dropped. \
Each deposit carries an idempotency key (chain:tx:logIndex). \
Caveats: amounts are transfer amounts and the block may not be final: call payments_verify_transfer before crediting. Solana addresses must be owner wallets (token accounts are scanned for you).";
    const PROFILES: &'static [Profile] = PROFILES;

    async fn execute(
        &self,
        ctx: &Ctx,
        input: ListDepositsIn,
    ) -> Result<OpOutput<DepositsOut>, DomainError> {
        let chain = ctx.chain(&input.chain)?;
        if input.addresses.is_empty() || input.addresses.len() > 20 {
            return Err(DomainError::invalid("give 1 to 20 addresses"));
        }
        let tokens: Vec<&StablecoinEntry> = match &input.tokens {
            Some(list) => list
                .iter()
                .map(|t| resolve_canonical(chain, t))
                .collect::<Result<_, _>>()?,
            None => registry()?.for_chain(&chain.id).collect(),
        };
        if tokens.is_empty() {
            return Err(DomainError::invalid(format!(
                "no verified stablecoins are registered for {}",
                chain.id
            )));
        }
        let assets: Vec<AssetId> = tokens.iter().map(|e| e.asset.clone()).collect();
        let limit = input.limit.unwrap_or(50).clamp(1, 200);

        let mut pages = Vec::new();
        let mut meta: Option<Provenance> = None;
        // ponytail: addresses are scanned sequentially; join them if 20-address polls get slow.
        for raw in &input.addresses {
            let owner = AccountAddress::parse(chain.family, raw)?;
            let query = TransferQuery {
                owner,
                direction: Direction::In,
                assets: Some(assets.clone()),
                from_block: input.from_block,
                to_block: None,
                cursor: input.cursors.get(raw).cloned(),
                limit,
            };
            let r = ctx
                .router()
                .failover::<dyn TransferHistory, _, _, _>(
                    ctx.route(Capability::TransferHistory)
                        .chain(chain.id.clone()),
                    |p| {
                        let q = query.clone();
                        async move { p.transfers(&q).await }
                    },
                )
                .await
                .map_err(|e| e.error)?;
            match &mut meta {
                None => meta = Some(r.provenance),
                Some(m) => m.providers_tried.extend(r.provenance.providers_tried),
            }
            pages.push(AddressPage {
                address: owner.to_string(),
                deposits: canonical_deposits(r.value.items, &owner, &tokens),
                next_cursor: r.value.next_cursor,
            });
        }
        let mut meta = meta.ok_or_else(|| DomainError::invalid("addresses must not be empty"))?;
        meta.chain = Some(chain.id.clone());
        Ok(OpOutput::new(DepositsOut { pages }, meta))
    }
}

// ------------------------------------------------------------------ payments_build_request

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RequestFormat {
    /// EIP-681 `ethereum:` URI (EVM chains; wallets/QR codes).
    Eip681,
    /// Solana Pay transfer-request URL with a fresh `reference` key (Solana).
    SolanaPay,
    /// x402 v2 `PaymentRequired` body with one `exact` `PaymentRequirements` entry.
    X402,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BuildRequestIn {
    /// Chain alias or CAIP-2 id.
    pub chain: String,
    /// Token symbol, address or CAIP-19 id (must be a canonical registry stablecoin).
    pub token: String,
    /// Who gets paid (EVM address or Solana owner wallet).
    pub recipient: String,
    /// Amount in token units, e.g. "10.5". Use this or `amount_raw`.
    #[serde(default)]
    pub amount: Option<String>,
    /// Amount in base units. Use this or `amount`.
    #[serde(default)]
    pub amount_raw: Option<String>,
    pub format: RequestFormat,
    /// Solana Pay: reference public key to reuse; omitted = a fresh random one is generated.
    #[serde(default)]
    pub reference: Option<String>,
    /// Solana Pay `label` / `message` / `memo` (memo is public on-chain).
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub memo: Option<String>,
    /// x402: URL of the protected resource (required for x402).
    #[serde(default)]
    pub resource_url: Option<String>,
    /// x402: human-readable description of the resource.
    #[serde(default)]
    pub description: Option<String>,
    /// x402: `maxTimeoutSeconds` (default 60).
    #[serde(default)]
    pub max_timeout_seconds: Option<u64>,
    /// x402 EVM: the token's EIP-712 domain `name` and `version` (for EIP-3009), put in `extra`.
    #[serde(default)]
    pub eip712_name: Option<String>,
    #[serde(default)]
    pub eip712_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct PaymentRequestOut {
    pub asset: AssetId,
    pub symbol: String,
    pub amount: Amount,
    /// EIP-681 URI or Solana Pay URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// Solana Pay reference key: pass it to payments_verify_transfer to match the payment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// x402 v2 `PaymentRequired` body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x402: Option<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// RFC 3986 percent-encoding of a query value (unreserved characters kept).
fn pct(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// `ethereum:<token>@<chainId>/transfer?address=<to>&uint256=<raw>` (EIP-681 ERC-20 transfer).
pub fn eip681_uri(
    entry: &StablecoinEntry,
    chain_id: u64,
    to: &AccountAddress,
    amount: &Amount,
) -> String {
    format!(
        "ethereum:{}@{chain_id}/transfer?address={to}&uint256={}",
        entry.address, amount.raw
    )
}

/// Solana Pay transfer request: `solana:<recipient>?amount=…&spl-token=<mint>&reference=…`.
pub fn solana_pay_url(
    entry: &StablecoinEntry,
    to: &AccountAddress,
    amount: &Amount,
    reference: &SolanaPubkey,
    label: Option<&str>,
    message: Option<&str>,
    memo: Option<&str>,
) -> String {
    let mut url = format!(
        "solana:{to}?amount={}&spl-token={}&reference={reference}",
        amount.format_units(),
        entry.address
    );
    for (k, v) in [("label", label), ("message", message), ("memo", memo)] {
        if let Some(v) = v {
            url.push_str(&format!("&{k}={}", pct(v)));
        }
    }
    url
}

/// x402 v2 `PaymentRequired` with one `exact` requirement.
pub fn x402_payment_required(
    entry: &StablecoinEntry,
    to: &AccountAddress,
    amount: &Amount,
    resource_url: &str,
    description: Option<&str>,
    max_timeout_seconds: u64,
    extra: Option<Value>,
) -> Value {
    let mut req = json!({
        "scheme": "exact",
        "network": entry.asset.chain.to_string(),
        "amount": amount.raw.to_string(),
        "asset": entry.address,
        "payTo": to.to_string(),
        "maxTimeoutSeconds": max_timeout_seconds,
    });
    if let (Some(extra), Some(m)) = (extra, req.as_object_mut()) {
        m.insert("extra".into(), extra);
    }
    let mut resource = json!({ "url": resource_url });
    if let (Some(d), Some(m)) = (description, resource.as_object_mut()) {
        m.insert("description".into(), json!(d));
    }
    json!({ "x402Version": 2, "resource": resource, "accepts": [req] })
}

pub struct BuildRequest;

#[async_trait]
impl Operation for BuildRequest {
    type Input = BuildRequestIn;
    type Output = PaymentRequestOut;
    const NAME: &'static str = "payments_build_request";
    const DOMAIN: Domain = Domain::Payments;
    const DESCRIPTION: &'static str = "Build a payment request for a canonical stablecoin: an EIP-681 URI (EVM), a Solana Pay URL with a fresh random reference key (Solana), or an x402 v2 PaymentRequired body (`exact` scheme, amount in base units). \
Use it to ask a person or agent to pay you; decimals and contract/mint come from the verified registry, so there is no decimals or wrong-token mix-up. \
Pure computation, no chain calls. Keep the returned `reference` (Solana Pay) and pass it to payments_verify_transfer later. \
Caveats: EIP-681 has no memo, so use a unique deposit address per invoice on EVM; for x402 on EVM pass the token's EIP-712 name/version for EIP-3009 wallets.";
    const PROFILES: &'static [Profile] = PROFILES;

    async fn execute(
        &self,
        ctx: &Ctx,
        input: BuildRequestIn,
    ) -> Result<OpOutput<PaymentRequestOut>, DomainError> {
        let chain = ctx.chain(&input.chain)?;
        let entry = resolve_canonical(chain, &input.token)?;
        let to = AccountAddress::parse(chain.family, &input.recipient)?;
        let amount = parse_amount(
            input.amount.as_deref(),
            input.amount_raw.as_deref(),
            entry.decimals,
        )?;
        let mut out = PaymentRequestOut {
            asset: entry.asset.clone(),
            symbol: entry.symbol.clone(),
            amount,
            uri: None,
            reference: None,
            x402: None,
            notes: Vec::new(),
        };
        match (input.format, chain.family) {
            (RequestFormat::Eip681, ChainFamily::Evm) => {
                if entry.address == to.to_string() {
                    return Err(DomainError::invalid(
                        "recipient is the token contract itself",
                    ));
                }
                let id = chain.id.evm_chain_id().unwrap_or_default();
                out.uri = Some(eip681_uri(entry, id, &to, &amount));
            }
            (RequestFormat::SolanaPay, ChainFamily::Solana) => {
                let reference = match &input.reference {
                    Some(r) => r.parse::<SolanaPubkey>()?,
                    None => SolanaPubkey(rand::random()),
                };
                out.uri = Some(solana_pay_url(
                    entry,
                    &to,
                    &amount,
                    &reference,
                    input.label.as_deref(),
                    input.message.as_deref(),
                    input.memo.as_deref(),
                ));
                out.reference = Some(reference.to_string());
            }
            (RequestFormat::X402, family) => {
                let url = input.resource_url.as_deref().ok_or_else(|| {
                    DomainError::invalid("x402 needs `resource_url` (the protected resource)")
                })?;
                let extra = match (&input.eip712_name, &input.eip712_version) {
                    (Some(n), Some(v)) => Some(json!({"name": n, "version": v})),
                    _ => {
                        if family == ChainFamily::Evm {
                            out.notes.push("no `extra`: pass eip712_name/eip712_version (the token's EIP-712 domain) so EIP-3009 clients can sign".into());
                        } else {
                            out.notes.push(
                                "Solana exact scheme: the facilitator adds `extra.feePayer`".into(),
                            );
                        }
                        None
                    }
                };
                out.x402 = Some(x402_payment_required(
                    entry,
                    &to,
                    &amount,
                    url,
                    input.description.as_deref(),
                    input.max_timeout_seconds.unwrap_or(60),
                    extra,
                ));
            }
            (f, family) => {
                return Err(DomainError::invalid(format!(
                    "format {f:?} is not available on {family:?} chains"
                )))
            }
        }
        let mut out = OpOutput::local(out);
        out.meta.chain = Some(chain.id.clone());
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bdm_domain::BalanceDelta;

    const SOL: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";
    const TO: &str = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";
    const FROM: &str = "0x1111111111111111111111111111111111111111";

    fn reg() -> &'static StablecoinRegistry {
        registry().unwrap()
    }
    fn base() -> ChainId {
        ChainId::evm(8453)
    }
    fn usdc() -> &'static StablecoinEntry {
        reg().by_symbol(&base(), "USDC").unwrap()
    }
    fn addr(s: &str) -> AccountAddress {
        s.parse().unwrap()
    }
    fn spoof() -> AssetId {
        "eip155:8453/erc20:0x2222222222222222222222222222222222222222"
            .parse()
            .unwrap()
    }

    fn expected(raw: u128) -> ExpectedPayment {
        ExpectedPayment {
            asset: usdc().asset.clone(),
            recipient: addr(TO),
            amount: Amount::from_u128(raw, 6),
            min_finality: Finality::Finalized,
            reference: None,
        }
    }

    fn delta(asset: &AssetId, before: u128, after: u128, decimals: u8) -> BalanceDelta {
        BalanceDelta {
            owner: addr(TO),
            asset: asset.clone(),
            before: Amount::from_u128(before, decimals),
            after: Amount::from_u128(after, decimals),
            withheld_fee: None,
        }
    }

    fn transfer(asset: &AssetId, amount: u128, log_index: u64) -> Transfer {
        Transfer {
            chain: base(),
            tx_hash: "0xabc".into(),
            log_index: Some(log_index),
            kind: TransferKind::Token,
            asset: asset.clone(),
            from: Some(addr(FROM)),
            to: addr(TO),
            amount: Amount::from_u128(amount, 6),
            block: None,
        }
    }

    fn tx(finality: Finality, transfers: Vec<Transfer>, deltas: Vec<BalanceDelta>) -> Tx {
        Tx {
            chain: base(),
            hash: "0xabc".into(),
            status: TxStatus::Success,
            finality,
            block: Some(BlockRef {
                number: 10,
                hash: Some("0xblock".into()),
                timestamp: None,
            }),
            from: Some(addr("0x3333333333333333333333333333333333333333")),
            to: None,
            fee: None,
            transfers,
            balance_deltas: deltas,
            raw: None,
        }
    }

    fn paid(amount_in_delta: u128, finality: Finality) -> Tx {
        let a = &usdc().asset;
        tx(
            finality,
            vec![transfer(a, 10_000_000, 7)],
            vec![delta(a, 5, 5 + amount_in_delta, 6)],
        )
    }

    #[test]
    fn exact_payment_finalized_with_idempotency_key_and_payer() {
        let c = check_payment(
            &expected(10_000_000),
            &paid(10_000_000, Finality::Finalized),
            reg(),
        );
        assert_eq!(c.status, PaymentStatus::Finalized);
        assert_eq!(c.net_received.raw, U256::from(10_000_000u64));
        assert_eq!(c.idempotency_key, "eip155:8453:0xabc:7");
        assert_eq!(c.payer, Some(addr(FROM)), "payer is the transfer sender");
        assert_eq!(c.block.unwrap().hash.as_deref(), Some("0xblock"));
        assert!(c.meets_min_finality);
    }

    #[test]
    fn balance_change_wins_over_transfer_amount() {
        // Transfer log says 10 USDC but the recipient's balance rose by 9.9 (fee-on-transfer).
        let c = check_payment(
            &expected(10_000_000),
            &paid(9_900_000, Finality::Finalized),
            reg(),
        );
        assert_eq!(c.status, PaymentStatus::Underpaid);
        assert_eq!(c.net_received.raw, U256::from(9_900_000u64));
        let c = check_payment(
            &expected(10_000_000),
            &paid(10_000_001, Finality::Finalized),
            reg(),
        );
        assert_eq!(c.status, PaymentStatus::Overpaid);
    }

    #[test]
    fn finality_gates_the_verdict() {
        let soft = Finality::Confirmed { confirmations: 3 };
        let c = check_payment(&expected(10_000_000), &paid(10_000_000, soft), reg());
        assert_eq!(c.status, PaymentStatus::Pending);
        assert!(!c.meets_min_finality);
        let mut e = expected(10_000_000);
        e.min_finality = MinFinality::Confirmed.floor();
        assert_eq!(
            check_payment(&e, &paid(10_000_000, soft), reg()).status,
            PaymentStatus::Confirmed
        );
        e.min_finality = MinFinality::Safe.floor();
        assert_eq!(
            check_payment(&e, &paid(10_000_000, Finality::Safe), reg()).status,
            PaymentStatus::Confirmed
        );
    }

    #[test]
    fn spoofed_token_with_same_amount_is_wrong_token() {
        let s = spoof();
        let t = tx(
            Finality::Finalized,
            vec![transfer(&s, 10_000_000, 1)],
            vec![delta(&s, 0, 10_000_000, 6)],
        );
        let c = check_payment(&expected(10_000_000), &t, reg());
        assert_eq!(c.status, PaymentStatus::WrongToken);
        assert!(c.net_received.is_zero());
        assert_eq!(c.other_assets_received, vec![s]);
        assert!(c.notes[0].contains("possible spoof"), "{:?}", c.notes);
    }

    #[test]
    fn other_canonical_stablecoin_is_wrong_token_and_named() {
        let eurc = reg().by_symbol(&base(), "EURC").unwrap().asset.clone();
        let t = tx(
            Finality::Finalized,
            vec![],
            vec![delta(&eurc, 0, 10_000_000, 6)],
        );
        let c = check_payment(&expected(10_000_000), &t, reg());
        assert_eq!(c.status, PaymentStatus::WrongToken);
        assert!(c.notes[0].contains("canonical EURC"));
    }

    #[test]
    fn failed_unverifiable_and_nothing_received() {
        let mut t = paid(10_000_000, Finality::Finalized);
        t.status = TxStatus::Failed;
        assert_eq!(
            check_payment(&expected(10_000_000), &t, reg()).status,
            PaymentStatus::Failed
        );
        let t = paid(10_000_000, Finality::Unverifiable);
        assert_eq!(
            check_payment(&expected(10_000_000), &t, reg()).status,
            PaymentStatus::Unverifiable
        );
        // transfer present but no balance change data → never trust the event amount
        let a = usdc().asset.clone();
        let t = tx(
            Finality::Finalized,
            vec![transfer(&a, 10_000_000, 0)],
            vec![],
        );
        assert_eq!(
            check_payment(&expected(10_000_000), &t, reg()).status,
            PaymentStatus::Unverifiable
        );
        // paid someone else entirely
        let t = tx(Finality::Finalized, vec![], vec![]);
        let c = check_payment(&expected(10_000_000), &t, reg());
        assert_eq!(c.status, PaymentStatus::Underpaid);
        assert_eq!(c.idempotency_key, "eip155:8453:0xabc");
    }

    #[test]
    fn decimals_mismatch_is_unverifiable() {
        // A parser reporting 18 decimals for a 6-decimal registry token must not be compared.
        let a = usdc().asset.clone();
        let t = tx(Finality::Finalized, vec![], vec![delta(&a, 0, 10, 18)]);
        assert_eq!(
            check_payment(&expected(10), &t, reg()).status,
            PaymentStatus::Unverifiable
        );
    }

    #[test]
    fn token2022_withheld_fee_and_reference() {
        let sol: ChainId = SOL.parse().unwrap();
        let pyusd = reg().by_symbol(&sol, "PYUSD").unwrap();
        let owner = addr("9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM");
        let reference = "Ref1111111111111111111111111111111111111111";
        let mut d = delta(&pyusd.asset, 0, 990_000, 6);
        d.owner = owner;
        d.withheld_fee = Some(Amount::from_u128(10_000, 6));
        let mut t = tx(Finality::Finalized, vec![], vec![d]);
        t.chain = sol;
        t.raw = Some(json!({"transaction": {"message": {"accountKeys": [reference]}}}));
        let e = ExpectedPayment {
            asset: pyusd.asset.clone(),
            recipient: owner,
            amount: Amount::from_u128(1_000_000, 6),
            min_finality: Finality::Finalized,
            reference: Some(reference.into()),
        };
        let c = check_payment(&e, &t, reg());
        assert_eq!(
            c.status,
            PaymentStatus::Underpaid,
            "net of the transfer fee"
        );
        assert_eq!(c.withheld_fee.unwrap().raw, U256::from(10_000u64));
        assert_eq!(c.reference_found, Some(true));
    }

    #[test]
    fn amount_parsing() {
        assert_eq!(
            parse_amount(Some("10.5"), None, 6).unwrap().raw,
            U256::from(10_500_000u64)
        );
        assert_eq!(
            parse_amount(None, Some("7"), 18).unwrap().raw,
            U256::from(7u8)
        );
        assert!(parse_amount(Some("1"), Some("1"), 6).is_err());
        assert!(parse_amount(None, None, 6).is_err());
        assert!(parse_amount(Some("0"), None, 6).is_err());
        assert!(parse_amount(Some("1.0000001"), None, 6).is_err());
    }

    #[test]
    fn deposits_keep_only_incoming_canonical_tokens() {
        let a = usdc().asset.clone();
        let mut out = transfer(&a, 5, 2);
        out.to = addr(FROM);
        let list = vec![transfer(&a, 5, 1), transfer(&spoof(), 5, 3), out];
        let d = canonical_deposits(list, &addr(TO), &[usdc()]);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].symbol, "USDC");
        assert_eq!(d[0].idempotency_key, "eip155:8453:0xabc:1");
    }

    #[test]
    fn eip681_and_x402_shapes() {
        let amt = Amount::from_u128(1_500_000, 6);
        assert_eq!(
            eip681_uri(usdc(), 8453, &addr(TO), &amt),
            format!("ethereum:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913@8453/transfer?address={TO}&uint256=1500000")
        );
        let v = x402_payment_required(
            usdc(),
            &addr(TO),
            &amt,
            "https://api.example.com/x",
            Some("data"),
            60,
            Some(json!({"name": "USD Coin", "version": "2"})),
        );
        assert_eq!(v["x402Version"], 2);
        assert_eq!(v["resource"]["url"], "https://api.example.com/x");
        let r = &v["accepts"][0];
        assert_eq!(r["scheme"], "exact");
        assert_eq!(r["network"], "eip155:8453");
        assert_eq!(r["amount"], "1500000");
        assert_eq!(r["asset"], "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
        assert_eq!(r["payTo"], TO);
        assert_eq!(r["maxTimeoutSeconds"], 60);
        assert_eq!(r["extra"]["version"], "2");
    }

    #[test]
    fn solana_pay_url_encodes_and_uses_decimal_amount() {
        let sol: ChainId = SOL.parse().unwrap();
        let usdc = reg().by_symbol(&sol, "USDC").unwrap();
        let to = addr("9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM");
        let r1 = SolanaPubkey(rand::random());
        let r2 = SolanaPubkey(rand::random());
        assert_ne!(r1, r2, "fresh reference per request");
        let url = solana_pay_url(
            usdc,
            &to,
            &Amount::from_u128(1_010_000, 6),
            &r1,
            Some("Shop & Co"),
            None,
            Some("order#42"),
        );
        assert_eq!(
            url,
            format!("solana:{to}?amount=1.01&spl-token=EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v&reference={r1}&label=Shop%20%26%20Co&memo=order%2342")
        );
    }
}
