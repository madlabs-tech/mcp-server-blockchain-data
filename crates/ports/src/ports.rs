//! The capability traits and their request/response types.
//!
//! Chain-bound ports (`EvmRpc`, `SolanaRpc`, `TokenBalances`, `TransferHistory`, `FeeOracle`,
//! `Simulator`, `Broadcaster`) are registered once per chain, so their methods take no chain
//! argument. Chain-agnostic ports (`PriceFeed`, `SwapQuoter`, `FxRates`, …) take ids that carry
//! the chain (CAIP-19 / CAIP-10).

use crate::PortResult;
use async_trait::async_trait;
use bdm_domain::{
    AccountAddress, AccountId, Amount, AssetId, BalanceDelta, ChainId, FeeEstimate, Price,
    RiskFlag, SwapQuote, Transfer, UnsignedTx,
};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ------------------------------------------------------------------ chain transport

/// Raw EVM JSON-RPC transport for one chain. Typed helpers (balances, logs, receipts, Multicall3)
/// are generic functions over this trait in `bdm-protocols`, so every RPC vendor gets them.
#[async_trait]
pub trait EvmRpc: Send + Sync {
    /// EIP-155 chain id this transport is bound to (asserted against `eth_chainId` at startup).
    fn chain_id(&self) -> u64;
    /// One JSON-RPC call; returns `result` or maps the error into [`crate::ProviderError`].
    async fn request(&self, method: &str, params: Value) -> PortResult<Value>;
}

/// Raw Solana JSON-RPC transport for one cluster.
#[async_trait]
pub trait SolanaRpc: Send + Sync {
    async fn request(&self, method: &str, params: Value) -> PortResult<Value>;
}

// ------------------------------------------------------------------ wallet data

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TokenBalance {
    pub asset: AssetId,
    pub amount: Amount,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// Solana: the token account holding it (owner-level balances sum all accounts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_account: Option<AccountAddress>,
}

/// Native + token balances of one owner on the port's chain.
#[async_trait]
pub trait TokenBalances: Send + Sync {
    /// `assets = None` means "everything the source knows"; `Some` restricts (and lets
    /// `rpc` implementations use Multicall3 on a known list).
    async fn balances(
        &self,
        owner: &AccountAddress,
        assets: Option<&[AssetId]>,
    ) -> PortResult<Vec<TokenBalance>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    In,
    Out,
    #[default]
    Both,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TransferQuery {
    pub owner: AccountAddress,
    #[serde(default)]
    pub direction: Direction,
    /// Restrict to these assets (payments pass the canonical stablecoin list).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assets: Option<Vec<AssetId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_block: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_block: Option<u64>,
    /// Opaque cursor from a previous page (vendor-specific; do not parse).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Page<T> {
    pub items: Vec<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[async_trait]
pub trait TransferHistory: Send + Sync {
    async fn transfers(&self, query: &TransferQuery) -> PortResult<Page<Transfer>>;
}

// ------------------------------------------------------------------ transactions

#[async_trait]
pub trait FeeOracle: Send + Sync {
    async fn fee_estimate(&self) -> PortResult<FeeEstimate>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SimulationResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// EVM gas used or Solana compute units consumed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub units_consumed: Option<u64>,
    #[serde(default)]
    pub balance_changes: Vec<BalanceDelta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub logs: Vec<String>,
}

#[async_trait]
pub trait Simulator: Send + Sync {
    async fn simulate(
        &self,
        from: &AccountAddress,
        tx: &UnsignedTx,
    ) -> PortResult<SimulationResult>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BroadcastReceipt {
    /// Hash / signature computed from the signed payload (identical across providers).
    pub tx_hash: String,
    /// Private relay or public mempool.
    pub private: bool,
}

/// Sends an already-signed transaction (0x-hex for EVM, base64 for Solana). Used both for the
/// public `broadcast` capability (fan-out) and `private_relay` (Flashbots, MEV Blocker, Jito…).
#[async_trait]
pub trait Broadcaster: Send + Sync {
    async fn send_raw(&self, signed: &str) -> PortResult<BroadcastReceipt>;
}

// ------------------------------------------------------------------ market

#[async_trait]
pub trait PriceFeed: Send + Sync {
    /// Current price of `asset` in `currency` (ISO 4217, usually "USD").
    async fn price(&self, asset: &AssetId, currency: &str) -> PortResult<Price>;
}

#[async_trait]
pub trait PriceHistory: Send + Sync {
    async fn price_at(
        &self,
        asset: &AssetId,
        currency: &str,
        at: DateTime<Utc>,
    ) -> PortResult<Price>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TokenInfo {
    pub asset: AssetId,
    pub decimals: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logo_url: Option<String>,
    /// Source-specific "verified/strict list" flag, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
    pub source: String,
}

#[async_trait]
pub trait TokenMetadata: Send + Sync {
    async fn metadata(&self, asset: &AssetId) -> PortResult<TokenInfo>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RiskAssessment {
    pub source: String,
    pub flags: Vec<RiskFlag>,
}

#[async_trait]
pub trait TokenRisk: Send + Sync {
    async fn assess(&self, asset: &AssetId) -> PortResult<RiskAssessment>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SwapRequest {
    pub chain: ChainId,
    pub sell_asset: AssetId,
    pub buy_asset: AssetId,
    pub sell_amount: Amount,
    pub slippage_bps: u32,
    /// Required to build a transaction; optional for indicative quotes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taker: Option<AccountAddress>,
}

#[async_trait]
pub trait SwapQuoter: Send + Sync {
    /// Indicative or firm quote without a transaction.
    async fn quote(&self, req: &SwapRequest) -> PortResult<SwapQuote>;
    /// Firm quote with an unsigned transaction and required approvals (needs `taker`).
    async fn build(&self, req: &SwapRequest) -> PortResult<SwapQuote>;
}

// ------------------------------------------------------------------ compliance / fiat

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScreenResult {
    pub sanctioned: bool,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub as_of: DateTime<Utc>,
}

#[async_trait]
pub trait SanctionsScreener: Send + Sync {
    async fn screen(&self, account: &AccountId) -> PortResult<ScreenResult>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FxRate {
    pub base: String,
    pub quote: String,
    #[serde(with = "bdm_domain::serde_str")]
    #[schemars(with = "String")]
    pub rate: Decimal,
    /// Business date the rate belongs to (ECB publishes on business days only).
    pub business_date: NaiveDate,
    pub source: String,
    pub as_of: DateTime<Utc>,
}

#[async_trait]
pub trait FxRates: Send + Sync {
    /// `date = None` means latest.
    async fn rate(&self, base: &str, quote: &str, date: Option<NaiveDate>) -> PortResult<FxRate>;
}
