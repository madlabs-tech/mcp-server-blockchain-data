//! Pre-refactor tools kept for one release with identical names, inputs and output shapes
//! (locked by `crates/server/tests/legacy_tools.rs`). They render bare JSON (no envelope) and
//! surface failures as protocol errors, exactly like the old server.

use super::chain::rpc_meta;
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
        Ok(OpOutput::new(
            BalanceOut {
                address: address.to_string(),
                chain: chain.name.clone(),
                balance_wei: quantity(&v)?.to_string(),
                symbol: chain.native.symbol.clone(),
                decimals: chain.native.decimals,
            },
            rpc_meta(&chain.id),
        ))
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
        Ok(OpOutput::new(
            CodeOut {
                address: address.to_string(),
                chain: chain.name.clone(),
                is_contract: size > 0,
                bytecode_size: size,
            },
            rpc_meta(&chain.id),
        ))
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
        Ok(OpOutput::new(
            GasOut {
                chain: chain.name.clone(),
                gas_price_wei: wei.to_string(),
                gas_price_gwei: gwei_2dp(wei),
                timestamp: chrono::Utc::now().to_rfc3339(),
            },
            rpc_meta(&chain.id),
        ))
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
        let transaction = if raw.is_null() {
            Value::Null
        } else {
            legacy_tx_json(raw)
                .map_err(|e| DomainError::internal(format!("Failed to get transaction: {e}")))?
        };
        Ok(OpOutput::new(
            TxOut {
                chain: chain.name.clone(),
                transaction,
            },
            rpc_meta(&chain.id),
        ))
    }
}

/// Round-trip through alloy's RPC type to keep the exact pre-refactor serialization.
fn legacy_tx_json(raw: Value) -> Result<Value, String> {
    let tx: alloy_rpc_types_eth::Transaction =
        serde_json::from_value(raw).map_err(|e| e.to_string())?;
    serde_json::to_value(tx).map_err(|e| e.to_string())
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

    const HASH: &str = "0x88df016429689c079f3b2f6ad39fa052532c56795b733da78a91ebe6a713944b";
    const BLOCK: &str = "0x1d59ff54b1eb26b013ce3cb5fc9dab3705b415a67127a003c3e61eb445bb8df2";
    const FROM: &str = "0xd8da6bf26964af9d7eed9e03e53415d37aa96045";
    const TO: &str = "0xdac17f958d2ee523a2206206994597c13d831ec7";
    const R: &str = "0x1b5e176d927f8e9ab405058b2d2457392da3e20f328b16ddabcebc33eaac5fea";
    const S: &str = "0x4ba69724e8f69de52f0125ad8b3c5c2cef33019bac3249e2c0a2192766d1721c";
    const OVER_U128: &str = "0x100000000000000000000000000000000";
    const KEY: &str = "0x0000000000000000000000000000000000000000000000000000000000000003";

    /// Raw `eth_getTransactionByHash` results: one per tx type plus quirks nodes emit.
    fn tx_cases() -> Vec<(&'static str, Value)> {
        let mined = json!({
            "blockHash": BLOCK, "blockNumber": "0x5daf3b", "transactionIndex": "0x41",
            "hash": HASH, "from": FROM, "nonce": "0x15", "gas": "0x5208", "input": "0x",
            "value": "0xf3dbb76162000", "r": R, "s": S
        });
        let with = |extra: Value| {
            let mut v = mined.clone();
            for (k, x) in extra.as_object().unwrap() {
                v[k] = x.clone();
            }
            v
        };
        vec![
            (
                "legacy_eip155",
                with(json!({
                    "type": "0x0", "chainId": "0x1", "gasPrice": "0x4a817c800", "to": TO,
                    "input": "0xa9059cbb", "v": "0x25"
                })),
            ),
            (
                "legacy_untyped_pre155_drops_chain_id",
                with(json!({
                    "chainId": "0x1", "gasPrice": "0x4a817c800", "to": TO, "v": "0x1c"
                })),
            ),
            (
                "legacy_chain_id_from_v",
                with(json!({
                    "type": "0x0", "gasPrice": "0x4a817c800", "to": null, "v": "0x26"
                })),
            ),
            (
                "legacy_system_tx_zero_sig",
                with(json!({
                    "type": "0x0", "chainId": "0xa", "gasPrice": "0x0", "to": TO,
                    "v": "0x0", "r": "0x0", "s": "0x0"
                })),
            ),
            (
                "eip2930",
                with(json!({
                    "type": "0x1", "chainId": "0x1", "gasPrice": "0x4a817c800", "to": null,
                    "accessList": [{"address": TO, "storageKeys": [KEY]}],
                    "v": "0x0", "yParity": "0x0"
                })),
            ),
            (
                "eip1559",
                with(json!({
                    "type": "0x2", "chainId": "0x1", "gasPrice": "0x4a817c800",
                    "maxFeePerGas": "0x4a817c800", "maxPriorityFeePerGas": "0x3b9aca00", "to": TO,
                    "accessList": [], "v": "0x1", "yParity": "0x1"
                })),
            ),
            (
                "eip1559_pending_quirks",
                json!({
                    "type": "0x02", "hash": HASH, "from": FROM, "nonce": 21, "gasLimit": "0x05208",
                    "input": "0xA9059CBB", "value": "1000", "chainId": "0x2105",
                    "maxFeePerGas": "0x4A817C800", "maxPriorityFeePerGas": "0x0",
                    "to": "0xDAC17F958D2EE523A2206206994597C13D831EC7", "accessList": null,
                    "v": "0x26", "r": "0x001b", "s": S, "blockHash": null,
                    "sourceHash": BLOCK, "l1Fee": "0x10"
                }),
            ),
            (
                "eip4844",
                with(json!({
                    "type": "0x3", "chainId": "0x1", "gasPrice": "0x4a817c800",
                    "maxFeePerGas": "0x4a817c800", "maxPriorityFeePerGas": "0x3b9aca00", "to": TO,
                    "accessList": [], "maxFeePerBlobGas": "0x1",
                    "blobVersionedHashes": [KEY], "v": "0x1", "yParity": "0x1"
                })),
            ),
            (
                "eip7702",
                with(json!({
                    "type": "0x4", "chainId": "0x1", "gasPrice": "0x4a817c800",
                    "maxFeePerGas": "0x4a817c800", "maxPriorityFeePerGas": "0x3b9aca00", "to": FROM,
                    "accessList": [],
                    "authorizationList": [
                        {"chainId": "0x1", "address": TO, "nonce": "0x2", "yParity": "0x1", "r": R, "s": S},
                        {"chainId": "0x0", "address": TO, "nonce": "0x0", "v": "0x0", "r": R, "s": S}
                    ],
                    "v": "0x0", "yParity": "0x0"
                })),
            ),
            (
                "legacy_system_tx_no_chain_id",
                with(json!({"gasPrice": "0x0", "to": TO, "v": "0x0", "r": "0x0", "s": "0x0"})),
            ),
            (
                "legacy_uppercase_tag",
                with(json!({"type": "0X0", "gasPrice": "0x1", "to": TO, "v": "0x25"})),
            ),
            (
                "legacy_null_type",
                with(json!({"type": null, "gasPrice": "0x1", "to": TO, "v": "0x1b"})),
            ),
            (
                "eip4844_with_sidecar",
                with(json!({
                    "type": "0x3", "chainId": "0x1", "maxFeePerGas": "0x2",
                    "maxPriorityFeePerGas": "0x1", "to": TO, "accessList": [],
                    "maxFeePerBlobGas": "0x1", "blobVersionedHashes": [KEY],
                    "blobs": [], "commitments": [format!("0x{}", "AB".repeat(48))],
                    "proofs": [format!("0x{}", "cd".repeat(48))], "yParity": "0x1"
                })),
            ),
            (
                "eip4844_invalid_sidecar_is_dropped",
                with(json!({
                    "type": "0x3", "chainId": "0x1", "maxFeePerGas": "0x2",
                    "maxPriorityFeePerGas": "0x1", "to": TO, "accessList": [],
                    "maxFeePerBlobGas": "0x1", "blobVersionedHashes": [],
                    "blobs": ["0x00"], "commitments": [], "proofs": [], "yParity": "0x1"
                })),
            ),
        ]
    }

    /// Inputs the alloy round-trip rejected; they must stay errors.
    fn bad_tx_cases() -> Vec<(&'static str, Value)> {
        let base = json!({
            "type": "0x2", "hash": HASH, "from": FROM, "nonce": "0x1", "gas": "0x5208",
            "input": "0x", "value": "0x0", "chainId": "0x1", "maxFeePerGas": "0x1",
            "maxPriorityFeePerGas": "0x1", "to": TO, "accessList": [], "yParity": "0x1",
            "r": R, "s": S
        });
        let with = |k: &str, x: Value| {
            let mut v = base.clone();
            v[k] = x;
            v
        };
        let without = |k: &str| {
            let mut v = base.clone();
            v.as_object_mut().unwrap().remove(k);
            v
        };
        let legacy = json!({
            "hash": HASH, "from": FROM, "nonce": "0x1", "gas": "0x5208", "input": "0x",
            "value": "0x0", "gasPrice": "0x1", "to": TO, "v": "0x25", "r": R, "s": S
        });
        vec![
            ("unknown_type", with("type", json!("0x7e"))),
            ("numeric_type", with("type", json!(2))),
            ("missing_hash", without("hash")),
            ("missing_from", without("from")),
            ("missing_nonce", without("nonce")),
            ("missing_access_list", without("accessList")),
            ("bad_y_parity", with("yParity", json!("0x2"))),
            ("no_signature_parity", without("yParity")),
            (
                "nonce_overflow",
                with("nonce", json!("0x10000000000000000")),
            ),
            ("gas_price_overflow", with("gasPrice", json!(OVER_U128))),
            ("legacy_gas_price_overflow", {
                let mut v = legacy.clone();
                v["gasPrice"] = json!(OVER_U128);
                v
            }),
            ("bad_hex", with("input", json!("0xzz"))),
            ("short_hash", with("hash", json!("0x1234"))),
            ("float_value", with("value", json!(1.5))),
            ("null_type_legacy_without_v", {
                let mut v = legacy.clone();
                v["type"] = Value::Null;
                v.as_object_mut().unwrap().remove("v");
                v
            }),
            ("blob_tx_create", {
                let mut v = with("type", json!("0x3"));
                v["to"] = Value::Null;
                v["maxFeePerBlobGas"] = json!("0x1");
                v["blobVersionedHashes"] = json!([]);
                v
            }),
            ("legacy_chain_id_mismatch", {
                let mut v = legacy.clone();
                v["chainId"] = json!("0x5");
                v
            }),
            ("legacy_bad_v", {
                let mut v = legacy.clone();
                v["v"] = json!("0x1d");
                v
            }),
        ]
    }

    /// Captured from the alloy 1.0.41 `alloy_rpc_types_eth::Transaction` round-trip.
    const EXPECTED: &[(&str, &str)] = &[
        (
            "legacy_eip155",
            r#"{"blockHash":"0x1d59ff54b1eb26b013ce3cb5fc9dab3705b415a67127a003c3e61eb445bb8df2","blockNumber":"0x5daf3b","chainId":"0x1","from":"0xd8da6bf26964af9d7eed9e03e53415d37aa96045","gas":"0x5208","gasPrice":"0x4a817c800","hash":"0x88df016429689c079f3b2f6ad39fa052532c56795b733da78a91ebe6a713944b","input":"0xa9059cbb","nonce":"0x15","r":"0x1b5e176d927f8e9ab405058b2d2457392da3e20f328b16ddabcebc33eaac5fea","s":"0x4ba69724e8f69de52f0125ad8b3c5c2cef33019bac3249e2c0a2192766d1721c","to":"0xdac17f958d2ee523a2206206994597c13d831ec7","transactionIndex":"0x41","type":"0x0","v":"0x25","value":"0xf3dbb76162000"}"#,
        ),
        (
            "legacy_untyped_pre155_drops_chain_id",
            r#"{"blockHash":"0x1d59ff54b1eb26b013ce3cb5fc9dab3705b415a67127a003c3e61eb445bb8df2","blockNumber":"0x5daf3b","from":"0xd8da6bf26964af9d7eed9e03e53415d37aa96045","gas":"0x5208","gasPrice":"0x4a817c800","hash":"0x88df016429689c079f3b2f6ad39fa052532c56795b733da78a91ebe6a713944b","input":"0x","nonce":"0x15","r":"0x1b5e176d927f8e9ab405058b2d2457392da3e20f328b16ddabcebc33eaac5fea","s":"0x4ba69724e8f69de52f0125ad8b3c5c2cef33019bac3249e2c0a2192766d1721c","to":"0xdac17f958d2ee523a2206206994597c13d831ec7","transactionIndex":"0x41","type":"0x0","v":"0x1c","value":"0xf3dbb76162000"}"#,
        ),
        (
            "legacy_chain_id_from_v",
            r#"{"blockHash":"0x1d59ff54b1eb26b013ce3cb5fc9dab3705b415a67127a003c3e61eb445bb8df2","blockNumber":"0x5daf3b","chainId":"0x1","from":"0xd8da6bf26964af9d7eed9e03e53415d37aa96045","gas":"0x5208","gasPrice":"0x4a817c800","hash":"0x88df016429689c079f3b2f6ad39fa052532c56795b733da78a91ebe6a713944b","input":"0x","nonce":"0x15","r":"0x1b5e176d927f8e9ab405058b2d2457392da3e20f328b16ddabcebc33eaac5fea","s":"0x4ba69724e8f69de52f0125ad8b3c5c2cef33019bac3249e2c0a2192766d1721c","to":null,"transactionIndex":"0x41","type":"0x0","v":"0x26","value":"0xf3dbb76162000"}"#,
        ),
        (
            "legacy_system_tx_zero_sig",
            r#"{"blockHash":"0x1d59ff54b1eb26b013ce3cb5fc9dab3705b415a67127a003c3e61eb445bb8df2","blockNumber":"0x5daf3b","chainId":"0xa","from":"0xd8da6bf26964af9d7eed9e03e53415d37aa96045","gas":"0x5208","gasPrice":"0x0","hash":"0x88df016429689c079f3b2f6ad39fa052532c56795b733da78a91ebe6a713944b","input":"0x","nonce":"0x15","r":"0x0","s":"0x0","to":"0xdac17f958d2ee523a2206206994597c13d831ec7","transactionIndex":"0x41","type":"0x0","v":"0x37","value":"0xf3dbb76162000"}"#,
        ),
        (
            "eip2930",
            r#"{"accessList":[{"address":"0xdac17f958d2ee523a2206206994597c13d831ec7","storageKeys":["0x0000000000000000000000000000000000000000000000000000000000000003"]}],"blockHash":"0x1d59ff54b1eb26b013ce3cb5fc9dab3705b415a67127a003c3e61eb445bb8df2","blockNumber":"0x5daf3b","chainId":"0x1","from":"0xd8da6bf26964af9d7eed9e03e53415d37aa96045","gas":"0x5208","gasPrice":"0x4a817c800","hash":"0x88df016429689c079f3b2f6ad39fa052532c56795b733da78a91ebe6a713944b","input":"0x","nonce":"0x15","r":"0x1b5e176d927f8e9ab405058b2d2457392da3e20f328b16ddabcebc33eaac5fea","s":"0x4ba69724e8f69de52f0125ad8b3c5c2cef33019bac3249e2c0a2192766d1721c","to":null,"transactionIndex":"0x41","type":"0x1","v":"0x0","value":"0xf3dbb76162000","yParity":"0x0"}"#,
        ),
        (
            "eip1559",
            r#"{"accessList":[],"blockHash":"0x1d59ff54b1eb26b013ce3cb5fc9dab3705b415a67127a003c3e61eb445bb8df2","blockNumber":"0x5daf3b","chainId":"0x1","from":"0xd8da6bf26964af9d7eed9e03e53415d37aa96045","gas":"0x5208","gasPrice":"0x4a817c800","hash":"0x88df016429689c079f3b2f6ad39fa052532c56795b733da78a91ebe6a713944b","input":"0x","maxFeePerGas":"0x4a817c800","maxPriorityFeePerGas":"0x3b9aca00","nonce":"0x15","r":"0x1b5e176d927f8e9ab405058b2d2457392da3e20f328b16ddabcebc33eaac5fea","s":"0x4ba69724e8f69de52f0125ad8b3c5c2cef33019bac3249e2c0a2192766d1721c","to":"0xdac17f958d2ee523a2206206994597c13d831ec7","transactionIndex":"0x41","type":"0x2","v":"0x1","value":"0xf3dbb76162000","yParity":"0x1"}"#,
        ),
        (
            "eip1559_pending_quirks",
            r#"{"accessList":[],"blockHash":null,"blockNumber":null,"chainId":"0x2105","from":"0xd8da6bf26964af9d7eed9e03e53415d37aa96045","gas":"0x5208","hash":"0x88df016429689c079f3b2f6ad39fa052532c56795b733da78a91ebe6a713944b","input":"0xa9059cbb","maxFeePerGas":"0x4a817c800","maxPriorityFeePerGas":"0x0","nonce":"0x15","r":"0x1b","s":"0x4ba69724e8f69de52f0125ad8b3c5c2cef33019bac3249e2c0a2192766d1721c","to":"0xdac17f958d2ee523a2206206994597c13d831ec7","transactionIndex":null,"type":"0x2","v":"0x1","value":"0x3e8","yParity":"0x1"}"#,
        ),
        (
            "eip4844",
            r#"{"accessList":[],"blobVersionedHashes":["0x0000000000000000000000000000000000000000000000000000000000000003"],"blockHash":"0x1d59ff54b1eb26b013ce3cb5fc9dab3705b415a67127a003c3e61eb445bb8df2","blockNumber":"0x5daf3b","chainId":"0x1","from":"0xd8da6bf26964af9d7eed9e03e53415d37aa96045","gas":"0x5208","gasPrice":"0x4a817c800","hash":"0x88df016429689c079f3b2f6ad39fa052532c56795b733da78a91ebe6a713944b","input":"0x","maxFeePerBlobGas":"0x1","maxFeePerGas":"0x4a817c800","maxPriorityFeePerGas":"0x3b9aca00","nonce":"0x15","r":"0x1b5e176d927f8e9ab405058b2d2457392da3e20f328b16ddabcebc33eaac5fea","s":"0x4ba69724e8f69de52f0125ad8b3c5c2cef33019bac3249e2c0a2192766d1721c","to":"0xdac17f958d2ee523a2206206994597c13d831ec7","transactionIndex":"0x41","type":"0x3","v":"0x1","value":"0xf3dbb76162000","yParity":"0x1"}"#,
        ),
        (
            "eip7702",
            r#"{"accessList":[],"authorizationList":[{"address":"0xdac17f958d2ee523a2206206994597c13d831ec7","chainId":"0x1","nonce":"0x2","r":"0x1b5e176d927f8e9ab405058b2d2457392da3e20f328b16ddabcebc33eaac5fea","s":"0x4ba69724e8f69de52f0125ad8b3c5c2cef33019bac3249e2c0a2192766d1721c","yParity":"0x1"},{"address":"0xdac17f958d2ee523a2206206994597c13d831ec7","chainId":"0x0","nonce":"0x0","r":"0x1b5e176d927f8e9ab405058b2d2457392da3e20f328b16ddabcebc33eaac5fea","s":"0x4ba69724e8f69de52f0125ad8b3c5c2cef33019bac3249e2c0a2192766d1721c","yParity":"0x0"}],"blockHash":"0x1d59ff54b1eb26b013ce3cb5fc9dab3705b415a67127a003c3e61eb445bb8df2","blockNumber":"0x5daf3b","chainId":"0x1","from":"0xd8da6bf26964af9d7eed9e03e53415d37aa96045","gas":"0x5208","gasPrice":"0x4a817c800","hash":"0x88df016429689c079f3b2f6ad39fa052532c56795b733da78a91ebe6a713944b","input":"0x","maxFeePerGas":"0x4a817c800","maxPriorityFeePerGas":"0x3b9aca00","nonce":"0x15","r":"0x1b5e176d927f8e9ab405058b2d2457392da3e20f328b16ddabcebc33eaac5fea","s":"0x4ba69724e8f69de52f0125ad8b3c5c2cef33019bac3249e2c0a2192766d1721c","to":"0xd8da6bf26964af9d7eed9e03e53415d37aa96045","transactionIndex":"0x41","type":"0x4","v":"0x0","value":"0xf3dbb76162000","yParity":"0x0"}"#,
        ),
        (
            "legacy_system_tx_no_chain_id",
            r#"{"blockHash":"0x1d59ff54b1eb26b013ce3cb5fc9dab3705b415a67127a003c3e61eb445bb8df2","blockNumber":"0x5daf3b","from":"0xd8da6bf26964af9d7eed9e03e53415d37aa96045","gas":"0x5208","gasPrice":"0x0","hash":"0x88df016429689c079f3b2f6ad39fa052532c56795b733da78a91ebe6a713944b","input":"0x","nonce":"0x15","r":"0x0","s":"0x0","to":"0xdac17f958d2ee523a2206206994597c13d831ec7","transactionIndex":"0x41","type":"0x0","v":"0x1b","value":"0xf3dbb76162000"}"#,
        ),
        (
            "legacy_uppercase_tag",
            r#"{"blockHash":"0x1d59ff54b1eb26b013ce3cb5fc9dab3705b415a67127a003c3e61eb445bb8df2","blockNumber":"0x5daf3b","chainId":"0x1","from":"0xd8da6bf26964af9d7eed9e03e53415d37aa96045","gas":"0x5208","gasPrice":"0x1","hash":"0x88df016429689c079f3b2f6ad39fa052532c56795b733da78a91ebe6a713944b","input":"0x","nonce":"0x15","r":"0x1b5e176d927f8e9ab405058b2d2457392da3e20f328b16ddabcebc33eaac5fea","s":"0x4ba69724e8f69de52f0125ad8b3c5c2cef33019bac3249e2c0a2192766d1721c","to":"0xdac17f958d2ee523a2206206994597c13d831ec7","transactionIndex":"0x41","type":"0x0","v":"0x25","value":"0xf3dbb76162000"}"#,
        ),
        (
            "legacy_null_type",
            r#"{"blockHash":"0x1d59ff54b1eb26b013ce3cb5fc9dab3705b415a67127a003c3e61eb445bb8df2","blockNumber":"0x5daf3b","from":"0xd8da6bf26964af9d7eed9e03e53415d37aa96045","gas":"0x5208","gasPrice":"0x1","hash":"0x88df016429689c079f3b2f6ad39fa052532c56795b733da78a91ebe6a713944b","input":"0x","nonce":"0x15","r":"0x1b5e176d927f8e9ab405058b2d2457392da3e20f328b16ddabcebc33eaac5fea","s":"0x4ba69724e8f69de52f0125ad8b3c5c2cef33019bac3249e2c0a2192766d1721c","to":"0xdac17f958d2ee523a2206206994597c13d831ec7","transactionIndex":"0x41","type":"0x0","v":"0x1b","value":"0xf3dbb76162000"}"#,
        ),
        (
            "eip4844_with_sidecar",
            r#"{"accessList":[],"blobVersionedHashes":["0x0000000000000000000000000000000000000000000000000000000000000003"],"blobs":[],"blockHash":"0x1d59ff54b1eb26b013ce3cb5fc9dab3705b415a67127a003c3e61eb445bb8df2","blockNumber":"0x5daf3b","chainId":"0x1","commitments":["0xabababababababababababababababababababababababababababababababababababababababababababababababab"],"from":"0xd8da6bf26964af9d7eed9e03e53415d37aa96045","gas":"0x5208","hash":"0x88df016429689c079f3b2f6ad39fa052532c56795b733da78a91ebe6a713944b","input":"0x","maxFeePerBlobGas":"0x1","maxFeePerGas":"0x2","maxPriorityFeePerGas":"0x1","nonce":"0x15","proofs":["0xcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"],"r":"0x1b5e176d927f8e9ab405058b2d2457392da3e20f328b16ddabcebc33eaac5fea","s":"0x4ba69724e8f69de52f0125ad8b3c5c2cef33019bac3249e2c0a2192766d1721c","to":"0xdac17f958d2ee523a2206206994597c13d831ec7","transactionIndex":"0x41","type":"0x3","v":"0x1","value":"0xf3dbb76162000","yParity":"0x1"}"#,
        ),
        (
            "eip4844_invalid_sidecar_is_dropped",
            r#"{"accessList":[],"blobVersionedHashes":[],"blockHash":"0x1d59ff54b1eb26b013ce3cb5fc9dab3705b415a67127a003c3e61eb445bb8df2","blockNumber":"0x5daf3b","chainId":"0x1","from":"0xd8da6bf26964af9d7eed9e03e53415d37aa96045","gas":"0x5208","hash":"0x88df016429689c079f3b2f6ad39fa052532c56795b733da78a91ebe6a713944b","input":"0x","maxFeePerBlobGas":"0x1","maxFeePerGas":"0x2","maxPriorityFeePerGas":"0x1","nonce":"0x15","r":"0x1b5e176d927f8e9ab405058b2d2457392da3e20f328b16ddabcebc33eaac5fea","s":"0x4ba69724e8f69de52f0125ad8b3c5c2cef33019bac3249e2c0a2192766d1721c","to":"0xdac17f958d2ee523a2206206994597c13d831ec7","transactionIndex":"0x41","type":"0x3","v":"0x1","value":"0xf3dbb76162000","yParity":"0x1"}"#,
        ),
    ];

    #[test]
    fn transaction_output_is_pinned_for_every_tx_type() {
        let cases = tx_cases();
        assert_eq!(cases.len(), EXPECTED.len());
        for ((name, raw), (want_name, want)) in cases.into_iter().zip(EXPECTED) {
            assert_eq!(name, *want_name);
            let got = serde_json::to_string(&legacy_tx_json(raw).unwrap()).unwrap();
            assert_eq!(got, *want, "{name}");
        }
    }

    #[test]
    fn malformed_transactions_are_errors() {
        for (name, raw) in bad_tx_cases() {
            assert!(legacy_tx_json(raw).is_err(), "{name}");
        }
    }
}
