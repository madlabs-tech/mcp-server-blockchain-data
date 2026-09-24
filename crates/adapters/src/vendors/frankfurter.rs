//! `frankfurter` vendor adapter (ECB reference rates, keyless). Owner: `neobank-wallet`.
//!
//! `GET {base}/{YYYY-MM-DD|latest}?base=EUR&symbols=USD` →
//! `{"amount":1.0,"base":"EUR","date":"2026-09-18","rates":{"USD":1.0956}}`.
//! The ECB publishes on TARGET business days only (~16:00 CET); a weekend/holiday date returns
//! the previous business day, reported as `business_date`. Docs: https://frankfurter.dev/

use crate::http::{HttpClient, DEFAULT_TIMEOUT};
use async_trait::async_trait;
use bdm_config::{Loaded, Redacted, VendorStatus};
use bdm_ports::{FxRate, FxRates, PortHandle, PortResult, ProviderError, Registration, VendorMeta};
use chrono::{NaiveDate, NaiveTime};
use rust_decimal::Decimal;
use serde_json::Value;
use std::{str::FromStr, sync::Arc};

const ID: &str = "frankfurter";
pub const BASE_URL: &str = "https://api.frankfurter.dev/v1";

/// Push this vendor's registration if it is active.
pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status(ID) != VendorStatus::Active {
        return;
    }
    let http = HttpClient::new(ID, DEFAULT_TIMEOUT);
    out.push(
        Registration::new(meta(loaded))
            .global_port(PortHandle::Fx(Arc::new(Frankfurter::new(http, BASE_URL)))),
    );
}

fn meta(loaded: &Loaded) -> VendorMeta {
    let e = loaded.registry.vendors.get(ID);
    VendorMeta {
        id: ID.into(),
        display_name: e.map_or("Frankfurter (ECB)".into(), |e| e.display_name.clone()),
        requires_key: false,
        signup_url: None,
        rpc_features: Default::default(),
    }
}

pub struct Frankfurter {
    http: HttpClient,
    base_url: String,
}

impl Frankfurter {
    pub fn new(http: HttpClient, base_url: &str) -> Self {
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
        }
    }
}

#[async_trait]
impl FxRates for Frankfurter {
    async fn rate(&self, base: &str, quote: &str, date: Option<NaiveDate>) -> PortResult<FxRate> {
        let path = date.map_or("latest".to_owned(), |d| d.to_string());
        let url = Redacted::new(format!(
            "{}/{path}?base={base}&symbols={quote}",
            self.base_url
        ));
        let v = self
            .http
            .get_json(
                &url,
                if date.is_some() {
                    "historical"
                } else {
                    "latest"
                },
                &[],
            )
            .await
            .map_err(|e| match e {
                // Unknown currency: let the next FX vendor try.
                ProviderError::NotFound | ProviderError::Invalid(_) => {
                    ProviderError::Unsupported(format!("ECB does not publish {base}/{quote}"))
                }
                e => e,
            })?;
        parse(&v, base, quote)
    }
}

/// JSON number → exact decimal via its shortest round-trip text (ECB rates have ≤ 6 significant
/// digits, so no precision is lost; no float arithmetic is done).
fn json_decimal(v: &Value) -> Option<Decimal> {
    let s = v.as_number()?.to_string();
    Decimal::from_str(&s)
        .or_else(|_| Decimal::from_scientific(&s))
        .ok()
}

#[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
fn parse(v: &Value, base: &str, quote: &str) -> PortResult<FxRate> {
    let business_date = v["date"]
        .as_str()
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .ok_or_else(|| ProviderError::Transient("frankfurter: response has no date".into()))?;
    let rate = json_decimal(&v["rates"][quote]).ok_or_else(|| {
        ProviderError::Unsupported(format!("ECB does not publish {base}/{quote}"))
    })?;
    Ok(FxRate {
        base: base.to_owned(),
        quote: quote.to_owned(),
        rate,
        business_date,
        source: ID.into(),
        as_of: business_date.and_time(NaiveTime::MIN).and_utc(),
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

    #[test]
    fn parses_exact_decimal_and_business_date() {
        let v = json!({"amount": 1.0, "base": "EUR", "date": "2026-09-18", "rates": {"USD": 1.0956, "JPY": 162.5}});
        let r = parse(&v, "EUR", "USD").unwrap();
        assert_eq!(r.rate, Decimal::from_str("1.0956").unwrap());
        assert_eq!(r.business_date.to_string(), "2026-09-18");
        assert!(matches!(
            parse(&v, "EUR", "XYZ"),
            Err(ProviderError::Unsupported(_))
        ));
    }

    #[tokio::test]
    async fn weekend_request_returns_previous_business_day() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/2026-09-20"))
            .and(query_param("base", "EUR"))
            .and(query_param("symbols", "USD"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"amount": 1.0, "base": "EUR", "date": "2026-09-18", "rates": {"USD": 1.1702}}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/latest"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({"message": "not found"})))
            .mount(&server)
            .await;
        let fx = Frankfurter::new(
            HttpClient::new(ID, DEFAULT_TIMEOUT),
            &format!("{}/v1", server.uri()),
        );
        let d = NaiveDate::from_ymd_opt(2026, 9, 20).unwrap();
        let r = fx.rate("EUR", "USD", Some(d)).await.unwrap();
        assert_eq!(
            r.business_date,
            NaiveDate::from_ymd_opt(2026, 9, 18).unwrap()
        );
        assert_eq!(r.rate.to_string(), "1.1702");
        // unknown currency → Unsupported, so routing fails over to the next FX vendor
        assert!(matches!(
            fx.rate("EUR", "XYZ", None).await,
            Err(ProviderError::Unsupported(_))
        ));
    }
}
