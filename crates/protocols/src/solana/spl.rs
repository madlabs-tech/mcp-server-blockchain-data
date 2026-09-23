//! SPL Token / Token-2022 helpers. Owner: `solana` (T1.S1). Used by payments, neobank, wallet.
//!
//! Program ids: <https://solana.com/docs/tokens> (Token, Token-2022, Associated Token Account).

use ems_domain::{DomainError, SolanaPubkey};
use ems_ports::{PortResult, ProviderError, SolanaRpc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const TOKEN_2022_PROGRAM: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
pub const ASSOCIATED_TOKEN_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
/// Native (wrapped) SOL mint, used by DEX APIs for SOL. <https://solana.com/docs/tokens#wrapped-sol>
pub const WRAPPED_SOL_MINT: &str = "So11111111111111111111111111111111111111112";
/// Both token programs; balances and history must always query both.
pub const TOKEN_PROGRAMS: [&str; 2] = [TOKEN_PROGRAM, TOKEN_2022_PROGRAM];

fn pk(s: &str) -> SolanaPubkey {
    s.parse().expect("hard-coded program id")
}

/// ATA address, derived with the mint's token program (Token-2022 mints differ from legacy).
/// Seeds are `[owner, token_program, mint]` under the Associated Token Account program.
pub fn associated_token_address(
    owner: &SolanaPubkey,
    mint: &SolanaPubkey,
    token_program: &SolanaPubkey,
) -> Result<SolanaPubkey, DomainError> {
    let program = token_program.to_string();
    if !TOKEN_PROGRAMS.contains(&program.as_str()) {
        return Err(DomainError::invalid(format!(
            "{program} is not a token program"
        )));
    }
    find_program_address(
        &[&owner.0, &token_program.0, &mint.0],
        &pk(ASSOCIATED_TOKEN_PROGRAM),
    )
    .map(|(addr, _)| addr)
}

/// `find_program_address`: highest bump (255 → 0) whose
/// `sha256(seeds ‖ bump ‖ program ‖ "ProgramDerivedAddress")` is NOT a valid ed25519 point.
pub fn find_program_address(
    seeds: &[&[u8]],
    program: &SolanaPubkey,
) -> Result<(SolanaPubkey, u8), DomainError> {
    for bump in (0..=255u8).rev() {
        let mut h = Sha256::new();
        for s in seeds {
            h.update(s);
        }
        h.update([bump]);
        h.update(program.0);
        h.update(b"ProgramDerivedAddress");
        let bytes: [u8; 32] = h.finalize().into();
        if !is_on_curve(&bytes) {
            return Ok((SolanaPubkey(bytes), bump));
        }
    }
    Err(DomainError::internal("no viable program-derived address"))
}

/// True when `bytes` decompresses to an ed25519 point (i.e. could have a private key).
pub fn is_on_curve(bytes: &[u8; 32]) -> bool {
    curve25519_dalek::edwards::CompressedEdwardsY(*bytes)
        .decompress()
        .is_some()
}

/// One parsed token account (`getTokenAccountsByOwner` / `getAccountInfo`, jsonParsed).
#[derive(Debug, Clone, PartialEq)]
pub struct TokenAccount {
    pub address: SolanaPubkey,
    pub mint: SolanaPubkey,
    pub owner: SolanaPubkey,
    pub program: SolanaPubkey,
    pub amount: u64,
    pub decimals: u8,
}

/// Every token account of `owner` under BOTH token programs (ATAs and non-ATAs).
pub async fn token_accounts(
    rpc: &dyn SolanaRpc,
    owner: &SolanaPubkey,
    commitment: &str,
) -> PortResult<Vec<TokenAccount>> {
    let mut out = Vec::new();
    for program in TOKEN_PROGRAMS {
        let v = rpc
            .request(
                "getTokenAccountsByOwner",
                json!([owner.to_string(), {"programId": program},
                       {"encoding": "jsonParsed", "commitment": commitment}]),
            )
            .await?;
        for item in v["value"].as_array().into_iter().flatten() {
            out.push(parse_token_account(item, program)?);
        }
    }
    Ok(out)
}

fn parse_token_account(item: &Value, program: &str) -> PortResult<TokenAccount> {
    let bad = |what: &str| ProviderError::Transient(format!("malformed token account: {what}"));
    let info = &item["account"]["data"]["parsed"]["info"];
    let key = |v: &Value, what: &str| -> PortResult<SolanaPubkey> {
        v.as_str()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| bad(what))
    };
    Ok(TokenAccount {
        address: key(&item["pubkey"], "pubkey")?,
        mint: key(&info["mint"], "mint")?,
        owner: key(&info["owner"], "owner")?,
        program: pk(program),
        amount: info["tokenAmount"]["amount"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| bad("amount"))?,
        decimals: info["tokenAmount"]["decimals"]
            .as_u64()
            .and_then(|d| u8::try_from(d).ok())
            .ok_or_else(|| bad("decimals"))?,
    })
}

/// Facts read from a mint account (`getAccountInfo`, jsonParsed).
#[derive(Debug, Clone, PartialEq)]
pub struct MintInfo {
    pub program: SolanaPubkey,
    pub decimals: u8,
    /// Token-2022 `tokenMetadata` extension, when present. Legacy mints keep metadata in
    /// Metaplex accounts, which this reader does not decode.
    pub name: Option<String>,
    pub symbol: Option<String>,
    pub uri: Option<String>,
    /// Token-2022 extension names (`transferFeeConfig`, `confidentialTransferMint`, …).
    pub extensions: Vec<String>,
}

/// Read a mint. `NotFound` if the account does not exist; `Invalid` if it is not a mint.
pub async fn mint_info(rpc: &dyn SolanaRpc, mint: &SolanaPubkey) -> PortResult<MintInfo> {
    let v = rpc
        .request(
            "getAccountInfo",
            json!([mint.to_string(), {"encoding": "jsonParsed"}]),
        )
        .await?;
    let acct = &v["value"];
    if acct.is_null() {
        return Err(ProviderError::NotFound);
    }
    let program = acct["owner"].as_str().unwrap_or_default();
    let parsed = &acct["data"]["parsed"];
    if !TOKEN_PROGRAMS.contains(&program) || parsed["type"] != "mint" {
        return Err(ProviderError::Invalid(format!(
            "{mint} is not a token mint"
        )));
    }
    let info = &parsed["info"];
    let decimals = info["decimals"]
        .as_u64()
        .and_then(|d| u8::try_from(d).ok())
        .ok_or_else(|| ProviderError::Transient("mint without decimals".into()))?;
    let exts = info["extensions"].as_array().cloned().unwrap_or_default();
    let meta = exts
        .iter()
        .find(|e| e["extension"] == "tokenMetadata")
        .map(|e| &e["state"]);
    let field = |k: &str| {
        meta.and_then(|m| m[k].as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    Ok(MintInfo {
        program: pk(program),
        decimals,
        name: field("name"),
        symbol: field("symbol"),
        uri: field("uri"),
        extensions: exts
            .iter()
            .filter_map(|e| e["extension"].as_str().map(str::to_owned))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Vectors from mainnet tx 2dyb2hr9…pmM4K (slot 449687808, fetched 2026-09-23 via
    // getTransaction): owner Go5EVX… holds PYUSD (Token-2022) in 3Rvy7A… and USDC (legacy) in
    // 5MjBG9…. Mints: PYUSD 2b1kV6… (paxos/PayPal docs), USDC EPjFWd… (Circle docs).
    const OWNER: &str = "Go5EVXxV3ob4CJVaq6YiGGUKqPmEfDo32kSmbWsYevsF";
    const PYUSD: &str = "2b1kV6DkPAnxd5ixfnxCpjxmKwqjjaYmCZfHsFu24GXo";
    const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

    #[test]
    fn ata_uses_the_mints_token_program() {
        let owner = pk(OWNER);
        let pyusd = associated_token_address(&owner, &pk(PYUSD), &pk(TOKEN_2022_PROGRAM)).unwrap();
        assert_eq!(
            pyusd.to_string(),
            "3Rvy7A1hViAyQh8sCJ2NhDMUVERpDv77tpLLHz7fGaaB"
        );
        let usdc = associated_token_address(&owner, &pk(USDC), &pk(TOKEN_PROGRAM)).unwrap();
        assert_eq!(
            usdc.to_string(),
            "5MjBG96YjNJWEL687DuGroThtGN5GPd9dFgQXg8o22TV"
        );
        // Wrong program → a different (wrong) address: the classic PYUSD pitfall.
        let wrong = associated_token_address(&owner, &pk(PYUSD), &pk(TOKEN_PROGRAM)).unwrap();
        assert_ne!(wrong, pyusd);
        assert!(!is_on_curve(&pyusd.0));
        assert!(is_on_curve(&owner.0));
        assert!(associated_token_address(&owner, &pk(USDC), &owner).is_err());
    }
}
