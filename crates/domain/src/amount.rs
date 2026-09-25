use crate::error::DomainError;
use alloy_primitives::U256;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Exact token amount: integer base units plus decimals. Never a float.
///
/// Serializes as `{"raw": "1500000", "decimals": 6, "formatted": "1.5"}`;
/// `formatted` is output-only and ignored on input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Amount {
    pub raw: U256,
    pub decimals: u8,
}

impl Amount {
    pub const fn new(raw: U256, decimals: u8) -> Self {
        Self { raw, decimals }
    }

    pub fn zero(decimals: u8) -> Self {
        Self {
            raw: U256::ZERO,
            decimals,
        }
    }

    pub fn from_u128(raw: u128, decimals: u8) -> Self {
        Self {
            raw: U256::from(raw),
            decimals,
        }
    }

    pub fn is_zero(&self) -> bool {
        self.raw.is_zero()
    }

    /// Parse a human decimal string ("1.5") into base units, exactly.
    /// Rejects more fractional digits than `decimals` rather than rounding.
    pub fn parse_units(s: &str, decimals: u8) -> Result<Self, DomainError> {
        let bad = || {
            DomainError::invalid(format!(
                "'{s}' is not a valid amount with {decimals} decimals"
            ))
        };
        let s = s.trim();
        let (int, frac) = s.split_once('.').unwrap_or((s, ""));
        if int.is_empty() && frac.is_empty()
            || !int.bytes().all(|b| b.is_ascii_digit())
            || !frac.bytes().all(|b| b.is_ascii_digit())
            || frac.len() > decimals as usize
        {
            return Err(bad());
        }
        let digits = format!("{int}{frac:0<width$}", width = decimals as usize);
        let raw = U256::from_str_radix(if digits.is_empty() { "0" } else { &digits }, 10)
            .map_err(|_| bad())?;
        Ok(Self { raw, decimals })
    }

    /// Exact decimal string without trailing zeros ("1.5", "0.000001", "42").
    pub fn format_units(&self) -> String {
        let digits = self.raw.to_string();
        let d = self.decimals as usize;
        if d == 0 {
            return digits;
        }
        let padded = format!("{digits:0>width$}", width = d + 1);
        let (int, frac) = padded.split_at(padded.len() - d);
        let frac = frac.trim_end_matches('0');
        if frac.is_empty() {
            int.to_owned()
        } else {
            format!("{int}.{frac}")
        }
    }

    /// Lossless conversion to `Decimal` when it fits (28 significant digits); for fiat math only.
    pub fn to_decimal(&self) -> Option<Decimal> {
        self.format_units().parse().ok()
    }

    fn same_scale(&self, other: &Self) -> Result<(), DomainError> {
        if self.decimals == other.decimals {
            Ok(())
        } else {
            Err(DomainError::internal(format!(
                "amount decimals mismatch: {} vs {}",
                self.decimals, other.decimals
            )))
        }
    }

    pub fn checked_add(&self, other: &Self) -> Result<Self, DomainError> {
        self.same_scale(other)?;
        let raw = self
            .raw
            .checked_add(other.raw)
            .ok_or_else(|| DomainError::internal("amount overflow"))?;
        Ok(Self {
            raw,
            decimals: self.decimals,
        })
    }

    /// `self - other`, or `None` if `other > self`.
    pub fn checked_sub(&self, other: &Self) -> Result<Option<Self>, DomainError> {
        self.same_scale(other)?;
        Ok(self.raw.checked_sub(other.raw).map(|raw| Self {
            raw,
            decimals: self.decimals,
        }))
    }
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "Amount")]
/// Exact token amount in base units.
struct AmountRepr {
    /// Integer amount in the token's smallest unit, as a decimal string.
    raw: String,
    /// Token decimals.
    decimals: u8,
    /// Human-readable exact value (output only).
    #[serde(default, skip_deserializing)]
    formatted: String,
}

impl Serialize for Amount {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        AmountRepr {
            raw: self.raw.to_string(),
            decimals: self.decimals,
            formatted: self.format_units(),
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for Amount {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let r = AmountRepr::deserialize(d)?;
        let raw = U256::from_str_radix(&r.raw, 10).map_err(serde::de::Error::custom)?;
        Ok(Self {
            raw,
            decimals: r.decimals,
        })
    }
}

impl JsonSchema for Amount {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Amount".into()
    }
    fn json_schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        AmountRepr::json_schema(g)
    }
}

/// Fiat value with its valuation time and source; the timestamp is the block time for history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Fiat {
    #[serde(with = "crate::serde_str")]
    #[schemars(with = "String")]
    pub amount: Decimal,
    /// ISO 4217 code, e.g. "USD".
    pub currency: String,
    pub as_of: DateTime<Utc>,
    /// Vendor or oracle that supplied the rate, e.g. "coingecko", "chainlink".
    pub source: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_parse_round_trip() {
        for (s, d, raw) in [
            ("1.5", 6, "1500000"),
            ("0.000001", 6, "1"),
            ("42", 18, "42000000000000000000"),
            ("0", 8, "0"),
            ("123.45678901", 8, "12345678901"),
            ("1000", 0, "1000"),
        ] {
            let a = Amount::parse_units(s, d).unwrap();
            assert_eq!(a.raw.to_string(), raw, "{s}");
            assert_eq!(a.format_units(), s, "{s}");
        }
    }

    #[test]
    fn round_trip_property_over_many_values() {
        for d in [0u8, 6, 8, 18] {
            for raw in [
                0u128,
                1,
                9,
                10,
                99,
                1_000_000,
                123_456_789,
                u64::MAX as u128,
                u128::MAX,
            ] {
                let a = Amount::from_u128(raw, d);
                let back = Amount::parse_units(&a.format_units(), d).unwrap();
                assert_eq!(back, a, "raw={raw} d={d}");
            }
        }
    }

    #[test]
    fn rejects_imprecise_and_garbage() {
        assert!(Amount::parse_units("1.0000001", 6).is_err());
        assert!(Amount::parse_units("-1", 6).is_err());
        assert!(Amount::parse_units("1e6", 6).is_err());
        assert!(Amount::parse_units("", 6).is_err());
        assert!(Amount::parse_units(".", 6).is_err());
        assert_eq!(
            Amount::parse_units(".5", 6).unwrap().raw,
            U256::from(500_000u64)
        );
    }

    #[test]
    fn arithmetic_requires_same_scale() {
        let a = Amount::from_u128(5, 6);
        let b = Amount::from_u128(3, 6);
        assert_eq!(a.checked_add(&b).unwrap().raw, U256::from(8u8));
        assert_eq!(a.checked_sub(&b).unwrap().unwrap().raw, U256::from(2u8));
        assert_eq!(b.checked_sub(&a).unwrap(), None);
        assert!(a.checked_add(&Amount::from_u128(1, 18)).is_err());
    }

    #[test]
    fn serde_shape() {
        let a = Amount::from_u128(1_500_000, 6);
        let v = serde_json::to_value(a).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"raw": "1500000", "decimals": 6, "formatted": "1.5"})
        );
        let back: Amount =
            serde_json::from_value(serde_json::json!({"raw": "1500000", "decimals": 6})).unwrap();
        assert_eq!(back, a);
    }
}
