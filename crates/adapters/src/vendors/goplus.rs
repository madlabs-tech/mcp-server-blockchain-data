//! `goplus` token security (30 req/min).
//!
//! The access token is optional per GoPlus docs. With `GOPLUS_APP_KEY` + `GOPLUS_APP_SECRET`,
//! `POST /api/v1/token` with `sign = sha1(app_key + time + app_secret)` returns a token (cached
//! until shortly before expiry), sent as `Authorization: <token>` (no `Bearer` prefix); without keys, requests
//! go out unauthenticated at the public limit.
//! Port: `TokenRisk` for EVM (`/token_security/{chain_id}`) and Solana (`/solana/token_security`).

use super::util;

use crate::http::HttpClient;
use async_trait::async_trait;
use bdm_config::{Loaded, Redacted, VendorStatus};
use bdm_domain::{AssetId, AssetRef, RiskFlag, Severity};
use bdm_ports::{PortHandle, PortResult, ProviderError, Registration, RiskAssessment, TokenRisk};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const ID: &str = "goplus";
const BASE: &str = "https://api.gopluslabs.io";

pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status(ID) != VendorStatus::Active {
        return;
    }
    let creds = loaded.key(ID, "app_key").zip(loaded.key(ID, "app_secret"));
    let a = Arc::new(GoPlus::new(util::http(loaded, ID), BASE, creds));
    out.push(Registration::new(loaded.vendor_meta(ID)).global_port(PortHandle::TokenRisk(a)));
}

pub struct GoPlus {
    http: HttpClient,
    base: String,
    creds: Option<(Redacted<String>, Redacted<String>)>,
    token: Mutex<Option<(String, Instant)>>,
}

fn sign(app_key: &str, time: i64, secret: &str) -> String {
    alloy_primitives::hex::encode(Sha1::digest(format!("{app_key}{time}{secret}").as_bytes()))
}

fn is_one(v: &Value) -> bool {
    v.as_str() == Some("1") || v.as_u64() == Some(1) || v["status"].as_str() == Some("1")
}

/// Tax as a fraction ("0.05" = 5%).
fn tax_flag(code: &str, v: &Value) -> Option<RiskFlag> {
    let t = util::dec(v).filter(|t| !t.is_zero())?;
    let sev = if t >= Decimal::new(5, 1) {
        Severity::High
    } else if t >= Decimal::new(1, 1) {
        Severity::Medium
    } else {
        Severity::Low
    };
    // A tax too large to express in percent still raises the flag, just without the detail.
    let pct = t.checked_mul(Decimal::ONE_HUNDRED);
    Some(util::risk_flag(
        ID,
        code,
        sev,
        pct.map(|p| format!("{}%", p.normalize())),
    ))
}

fn evm_flags(r: &Value) -> Vec<RiskFlag> {
    const BOOL_FLAGS: &[(&str, &str, Severity)] = &[
        ("is_honeypot", "honeypot", Severity::Critical),
        ("is_airdrop_scam", "airdrop_scam", Severity::Critical),
        ("cannot_sell_all", "cannot_sell_all", Severity::High),
        (
            "owner_change_balance",
            "owner_can_change_balance",
            Severity::High,
        ),
        ("hidden_owner", "hidden_owner", Severity::High),
        ("selfdestruct", "selfdestruct", Severity::High),
        ("cannot_buy", "cannot_buy", Severity::High),
        ("is_mintable", "mintable", Severity::Medium),
        ("is_blacklisted", "blacklist", Severity::Medium),
        ("transfer_pausable", "transfer_pausable", Severity::Medium),
        ("slippage_modifiable", "tax_modifiable", Severity::Medium),
        (
            "personal_slippage_modifiable",
            "per_address_tax",
            Severity::Medium,
        ),
        ("external_call", "external_call", Severity::Low),
        ("trading_cooldown", "trading_cooldown", Severity::Low),
        ("is_proxy", "proxy_upgradeable", Severity::Low),
    ];
    let mut out: Vec<RiskFlag> = BOOL_FLAGS
        .iter()
        .filter(|(k, _, _)| is_one(&r[*k]))
        .map(|(_, code, sev)| util::risk_flag(ID, code, *sev, None))
        .collect();
    if r["is_open_source"].as_str() == Some("0") {
        out.push(util::risk_flag(
            ID,
            "not_open_source",
            Severity::Medium,
            None,
        ));
    }
    out.extend(tax_flag("buy_tax", &r["buy_tax"]));
    out.extend(tax_flag("sell_tax", &r["sell_tax"]));
    out
}

fn solana_flags(r: &Value) -> Vec<RiskFlag> {
    const FLAGS: &[(&str, &str, Severity)] = &[
        (
            "balance_mutable_authority",
            "balance_mutable_authority",
            Severity::High,
        ),
        ("non_transferable", "non_transferable", Severity::High),
        ("mintable", "mint_authority_active", Severity::Medium),
        ("freezable", "freeze_authority_active", Severity::Medium),
        ("closable", "closable", Severity::Medium),
        (
            "transfer_fee_upgradable",
            "transfer_fee_upgradable",
            Severity::Medium,
        ),
        ("metadata_mutable", "metadata_mutable", Severity::Low),
    ];
    let mut out: Vec<RiskFlag> = FLAGS
        .iter()
        .filter(|(k, _, _)| is_one(&r[*k]))
        .map(|(_, code, sev)| util::risk_flag(ID, code, *sev, None))
        .collect();
    if r["transfer_hook"].as_array().is_some_and(|a| !a.is_empty()) {
        out.push(util::risk_flag(ID, "transfer_hook", Severity::Medium, None));
    }
    out
}

impl GoPlus {
    /// `creds = Some((app_key, app_secret))` enables authenticated requests.
    pub fn new(http: HttpClient, base: &str, creds: Option<(&str, &str)>) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            creds: creds.map(|(k, s)| (Redacted::new(k.to_owned()), Redacted::new(s.to_owned()))),
            token: Mutex::new(None),
        }
    }

    /// `Authorization` header value, or `None` when running without keys.
    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn authorization(&self) -> PortResult<Option<String>> {
        let Some((key, secret)) = &self.creds else {
            return Ok(None);
        };
        let cached = self.token.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some((t, until)) = cached {
            if Instant::now() < until {
                return Ok(Some(t));
            }
        }
        let time = chrono::Utc::now().timestamp();
        let body = json!({
            "app_key": key.expose(),
            "time": time,
            "sign": sign(key.expose(), time, secret.expose()),
        });
        let url = Redacted::new(format!("{}/api/v1/token", self.base));
        let v = self.http.post_json(&url, "/api/v1/token", &body).await?;
        let r = &v["result"];
        let token = r["access_token"]
            .as_str()
            .ok_or_else(|| ProviderError::Unsupported("goplus rejected app key/secret".into()))?
            .to_owned();
        // GoPlus expects the raw token, not `Bearer <token>` (verified live: Bearer → code 4012).
        let ttl = r["expires_in"].as_u64().unwrap_or(3600).saturating_sub(60);
        // An absurd vendor `expires_in` would overflow `Instant + Duration` (a panic): don't cache.
        if let Some(until) = Instant::now().checked_add(Duration::from_secs(ttl)) {
            *self.token.lock().unwrap_or_else(|e| e.into_inner()) = Some((token.clone(), until));
        }
        Ok(Some(token))
    }
}

#[async_trait]
impl TokenRisk for GoPlus {
    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn assess(&self, asset: &AssetId) -> PortResult<RiskAssessment> {
        let unsupported = || ProviderError::Unsupported(format!("goplus does not cover {asset}"));
        let (path, solana) = match &asset.asset {
            AssetRef::Erc20(_) => {
                let id = asset.chain.evm_chain_id().ok_or_else(unsupported)?;
                if !matches!(id, 1 | 10 | 56 | 137 | 8453 | 42161 | 43114) {
                    return Err(unsupported());
                }
                (format!("/api/v1/token_security/{id}"), false)
            }
            AssetRef::SplToken(_) if util::is_solana_mainnet(&asset.chain) => {
                ("/api/v1/solana/token_security".to_owned(), true)
            }
            _ => return Err(unsupported()),
        };
        let addr = util::token_address(asset).ok_or_else(unsupported)?;
        let auth = self.authorization().await?;
        let headers: Vec<(&str, &str)> = auth
            .as_deref()
            .map(|a| ("authorization", a))
            .into_iter()
            .collect();
        let url = Redacted::new(format!("{}{path}?contract_addresses={addr}", self.base));
        let label = if solana {
            "/solana/token_security"
        } else {
            "/token_security"
        };
        let v = self.http.get_json(&url, label, &headers).await?;
        if v["code"].as_i64() != Some(1) {
            return Err(ProviderError::Transient(format!(
                "goplus code {}: {}",
                v["code"],
                v["message"].as_str().unwrap_or("")
            )));
        }
        let r = util::get_ci(&v["result"], &addr).ok_or(ProviderError::NotFound)?;
        Ok(RiskAssessment {
            source: ID.into(),
            flags: if solana {
                solana_flags(r)
            } else {
                evm_flags(r)
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::DEFAULT_TIMEOUT;
    use bdm_testkit::wiremock::{
        matchers::{header, method, path, query_param},
        Mock, MockServer, ResponseTemplate,
    };

    #[test]
    fn sign_is_sha1_hex() {
        // sha1("0") and sha1("abc"): app_key + time + secret concatenated.
        assert_eq!(sign("", 0, ""), "b6589fc6ab0dc82cf12099d1c2d40ab994e8410c");
    }

    #[test]
    fn huge_tax_flags_without_panicking() {
        let f = tax_flag("sell_tax", &Value::from(Decimal::MAX.to_string())).unwrap();
        assert_eq!((f.severity, f.detail), (Severity::High, None));
        let f = tax_flag("buy_tax", &"0.05".into()).unwrap();
        assert_eq!(f.detail.as_deref(), Some("5%"));
    }

    #[tokio::test]
    async fn absurd_token_expiry_is_not_cached_and_does_not_panic() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"code": 1, "result": {"access_token": "tok", "expires_in": u64::MAX}}),
            ))
            .expect(2)
            .mount(&server)
            .await;
        let g = GoPlus::new(
            HttpClient::new(ID, DEFAULT_TIMEOUT),
            &server.uri(),
            Some(("k", "s")),
        );
        for _ in 0..2 {
            assert_eq!(g.authorization().await.unwrap().as_deref(), Some("tok"));
        }
    }

    #[tokio::test]
    async fn evm_token_security_flags() {
        let server = MockServer::start().await;
        let fx = |c| bdm_testkit::vendor_fixture(env!("CARGO_MANIFEST_DIR"), ID, c);
        Mock::given(method("POST"))
            .and(path("/api/v1/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fx("access_token")))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/token_security/56"))
            .and(query_param(
                "contract_addresses",
                "0x55d398326f99059fF775485246999027B3197955",
            ))
            .and(header("authorization", "tok-abc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fx("token_security_evm")))
            .mount(&server)
            .await;
        let g = GoPlus::new(
            HttpClient::new(ID, DEFAULT_TIMEOUT),
            &server.uri(),
            Some(("app-key", "app-secret")),
        );
        let asset: AssetId = "eip155:56/erc20:0x55d398326f99059fF775485246999027B3197955"
            .parse()
            .unwrap();
        let r = g.assess(&asset).await.unwrap();
        let codes: Vec<&str> = r.flags.iter().map(|f| f.code.as_str()).collect();
        assert!(
            codes.contains(&"honeypot") && codes.contains(&"sell_tax"),
            "{codes:?}"
        );
        assert!(!codes.contains(&"buy_tax"), "zero tax is not a flag");
        let sell = r.flags.iter().find(|f| f.code == "sell_tax").unwrap();
        assert_eq!(
            (sell.severity, sell.detail.as_deref()),
            (Severity::High, Some("99%"))
        );
        // Token is cached: a second call does not re-authenticate (expect(1) above).
        g.assess(&asset).await.unwrap();
    }
}
