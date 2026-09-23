use crate::{AccountAddress, Amount, AssetId, ChainId, Fiat};
use alloy_primitives::U256;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Chain-agnostic finality. EVM maps block tags (`latest` → Confirmed(n), `safe`, `finalized`);
/// Solana maps commitment levels (`processed` → Pending, `confirmed` → Confirmed, `finalized`).
/// Ordered weakest → strongest so policies can compare (`finality >= Finality::Safe`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(tag = "level", rename_all = "snake_case")]
pub enum Finality {
    /// Cannot be determined from chain data (e.g. confidential transfer). Never treat as settled.
    Unverifiable,
    /// Seen (mempool / processed) but not yet included with any confirmation.
    Pending,
    /// Included; `confirmations` blocks on top (L2: only the sequencer's word).
    Confirmed { confirmations: u64 },
    /// EVM `safe` tag (L2: batch posted to L1).
    Safe,
    /// Irreversible under the chain's rules.
    Finalized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TxStatus {
    Pending,
    Success,
    Failed,
    /// Solana: blockhash expired (`lastValidBlockHeight` passed) / EVM: replaced or dropped.
    Dropped,
    NotFound,
}

/// Block (EVM) or slot (Solana) reference. `hash` is the proof used for quorum/reorg checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BlockRef {
    /// Block number (EVM) or slot (Solana).
    pub number: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransferKind {
    Native,
    Token,
    /// Native value moved by an internal call (EVM traces) or CPI.
    Internal,
}

/// One value movement. For payments, prefer [`BalanceDelta`]: the transfer amount can differ
/// from what the recipient received (fee-on-transfer, Token-2022 transfer fees).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Transfer {
    pub chain: ChainId,
    pub tx_hash: String,
    /// EVM log index or Solana (outer, inner) instruction position; part of the idempotency key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_index: Option<u64>,
    pub kind: TransferKind,
    pub asset: AssetId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<AccountAddress>,
    pub to: AccountAddress,
    pub amount: Amount,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<BlockRef>,
}

/// Balance change of one owner for one asset inside one transaction (source of truth for payments).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BalanceDelta {
    /// Owner wallet (Solana: the token account's owner, not the token account).
    pub owner: AccountAddress,
    pub asset: AssetId,
    pub before: Amount,
    pub after: Amount,
    /// Token-2022 transfer fee withheld at the recipient, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub withheld_fee: Option<Amount>,
}

impl BalanceDelta {
    /// Net amount received (`after - before`), `None` if the balance decreased.
    pub fn received(&self) -> Option<Amount> {
        self.after.checked_sub(&self.before).ok().flatten()
    }
}

/// Normalized transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Tx {
    pub chain: ChainId,
    pub hash: String,
    pub status: TxStatus,
    pub finality: Finality,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<BlockRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<AccountAddress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<AccountAddress>,
    /// Fee paid in the native asset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fee: Option<Amount>,
    #[serde(default)]
    pub transfers: Vec<Transfer>,
    #[serde(default)]
    pub balance_deltas: Vec<BalanceDelta>,
    /// Vendor-neutral raw RPC object when a caller asked for it (legacy alias uses this).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeeSpeed {
    Slow,
    Standard,
    Fast,
}

/// One fee tier. EVM fields are wei per gas; Solana uses micro-lamports per compute unit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FeeTier {
    pub speed: FeeSpeed,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "opt_u256")]
    #[schemars(with = "Option<String>")]
    pub max_fee_per_gas: Option<U256>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "opt_u256")]
    #[schemars(with = "Option<String>")]
    pub max_priority_fee_per_gas: Option<U256>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compute_unit_price_micro_lamports: Option<u64>,
    /// Estimated total for a simple transfer, in the native asset (incl. L1 data fee on L2s).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_total: Option<Amount>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_total_fiat: Option<Fiat>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FeeEstimate {
    pub chain: ChainId,
    pub tiers: Vec<FeeTier>,
    /// L2 L1-data component (OP-stack / Arbitrum), already included in `estimated_total`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub l1_data_fee: Option<Amount>,
    /// Suggested Jito tip (Solana), lamports as an Amount with 9 decimals.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tip: Option<Amount>,
    pub as_of: DateTime<Utc>,
}

/// Unsigned transaction for an external signer. We never hold keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "family", rename_all = "lowercase")]
pub enum UnsignedTx {
    Evm {
        chain_id: u64,
        to: String,
        /// 0x-prefixed calldata.
        data: String,
        /// Wei, decimal string.
        value: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        gas_limit: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_fee_per_gas: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_priority_fee_per_gas: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        nonce: Option<u64>,
    },
    Solana {
        /// Base64-encoded serialized message (legacy or v0) to be signed.
        message_base64: String,
        recent_blockhash: String,
        last_valid_block_height: u64,
    },
}

mod opt_u256 {
    use alloy_primitives::U256;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<U256>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(v) => s.collect_str(v),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<U256>, D::Error> {
        Option::<String>::deserialize(d)?
            .map(|s| U256::from_str_radix(&s, 10).map_err(serde::de::Error::custom))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finality_is_ordered_weak_to_strong() {
        assert!(Finality::Pending < Finality::Confirmed { confirmations: 1 });
        assert!(Finality::Confirmed { confirmations: 64 } < Finality::Safe);
        assert!(Finality::Safe < Finality::Finalized);
        assert!(Finality::Unverifiable < Finality::Pending);
        assert_eq!(
            serde_json::to_value(Finality::Confirmed { confirmations: 3 }).unwrap(),
            serde_json::json!({"level": "confirmed", "confirmations": 3})
        );
    }

    #[test]
    fn balance_delta_received() {
        let owner: AccountAddress = "0xd8da6bf26964af9d7eed9e03e53415d37aa96045"
            .parse()
            .unwrap();
        let asset: AssetId = "eip155:1/slip44:60".parse().unwrap();
        let d = BalanceDelta {
            owner,
            asset,
            before: Amount::from_u128(10, 6),
            after: Amount::from_u128(25, 6),
            withheld_fee: None,
        };
        assert_eq!(d.received().unwrap().raw, U256::from(15u8));
        let down = BalanceDelta {
            before: d.after,
            after: d.before,
            ..d
        };
        assert_eq!(down.received(), None);
    }
}
