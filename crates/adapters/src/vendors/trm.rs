//! `trm` vendor adapter: TRM Labs free sanctions screening API → `SanctionsScreener`.
//! Owner: `payments-stablecoin` (T1.P2).
//!
//! API per <https://docs.sanctions.trmlabs.com/> (verified 2026-09-23):
//! `POST https://api.trmlabs.com/public/v1/sanctions/screening`, body `[{"address": "…"}]`,
//! response `[{"address": "…", "isSanctioned": bool}]`. Works keyless (1 req/s, 100/day); with
//! `TRM_API_KEY` it sends HTTP Basic with the key as both username and password (higher limits).
//! There is no chain field: the address alone is screened.

use super::util;

use crate::http::HttpClient;
use async_trait::async_trait;
use base64::Engine;
use bdm_config::{Loaded, Redacted, VendorStatus};
use bdm_domain::AccountId;
use bdm_ports::{
    PortHandle, PortResult, ProviderError, Registration, SanctionsScreener, ScreenResult,
};
use chrono::Utc;
use reqwest::Method;
use serde_json::{json, Value};
use std::sync::Arc;

pub const VENDOR: &str = "trm";
const URL: &str = "https://api.trmlabs.com/public/v1/sanctions/screening";

struct Trm {
    http: HttpClient,
    url: Redacted<String>,
    /// `Basic base64(key:key)`, when a key is configured.
    auth: Option<Redacted<String>>,
}

impl Trm {
    fn new(http: HttpClient, url: String, key: Option<&str>) -> Self {
        let auth = key.map(|k| {
            let b64 = base64::engine::general_purpose::STANDARD.encode(format!("{k}:{k}"));
            Redacted::new(format!("Basic {b64}"))
        });
        Self {
            http,
            url: Redacted::new(url),
            auth,
        }
    }
}

/// Find `isSanctioned` for `address` in the response array (case-insensitive address match).
fn parse_response(v: &Value, address: &str) -> PortResult<bool> {
    v.as_array()
        .into_iter()
        .flatten()
        .find(|r| {
            r.get("address")
                .and_then(Value::as_str)
                .is_some_and(|a| a.eq_ignore_ascii_case(address))
        })
        .and_then(|r| r.get("isSanctioned").and_then(Value::as_bool))
        .ok_or_else(|| ProviderError::Fatal("TRM response has no result for the address".into()))
}

#[async_trait]
impl SanctionsScreener for Trm {
    async fn screen(&self, account: &AccountId) -> PortResult<ScreenResult> {
        let address = account.address.to_string();
        let mut headers = vec![("accept", "application/json")];
        if let Some(a) = &self.auth {
            headers.push(("authorization", a.expose().as_str()));
        }
        let body = json!([{ "address": address }]);
        let v = self
            .http
            .request(
                Method::POST,
                &self.url,
                "sanctions/screening",
                &headers,
                Some(&body),
            )
            .await?;
        Ok(ScreenResult {
            sanctioned: parse_response(&v, &address)?,
            source: VENDOR.into(),
            detail: Some("TRM sanctions screening (address only, no chain context)".into()),
            as_of: Utc::now(),
        })
    }
}

/// Push this vendor's registration if it is active (`loaded.vendor_status("trm")`).
pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status(VENDOR) != VendorStatus::Active {
        return;
    }
    let trm = Trm::new(
        util::http(loaded, VENDOR),
        URL.into(),
        loaded.key(VENDOR, "api_key"),
    );
    out.push(
        Registration::new(loaded.vendor_meta(VENDOR))
            .global_port(PortHandle::Sanctions(Arc::new(trm))),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::DEFAULT_TIMEOUT;
    use bdm_testkit::wiremock::{
        matchers::{body_json, header, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    const ADDR: &str = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";

    fn acct() -> AccountId {
        format!("eip155:1:{ADDR}").parse().unwrap()
    }

    #[tokio::test]
    async fn screens_with_basic_auth() {
        let server = MockServer::start().await;
        // base64("k1:k1") = "azE6azE="
        Mock::given(method("POST"))
            .and(path("/public/v1/sanctions/screening"))
            .and(header("authorization", "Basic azE6azE="))
            .and(body_json(json!([{ "address": ADDR }])))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(
                    json!([{ "address": ADDR.to_lowercase(), "isSanctioned": true }]),
                ),
            )
            .mount(&server)
            .await;
        let trm = Trm::new(
            HttpClient::new(VENDOR, DEFAULT_TIMEOUT),
            format!("{}/public/v1/sanctions/screening", server.uri()),
            Some("k1"),
        );
        let r = trm.screen(&acct()).await.unwrap();
        assert!(r.sanctioned);
        assert_eq!(r.source, "trm");
    }

    #[tokio::test]
    async fn keyless_and_rate_limited() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "30"))
            .mount(&server)
            .await;
        let trm = Trm::new(HttpClient::new(VENDOR, DEFAULT_TIMEOUT), server.uri(), None);
        assert!(trm.auth.is_none());
        assert!(matches!(
            trm.screen(&acct()).await,
            Err(ProviderError::RateLimited { .. })
        ));
    }

    #[test]
    fn missing_result_is_an_error() {
        assert!(!parse_response(&json!([{"address": ADDR, "isSanctioned": false}]), ADDR).unwrap());
        assert!(parse_response(&json!([]), ADDR).is_err());
        assert!(parse_response(&json!({"error": "x"}), ADDR).is_err());
    }
}
