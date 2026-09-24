//! `openexchangerates` vendor adapter. Owner: `neobank-wallet`.
//!
//! Free plan: USD base only, hourly updates, 1,000 requests/month, `app_id` query parameter
//! (`OPENEXCHANGERATES_APP_ID`). `GET /latest.json` or `/historical/YYYY-MM-DD.json` →
//! `{"timestamp": 1758124800, "base": "USD", "rates": {"EUR": 0.8546, ...}}`. Cross rates are
//! computed here: `base→quote = rates[quote] / rates[base]` in `rust_decimal`.
//! `/usage.json` feeds the quota dashboard. Docs: https://docs.openexchangerates.org/

use crate::http::{HttpClient, DEFAULT_TIMEOUT};
use async_trait::async_trait;
use bdm_config::{Loaded, Redacted, VendorStatus};
use bdm_ports::{
    FxRate, FxRates, PortHandle, PortResult, ProviderError, QuotaReporter, Registration, UsageUnit,
    UsageWindow, VendorMeta, VendorUsage, WindowKind,
};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde_json::Value;
use std::{str::FromStr, sync::Arc};

const ID: &str = "openexchangerates";
pub const BASE_URL: &str = "https://openexchangerates.org/api";

/// Push this vendor's registration if it is active (needs `OPENEXCHANGERATES_APP_ID`).
pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status(ID) != VendorStatus::Active {
        return;
    }
    let Some(app_id) = loaded.key(ID, "app_id") else {
        return;
    };
    let http = HttpClient::new(ID, DEFAULT_TIMEOUT).with_secrets(loaded.secret_values());
    let oxr = Arc::new(OpenExchangeRates::new(http, BASE_URL, app_id));
    let e = loaded.registry.vendors.get(ID);
    let meta = VendorMeta {
        id: ID.into(),
        display_name: e.map_or("Open Exchange Rates".into(), |e| e.display_name.clone()),
        requires_key: true,
        signup_url: e.and_then(|e| e.signup_url.clone()),
        rpc_features: Default::default(),
    };
    out.push(
        Registration::new(meta)
            .global_port(PortHandle::Fx(oxr.clone()))
            .with_quota_reporter(oxr),
    );
}

pub struct OpenExchangeRates {
    http: HttpClient,
    base_url: String,
    app_id: Redacted<String>,
}

impl OpenExchangeRates {
    pub fn new(http: HttpClient, base_url: &str, app_id: &str) -> Self {
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
            app_id: Redacted::new(app_id.to_owned()),
        }
    }

    fn url(&self, path: &str) -> Redacted<String> {
        Redacted::new(format!(
            "{}/{path}?app_id={}",
            self.base_url,
            self.app_id.expose()
        ))
    }
}

#[async_trait]
impl FxRates for OpenExchangeRates {
    async fn rate(&self, base: &str, quote: &str, date: Option<NaiveDate>) -> PortResult<FxRate> {
        let (path, label) = match date {
            Some(d) => (format!("historical/{d}.json"), "historical"),
            None => ("latest.json".to_owned(), "latest"),
        };
        let v = self.http.get_json(&self.url(&path), label, &[]).await?;
        parse(&v, base, quote, date)
    }
}

#[async_trait]
impl QuotaReporter for OpenExchangeRates {
    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    async fn usage(&self) -> PortResult<VendorUsage> {
        let v = self
            .http
            .get_json(&self.url("usage.json"), "usage", &[])
            .await?;
        let d = &v["data"];
        let used = d["usage"]["requests"]
            .as_u64()
            .ok_or_else(|| ProviderError::Transient("openexchangerates: no usage data".into()))?;
        Ok(VendorUsage {
            plan: d["plan"]["name"].as_str().map(str::to_owned),
            windows: vec![UsageWindow {
                kind: WindowKind::Month,
                unit: UsageUnit::Requests,
                used,
                limit: d["usage"]["requests_quota"].as_u64(),
                resets_at: None,
            }],
            fetched_at: Utc::now(),
        })
    }
}

/// JSON number → exact decimal via its shortest round-trip text (no float arithmetic).
fn json_decimal(v: &Value) -> Option<Decimal> {
    let s = v.as_number()?.to_string();
    Decimal::from_str(&s)
        .or_else(|_| Decimal::from_scientific(&s))
        .ok()
}

/// Cross rate from a USD-based table.
#[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
fn parse(v: &Value, base: &str, quote: &str, date: Option<NaiveDate>) -> PortResult<FxRate> {
    if v["base"].as_str() != Some("USD") {
        return Err(ProviderError::Fatal(
            "openexchangerates: expected a USD base table".into(),
        ));
    }
    let usd_per = |c: &str| -> PortResult<Decimal> {
        if c == "USD" {
            return Ok(Decimal::ONE);
        }
        json_decimal(&v["rates"][c])
            .filter(|d| !d.is_zero())
            .ok_or_else(|| ProviderError::Unsupported(format!("no {c} rate")))
    };
    let rate = usd_per(quote)?
        .checked_div(usd_per(base)?)
        .ok_or_else(|| ProviderError::Transient("openexchangerates: rate overflow".into()))?
        .normalize();
    let as_of = v["timestamp"]
        .as_i64()
        .and_then(|t| DateTime::from_timestamp(t, 0))
        .ok_or_else(|| ProviderError::Transient("openexchangerates: no timestamp".into()))?;
    Ok(FxRate {
        base: base.to_owned(),
        quote: quote.to_owned(),
        rate,
        business_date: date.unwrap_or(as_of.date_naive()),
        source: ID.into(),
        as_of,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bdm_testkit::wiremock::{
        matchers::{method, path, query_param},
        Mock, MockServer, ResponseTemplate,
    };
    use serde_json::json;

    fn table() -> Value {
        json!({"timestamp": 1_758_124_800, "base": "USD", "rates": {"EUR": 0.8, "GBP": 0.75, "JPY": 150}})
    }

    #[test]
    fn cross_rates_are_decimal_exact() {
        let eur_gbp = parse(&table(), "EUR", "GBP", None).unwrap();
        assert_eq!(eur_gbp.rate, Decimal::from_str("0.9375").unwrap());
        let usd_jpy = parse(&table(), "USD", "JPY", None).unwrap();
        assert_eq!(usd_jpy.rate, Decimal::from(150));
        let eur_usd = parse(&table(), "EUR", "USD", None).unwrap();
        assert_eq!(eur_usd.rate, Decimal::from_str("1.25").unwrap());
        assert_eq!(eur_usd.business_date.to_string(), "2025-09-17");
        assert!(matches!(
            parse(&table(), "EUR", "XYZ", None),
            Err(ProviderError::Unsupported(_))
        ));
    }

    #[tokio::test]
    async fn historical_request_and_key_scrubbing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/historical/2026-09-18.json"))
            .and(query_param("app_id", "oxr_secret_app_id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(table()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/latest.json"))
            .respond_with(
                ResponseTemplate::new(400).set_body_string("invalid app_id oxr_secret_app_id"),
            )
            .mount(&server)
            .await;
        let oxr = OpenExchangeRates::new(
            HttpClient::new(ID, DEFAULT_TIMEOUT),
            &format!("{}/api", server.uri()),
            "oxr_secret_app_id",
        );
        let d = NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
        let r = oxr.rate("EUR", "GBP", Some(d)).await.unwrap();
        assert_eq!(
            (r.business_date, r.rate.to_string().as_str()),
            (d, "0.9375")
        );
        let err = oxr.rate("EUR", "GBP", None).await.unwrap_err();
        assert!(!err.to_string().contains("oxr_secret_app_id"), "{err}");
    }

    #[tokio::test]
    async fn usage_report() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/usage.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": 200,
                "data": {"app_id": "x", "plan": {"name": "Free"}, "usage": {"requests": 12, "requests_quota": 1000}}
            })))
            .mount(&server)
            .await;
        let oxr = OpenExchangeRates::new(
            HttpClient::new(ID, DEFAULT_TIMEOUT),
            &format!("{}/api", server.uri()),
            "oxr_secret_app_id",
        );
        let u = oxr.usage().await.unwrap();
        assert_eq!(u.plan.as_deref(), Some("Free"));
        assert_eq!((u.windows[0].used, u.windows[0].limit), (12, Some(1000)));
    }
}
