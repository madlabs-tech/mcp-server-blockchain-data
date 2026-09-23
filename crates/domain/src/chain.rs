use crate::error::DomainError;
use alloy_primitives::Address;
use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, fmt, str::FromStr};

/// Chain family decides which ports and parsers apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ChainFamily {
    Evm,
    Solana,
}

/// CAIP-2 chain id, e.g. `eip155:8453` (Base) or `solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp`.
///
/// Friendly aliases (`base`, `solana`) are resolved by the chain registry in `ems-config`,
/// not here, so the alias table lives in one place (`registry/chains.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChainId {
    namespace: String,
    reference: String,
}

impl ChainId {
    pub fn new(namespace: &str, reference: &str) -> Result<Self, DomainError> {
        let ok_ns = (3..=8).contains(&namespace.len())
            && namespace
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        let ok_ref = (1..=32).contains(&reference.len())
            && reference
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if !ok_ns || !ok_ref {
            return Err(DomainError::invalid(format!(
                "invalid CAIP-2 chain id '{namespace}:{reference}'"
            )));
        }
        Ok(Self {
            namespace: namespace.to_owned(),
            reference: reference.to_owned(),
        })
    }

    pub fn evm(chain_id: u64) -> Self {
        Self {
            namespace: "eip155".into(),
            reference: chain_id.to_string(),
        }
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn reference(&self) -> &str {
        &self.reference
    }

    /// Family derived from the CAIP-2 namespace; `None` for namespaces we don't support.
    pub fn family(&self) -> Option<ChainFamily> {
        match self.namespace.as_str() {
            "eip155" => Some(ChainFamily::Evm),
            "solana" => Some(ChainFamily::Solana),
            _ => None,
        }
    }

    /// Numeric EIP-155 chain id for EVM chains.
    pub fn evm_chain_id(&self) -> Option<u64> {
        (self.namespace == "eip155")
            .then(|| self.reference.parse().ok())
            .flatten()
    }
}

impl fmt::Display for ChainId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.namespace, self.reference)
    }
}

impl FromStr for ChainId {
    type Err = DomainError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (ns, r) = s
            .split_once(':')
            .ok_or_else(|| DomainError::invalid(format!("'{s}' is not a CAIP-2 chain id")))?;
        Self::new(ns, r)
    }
}

/// Solana public key (32 bytes, base58 on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SolanaPubkey(pub [u8; 32]);

impl fmt::Display for SolanaPubkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&bs58::encode(self.0).into_string())
    }
}

impl FromStr for SolanaPubkey {
    type Err = DomainError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = bs58::decode(s)
            .into_vec()
            .map_err(|_| DomainError::invalid(format!("'{s}' is not valid base58")))?;
        let arr: [u8; 32] = bytes.try_into().map_err(|_| {
            DomainError::invalid(format!("'{s}' is not a 32-byte Solana public key"))
        })?;
        Ok(Self(arr))
    }
}

/// Chain-family-specific address. Displays as EIP-55 checksum (EVM) or base58 (Solana).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AccountAddress {
    Evm(Address),
    Solana(SolanaPubkey),
}

impl AccountAddress {
    pub fn parse(family: ChainFamily, s: &str) -> Result<Self, DomainError> {
        match family {
            ChainFamily::Evm => {
                let s = s.trim();
                if !(s.len() == 42 && s.starts_with("0x")) {
                    return Err(DomainError::invalid(format!(
                        "'{s}' is not a 0x-prefixed 20-byte address"
                    )));
                }
                // Accept all-lowercase/all-uppercase; reject a wrong mixed-case checksum.
                let addr = Address::from_str(s).map_err(|_| {
                    DomainError::invalid(format!("'{s}' is not a valid EVM address"))
                })?;
                let hex = &s[2..];
                let mixed = hex.chars().any(|c| c.is_ascii_lowercase())
                    && hex.chars().any(|c| c.is_ascii_uppercase());
                if mixed && addr.to_checksum(None) != s {
                    return Err(DomainError::invalid(format!(
                        "'{s}' has an invalid EIP-55 checksum"
                    )));
                }
                Ok(Self::Evm(addr))
            }
            ChainFamily::Solana => Ok(Self::Solana(s.trim().parse()?)),
        }
    }

    pub fn family(&self) -> ChainFamily {
        match self {
            Self::Evm(_) => ChainFamily::Evm,
            Self::Solana(_) => ChainFamily::Solana,
        }
    }
}

impl fmt::Display for AccountAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Evm(a) => f.write_str(&a.to_checksum(None)),
            Self::Solana(p) => p.fmt(f),
        }
    }
}

/// CAIP-10 account id: `<chain_id>:<address>`, e.g. `eip155:1:0xd8dA...6045`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountId {
    pub chain: ChainId,
    pub address: AccountAddress,
}

impl AccountId {
    pub fn new(chain: ChainId, address: AccountAddress) -> Result<Self, DomainError> {
        if chain.family() != Some(address.family()) {
            return Err(DomainError::invalid(format!(
                "address {address} does not belong to chain {chain}"
            )));
        }
        Ok(Self { chain, address })
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.chain, self.address)
    }
}

impl FromStr for AccountId {
    type Err = DomainError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (chain, addr) = s
            .rsplit_once(':')
            .ok_or_else(|| DomainError::invalid(format!("'{s}' is not a CAIP-10 account id")))?;
        let chain: ChainId = chain.parse()?;
        let family = chain
            .family()
            .ok_or_else(|| DomainError::invalid(format!("unsupported chain namespace in '{s}'")))?;
        Self::new(chain, AccountAddress::parse(family, addr)?)
    }
}

/// Asset reference inside a chain (the `<namespace>:<reference>` part of CAIP-19).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AssetRef {
    /// Native coin, identified by its SLIP-44 coin type (60 = ETH, 501 = SOL, ...).
    Native { slip44: u32 },
    /// ERC-20 contract.
    Erc20(Address),
    /// SPL / Token-2022 mint.
    SplToken(SolanaPubkey),
}

/// CAIP-19 asset id, e.g. `eip155:8453/erc20:0x8335...2913` or `solana:5eyk.../slip44:501`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AssetId {
    pub chain: ChainId,
    pub asset: AssetRef,
}

impl AssetId {
    pub fn native(chain: ChainId, slip44: u32) -> Self {
        Self {
            chain,
            asset: AssetRef::Native { slip44 },
        }
    }

    pub fn is_native(&self) -> bool {
        matches!(self.asset, AssetRef::Native { .. })
    }
}

impl fmt::Display for AssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.asset {
            AssetRef::Native { slip44 } => write!(f, "{}/slip44:{slip44}", self.chain),
            AssetRef::Erc20(a) => write!(f, "{}/erc20:{}", self.chain, a.to_checksum(None)),
            AssetRef::SplToken(m) => write!(f, "{}/token:{m}", self.chain),
        }
    }
}

impl FromStr for AssetId {
    type Err = DomainError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || DomainError::invalid(format!("'{s}' is not a supported CAIP-19 asset id"));
        let (chain, asset) = s.split_once('/').ok_or_else(bad)?;
        let chain: ChainId = chain.parse()?;
        let (ns, reference) = asset.split_once(':').ok_or_else(bad)?;
        let asset = match (chain.family(), ns) {
            (_, "slip44") => AssetRef::Native {
                slip44: reference.parse().map_err(|_| bad())?,
            },
            (Some(ChainFamily::Evm), "erc20") => {
                match AccountAddress::parse(ChainFamily::Evm, reference)? {
                    AccountAddress::Evm(a) => AssetRef::Erc20(a),
                    AccountAddress::Solana(_) => unreachable!(),
                }
            }
            (Some(ChainFamily::Solana), "token") => AssetRef::SplToken(reference.parse()?),
            _ => return Err(bad()),
        };
        Ok(Self { chain, asset })
    }
}

// ---- serde + schema: every identifier travels as its canonical string ----

macro_rules! string_repr {
    ($ty:ty, $desc:literal, $example:literal) => {
        impl Serialize for $ty {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_str(self)
            }
        }
        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                s.parse().map_err(serde::de::Error::custom)
            }
        }
        impl JsonSchema for $ty {
            fn schema_name() -> Cow<'static, str> {
                stringify!($ty).into()
            }
            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({ "type": "string", "description": $desc, "examples": [$example] })
            }
        }
    };
}

string_repr!(ChainId, "CAIP-2 chain id", "eip155:8453");
string_repr!(
    AccountId,
    "CAIP-10 account id",
    "eip155:1:0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"
);
string_repr!(
    AssetId,
    "CAIP-19 asset id",
    "eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
);
string_repr!(
    SolanaPubkey,
    "Solana public key (base58)",
    "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
);

impl Serialize for AccountAddress {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

/// Unambiguous without chain context: base58 has no '0', so a `0x` prefix means EVM.
impl FromStr for AccountAddress {
    type Err = DomainError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let family = if s.trim().starts_with("0x") {
            ChainFamily::Evm
        } else {
            ChainFamily::Solana
        };
        Self::parse(family, s)
    }
}

impl<'de> Deserialize<'de> for AccountAddress {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for AccountAddress {
    fn schema_name() -> Cow<'static, str> {
        "AccountAddress".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "string", "description": "EVM address (0x…, EIP-55) or Solana public key (base58)" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOL: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";

    #[test]
    fn caip2_round_trip_and_family() {
        let base: ChainId = "eip155:8453".parse().unwrap();
        assert_eq!(base.to_string(), "eip155:8453");
        assert_eq!(base.family(), Some(ChainFamily::Evm));
        assert_eq!(base.evm_chain_id(), Some(8453));
        let sol: ChainId = SOL.parse().unwrap();
        assert_eq!(sol.family(), Some(ChainFamily::Solana));
        assert_eq!(sol.evm_chain_id(), None);
        assert_eq!(ChainId::evm(4663).to_string(), "eip155:4663");
        assert!("base".parse::<ChainId>().is_err());
        assert!("EIP155:1".parse::<ChainId>().is_err());
    }

    #[test]
    fn evm_address_checksum_rules() {
        let lower = "0xd8da6bf26964af9d7eed9e03e53415d37aa96045";
        let good = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";
        let bad = "0xD8dA6BF26964aF9D7eEd9e03E53415D37aA96045";
        assert_eq!(
            AccountAddress::parse(ChainFamily::Evm, lower)
                .unwrap()
                .to_string(),
            good
        );
        assert!(AccountAddress::parse(ChainFamily::Evm, good).is_ok());
        assert!(AccountAddress::parse(ChainFamily::Evm, bad).is_err());
        assert!(AccountAddress::parse(ChainFamily::Evm, "0x1234").is_err());
    }

    #[test]
    fn solana_pubkey_validation() {
        let usdc = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
        assert_eq!(
            AccountAddress::parse(ChainFamily::Solana, usdc)
                .unwrap()
                .to_string(),
            usdc
        );
        assert!(AccountAddress::parse(ChainFamily::Solana, "0OIl").is_err());
        assert!(AccountAddress::parse(ChainFamily::Solana, "3yZe7d").is_err());
    }

    #[test]
    fn caip10_and_caip19_round_trip() {
        let acct: AccountId = "eip155:1:0xd8da6bf26964af9d7eed9e03e53415d37aa96045"
            .parse()
            .unwrap();
        assert_eq!(
            acct.to_string(),
            "eip155:1:0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"
        );
        let sol_acct: AccountId = format!("{SOL}:EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v")
            .parse()
            .unwrap();
        assert_eq!(sol_acct.chain.family(), Some(ChainFamily::Solana));
        assert!(format!("{SOL}:0xd8da6bf26964af9d7eed9e03e53415d37aa96045")
            .parse::<AccountId>()
            .is_err());

        for s in [
            "eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
            "eip155:1/slip44:60",
            &format!("{SOL}/token:EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"),
            &format!("{SOL}/slip44:501"),
        ] {
            let a: AssetId = s.parse().unwrap();
            assert_eq!(a.to_string(), s);
        }
        assert!(
            "eip155:1/token:EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
                .parse::<AssetId>()
                .is_err()
        );
    }

    #[test]
    fn identifiers_serialize_as_strings() {
        let a: AssetId = "eip155:1/slip44:60".parse().unwrap();
        assert_eq!(serde_json::to_string(&a).unwrap(), "\"eip155:1/slip44:60\"");
        let back: AssetId = serde_json::from_str("\"eip155:1/slip44:60\"").unwrap();
        assert_eq!(back, a);
    }
}
