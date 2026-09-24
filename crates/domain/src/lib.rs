#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )
)]
//! Pure domain types shared by every layer. No I/O, no vendor types.
//!
//! Identifiers follow the CAIP standards so the API is chain-agnostic:
//! [`ChainId`] (CAIP-2), [`AccountId`] (CAIP-10) and [`AssetId`] (CAIP-19).
//! Money is always an integer [`Amount`] in base units plus decimals: never `f64`.

mod amount;
mod chain;
mod error;
mod market;
mod provenance;
mod tx;

pub use amount::{Amount, Fiat};
pub use chain::{AccountAddress, AccountId, AssetId, AssetRef, ChainFamily, ChainId, SolanaPubkey};
pub use error::{DomainError, ErrorCode};
pub use market::{
    Price, PriceAggregate, PriceStatus, RiskFlag, RiskLevel, RiskReport, Severity, SwapQuote,
};
pub use provenance::{Attempt, AttemptOutcome, Provenance, SourceKind};
pub use tx::{
    BalanceDelta, BlockRef, FeeEstimate, FeeSpeed, FeeTier, Finality, Transfer, TransferKind, Tx,
    TxStatus, UnsignedTx,
};

/// Serde + JSON-schema helpers for types that travel as strings (U256, Decimal, addresses).
pub mod serde_str {
    use serde::{de::Error, Deserialize, Deserializer, Serializer};
    use std::{fmt::Display, str::FromStr};

    pub fn serialize<T: Display, S: Serializer>(v: &T, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(v)
    }

    pub fn deserialize<'de, T, D>(d: D) -> Result<T, D::Error>
    where
        T: FromStr,
        T::Err: Display,
        D: Deserializer<'de>,
    {
        let s = String::deserialize(d)?;
        s.parse().map_err(D::Error::custom)
    }
}
