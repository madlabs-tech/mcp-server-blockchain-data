//! Pre-refactor tools kept for one release with identical names, inputs and output shapes
//! (locked by `crates/server/tests/legacy_tools.rs`). They render bare JSON (no envelope) and
//! surface failures as protocol errors, exactly like the old server.

use crate::{Catalog, Ctx, Domain, OpOutput, Operation, Profile};
use alloy_primitives::{Address, B256, U256};
use async_trait::async_trait;
use bdm_config::ChainEntry;
use bdm_domain::{ChainFamily, DomainError};
use bdm_ports::{EvmRpc, ProviderError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::str::FromStr;

pub fn register(c: &mut Catalog) {
    c.register(EthGetBalance);
    c.register(EthGetCode);
    c.register(EthGasPrice);
    c.register(EthGetTransactionByHash);
}

const ALL: &[Profile] = Profile::ALL;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddressOnChain {
    /// The Ethereum address to check
    pub address: String,
    /// The chain to check (ethereum, base, arbitrum, avalanche, bsc)
    pub chain: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ChainOnly {
    /// The chain to check (ethereum, base, arbitrum, avalanche, bsc)
    pub chain: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TxByHash {
    /// Transaction hash (0x-prefixed, 32 bytes)
    pub hash: String,
    /// The chain to check (ethereum, base, arbitrum, avalanche, bsc)
    pub chain: String,
}

fn evm_chain<'a>(ctx: &'a Ctx, chain: &str) -> Result<&'a ChainEntry, DomainError> {
    let c = ctx.chain(chain)?;
    if c.family != ChainFamily::Evm {
        return Err(DomainError::invalid(format!("Unsupported chain: {chain}")));
    }
    Ok(c)
}

fn parse_address(s: &str) -> Result<Address, DomainError> {
    Address::from_str(s).map_err(|e| DomainError::invalid(e.to_string()))
}

fn quantity(v: &Value) -> Result<U256, DomainError> {
    let s = v
        .as_str()
        .ok_or_else(|| DomainError::internal(format!("expected hex quantity, got {v}")))?;
    U256::from_str_radix(s.trim_start_matches("0x"), 16)
        .map_err(|e| DomainError::internal(e.to_string()))
}

fn failed(what: &str) -> impl Fn(ProviderError) -> DomainError + '_ {
    move |e| {
        let mut d = DomainError::from(e);
        d.message = format!("Failed to {what}: {}", d.message);
        d
    }
}

async fn rpc_call(
    ctx: &Ctx,
    chain: &ChainEntry,
    method: &str,
    params: Value,
    what: &str,
) -> Result<Value, DomainError> {
    ctx.evm_rpc(chain)?
        .request(method, params)
        .await
        .map_err(failed(what))
}

// ------------------------------------------------------------------ eth_get_balance

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BalanceOut {
    pub address: String,
    pub chain: String,
    pub balance_wei: String,
    pub symbol: String,
    pub decimals: u8,
}

pub struct EthGetBalance;

#[async_trait]
impl Operation for EthGetBalance {
    type Input = AddressOnChain;
    type Output = BalanceOut;
    const NAME: &'static str = "eth_get_balance";
    const DOMAIN: Domain = Domain::Legacy;
    const DESCRIPTION: &'static str =
        "Get the ETH/native token balance of an address. Legacy alias: prefer wallet_get_balances.";
    const PROFILES: &'static [Profile] = ALL;
    const LEGACY: bool = true;

    async fn execute(
        &self,
        ctx: &Ctx,
        input: AddressOnChain,
    ) -> Result<OpOutput<BalanceOut>, DomainError> {
        let chain = evm_chain(ctx, &input.chain)?;
        let address = parse_address(&input.address)?;
        let v = rpc_call(
            ctx,
            chain,
            "eth_getBalance",
            json!([address, "latest"]),
            "get balance",
        )
        .await?;
        Ok(OpOutput::local(BalanceOut {
            address: address.to_string(),
            chain: chain.name.clone(),
            balance_wei: quantity(&v)?.to_string(),
            symbol: chain.native.symbol.clone(),
            decimals: chain.native.decimals,
        }))
    }
}

// ------------------------------------------------------------------ eth_get_code

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CodeOut {
    pub address: String,
    pub chain: String,
    pub is_contract: bool,
    pub bytecode_size: usize,
}

pub struct EthGetCode;

#[async_trait]
impl Operation for EthGetCode {
    type Input = AddressOnChain;
    type Output = CodeOut;
    const NAME: &'static str = "eth_get_code";
    const DOMAIN: Domain = Domain::Legacy;
    const DESCRIPTION: &'static str =
        "Detect whether an address is a contract or wallet. Legacy alias: prefer address_validate.";
    const PROFILES: &'static [Profile] = ALL;
    const LEGACY: bool = true;

    async fn execute(
        &self,
        ctx: &Ctx,
        input: AddressOnChain,
    ) -> Result<OpOutput<CodeOut>, DomainError> {
        let chain = evm_chain(ctx, &input.chain)?;
        let address = parse_address(&input.address)?;
        let v = rpc_call(
            ctx,
            chain,
            "eth_getCode",
            json!([address, "latest"]),
            "get code",
        )
        .await?;
        let hex = v.as_str().unwrap_or("0x").trim_start_matches("0x");
        let size = hex.len() / 2;
        Ok(OpOutput::local(CodeOut {
            address: address.to_string(),
            chain: chain.name.clone(),
            is_contract: size > 0,
            bytecode_size: size,
        }))
    }
}

// ------------------------------------------------------------------ eth_gas_price

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GasOut {
    pub chain: String,
    pub gas_price_wei: String,
    pub gas_price_gwei: String,
    pub timestamp: String,
}

/// Exact wei → gwei with 2 decimals, rounding half up (no floating point).
fn gwei_2dp(wei: U256) -> String {
    let hundredths = (wei + U256::from(5_000_000u64)) / U256::from(10_000_000u64);
    let (int, frac) = (
        hundredths / U256::from(100u8),
        hundredths % U256::from(100u8),
    );
    format!("{int}.{:02}", frac.to::<u64>())
}

pub struct EthGasPrice;

#[async_trait]
impl Operation for EthGasPrice {
    type Input = ChainOnly;
    type Output = GasOut;
    const NAME: &'static str = "eth_gas_price";
    const DOMAIN: Domain = Domain::Legacy;
    const DESCRIPTION: &'static str =
        "Get the current gas price on the specified chain. Legacy alias: prefer tx_estimate_fee.";
    const PROFILES: &'static [Profile] = ALL;
    const LEGACY: bool = true;

    async fn execute(&self, ctx: &Ctx, input: ChainOnly) -> Result<OpOutput<GasOut>, DomainError> {
        let chain = evm_chain(ctx, &input.chain)?;
        let wei =
            quantity(&rpc_call(ctx, chain, "eth_gasPrice", json!([]), "get gas price").await?)?;
        Ok(OpOutput::local(GasOut {
            chain: chain.name.clone(),
            gas_price_wei: wei.to_string(),
            gas_price_gwei: gwei_2dp(wei),
            timestamp: chrono::Utc::now().to_rfc3339(),
        }))
    }
}

// ------------------------------------------------------------------ eth_get_transaction_by_hash

#[derive(Debug, Serialize, JsonSchema)]
pub struct TxOut {
    pub chain: String,
    /// Raw RPC transaction (null if unknown).
    pub transaction: Value,
}

pub struct EthGetTransactionByHash;

#[async_trait]
impl Operation for EthGetTransactionByHash {
    type Input = TxByHash;
    type Output = TxOut;
    const NAME: &'static str = "eth_get_transaction_by_hash";
    const DOMAIN: Domain = Domain::Legacy;
    const DESCRIPTION: &'static str =
        "Get transaction details by hash. Legacy alias: prefer tx_get.";
    const PROFILES: &'static [Profile] = ALL;
    const LEGACY: bool = true;

    async fn execute(&self, ctx: &Ctx, input: TxByHash) -> Result<OpOutput<TxOut>, DomainError> {
        let chain = evm_chain(ctx, &input.chain)?;
        let hash = B256::from_str(&input.hash).map_err(|e| DomainError::invalid(e.to_string()))?;
        let raw = rpc_call(
            ctx,
            chain,
            "eth_getTransactionByHash",
            json!([hash]),
            "get transaction",
        )
        .await?;
        // Round-trip through alloy's RPC type to keep the exact pre-refactor serialization.
        let transaction = if raw.is_null() {
            Value::Null
        } else {
            let tx: alloy_rpc_types_eth::Transaction = serde_json::from_value(raw)
                .map_err(|e| DomainError::internal(format!("Failed to get transaction: {e}")))?;
            serde_json::to_value(tx).map_err(|e| DomainError::internal(e.to_string()))?
        };
        Ok(OpOutput::local(TxOut {
            chain: chain.name.clone(),
            transaction,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gwei_formatting_is_exact() {
        assert_eq!(gwei_2dp(U256::from(20_000_000_000u64)), "20.00");
        assert_eq!(gwei_2dp(U256::from(1_234_567_890u64)), "1.23");
        assert_eq!(gwei_2dp(U256::from(1_235_000_000u64)), "1.24");
        assert_eq!(gwei_2dp(U256::from(0u8)), "0.00");
    }
}
