//! Canonical stablecoin registry (`registry/stablecoins.toml`). Owner: `payments-stablecoin` (T1.P1).
//! Other teams use only [`StablecoinRegistry`]'s lookup API; the owner extends the entry fields.
//!
//! Loading validates every row: `source_url` (https) and `verified_at` are required, EVM
//! addresses must be written EIP-55 checksummed, Solana mints must be valid base58, and
//! `(chain, address)` / `(chain, symbol)` are unique. A bad row fails `builtin()` and therefore CI.

use bdm_domain::{AccountAddress, AssetId, AssetRef, ChainFamily, ChainId};
use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const BUILTIN: &str = include_str!("../../../registry/stablecoins.toml");

/// How this deployment is issued (separates native USDC from bridged / exchange-pegged copies).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Issuance {
    /// Minted directly by the issuer on this chain.
    Native,
    /// LayerZero OFT (issuer-operated mesh, e.g. USDT0, USDG).
    Oft,
    /// Bridged representation (e.g. `USDC.e`), not redeemable with the issuer directly.
    Bridged,
    /// Exchange-issued peg (e.g. Binance-Peg tokens).
    ExchangePeg,
}

/// Per-address issuer restriction check for this deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FreezeCheck {
    /// Circle FiatToken: `isBlacklisted(address)`.
    IsBlacklisted,
    /// Legacy Tether: `isBlackListed(address)` (also `getBlackListStatus(address)`).
    IsBlackListed,
    /// Newer Tether / USDT0: `isBlocked(address)`.
    IsBlocked,
    /// Paxos (PYUSD, USDG): `isFrozen(address)`.
    IsFrozen,
    /// Ripple RLUSD: `accountPaused(address)`.
    AccountPaused,
    /// Solana: the owner's token account `state == frozen`.
    TokenAccountState,
    /// No per-address freeze on this token.
    None,
}

impl FreezeCheck {
    /// Contract method (or account field) the check reads.
    pub fn method(&self) -> &'static str {
        match self {
            Self::IsBlacklisted => "isBlacklisted(address)",
            Self::IsBlackListed => "isBlackListed(address)",
            Self::IsBlocked => "isBlocked(address)",
            Self::IsFrozen => "isFrozen(address)",
            Self::AccountPaused => "accountPaused(address)",
            Self::TokenAccountState => "tokenAccount.state",
            Self::None => "none",
        }
    }
}

/// Token-wide pause check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PauseCheck {
    /// `paused()` view.
    Paused,
    /// No pause switch (Solana Token-2022 `pausable` is read from the mint when present).
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct StablecoinEntry {
    /// CAIP-19 id (chain + contract/mint): the only thing payments match on.
    pub asset: AssetId,
    /// Contract address (EIP-55) or Solana mint.
    pub address: String,
    pub symbol: String,
    /// Other symbols this deployment answers to (e.g. USDT0 is "USDT" on its chains).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    pub name: String,
    pub issuer_entity: String,
    /// Peg currency, ISO 4217 ("USD", "EUR").
    pub peg: String,
    pub decimals: u8,
    pub issuance: Issuance,
    pub freeze_check: FreezeCheck,
    pub pause_check: PauseCheck,
    /// Legacy Tether: `deprecated()` / `upgradedAddress()` redirect.
    pub deprecated_check: bool,
    /// Solana token program id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_program: Option<String>,
    /// Solana Token-2022 extensions observed on the mint.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    /// Solana: an issuer permanent delegate can move funds out of any token account.
    pub permanent_delegate: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cctp_domain: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oft_adapter: Option<String>,
    /// Chainlink aggregator on this chain (`<peg>` price of the coin), when verified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chainlink_feed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pyth_feed_id: Option<String>,
    /// Authorized EU e-money token under MiCA (only set when an issuer source says so).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mica_emt: Option<bool>,
    pub source_url: String,
    pub verified_at: NaiveDate,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl StablecoinEntry {
    pub fn matches_symbol(&self, symbol: &str) -> bool {
        self.symbol.eq_ignore_ascii_case(symbol)
            || self.aliases.iter().any(|a| a.eq_ignore_ascii_case(symbol))
    }
}

/// One TOML row (strings are validated into typed fields by [`StablecoinRegistry::parse`]).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    symbol: String,
    #[serde(default)]
    aliases: Vec<String>,
    name: String,
    issuer_entity: String,
    peg: String,
    chain: ChainId,
    address: String,
    decimals: u8,
    issuance: Issuance,
    freeze_check: FreezeCheck,
    pause_check: PauseCheck,
    #[serde(default)]
    deprecated_check: bool,
    token_program: Option<String>,
    #[serde(default)]
    extensions: Vec<String>,
    #[serde(default)]
    permanent_delegate: bool,
    cctp_domain: Option<u32>,
    oft_adapter: Option<String>,
    chainlink_feed: Option<String>,
    pyth_feed_id: Option<String>,
    mica_emt: Option<bool>,
    #[serde(default)]
    source_url: String,
    verified_at: Option<NaiveDate>,
    note: Option<String>,
}

#[derive(Deserialize)]
struct File {
    stablecoin: Vec<Row>,
}

#[derive(Debug, Clone, Default)]
pub struct StablecoinRegistry {
    entries: Vec<StablecoinEntry>,
}

impl StablecoinRegistry {
    /// Built-in registry from `registry/stablecoins.toml`.
    pub fn builtin() -> Result<Self, String> {
        Self::parse(BUILTIN)
    }

    /// Parse and validate. Returns every problem found, one per line.
    pub fn parse(toml_text: &str) -> Result<Self, String> {
        let file: File =
            toml::from_str(toml_text).map_err(|e| format!("registry/stablecoins.toml: {e}"))?;
        let mut errors = Vec::new();
        let mut entries: Vec<StablecoinEntry> = Vec::new();
        for (i, r) in file.stablecoin.into_iter().enumerate() {
            let at = format!("stablecoin[{i}] {} on {}", r.symbol, r.chain);
            match validate(r) {
                Ok(e) => {
                    if entries.iter().any(|x| x.asset == e.asset) {
                        errors.push(format!("{at}: duplicate address {}", e.address));
                    } else if entries
                        .iter()
                        .any(|x| x.asset.chain == e.asset.chain && x.matches_symbol(&e.symbol))
                    {
                        errors.push(format!("{at}: duplicate symbol on chain"));
                    } else {
                        entries.push(e);
                    }
                }
                Err(e) => errors.push(format!("{at}: {e}")),
            }
        }
        if errors.is_empty() {
            Ok(Self { entries })
        } else {
            Err(errors.join("\n"))
        }
    }

    pub fn all(&self) -> &[StablecoinEntry] {
        &self.entries
    }

    pub fn by_asset(&self, asset: &AssetId) -> Option<&StablecoinEntry> {
        self.entries.iter().find(|e| &e.asset == asset)
    }

    pub fn by_symbol(&self, chain: &ChainId, symbol: &str) -> Option<&StablecoinEntry> {
        self.entries
            .iter()
            .find(|e| &e.asset.chain == chain && e.matches_symbol(symbol))
    }

    pub fn for_chain<'a>(&'a self, chain: &ChainId) -> impl Iterator<Item = &'a StablecoinEntry> {
        let chain = chain.clone();
        self.entries.iter().filter(move |e| e.asset.chain == chain)
    }

    /// Resolve user input to a canonical entry: a CAIP-19 id, a bare contract/mint on `chain`,
    /// or a symbol on `chain`. Never matches an unknown contract by symbol.
    pub fn resolve(&self, chain: &ChainId, token: &str) -> Option<&StablecoinEntry> {
        let token = token.trim();
        if let Ok(asset) = token.parse::<AssetId>() {
            return (&asset.chain == chain)
                .then(|| self.by_asset(&asset))
                .flatten();
        }
        if let Some(asset) = asset_on(chain, token) {
            return self.by_asset(&asset);
        }
        self.by_symbol(chain, token)
    }
}

/// `address` on `chain` as a CAIP-19 token id (lenient on EVM checksum casing).
pub fn asset_on(chain: &ChainId, address: &str) -> Option<AssetId> {
    let asset = match (
        chain.family()?,
        AccountAddress::parse(chain.family()?, address).ok()?,
    ) {
        (ChainFamily::Evm, AccountAddress::Evm(a)) => AssetRef::Erc20(a),
        (ChainFamily::Solana, AccountAddress::Solana(m)) => AssetRef::SplToken(m),
        _ => return None,
    };
    Some(AssetId {
        chain: chain.clone(),
        asset,
    })
}

fn validate(r: Row) -> Result<StablecoinEntry, String> {
    if !r.source_url.starts_with("https://") {
        return Err("missing or non-https source_url".into());
    }
    let verified_at = r.verified_at.ok_or("missing verified_at")?;
    let family = r.chain.family().ok_or("unsupported chain namespace")?;
    let asset = match family {
        ChainFamily::Evm => {
            let a: alloy_primitives::Address = r
                .address
                .parse()
                .map_err(|_| format!("invalid EVM address {}", r.address))?;
            if a.to_checksum(None) != r.address {
                return Err(format!(
                    "EVM address {} is not EIP-55 checksummed (expected {})",
                    r.address,
                    a.to_checksum(None)
                ));
            }
            AssetRef::Erc20(a)
        }
        ChainFamily::Solana => AssetRef::SplToken(
            r.address
                .parse()
                .map_err(|_| format!("invalid Solana mint {}", r.address))?,
        ),
    };
    if family == ChainFamily::Evm {
        for extra in [&r.oft_adapter, &r.chainlink_feed].into_iter().flatten() {
            let ok = extra
                .parse::<alloy_primitives::Address>()
                .is_ok_and(|a| &a.to_checksum(None) == extra);
            if !ok {
                return Err(format!("address {extra} is not EIP-55 checksummed"));
            }
        }
    }
    if family == ChainFamily::Solana && r.token_program.is_none() {
        return Err("Solana rows need token_program".into());
    }
    if r.peg.len() != 3 || !r.peg.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(format!("peg '{}' is not an ISO 4217 code", r.peg));
    }
    Ok(StablecoinEntry {
        asset: AssetId {
            chain: r.chain,
            asset,
        },
        address: r.address,
        symbol: r.symbol,
        aliases: r.aliases,
        name: r.name,
        issuer_entity: r.issuer_entity,
        peg: r.peg,
        decimals: r.decimals,
        issuance: r.issuance,
        freeze_check: r.freeze_check,
        pause_check: r.pause_check,
        deprecated_check: r.deprecated_check,
        token_program: r.token_program,
        extensions: r.extensions,
        permanent_delegate: r.permanent_delegate,
        cctp_domain: r.cctp_domain,
        oft_adapter: r.oft_adapter,
        chainlink_feed: r.chainlink_feed,
        pyth_feed_id: r.pyth_feed_id,
        mica_emt: r.mica_emt,
        source_url: r.source_url,
        verified_at,
        note: r.note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOL: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";

    #[test]
    fn builtin_registry_is_valid_and_on_known_chains() {
        let reg = StablecoinRegistry::builtin().unwrap_or_else(|e| panic!("{e}"));
        let chains = bdm_config::Registry::builtin().unwrap().chains;
        assert!(reg.all().len() >= 20, "{}", reg.all().len());
        for e in reg.all() {
            assert!(
                chains.find(&e.asset.chain.to_string()).is_some(),
                "{} on unknown chain {}",
                e.symbol,
                e.asset.chain
            );
            assert!(e.source_url.starts_with("https://"));
        }
    }

    #[test]
    fn lookups_match_by_contract_and_symbol() {
        let reg = StablecoinRegistry::builtin().unwrap();
        let base = ChainId::evm(8453);
        let usdc = reg.by_symbol(&base, "usdc").unwrap();
        assert_eq!(usdc.address, "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
        assert_eq!(usdc.decimals, 6);
        assert_eq!(usdc.cctp_domain, Some(6));
        // lenient casing on input, same entry
        assert_eq!(
            reg.resolve(&base, "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"),
            Some(usdc)
        );
        assert_eq!(reg.resolve(&base, &usdc.asset.to_string()), Some(usdc));
        // CAIP-19 of another chain is not silently accepted
        assert_eq!(reg.resolve(&ChainId::evm(1), &usdc.asset.to_string()), None);
        // unknown contract is never matched by symbol
        assert_eq!(
            reg.resolve(&base, "0x0000000000000000000000000000000000000001"),
            None
        );
        // USDT0 answers to USDT on Arbitrum
        let arb = ChainId::evm(42161);
        assert_eq!(reg.by_symbol(&arb, "USDT").unwrap().symbol, "USDT0");
        // Robinhood: USDG from Paxos docs, no native USDC listed by Circle
        let rh = ChainId::evm(4663);
        assert_eq!(
            reg.by_symbol(&rh, "USDG").unwrap().address,
            "0x5fc5360D0400a0Fd4f2af552ADD042D716F1d168"
        );
        assert!(reg.by_symbol(&rh, "USDC").is_none());
        let sol: ChainId = SOL.parse().unwrap();
        let pyusd = reg.by_symbol(&sol, "PYUSD").unwrap();
        assert!(pyusd.permanent_delegate);
        assert_eq!(pyusd.freeze_check, FreezeCheck::TokenAccountState);
        assert!(reg.for_chain(&sol).count() >= 4);
    }

    fn row(chain: &str, address: &str, source: &str) -> String {
        format!(
            r#"[[stablecoin]]
symbol = "X"
name = "X"
issuer_entity = "X"
peg = "USD"
chain = "{chain}"
address = "{address}"
decimals = 6
issuance = "native"
freeze_check = "none"
pause_check = "none"
source_url = "{source}"
verified_at = "2026-09-23"
"#
        )
    }

    #[test]
    fn rejects_unsourced_and_unchecksummed_rows() {
        let good = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
        assert!(StablecoinRegistry::parse(&row("eip155:8453", good, "https://x.test")).is_ok());
        let err = StablecoinRegistry::parse(&row("eip155:8453", good, "")).unwrap_err();
        assert!(err.contains("source_url"), "{err}");
        let lower = good.to_lowercase();
        let err =
            StablecoinRegistry::parse(&row("eip155:8453", &lower, "https://x.test")).unwrap_err();
        assert!(err.contains("checksummed"), "{err}");
        let dup = format!(
            "{}{}",
            row("eip155:8453", good, "https://x.test"),
            row("eip155:8453", good, "https://x.test")
        );
        assert!(StablecoinRegistry::parse(&dup)
            .unwrap_err()
            .contains("duplicate"));
        let no_date = row("eip155:8453", good, "https://x.test")
            .replace("verified_at = \"2026-09-23\"\n", "");
        assert!(StablecoinRegistry::parse(&no_date)
            .unwrap_err()
            .contains("verified_at"));
    }
}
