//! `rugcheck` (Solana token reports). Disabled by default: auth and limits are unverified
//! (~60/min reported). An optional `RUGCHECK_API_KEY` is sent as `X-API-KEY` ⚠ unverified.
//! Owner: `market-trading` (T1.M2). Port: `TokenRisk`.

use super::market_util as util;

use crate::http::HttpClient;
use async_trait::async_trait;
use bdm_config::{Loaded, Redacted, VendorStatus};
use bdm_domain::{AssetId, AssetRef, RiskFlag, Severity};
use bdm_ports::{PortHandle, PortResult, ProviderError, Registration, RiskAssessment, TokenRisk};
use std::sync::Arc;

pub const ID: &str = "rugcheck";
const BASE: &str = "https://api.rugcheck.xyz";

pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status(ID) != VendorStatus::Active {
        return;
    }
    let a = Arc::new(RugCheck::new(
        util::http(loaded, ID),
        BASE,
        loaded.key(ID, "api_key"),
    ));
    out.push(Registration::new(util::meta(loaded, ID)).global_port(PortHandle::TokenRisk(a)));
}

pub struct RugCheck {
    http: HttpClient,
    base: String,
    key: Option<Redacted<String>>,
}

/// "Mutable metadata" → "mutable_metadata".
fn slug(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    s.split('_')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

impl RugCheck {
    pub fn new(http: HttpClient, base: &str, key: Option<&str>) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            key: key.map(|k| Redacted::new(k.to_owned())),
        }
    }
}

#[async_trait]
impl TokenRisk for RugCheck {
    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn assess(&self, asset: &AssetId) -> PortResult<RiskAssessment> {
        let AssetRef::SplToken(mint) = &asset.asset else {
            return Err(ProviderError::Unsupported(
                "rugcheck covers Solana tokens only".into(),
            ));
        };
        let url = Redacted::new(format!("{}/v1/tokens/{mint}/report/summary", self.base));
        let headers: Vec<(&str, &str)> = self
            .key
            .as_ref()
            .map(|k| ("X-API-KEY", k.expose().as_str()))
            .into_iter()
            .collect();
        let v = self
            .http
            .get_json(&url, "/v1/tokens/report/summary", &headers)
            .await?;
        let flags = v["risks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| {
                let name = r["name"].as_str()?;
                let severity = match r["level"].as_str() {
                    Some("danger") => Severity::High,
                    Some("warn") => Severity::Medium,
                    _ => Severity::Info,
                };
                Some(RiskFlag {
                    code: slug(name),
                    severity,
                    source: ID.into(),
                    detail: r["description"].as_str().map(str::to_owned),
                })
            })
            .collect();
        Ok(RiskAssessment {
            source: ID.into(),
            flags,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::DEFAULT_TIMEOUT;
    use bdm_testkit::wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    #[tokio::test]
    async fn report_summary_risks() {
        let server = MockServer::start().await;
        let mint = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
        Mock::given(method("GET"))
            .and(path(format!("/v1/tokens/{mint}/report/summary")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(bdm_testkit::vendor_fixture(
                    env!("CARGO_MANIFEST_DIR"),
                    ID,
                    "report_summary",
                )),
            )
            .mount(&server)
            .await;
        let r = RugCheck::new(HttpClient::new(ID, DEFAULT_TIMEOUT), &server.uri(), None);
        let asset: AssetId = format!("{}/token:{mint}", util::SOLANA_MAINNET)
            .parse()
            .unwrap();
        let out = r.assess(&asset).await.unwrap();
        assert_eq!(out.flags[0].code, "freeze_authority_still_enabled");
        assert_eq!(out.flags[0].severity, Severity::High);
        assert_eq!(out.flags[1].severity, Severity::Medium);
    }
}
