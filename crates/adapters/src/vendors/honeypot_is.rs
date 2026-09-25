//! `honeypot_is` (keyless). Buy/sell simulation for Ethereum, BSC and Base only.
//! Owner: `market-trading` (T1.M2). Port: `TokenRisk`.

use super::util;

use crate::http::HttpClient;
use async_trait::async_trait;
use bdm_config::{Loaded, Redacted, VendorStatus};
use bdm_domain::{AssetId, AssetRef, RiskFlag, Severity};
use bdm_ports::{PortHandle, PortResult, ProviderError, Registration, RiskAssessment, TokenRisk};
use rust_decimal::Decimal;
use serde_json::Value;
use std::sync::Arc;

pub const ID: &str = "honeypot_is";
const BASE: &str = "https://api.honeypot.is";
const CHAINS: &[u64] = &[1, 56, 8453];

pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status(ID) != VendorStatus::Active {
        return;
    }
    let a = Arc::new(HoneypotIs::new(util::http(loaded, ID), BASE));
    out.push(Registration::new(loaded.vendor_meta(ID)).global_port(PortHandle::TokenRisk(a)));
}

pub struct HoneypotIs {
    http: HttpClient,
    base: String,
}

/// Tax in percent (5 = 5%).
fn tax_flag(code: &str, v: &Value) -> Option<RiskFlag> {
    let t = util::dec(v).filter(|t| !t.is_zero())?;
    let sev = if t >= Decimal::from(50) {
        Severity::High
    } else if t >= Decimal::from(10) {
        Severity::Medium
    } else {
        Severity::Low
    };
    Some(util::risk_flag(
        ID,
        code,
        sev,
        Some(format!("{}%", t.normalize())),
    ))
}

fn severity(s: &str) -> Severity {
    match s {
        "critical" => Severity::Critical,
        "high" => Severity::High,
        "medium" => Severity::Medium,
        "low" => Severity::Low,
        _ => Severity::Info,
    }
}

#[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
fn flags(v: &Value) -> Vec<RiskFlag> {
    let mut out = Vec::new();
    if v["honeypotResult"]["isHoneypot"].as_bool() == Some(true) {
        let reason = v["honeypotResult"]["honeypotReason"]
            .as_str()
            .map(str::to_owned);
        out.push(util::risk_flag(ID, "honeypot", Severity::Critical, reason));
    }
    if v["simulationSuccess"].as_bool() == Some(false) {
        out.push(util::risk_flag(
            ID,
            "simulation_failed",
            Severity::Medium,
            None,
        ));
    }
    let sim = &v["simulationResult"];
    out.extend(tax_flag("buy_tax", &sim["buyTax"]));
    out.extend(tax_flag("sell_tax", &sim["sellTax"]));
    out.extend(tax_flag("transfer_tax", &sim["transferTax"]));
    for f in v["summary"]["flags"].as_array().into_iter().flatten() {
        if let Some(code) = f["flag"].as_str() {
            out.push(util::risk_flag(
                ID,
                code,
                severity(f["severity"].as_str().unwrap_or("")),
                f["description"].as_str().map(str::to_owned),
            ));
        }
    }
    out
}

impl HoneypotIs {
    pub fn new(http: HttpClient, base: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
        }
    }
}

#[async_trait]
impl TokenRisk for HoneypotIs {
    async fn assess(&self, asset: &AssetId) -> PortResult<RiskAssessment> {
        let chain = asset.chain.evm_chain_id().filter(|c| CHAINS.contains(c));
        let (Some(chain), AssetRef::Erc20(addr)) = (chain, &asset.asset) else {
            return Err(ProviderError::Unsupported(
                "honeypot.is covers ERC-20s on Ethereum, BSC and Base only".into(),
            ));
        };
        let url = Redacted::new(format!(
            "{}/v2/IsHoneypot?address={}&chainID={chain}",
            self.base,
            addr.to_checksum(None)
        ));
        let v = self.http.get_json(&url, "/v2/IsHoneypot", &[]).await?;
        Ok(RiskAssessment {
            source: ID.into(),
            flags: flags(&v),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::DEFAULT_TIMEOUT;
    use bdm_testkit::wiremock::{
        matchers::{method, path, query_param},
        Mock, MockServer, ResponseTemplate,
    };

    #[tokio::test]
    async fn honeypot_with_taxes_and_summary_flags() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/IsHoneypot"))
            .and(query_param("chainID", "8453"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(bdm_testkit::vendor_fixture(
                    env!("CARGO_MANIFEST_DIR"),
                    ID,
                    "is_honeypot",
                )),
            )
            .mount(&server)
            .await;
        let h = HoneypotIs::new(HttpClient::new(ID, DEFAULT_TIMEOUT), &server.uri());
        let asset: AssetId = "eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
            .parse()
            .unwrap();
        let r = h.assess(&asset).await.unwrap();
        let get = |c: &str| r.flags.iter().find(|f| f.code == c).unwrap();
        assert_eq!(get("honeypot").severity, Severity::Critical);
        assert_eq!(get("sell_tax").severity, Severity::High);
        assert_eq!(get("buy_tax").detail.as_deref(), Some("5%"));
        assert_eq!(get("closed_source").severity, Severity::High);
        assert!(r.flags.iter().all(|f| f.source == ID));

        let arb: AssetId = "eip155:42161/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
            .parse()
            .unwrap();
        assert!(matches!(
            h.assess(&arb).await,
            Err(ProviderError::Unsupported(_))
        ));
    }
}
