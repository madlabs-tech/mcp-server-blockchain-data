//! Helpers shared by the vendor modules (`use super::util;`).
#![allow(dead_code)] // not every helper is used when a single vendor feature (or none) builds

use crate::http::{HttpClient, DEFAULT_TIMEOUT};
use alloy_primitives::{Address, U256};
use bdm_config::Loaded;
use bdm_domain::{AssetId, AssetRef, ChainId, Price, RiskFlag, Severity, UnsignedTx};
use bdm_ports::{PortResult, ProviderError, TokenInfo};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::Value;
use std::str::FromStr;

pub use bdm_protocols::solana::SOLANA_MAINNET;
/// Native-token placeholder used by most EVM aggregators.
pub const EVM_NATIVE: &str = "0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE";
pub const ZERO_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

pub fn http(loaded: &Loaded, vendor: &str) -> HttpClient {
    HttpClient::new(vendor, DEFAULT_TIMEOUT).with_secrets(loaded.secret_values())
}

pub fn is_solana_mainnet(chain: &ChainId) -> bool {
    chain.to_string() == SOLANA_MAINNET
}

/// Case-insensitive object lookup (EVM addresses come back lowercased).
pub fn get_ci<'a>(obj: &'a Value, key: &str) -> Option<&'a Value> {
    obj.as_object()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
}

pub fn risk_flag(source: &str, code: &str, severity: Severity, detail: Option<String>) -> RiskFlag {
    RiskFlag {
        code: code.into(),
        severity,
        source: source.into(),
        detail,
    }
}

/// CoinGecko coin id of a native asset by SLIP-44 coin type (CoinGecko, DefiLlama).
pub fn coingecko_native_id(slip44: u32) -> Option<&'static str> {
    Some(match slip44 {
        60 => "ethereum",
        501 => "solana",
        714 => "binancecoin",
        966 => "polygon-ecosystem-token",
        9000 => "avalanche-2",
        _ => return None,
    })
}

/// Chain slug used by Birdeye and DexScreener.
pub fn dex_chain_slug(chain: &ChainId) -> Option<&'static str> {
    if is_solana_mainnet(chain) {
        return Some("solana");
    }
    Some(match chain.evm_chain_id()? {
        1 => "ethereum",
        8453 => "base",
        42161 => "arbitrum",
        10 => "optimism",
        137 => "polygon",
        43114 => "avalanche",
        56 => "bsc",
        _ => return None, // Robinhood Chain coverage unverified
    })
}

/// GeckoTerminal network slug (also CoinGecko's on-chain endpoints).
pub fn gecko_network(chain: &ChainId) -> Option<&'static str> {
    if is_solana_mainnet(chain) {
        return Some("solana");
    }
    Some(match chain.evm_chain_id()? {
        1 => "eth",
        8453 => "base",
        42161 => "arbitrum",
        10 => "optimism",
        137 => "polygon_pos",
        43114 => "avax",
        56 => "bsc",
        _ => return None, // Robinhood Chain coverage unverified
    })
}

/// GeckoTerminal `token_price` answer → USD price, pool reserve as liquidity.
#[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
pub fn gecko_token_price(
    asset: &AssetId,
    source: &str,
    v: &Value,
    addr: &str,
) -> PortResult<Price> {
    let attrs = &v["data"]["attributes"];
    Ok(Price {
        asset: asset.clone(),
        currency: "USD".into(),
        value: price(get_ci(&attrs["token_prices"], addr).unwrap_or(&Value::Null))?,
        as_of: Utc::now(),
        source: source.into(),
        liquidity_usd: get_ci(&attrs["total_reserve_in_usd"], addr).and_then(dec),
    })
}

/// GeckoTerminal `tokens/{addr}` answer → token metadata (https logos only).
#[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
pub fn gecko_token_info(asset: &AssetId, source: &str, v: &Value) -> PortResult<TokenInfo> {
    let a = &v["data"]["attributes"];
    let s = |k: &str| a.get(k).and_then(Value::as_str).map(str::to_owned);
    Ok(TokenInfo {
        asset: asset.clone(),
        decimals: a
            .get("decimals")
            .and_then(Value::as_u64)
            .and_then(|d| u8::try_from(d).ok())
            .ok_or(ProviderError::NotFound)?,
        symbol: s("symbol"),
        name: s("name"),
        logo_url: s("image_url").filter(|u| u.starts_with("https://")),
        verified: None,
        source: source.into(),
    })
}

/// Decimal from a JSON string or number. Numbers are read from their JSON text, never through
/// float arithmetic of ours.
pub fn dec(v: &Value) -> Option<Decimal> {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let s = s.trim();
    Decimal::from_str(s)
        .or_else(|_| Decimal::from_scientific(s))
        .ok()
}

/// A usable price: present and strictly positive. A 0 or missing price is "unknown", never 0.
pub fn price(v: &Value) -> PortResult<Decimal> {
    dec(v)
        .filter(|d| d.is_sign_positive() && !d.is_zero())
        .ok_or(ProviderError::NotFound)
}

/// Integer base units from a decimal string (or integer JSON number).
pub fn u256(v: &Value) -> Option<U256> {
    match v {
        Value::String(s) => U256::from_str_radix(s.trim(), 10).ok(),
        Value::Number(n) => n.as_u64().map(U256::from),
        _ => None,
    }
}

pub fn u256_field(v: &Value, what: &str) -> PortResult<U256> {
    u256(v).ok_or_else(|| ProviderError::Transient(format!("missing or invalid {what}")))
}

pub fn unix(secs: i64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(secs, 0)
}

/// Contract / mint address of a token asset; `None` for native assets.
pub fn token_address(asset: &AssetId) -> Option<String> {
    match &asset.asset {
        AssetRef::Erc20(a) => Some(a.to_checksum(None)),
        AssetRef::SplToken(m) => Some(m.to_string()),
        AssetRef::Native { .. } => None,
    }
}

/// Address as an EVM aggregator expects it (native → `0xEeee…`).
pub fn evm_token_or_native(asset: &AssetId) -> PortResult<String> {
    match &asset.asset {
        AssetRef::Erc20(a) => Ok(a.to_checksum(None)),
        AssetRef::Native { .. } => Ok(EVM_NATIVE.to_owned()),
        AssetRef::SplToken(_) => Err(ProviderError::Unsupported("not an EVM asset".into())),
    }
}

pub fn evm_chain_id(chain: &ChainId) -> PortResult<u64> {
    chain
        .evm_chain_id()
        .ok_or_else(|| ProviderError::Unsupported(format!("{chain} is not an EVM chain")))
}

/// EVM chain id of `chain` if `vendor` (as named in the error) covers it.
pub fn covered_evm_chain(chain: &ChainId, covered: &[u64], vendor: &str) -> PortResult<u64> {
    let id = evm_chain_id(chain)?;
    if covered.contains(&id) {
        Ok(id)
    } else {
        Err(ProviderError::Unsupported(format!(
            "{vendor} does not cover {chain}"
        )))
    }
}

/// `buy × (10000 − slippage) / 10000`, rounded down.
pub fn min_out(buy: U256, slippage_bps: u32) -> U256 {
    let bps = U256::from(10_000u32.saturating_sub(slippage_bps.min(10_000)));
    buy * bps / U256::from(10_000u32)
}

/// Slippage in percent for vendors that take it that way (50 bps → "0.5").
pub fn slippage_percent(bps: u32) -> String {
    Decimal::new(i64::from(bps), 2).normalize().to_string()
}

/// ERC-20 `approve(spender, amount)`.
pub fn approve_tx(
    chain_id: u64,
    token: &str,
    spender: &str,
    amount: U256,
) -> PortResult<UnsignedTx> {
    let spender: Address = spender.parse().map_err(|_| {
        ProviderError::Transient(format!("vendor returned bad spender '{spender}'"))
    })?;
    let data = format!(
        "0x095ea7b3{:0>64}{:064x}",
        alloy_primitives::hex::encode(spender),
        amount
    );
    Ok(UnsignedTx::Evm {
        chain_id,
        to: token.to_owned(),
        data,
        value: "0".into(),
        gas_limit: None,
        max_fee_per_gas: None,
        max_priority_fee_per_gas: None,
        nonce: None,
    })
}

/// EVM transaction from a vendor `{to, data, value, gas|gasLimit}` object.
pub fn evm_tx(chain_id: u64, tx: &Value) -> PortResult<UnsignedTx> {
    let s = |k: &str| tx.get(k).and_then(Value::as_str).map(str::to_owned);
    let to = s("to").ok_or_else(|| ProviderError::Transient("vendor tx has no 'to'".into()))?;
    let data = s("data")
        .or_else(|| s("input"))
        .ok_or_else(|| ProviderError::Transient("vendor tx has no 'data'".into()))?;
    let value = match tx.get("value") {
        Some(Value::String(v)) if v.starts_with("0x") => {
            U256::from_str_radix(v.trim_start_matches("0x"), 16)
                .map(|u| u.to_string())
                .unwrap_or_else(|_| "0".into())
        }
        Some(Value::String(v)) if !v.is_empty() => v.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => "0".into(),
    };
    let gas_limit = ["gas", "gasLimit"].iter().find_map(|k| match tx.get(*k) {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(v)) => v.parse().ok(),
        _ => None,
    });
    Ok(UnsignedTx::Evm {
        chain_id,
        to,
        data,
        value,
        gas_limit,
        max_fee_per_gas: None,
        max_priority_fee_per_gas: None,
        nonce: None,
    })
}

/// How long an aggregator quote is treated as valid when the vendor gives no expiry.
pub const QUOTE_TTL_SECS: i64 = 30;

/// Build a [`SwapQuote`] from a vendor answer. `min_buy = None` applies the requested slippage.
/// `buy_decimals = None` (vendor doesn't report them) is stored as 0: the trade operations
/// always re-stamp decimals from on-chain token metadata, so raw amounts are what matters here.
pub fn swap_quote(
    req: &bdm_ports::SwapRequest,
    source: &str,
    buy: U256,
    buy_decimals: Option<u8>,
    min_buy: Option<U256>,
) -> bdm_domain::SwapQuote {
    let d = buy_decimals.unwrap_or(0);
    bdm_domain::SwapQuote {
        chain: req.chain.clone(),
        sell_asset: req.sell_asset.clone(),
        sell_amount: req.sell_amount,
        buy_asset: req.buy_asset.clone(),
        buy_amount: bdm_domain::Amount::new(buy, d),
        min_buy_amount: bdm_domain::Amount::new(
            min_buy.unwrap_or_else(|| min_out(buy, req.slippage_bps)),
            d,
        ),
        price_impact_bps: None,
        source: source.to_owned(),
        expires_at: Utc::now() + chrono::Duration::seconds(QUOTE_TTL_SECS),
        tx: None,
        required_approvals: Vec::new(),
    }
}

/// `taker` as a checksummed EVM address, required to build a transaction.
pub fn evm_taker(req: &bdm_ports::SwapRequest) -> PortResult<String> {
    match req.taker {
        Some(bdm_domain::AccountAddress::Evm(a)) => Ok(a.to_checksum(None)),
        Some(_) => Err(ProviderError::Invalid("taker is not an EVM address".into())),
        None => Err(ProviderError::Invalid(
            "taker is required to build a swap".into(),
        )),
    }
}

/// ERC-20 approval for the sell token (none for native sells).
pub fn sell_approval(req: &bdm_ports::SwapRequest, spender: &str) -> PortResult<Vec<UnsignedTx>> {
    match &req.sell_asset.asset {
        AssetRef::Erc20(t) => Ok(vec![approve_tx(
            evm_chain_id(&req.chain)?,
            &t.to_checksum(None),
            spender,
            req.sell_amount.raw,
        )?]),
        _ => Ok(Vec::new()),
    }
}

/// Percent (e.g. "-0.35" or 0.35) → basis points, rounded toward zero.
pub fn percent_to_bps(v: &Value) -> Option<i32> {
    use rust_decimal::prelude::ToPrimitive;
    dec(v)?.checked_mul(Decimal::ONE_HUNDRED)?.trunc().to_i32()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn percent_to_bps_never_panics_on_vendor_data() {
        assert_eq!(percent_to_bps(&json!("-0.35")), Some(-35));
        assert_eq!(percent_to_bps(&json!(0.35)), Some(35));
        assert_eq!(percent_to_bps(&json!(Decimal::MAX.to_string())), None);
        assert_eq!(percent_to_bps(&json!("1e27")), None);
        assert_eq!(percent_to_bps(&json!("1e99999999999")), None);
    }

    #[test]
    fn util_decimals_without_floats() {
        assert_eq!(dec(&json!("1.0001")), Some(Decimal::new(10001, 4)));
        assert_eq!(dec(&json!(0.9998)), Some(Decimal::new(9998, 4)));
        assert_eq!(dec(&json!(1.5e-7)), Decimal::from_scientific("1.5e-7").ok());
        assert_eq!(price(&json!(0)), Err(ProviderError::NotFound));
        assert_eq!(price(&json!(null)), Err(ProviderError::NotFound));
    }

    #[test]
    fn util_min_out_approve_and_tx() {
        use alloy_primitives::U256;
        use bdm_domain::UnsignedTx;
        assert_eq!(
            min_out(U256::from(1_000_000u64), 50),
            U256::from(995_000u64)
        );
        assert_eq!(slippage_percent(50), "0.5");
        assert_eq!(slippage_percent(100), "1");
        let tx = approve_tx(
            1,
            "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            "0x111111125421ca6dc452d289314280a0f8842a65",
            U256::from(255u8),
        )
        .unwrap();
        let UnsignedTx::Evm { data, .. } = tx else {
            panic!()
        };
        assert_eq!(data.len(), 2 + 8 + 128);
        assert!(data.starts_with(
            "0x095ea7b3000000000000000000000000111111125421ca6dc452d289314280a0f8842a65"
        ));
        assert!(data.ends_with("ff"));
        let t = evm_tx(
            1,
            &json!({"to": "0x1", "data": "0xab", "value": "0x10", "gas": 21000}),
        )
        .unwrap();
        let UnsignedTx::Evm {
            value, gas_limit, ..
        } = t
        else {
            panic!()
        };
        assert_eq!((value.as_str(), gas_limit), ("16", Some(21000)));
    }
}
