//! Tokenized-stock issuer registry (`registry/rwa.toml`) and small RWA contract reads.
//! Owner: `market-trading` (T1.M4).
//!
//! Only addresses copied from issuer or oracle docs are listed, each with a `source_url`.
//! A token whose ticker matches but whose address is not on the issuer's list is a lookalike.

use alloy_primitives::{keccak256, Address};
use bdm_domain::{AssetId, AssetRef, ChainId};
use bdm_ports::{EvmRpc, PortResult, ProviderError};
use serde::Deserialize;
use serde_json::json;

const BUILTIN: &str = include_str!("../../../registry/rwa.toml");

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RwaToken {
    pub ticker: String,
    /// CAIP-2 chain id.
    pub chain: String,
    /// EIP-55 checksummed token address.
    pub address: String,
    /// Chainlink price feed (proxy) for this token, when the oracle docs list one.
    #[serde(default)]
    pub feed: Option<String>,
    pub source_url: String,
    pub verified_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Issuer {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub legal_entity: Option<String>,
    pub chains: Vec<String>,
    pub standard: String,
    #[serde(default)]
    pub decimals: Option<u8>,
    /// How corporate actions reach holders: "ui_multiplier" (ERC-8056) or "rebase".
    #[serde(default)]
    pub corporate_actions: Option<String>,
    #[serde(default)]
    pub official_list_url: Option<String>,
    pub source_url: String,
    /// False for placeholders nobody has checked against the issuer's docs yet.
    pub verified: bool,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub tokens: Vec<RwaToken>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequencerFeed {
    pub chain: String,
    pub address: String,
    pub source_url: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RwaRegistry {
    #[serde(default, rename = "issuer")]
    pub issuers: Vec<Issuer>,
    #[serde(default, rename = "sequencer_feed")]
    pub sequencer_feeds: Vec<SequencerFeed>,
}

impl RwaRegistry {
    pub fn builtin() -> Result<Self, String> {
        Self::parse(BUILTIN)
    }

    /// Parse and validate: every row has a source, every address is checksummed.
    pub fn parse(text: &str) -> Result<Self, String> {
        let reg: Self = toml::from_str(text).map_err(|e| format!("rwa.toml: {e}"))?;
        for i in &reg.issuers {
            if i.source_url.trim().is_empty() {
                return Err(format!("rwa.toml: issuer {} has no source_url", i.id));
            }
            for t in &i.tokens {
                if t.source_url.trim().is_empty() {
                    return Err(format!("rwa.toml: {} {} has no source_url", i.id, t.ticker));
                }
                for a in std::iter::once(&t.address).chain(t.feed.as_ref()) {
                    checksummed(a).map_err(|e| format!("rwa.toml: {} {}: {e}", i.id, t.ticker))?;
                }
            }
        }
        for s in &reg.sequencer_feeds {
            checksummed(&s.address).map_err(|e| format!("rwa.toml: sequencer {}: {e}", s.chain))?;
        }
        Ok(reg)
    }

    /// Issuer + token for an asset on an issuer's official list.
    pub fn lookup(&self, asset: &AssetId) -> Option<(&Issuer, &RwaToken)> {
        let AssetRef::Erc20(addr) = asset.asset else {
            return None;
        };
        let chain = asset.chain.to_string();
        self.issuers.iter().find_map(|i| {
            i.tokens
                .iter()
                .find(|t| t.chain == chain && t.address.parse::<Address>().ok() == Some(addr))
                .map(|t| (i, t))
        })
    }

    /// Issuers that deploy on `chain` (to flag lookalikes and explain unknown tokens).
    pub fn issuers_on<'a>(&'a self, chain: &'a ChainId) -> impl Iterator<Item = &'a Issuer> {
        let c = chain.to_string();
        self.issuers.iter().filter(move |i| i.chains.contains(&c))
    }

    pub fn sequencer_feed(&self, chain: &ChainId) -> Option<Address> {
        let c = chain.to_string();
        self.sequencer_feeds
            .iter()
            .find(|s| s.chain == c)
            .and_then(|s| s.address.parse().ok())
    }
}

fn checksummed(a: &str) -> Result<Address, String> {
    Address::parse_checksummed(a, None).map_err(|_| format!("'{a}' is not an EIP-55 address"))
}

/// Call a no-argument `view returns (bool)` function.
async fn call_bool(rpc: &dyn EvmRpc, to: Address, signature: &str) -> PortResult<bool> {
    let digest = keccak256(signature.as_bytes());
    let (selector, _) = digest.0.split_at(4);
    let data = format!("0x{}", hex::encode(selector));
    let v = rpc
        .request(
            "eth_call",
            json!([{ "to": to.to_checksum(None), "data": data }, "latest"]),
        )
        .await?;
    let s = v
        .as_str()
        .ok_or_else(|| ProviderError::Transient(format!("eth_call returned {v}")))?;
    let bytes = hex::decode(s.trim_start_matches("0x"))
        .map_err(|e| ProviderError::Transient(format!("bad eth_call hex: {e}")))?;
    match bytes.len() {
        32 => Ok(bytes.last().is_some_and(|b| *b != 0)),
        // Empty return: no such function (or not a contract).
        0 => Err(ProviderError::Unsupported(format!(
            "{signature} not implemented by {to}"
        ))),
        n => Err(ProviderError::Transient(format!(
            "{signature} returned {n} bytes"
        ))),
    }
}

/// `oraclePaused()` on a Robinhood stock token: true during corporate actions, meaning the
/// price must be treated as unavailable.
pub async fn oracle_paused(rpc: &dyn EvmRpc, token: Address) -> PortResult<bool> {
    call_bool(rpc, token, "oraclePaused()").await
}

#[cfg(test)]
mod tests {
    use super::*;
    use bdm_testkit::mocks::MockEvmRpc;

    #[test]
    fn builtin_registry_is_valid() {
        let r = RwaRegistry::builtin().unwrap();
        let rh = r.issuers.iter().find(|i| i.id == "robinhood").unwrap();
        assert!(rh.verified);
        assert_eq!(rh.decimals, Some(18));
        assert!(rh.official_list_url.is_some());
        for i in &r.issuers {
            if !i.verified {
                assert!(
                    i.tokens.is_empty(),
                    "unverified issuer {} lists tokens",
                    i.id
                );
            }
        }
    }

    #[test]
    fn rejects_rows_without_sources_or_checksums() {
        let base = r#"[[issuer]]
id = "x"
name = "X"
chains = ["eip155:1"]
standard = "ERC-20"
source_url = "https://example.com"
verified = true
"#;
        assert!(RwaRegistry::parse(base).is_ok());
        let bad_addr = format!(
            "{base}[[issuer.tokens]]\nticker = \"T\"\nchain = \"eip155:1\"\naddress = \"0xd8da6bf26964af9d7eed9e03e53415d37aa96045\"\nsource_url = \"https://e\"\nverified_at = \"2026-09-23\"\n"
        );
        assert!(RwaRegistry::parse(&bad_addr)
            .unwrap_err()
            .contains("EIP-55"));
        let no_src = bad_addr
            .replace(
                "0xd8da6bf26964af9d7eed9e03e53415d37aa96045",
                "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045",
            )
            .replace("source_url = \"https://e\"", "source_url = \"\"");
        assert!(RwaRegistry::parse(&no_src)
            .unwrap_err()
            .contains("source_url"));
        let ok = bad_addr.replace(
            "0xd8da6bf26964af9d7eed9e03e53415d37aa96045",
            "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045",
        );
        let r = RwaRegistry::parse(&ok).unwrap();
        let asset: AssetId = "eip155:1/erc20:0xd8da6bf26964af9d7eed9e03e53415d37aa96045"
            .parse()
            .unwrap();
        assert_eq!(r.lookup(&asset).unwrap().1.ticker, "T");
        let other: AssetId = "eip155:8453/erc20:0xd8da6bf26964af9d7eed9e03e53415d37aa96045"
            .parse()
            .unwrap();
        assert!(r.lookup(&other).is_none());
    }

    #[tokio::test]
    async fn reads_oracle_paused() {
        let rpc = MockEvmRpc::default();
        rpc.script
            .push_ok(json!(format!("0x{}1", "0".repeat(63))))
            .push_ok(json!(format!("0x{}", "0".repeat(64))))
            .push_ok(json!("0x"));
        let t = Address::ZERO;
        assert!(oracle_paused(&rpc, t).await.unwrap());
        assert!(!oracle_paused(&rpc, t).await.unwrap());
        assert!(matches!(
            oracle_paused(&rpc, t).await,
            Err(ProviderError::Unsupported(_))
        ));
    }
}
