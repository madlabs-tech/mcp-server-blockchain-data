//! `wallet` tools: `wallet_get_balances`, `wallet_get_transfers`, `address_validate`.
//! See the ownership table in `ops/mod.rs`.

use super::chain::{
    merge_meta, native_asset, parse_address, resolve_asset, rpc_meta, stablecoin, stablecoins,
};
use crate::{Catalog, Ctx, Domain, OpOutput, Operation, Profile};
use async_trait::async_trait;
use bdm_config::ChainEntry;
use bdm_domain::{
    AccountAddress, Amount, AssetId, AssetRef, ChainFamily, ChainId, DomainError, Provenance,
    Transfer,
};
use bdm_ports::{
    Capability, Direction, EvmRpc, Page, SolanaRpc, TokenBalance, TokenBalances, TransferHistory,
    TransferQuery,
};
use bdm_protocols::solana::spl;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{str::FromStr, sync::Arc, time::Duration};

pub fn register(c: &mut Catalog) {
    c.register(WalletGetBalances);
    c.register(WalletGetTransfers);
    c.register(AddressValidate);
}

const ALL: &[Profile] = Profile::ALL;
const MONEY: &[Profile] = &[Profile::Payments, Profile::Neobank, Profile::Trading];

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StablecoinInfo {
    pub symbol: String,
    pub issuer: String,
}

pub(crate) fn stablecoin_info(asset: &AssetId) -> Option<StablecoinInfo> {
    stablecoin(asset).map(|e| StablecoinInfo {
        symbol: e.symbol.clone(),
        issuer: e.issuer_entity.clone(),
    })
}

// ------------------------------------------------------------------ wallet_get_balances

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BalancesIn {
    /// Wallet address: 0x… (EVM) or base58 owner wallet (Solana; not a token account).
    pub address: String,
    /// Chains to query (CAIP-2 ids or aliases). Default: every enabled chain of the address's family.
    #[serde(default)]
    pub chains: Option<Vec<String>>,
    /// Only canonical (issuer-listed) stablecoins, plus the native coin.
    #[serde(default)]
    pub stablecoins_only: bool,
    /// Restrict to these CAIP-19 assets (native is always included).
    #[serde(default)]
    pub assets: Option<Vec<AssetId>>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BalanceRow {
    pub asset: AssetId,
    /// Exact amount in base units (+ exact formatted value).
    pub amount: Amount,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub native: bool,
    /// Set when the asset is a canonical stablecoin per the registry (matched by contract/mint).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stablecoin: Option<StablecoinInfo>,
    /// Solana: every token account summed into this owner-level balance.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub token_accounts: Vec<AccountAddress>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ChainBalances {
    pub chain: ChainId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub balances: Vec<BalanceRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<DomainError>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct BalancesOut {
    pub owner: AccountAddress,
    pub chains: Vec<ChainBalances>,
}

/// Owner-level balances: sums token accounts of the same asset (Solana), native first.
pub(crate) fn sum_by_asset(
    chain: &ChainEntry,
    items: Vec<TokenBalance>,
) -> Result<Vec<BalanceRow>, DomainError> {
    let mut rows: Vec<BalanceRow> = Vec::new();
    for b in items {
        if let Some(r) = rows.iter_mut().find(|r| r.asset == b.asset) {
            r.amount = r.amount.checked_add(&b.amount)?;
            r.token_accounts.extend(b.token_account);
            continue;
        }
        let native = b.asset.is_native();
        let sc = stablecoin_info(&b.asset);
        rows.push(BalanceRow {
            symbol: b
                .symbol
                .or_else(|| sc.as_ref().map(|s| s.symbol.clone()))
                .or_else(|| native.then(|| chain.native.symbol.clone())),
            stablecoin: sc,
            native,
            token_accounts: b.token_account.into_iter().collect(),
            asset: b.asset,
            amount: b.amount,
        });
    }
    rows.sort_by_key(|r| !r.native);
    Ok(rows)
}

pub struct WalletGetBalances;

#[async_trait]
impl Operation for WalletGetBalances {
    type Input = BalancesIn;
    type Output = BalancesOut;
    const NAME: &'static str = "wallet_get_balances";
    const DOMAIN: Domain = Domain::Wallet;
    const DESCRIPTION: &'static str = "Native coin and token balances of one wallet across chains \
        (all EVM chains for a 0x address, Solana for a base58 address, or the `chains` you list). \
        Amounts are exact integers in base units plus decimals; never floats. Solana balances are \
        owner-level: every token account (both token programs) holding a mint is summed. Set \
        `stablecoins_only` for canonical issuer-listed stablecoins only (matched by contract/mint, \
        never by symbol). A failing chain is reported in its `error` without failing the others.";
    const PROFILES: &'static [Profile] = ALL;

    fn cache_ttl(&self) -> Option<Duration> {
        Some(Duration::from_secs(5))
    }

    async fn execute(
        &self,
        ctx: &Ctx,
        input: BalancesIn,
    ) -> Result<OpOutput<BalancesOut>, DomainError> {
        let owner = AccountAddress::from_str(&input.address)?;
        let chains: Vec<&ChainEntry> = match (&input.chains, &input.assets) {
            (Some(list), _) => list
                .iter()
                .map(|c| ctx.chain(c))
                .collect::<Result<_, _>>()?,
            (None, Some(assets)) => {
                let mut ids: Vec<&ChainId> = assets.iter().map(|a| &a.chain).collect();
                ids.sort();
                ids.dedup();
                ids.into_iter()
                    .map(|c| ctx.chain(&c.to_string()))
                    .collect::<Result<_, _>>()?
            }
            (None, None) => ctx
                .config()
                .registry
                .chains
                .enabled()
                .filter(|c| c.family == owner.family())
                .collect(),
        };
        // A chain the address can't exist on gets its own error row; the other chains still
        // answer (agents often ask for "all my chains" with one address family).
        let (chains, mismatched): (Vec<&ChainEntry>, Vec<&ChainEntry>) =
            chains.into_iter().partition(|c| c.family == owner.family());
        if chains.is_empty() {
            let c = mismatched.first().expect("at least one chain");
            return Err(DomainError::invalid(format!(
                "{owner} is not a valid address on {} ({:?} chain)",
                c.id, c.family
            )));
        }

        let results = futures::future::join_all(chains.iter().map(|chain| {
            let filter: Option<Vec<AssetId>> = if input.stablecoins_only {
                Some(
                    std::iter::once(native_asset(chain))
                        .chain(stablecoins().for_chain(&chain.id).map(|e| e.asset.clone()))
                        .collect(),
                )
            } else {
                input.assets.as_ref().map(|a| {
                    std::iter::once(native_asset(chain))
                        .chain(a.iter().filter(|x| x.chain == chain.id).cloned())
                        .collect()
                })
            };
            chain_balances(ctx, chain, owner, filter)
        }))
        .await;

        let mut out: Vec<ChainBalances> = mismatched
            .iter()
            .map(|c| ChainBalances {
                chain: c.id.clone(),
                provider: None,
                balances: Vec::new(),
                error: Some(DomainError::invalid(format!(
                    "{owner} is not a valid address on {} ({:?} chain)",
                    c.id, c.family
                ))),
            })
            .collect();
        let mut metas = Vec::new();
        for (chain, r) in chains.iter().zip(results) {
            match r {
                Ok((rows, meta)) => {
                    out.push(ChainBalances {
                        chain: chain.id.clone(),
                        provider: meta.provider.clone(),
                        balances: rows,
                        error: None,
                    });
                    metas.push(meta);
                }
                Err(e) => out.push(ChainBalances {
                    chain: chain.id.clone(),
                    provider: None,
                    balances: Vec::new(),
                    error: Some(e),
                }),
            }
        }
        if metas.is_empty() {
            if let Some(e) = out.iter_mut().find_map(|c| c.error.take()) {
                return Err(e);
            }
        }
        let single = (chains.len() == 1).then(|| chains[0].id.clone());
        Ok(OpOutput::new(
            BalancesOut { owner, chains: out },
            merge_meta(single, metas),
        ))
    }
}

async fn chain_balances(
    ctx: &Ctx,
    chain: &ChainEntry,
    owner: AccountAddress,
    filter: Option<Vec<AssetId>>,
) -> Result<(Vec<BalanceRow>, Provenance), DomainError> {
    let req = ctx.route(Capability::TokenBalances).chain(chain.id.clone());
    let filter = filter.as_deref();
    let r =
        ctx.router()
            .failover::<dyn TokenBalances, _, _, _>(req, |p| async move {
                p.balances(&owner, filter).await
            })
            .await
            .map_err(|e| e.error)?;
    let mut rows = sum_by_asset(chain, r.value)?;
    if let Some(f) = filter {
        rows.retain(|b| f.contains(&b.asset));
    }
    Ok((rows, r.provenance))
}

// ------------------------------------------------------------------ wallet_get_transfers

/// Shared transfer filters (`wallet_get_transfers`, `neobank_get_ledger`).
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct TransferFilter {
    /// in | out | both (default).
    #[serde(default)]
    pub direction: Direction,
    /// Restrict to these assets (CAIP-19, contract/mint address, or stablecoin symbol like "USDC").
    #[serde(default)]
    pub assets: Option<Vec<String>>,
    /// Only canonical stablecoins from the registry.
    #[serde(default)]
    pub stablecoins_only: bool,
    #[serde(default)]
    pub from_block: Option<u64>,
    #[serde(default)]
    pub to_block: Option<u64>,
    /// `next_cursor` from the previous page, passed back unchanged.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Page size, 1–200 (default 50).
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TransfersIn {
    /// Wallet address (0x… or Solana owner wallet).
    pub address: String,
    /// Chain (CAIP-2 id or alias).
    pub chain: String,
    #[serde(flatten)]
    pub filter: TransferFilter,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TransfersOut {
    pub items: Vec<Transfer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// True when the provider that issued `cursor` was unavailable and another provider served
    /// the FIRST page instead: de-duplicate by (tx_hash, log_index).
    pub cursor_reset: bool,
}

pub(crate) struct TransfersPage {
    pub page: Page<Transfer>,
    pub meta: Provenance,
    pub cursor_reset: bool,
}

/// Build the port query from the shared tool inputs.
pub(crate) fn transfer_query(
    chain: &ChainEntry,
    owner: AccountAddress,
    f: TransferFilter,
) -> Result<TransferQuery, DomainError> {
    let assets = if f.stablecoins_only {
        Some(
            stablecoins()
                .for_chain(&chain.id)
                .map(|e| e.asset.clone())
                .collect(),
        )
    } else {
        f.assets
            .map(|a| {
                a.iter()
                    .map(|s| resolve_asset(chain, Some(s)))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
    };
    Ok(TransferQuery {
        owner,
        direction: f.direction,
        assets,
        from_block: f.from_block,
        to_block: f.to_block,
        cursor: f.cursor,
        limit: f.limit.unwrap_or(50).clamp(1, 200),
    })
}

/// Page of transfers. Cursors are `<vendor>:<vendor cursor>` so a page is only resumed on the
/// vendor that issued it; if that vendor is unavailable the next one serves the first page and
/// `cursor_reset` is set.
pub(crate) async fn fetch_transfers(
    ctx: &Ctx,
    chain: &ChainEntry,
    mut query: TransferQuery,
) -> Result<TransfersPage, DomainError> {
    let (vendor, inner) = match query.cursor.take() {
        Some(c) => {
            let (v, rest) = c
                .split_once(':')
                .ok_or_else(|| DomainError::invalid("malformed cursor; pass next_cursor as-is"))?;
            (Some(v.to_owned()), Some(rest.to_owned()))
        }
        None => (None, None),
    };
    let req = ctx
        .route(Capability::TransferHistory)
        .chain(chain.id.clone());
    let pinned: Option<Arc<dyn TransferHistory>> = vendor.as_ref().and_then(|v| {
        ctx.router()
            .candidates::<dyn TransferHistory>(ctx.table(), &req)
            .list
            .into_iter()
            .find(|c| &c.vendor == v)
            .map(|c| c.port)
    });
    let (query, inner, pinned) = (&query, &inner, &pinned);
    let r = ctx
        .router()
        .failover::<dyn TransferHistory, _, _, _>(req, |p| {
            let mut q = query.clone();
            if pinned.as_ref().is_some_and(|x| Arc::ptr_eq(x, &p)) {
                q.cursor = inner.clone();
            }
            async move { p.transfers(&q).await }
        })
        .await
        .map_err(|e| e.error)?;
    let winner = r.provenance.provider.clone().unwrap_or_default();
    let cursor_reset = inner.is_some() && vendor.as_deref() != Some(winner.as_str());
    let mut page = r.value;
    page.next_cursor = page.next_cursor.map(|c| format!("{winner}:{c}"));
    Ok(TransfersPage {
        page,
        meta: r.provenance,
        cursor_reset,
    })
}

pub struct WalletGetTransfers;

#[async_trait]
impl Operation for WalletGetTransfers {
    type Input = TransfersIn;
    type Output = TransfersOut;
    const NAME: &'static str = "wallet_get_transfers";
    const DOMAIN: Domain = Domain::Wallet;
    const DESCRIPTION: &'static str = "Incoming and/or outgoing native and token transfers of a \
        wallet on one chain, newest first, paginated with `cursor` (pass `next_cursor` back \
        unchanged). Solana includes transfers into the wallet's token accounts. Filter with \
        `assets` or `stablecoins_only`. For payment confirmation prefer \
        payments_verify_transfer; for bank-style statements with fiat values use neobank_get_ledger.";
    const PROFILES: &'static [Profile] = MONEY;

    async fn execute(
        &self,
        ctx: &Ctx,
        input: TransfersIn,
    ) -> Result<OpOutput<TransfersOut>, DomainError> {
        let chain = ctx.chain(&input.chain)?;
        let q = transfer_query(chain, parse_address(chain, &input.address)?, input.filter)?;
        let p = fetch_transfers(ctx, chain, q).await?;
        Ok(OpOutput::new(
            TransfersOut {
                items: p.page.items,
                next_cursor: p.page.next_cursor,
                cursor_reset: p.cursor_reset,
            },
            p.meta,
        ))
    }
}

// ------------------------------------------------------------------ address_validate

pub(crate) const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddressValidateIn {
    /// Address to check: 0x… (EVM) or base58 (Solana).
    pub address: String,
    /// Chain the funds will be sent on (CAIP-2 id or alias).
    pub chain: String,
    /// Optional token that will be sent (CAIP-19, contract/mint, or stablecoin symbol "USDC"):
    /// checks it exists on this chain and whether it is the canonical issuer deployment.
    #[serde(default)]
    pub token: Option<String>,
    /// EVM: when the address has no code here, look for code on the other EVM chains (catches
    /// Safes/smart accounts deployed on another network). Default true.
    #[serde(default = "yes")]
    pub check_other_chains: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AddressKind {
    Invalid,
    /// EVM externally owned account (no code).
    Eoa,
    /// EVM EOA with EIP-7702 delegation code (`0xef0100` + delegate): still key-controlled.
    Eip7702Delegated,
    /// EVM smart-contract wallet (Safe, ERC-4337 account).
    SmartAccount,
    /// EVM token contract: sending funds to it usually loses them.
    TokenContract,
    Contract,
    /// Solana system-owned wallet.
    Wallet,
    /// Solana address with no account yet (can receive SOL; token sends must create its ATA).
    UnfundedWallet,
    /// Solana token account (not a wallet).
    TokenAccount,
    /// Solana token mint.
    Mint,
    /// Solana executable program.
    Program,
    /// Solana account owned by a program (PDA / program state).
    ProgramOwned,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TokenAccountInfo {
    pub mint: String,
    pub owner: String,
    pub token_program: String,
    /// True if this is the owner's associated token account for the mint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_ata: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TokenCheck {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset: Option<AssetId>,
    /// Contract/mint exists on this chain (`None` = could not check).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exists_on_chain: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_stablecoin: Option<StablecoinInfo>,
    /// canonical (issuer-listed deployment) | not_canonical (e.g. bridged USDC.e or a spoof) | unknown.
    pub issuance: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AddressReport {
    pub valid: bool,
    pub chain: ChainId,
    pub family: ChainFamily,
    /// Canonical form: EIP-55 checksum (EVM) or base58 (Solana).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normalized: Option<String>,
    /// EVM: valid | not_checksummed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
    pub kind: AddressKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delegate: Option<String>,
    /// EVM smart-account type: safe | erc4337.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smart_account: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytecode_size: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_account: Option<TokenAccountInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<TokenCheck>,
    /// EVM chains where this address has contract code (when it has none on `chain`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub code_on_other_chains: Vec<ChainId>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl AddressReport {
    fn new(chain: &ChainEntry) -> Self {
        Self {
            valid: false,
            chain: chain.id.clone(),
            family: chain.family,
            normalized: None,
            checksum: None,
            kind: AddressKind::Invalid,
            delegate: None,
            smart_account: None,
            bytecode_size: None,
            token_account: None,
            token: None,
            code_on_other_chains: Vec::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

/// EVM code classification: empty → EOA; `0xef0100 ‖ address` → EIP-7702 delegation.
pub(crate) fn classify_code(code: &[u8]) -> (AddressKind, Option<alloy_primitives::Address>) {
    match code {
        [] => (AddressKind::Eoa, None),
        [0xef, 0x01, 0x00, rest @ ..] if rest.len() == 20 => (
            AddressKind::Eip7702Delegated,
            Some(alloy_primitives::Address::from_slice(rest)),
        ),
        _ => (AddressKind::Contract, None),
    }
}

fn hex_bytes(v: &Value) -> Option<Vec<u8>> {
    hex_decode(v.as_str()?.trim_start_matches("0x"))
}

pub(crate) fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

async fn evm_code(rpc: &dyn EvmRpc, a: alloy_primitives::Address) -> Option<Vec<u8>> {
    hex_bytes(
        &rpc.request("eth_getCode", json!([a, "latest"]))
            .await
            .ok()?,
    )
}

/// `eth_call` returning one 32-byte word, or `None` on revert / empty.
async fn call_word(rpc: &dyn EvmRpc, to: alloy_primitives::Address, data: &str) -> Option<Vec<u8>> {
    let v = rpc
        .request("eth_call", json!([{ "to": to, "data": data }, "latest"]))
        .await
        .ok()?;
    hex_bytes(&v).filter(|b| b.len() == 32)
}

pub(crate) async fn sol_account(rpc: &dyn SolanaRpc, address: &str) -> Result<Value, DomainError> {
    let v = rpc
        .request(
            "getAccountInfo",
            json!([address, { "encoding": "jsonParsed", "commitment": "confirmed" }]),
        )
        .await?;
    Ok(v.get("value").cloned().unwrap_or(Value::Null))
}

pub(crate) fn is_token_program(owner: &str) -> bool {
    owner == spl::TOKEN_PROGRAM || owner == spl::TOKEN_2022_PROGRAM
}

pub struct AddressValidate;

#[async_trait]
impl Operation for AddressValidate {
    type Input = AddressValidateIn;
    type Output = AddressReport;
    const NAME: &'static str = "address_validate";
    const DOMAIN: Domain = Domain::Wallet;
    const DESCRIPTION: &'static str = "Check a recipient address before sending funds on a chain. \
        EVM: EIP-55 checksum, EOA vs contract vs smart account (Safe / ERC-4337), EIP-7702 \
        delegated EOAs flagged separately, token contracts flagged, and a warning when the address \
        is a contract on another EVM network but not on this one (same 0x address, wrong network). \
        Solana: base58, owner wallet vs token account (ATA) vs mint vs program; payment flows need \
        the OWNER wallet, not a token account. With `token`, also checks the token exists on this \
        chain and whether it is the canonical issuer deployment or not (bridged/spoofed). Always \
        read `warnings`.";
    const PROFILES: &'static [Profile] = ALL;

    async fn execute(
        &self,
        ctx: &Ctx,
        input: AddressValidateIn,
    ) -> Result<OpOutput<AddressReport>, DomainError> {
        let chain = ctx.chain(&input.chain)?;
        let mut rep = AddressReport::new(chain);
        let addr = match parse_address(chain, &input.address) {
            Ok(a) => a,
            Err(e) => {
                rep.errors.push(e.message);
                let other = match chain.family {
                    ChainFamily::Evm => ChainFamily::Solana,
                    ChainFamily::Solana => ChainFamily::Evm,
                };
                if AccountAddress::parse(other, &input.address).is_ok() {
                    rep.errors.push(format!(
                        "this is a {other:?} address, but {} is a {:?} chain",
                        chain.id, chain.family
                    ));
                }
                return Ok(OpOutput::local(rep));
            }
        };
        rep.valid = true;
        rep.normalized = Some(addr.to_string());
        let meta = rpc_meta(&chain.id);
        match addr {
            AccountAddress::Evm(a) => validate_evm(ctx, chain, a, &input, &mut rep).await?,
            AccountAddress::Solana(_) => {
                validate_solana(ctx, chain, &addr, &input, &mut rep).await?
            }
        }
        Ok(OpOutput::new(rep, meta))
    }
}

async fn validate_evm(
    ctx: &Ctx,
    chain: &ChainEntry,
    a: alloy_primitives::Address,
    input: &AddressValidateIn,
    rep: &mut AddressReport,
) -> Result<(), DomainError> {
    let hex = input.address.trim().trim_start_matches("0x");
    let mixed =
        hex.chars().any(|c| c.is_ascii_lowercase()) && hex.chars().any(|c| c.is_ascii_uppercase());
    rep.checksum = Some(if mixed { "valid" } else { "not_checksummed" }.into());
    if !mixed {
        rep.warnings
            .push("address has no EIP-55 checksum (all one case); typos cannot be detected".into());
    }
    if a.is_zero() {
        rep.warnings
            .push("zero address: funds sent here are burned".into());
    }
    let rpc = ctx.evm_rpc(chain)?;
    let code = hex_bytes(&rpc.request("eth_getCode", json!([a, "latest"])).await?)
        .ok_or_else(|| DomainError::internal("eth_getCode returned non-hex"))?;
    rep.bytecode_size = Some(code.len());
    let (mut kind, delegate) = classify_code(&code);
    rep.delegate = delegate.map(|d| d.to_checksum(None));
    let as_token = AssetId {
        chain: chain.id.clone(),
        asset: AssetRef::Erc20(a),
    };
    if kind == AddressKind::Contract {
        if stablecoin(&as_token).is_some() {
            kind = AddressKind::TokenContract;
            rep.warnings.push(
                "this is a token contract, not a wallet: tokens sent here are usually lost".into(),
            );
        } else if call_word(&rpc, a, "0xe75235b8").await.is_some() {
            // getThreshold() answers → Safe multisig
            kind = AddressKind::SmartAccount;
            rep.smart_account = Some("safe".into());
        } else if call_word(&rpc, a, "0xb0d691fe")
            .await
            .is_some_and(|w| w[..12].iter().all(|b| *b == 0) && w[12..].iter().any(|b| *b != 0))
        {
            // entryPoint() returns an address → ERC-4337 account
            kind = AddressKind::SmartAccount;
            rep.smart_account = Some("erc4337".into());
        }
    }
    rep.kind = kind;
    if kind == AddressKind::Eip7702Delegated {
        rep.warnings.push(
            "EOA with EIP-7702 delegation: the key holder controls it, but the delegate contract \
             runs on every call"
                .into(),
        );
    }

    if kind == AddressKind::Eoa && input.check_other_chains {
        let others: Vec<&ChainEntry> = ctx
            .config()
            .registry
            .chains
            .enabled()
            .filter(|c| c.family == ChainFamily::Evm && c.id != chain.id)
            .collect();
        let codes = futures::future::join_all(others.iter().map(|c| async move {
            match ctx.evm_rpc(c) {
                Ok(rpc) => evm_code(&rpc, a).await,
                Err(_) => None,
            }
        }))
        .await;
        for (c, code) in others.iter().zip(codes) {
            if code.is_some_and(|code| classify_code(&code).0 == AddressKind::Contract) {
                rep.code_on_other_chains.push(c.id.clone());
            }
        }
        if !rep.code_on_other_chains.is_empty() {
            let list: Vec<String> = rep
                .code_on_other_chains
                .iter()
                .map(|c| c.to_string())
                .collect();
            rep.warnings.push(format!(
                "no code on {} but this address is a contract on {}: if it is a Safe or smart \
                 account deployed only there, funds sent on {} may be unrecoverable",
                chain.id,
                list.join(", "),
                chain.id
            ));
        }
    }

    if let Some(t) = &input.token {
        rep.token = Some(check_token(ctx, chain, t, rep).await?);
    }
    Ok(())
}

async fn validate_solana(
    ctx: &Ctx,
    chain: &ChainEntry,
    addr: &AccountAddress,
    input: &AddressValidateIn,
    rep: &mut AddressReport,
) -> Result<(), DomainError> {
    let rpc = ctx.solana_rpc(chain)?;
    let s = addr.to_string();
    let acct = sol_account(&rpc, &s).await?;
    let owner = acct["owner"].as_str().unwrap_or_default();
    let parsed = &acct["data"]["parsed"];
    rep.kind = if acct.is_null() {
        AddressKind::UnfundedWallet
    } else if acct["executable"].as_bool() == Some(true) {
        AddressKind::Program
    } else if owner == SYSTEM_PROGRAM {
        AddressKind::Wallet
    } else if is_token_program(owner) && parsed["type"] == "account" {
        AddressKind::TokenAccount
    } else if is_token_program(owner) && parsed["type"] == "mint" {
        AddressKind::Mint
    } else {
        AddressKind::ProgramOwned
    };
    match rep.kind {
        AddressKind::UnfundedWallet => rep.warnings.push(
            "no account exists yet: it can receive SOL; a token transfer must create the \
             recipient's associated token account"
                .into(),
        ),
        AddressKind::TokenAccount => {
            let info = &parsed["info"];
            let (mint, token_owner) = (
                info["mint"].as_str().unwrap_or_default().to_owned(),
                info["owner"].as_str().unwrap_or_default().to_owned(),
            );
            let is_ata = match (
                token_owner.parse(),
                mint.parse(),
                owner.parse::<bdm_domain::SolanaPubkey>(),
            ) {
                (Ok(o), Ok(m), Ok(p)) => spl::associated_token_address(&o, &m, &p)
                    .ok()
                    .map(|ata| ata.to_string() == s),
                _ => None,
            };
            rep.warnings.push(format!(
                "this is a token account, not a wallet: most payment flows need the owner wallet \
                 {token_owner}"
            ));
            if info["state"] == "frozen" {
                rep.warnings.push("token account is frozen".into());
            }
            rep.token_account = Some(TokenAccountInfo {
                mint,
                owner: token_owner,
                token_program: owner.to_owned(),
                is_ata,
                state: info["state"].as_str().map(str::to_owned),
            });
        }
        AddressKind::Mint => rep
            .warnings
            .push("this is a token mint, not a wallet: funds sent here are lost".into()),
        AddressKind::Program | AddressKind::ProgramOwned => rep.warnings.push(
            "account is owned by a program, not a wallet key: only send if the program expects it"
                .into(),
        ),
        _ => {}
    }
    if let Some(t) = &input.token {
        let check = check_token(ctx, chain, t, rep).await?;
        if let (Some(ta), Some(asset)) = (&rep.token_account, &check.asset) {
            if !asset.to_string().ends_with(&ta.mint) {
                rep.warnings
                    .push(format!("token account holds mint {}, not {asset}", ta.mint));
            }
        }
        rep.token = Some(check);
    }
    Ok(())
}

/// Token existence on `chain` + canonical-vs-other via the stablecoin registry.
async fn check_token(
    ctx: &Ctx,
    chain: &ChainEntry,
    token: &str,
    rep: &mut AddressReport,
) -> Result<TokenCheck, DomainError> {
    let looks_like_symbol = !token.contains('/')
        && !token.starts_with("0x")
        && token.len() <= 12
        && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '.');
    let asset = match resolve_asset(chain, Some(token)) {
        Ok(a) => a,
        Err(_) if looks_like_symbol => {
            let elsewhere: Vec<String> = ctx
                .config()
                .registry
                .chains
                .enabled()
                .filter(|c| stablecoins().by_symbol(&c.id, token).is_some())
                .map(|c| c.id.to_string())
                .collect();
            rep.warnings.push(if elsewhere.is_empty() {
                format!("{token} is not a canonical stablecoin in the registry")
            } else {
                format!(
                    "{token} has no canonical deployment on {} (it does on {}): wrong network?",
                    chain.id,
                    elsewhere.join(", ")
                )
            });
            return Ok(TokenCheck {
                asset: None,
                exists_on_chain: None,
                canonical_stablecoin: None,
                issuance: "unknown".into(),
            });
        }
        Err(e) => return Err(e),
    };
    let canonical = stablecoin_info(&asset);
    let exists = match &asset.asset {
        AssetRef::Native { .. } => Some(true),
        AssetRef::Erc20(t) => evm_code(&ctx.evm_rpc(chain)?, *t)
            .await
            .map(|c| !c.is_empty()),
        AssetRef::SplToken(m) => sol_account(&ctx.solana_rpc(chain)?, &m.to_string())
            .await
            .ok()
            .map(|a| {
                is_token_program(a["owner"].as_str().unwrap_or_default())
                    && a["data"]["parsed"]["type"] == "mint"
            }),
    };
    if exists == Some(false) {
        rep.warnings
            .push(format!("token {asset} does not exist on {}", chain.id));
    }
    let mut issuance = if canonical.is_some() || asset.is_native() {
        "canonical"
    } else {
        "unknown"
    };
    if canonical.is_none() {
        if let AssetRef::Erc20(t) = asset.asset {
            let sym = bdm_protocols::evm::erc20::symbol(&ctx.evm_rpc(chain)?, t)
                .await
                .ok()
                .flatten();
            let base = sym
                .as_deref()
                .map(|s| s.trim_end_matches(".e").trim_end_matches(".E"));
            if let Some(entry) = base.and_then(|s| stablecoins().by_symbol(&chain.id, s)) {
                issuance = "not_canonical";
                rep.warnings.push(format!(
                    "token symbol {} but the canonical {} on {} is {}: this is a bridged or \
                     spoofed token",
                    sym.unwrap_or_default(),
                    entry.symbol,
                    chain.id,
                    entry.asset
                ));
            }
        }
    }
    Ok(TokenCheck {
        asset: Some(asset),
        exists_on_chain: exists,
        canonical_stablecoin: canonical,
        issuance: issuance.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(id: &str) -> ChainEntry {
        bdm_config::Registry::builtin()
            .unwrap()
            .chains
            .resolve(id)
            .unwrap()
            .clone()
    }

    #[test]
    fn code_classification() {
        assert_eq!(classify_code(&[]).0, AddressKind::Eoa);
        let mut d = vec![0xef, 0x01, 0x00];
        d.extend([0x11; 20]);
        let (k, del) = classify_code(&d);
        assert_eq!(k, AddressKind::Eip7702Delegated);
        assert_eq!(del.unwrap().as_slice(), &[0x11; 20]);
        assert_eq!(classify_code(&[0x60, 0x80]).0, AddressKind::Contract);
        assert_eq!(classify_code(&d[..10]).0, AddressKind::Contract);
    }

    #[test]
    fn solana_token_accounts_sum_to_owner_level() {
        let sol = chain("solana");
        let mint: AssetId = format!(
            "{}/token:EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            sol.id
        )
        .parse()
        .unwrap();
        let acct = |s: &str| Some(AccountAddress::from_str(s).unwrap());
        let items = vec![
            TokenBalance {
                asset: mint.clone(),
                amount: Amount::from_u128(1_500_000, 6),
                symbol: None,
                token_account: acct("So11111111111111111111111111111111111111112"),
            },
            TokenBalance {
                asset: native_asset(&sol),
                amount: Amount::from_u128(10, 9),
                symbol: None,
                token_account: None,
            },
            TokenBalance {
                asset: mint.clone(),
                amount: Amount::from_u128(2_500_001, 6),
                symbol: Some("USDC".into()),
                token_account: acct("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"),
            },
        ];
        let rows = sum_by_asset(&sol, items).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].native && rows[0].symbol.as_deref() == Some("SOL"));
        assert_eq!(rows[1].amount, Amount::from_u128(4_000_001, 6));
        assert_eq!(rows[1].amount.format_units(), "4.000001");
        assert_eq!(rows[1].token_accounts.len(), 2);
    }

    #[test]
    fn hex_decoding() {
        assert_eq!(hex_decode("00ff10"), Some(vec![0, 255, 16]));
        assert_eq!(hex_decode("0"), None);
        assert_eq!(hex_decode("zz"), None);
    }
}
