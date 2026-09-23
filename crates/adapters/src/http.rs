//! Shared vendor HTTP client: timeouts, metering, rate-limit headers, error mapping, and secret
//! scrubbing. Never logs or returns a full URL; only `vendor` + method label.

use chrono::{DateTime, Utc};
use ems_config::Redacted;
use ems_ports::{
    metering::{self, RateLimitSnapshot},
    PortResult, ProviderError, WindowKind,
};
use reqwest::{header::HeaderMap, Method};
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Default per-request timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// HTTP client bound to one vendor.
#[derive(Clone)]
pub struct HttpClient {
    vendor: String,
    client: reqwest::Client,
    secrets: Vec<String>,
}

impl HttpClient {
    pub fn new(vendor: impl Into<String>, timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client");
        Self {
            vendor: vendor.into(),
            client,
            secrets: Vec::new(),
        }
    }

    /// Extra values to scrub from error messages (e.g. `Loaded::secret_values()`).
    pub fn with_secrets<I: IntoIterator<Item = S>, S: Into<String>>(mut self, secrets: I) -> Self {
        self.secrets
            .extend(secrets.into_iter().map(Into::into).filter(|s| s.len() >= 6));
        self
    }

    pub fn vendor(&self) -> &str {
        &self.vendor
    }

    /// Replace every known secret, plus key-looking URL parts, with `***`.
    pub fn scrub(&self, text: &str, url: &Redacted<String>) -> String {
        url_secrets(url.expose())
            .iter()
            .map(String::as_str)
            .chain(self.secrets.iter().map(String::as_str))
            .fold(text.to_owned(), |acc, s| acc.replace(s, "***"))
    }

    pub async fn post_json(
        &self,
        url: &Redacted<String>,
        label: &str,
        body: &Value,
    ) -> PortResult<Value> {
        self.request(Method::POST, url, label, &[], Some(body))
            .await
    }

    pub async fn get_json(
        &self,
        url: &Redacted<String>,
        label: &str,
        headers: &[(&str, &str)],
    ) -> PortResult<Value> {
        self.request(Method::GET, url, label, headers, None).await
    }

    /// Send one request. Records metering (`label`) and rate-limit headers; maps failures into
    /// the routing taxonomy. `headers` may carry secrets and are never logged.
    pub async fn request(
        &self,
        method: Method,
        url: &Redacted<String>,
        label: &str,
        headers: &[(&str, &str)],
        body: Option<&Value>,
    ) -> PortResult<Value> {
        metering::record_request(&self.vendor, label);
        let mut req = self.client.request(method, url.expose());
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| self.map_send_error(e, url, label))?;

        let snapshot = parse_rate_limit(resp.headers(), SystemTime::now());
        if snapshot != RateLimitSnapshot::default() {
            metering::record_rate_limit(&self.vendor, &snapshot);
        }
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            tracing::debug!(vendor = %self.vendor, label, status = status.as_u16(), "vendor HTTP error");
            return Err(ProviderError::from_http_status(
                status.as_u16(),
                &self.scrub(&text, url),
                snapshot.retry_after,
            ));
        }
        resp.json::<Value>().await.map_err(|e| {
            tracing::debug!(vendor = %self.vendor, label, "invalid JSON from vendor");
            ProviderError::Transient(self.scrub(&format!("invalid JSON: {}", e.without_url()), url))
        })
    }

    fn map_send_error(
        &self,
        e: reqwest::Error,
        url: &Redacted<String>,
        label: &str,
    ) -> ProviderError {
        tracing::debug!(vendor = %self.vendor, label, timeout = e.is_timeout(), "vendor request failed");
        if e.is_timeout() {
            ProviderError::Transient("timeout".into())
        } else if e.is_connect() {
            ProviderError::Transient("connect error".into())
        } else {
            ProviderError::Transient(self.scrub(&e.without_url().to_string(), url))
        }
    }
}

/// Key-looking parts of a URL (path segments and query values of 10+ chars) to scrub.
fn url_secrets(url: &str) -> Vec<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let path_parts = path.split('/').skip(1); // skip host
    let query_values = query
        .split('&')
        .filter_map(|kv| kv.split_once('=').map(|(_, v)| v));
    let mut out: Vec<String> = path_parts
        .chain(query_values)
        .filter(|s| s.len() >= 10)
        .map(str::to_owned)
        .collect();
    // Also the host label when it looks like an endpoint token (QuickNode `name.chain.quiknode.pro`).
    if let Some(host) = path.split('/').next() {
        out.extend(host.split('.').filter(|p| p.len() >= 16).map(str::to_owned));
    }
    out
}

/// Parse rate-limit headers: `X-RateLimit-*`, IETF `RateLimit-*`, structured `RateLimit` /
/// `RateLimit-Policy` (`limit=,remaining=,reset=` or `q=,w=` / `r=,t=`), and `Retry-After`.
pub fn parse_rate_limit(headers: &HeaderMap, now: SystemTime) -> RateLimitSnapshot {
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
    };
    let num = |name: &str| get(name).and_then(first_number);

    let mut s = RateLimitSnapshot {
        limit: num("x-ratelimit-limit").or_else(|| num("ratelimit-limit")),
        remaining: num("x-ratelimit-remaining").or_else(|| num("ratelimit-remaining")),
        reset_after: num("x-ratelimit-reset")
            .or_else(|| num("ratelimit-reset"))
            .map(|n| reset_to_delta(n, now)),
        window: None,
        retry_after: get("retry-after").and_then(|v| parse_retry_after(v, now)),
    };

    for (name, is_policy) in [("ratelimit", false), ("ratelimit-policy", true)] {
        if let Some(v) = get(name) {
            for (k, val) in structured_pairs(v) {
                match (k.as_str(), is_policy) {
                    ("limit" | "q", _) => s.limit = s.limit.or(Some(val)),
                    ("remaining" | "r", false) => s.remaining = s.remaining.or(Some(val)),
                    ("reset" | "t", false) => {
                        s.reset_after = s.reset_after.or(Some(Duration::from_secs(val)))
                    }
                    ("w", true) => s.window = window_kind(val),
                    _ => {}
                }
            }
        }
    }
    s
}

fn first_number(v: &str) -> Option<u64> {
    v.split([',', ';']).next()?.trim().parse().ok()
}

/// `key=value` pairs from structured header values; ignores quoted policy names.
fn structured_pairs(v: &str) -> Vec<(String, u64)> {
    v.split([',', ';'])
        .filter_map(|p| p.split_once('='))
        .filter_map(|(k, val)| {
            Some((
                k.trim().to_lowercase(),
                val.trim().trim_matches('"').parse().ok()?,
            ))
        })
        .collect()
}

/// `X-RateLimit-Reset` is delta-seconds (IETF) or an epoch timestamp (GitHub-style, s or ms).
fn reset_to_delta(n: u64, now: SystemTime) -> Duration {
    let now_s = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    if n > 1_000_000_000_000 {
        Duration::from_secs((n / 1000).saturating_sub(now_s))
    } else if n > 1_000_000_000 {
        Duration::from_secs(n.saturating_sub(now_s))
    } else {
        Duration::from_secs(n)
    }
}

fn parse_retry_after(v: &str, now: SystemTime) -> Option<Duration> {
    if let Ok(secs) = v.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let at: DateTime<Utc> = DateTime::parse_from_rfc2822(v).ok()?.with_timezone(&Utc);
    let now: DateTime<Utc> = now.into();
    Some((at - now).to_std().unwrap_or_default())
}

fn window_kind(secs: u64) -> Option<WindowKind> {
    match secs {
        1 => Some(WindowKind::Second),
        60 => Some(WindowKind::Minute),
        86_400 => Some(WindowKind::Day),
        2_419_200..=2_678_400 => Some(WindowKind::Month),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderName, HeaderValue};

    fn h(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.insert(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        m
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn x_ratelimit_delta_and_epoch() {
        let s = parse_rate_limit(
            &h(&[
                ("x-ratelimit-limit", "100"),
                ("x-ratelimit-remaining", "42"),
                ("x-ratelimit-reset", "30"),
            ]),
            at(1_700_000_000),
        );
        assert_eq!(
            (s.limit, s.remaining, s.reset_after),
            (Some(100), Some(42), Some(Duration::from_secs(30)))
        );

        let epoch = parse_rate_limit(
            &h(&[("x-ratelimit-reset", "1700000060")]),
            at(1_700_000_000),
        );
        assert_eq!(epoch.reset_after, Some(Duration::from_secs(60)));
        let ms = parse_rate_limit(
            &h(&[("x-ratelimit-reset", "1700000090000")]),
            at(1_700_000_000),
        );
        assert_eq!(ms.reset_after, Some(Duration::from_secs(90)));
    }

    #[test]
    fn ietf_fields_and_lists() {
        let s = parse_rate_limit(
            &h(&[
                ("ratelimit-limit", "10, 10;w=1"),
                ("ratelimit-remaining", "3"),
                ("ratelimit-reset", "1"),
            ]),
            at(0),
        );
        assert_eq!(
            (s.limit, s.remaining, s.reset_after),
            (Some(10), Some(3), Some(Duration::from_secs(1)))
        );
    }

    #[test]
    fn structured_headers() {
        let s = parse_rate_limit(
            &h(&[
                ("ratelimit", "\"default\";r=50;t=30"),
                ("ratelimit-policy", "\"default\";q=100;w=60"),
            ]),
            at(0),
        );
        assert_eq!(s.remaining, Some(50));
        assert_eq!(s.reset_after, Some(Duration::from_secs(30)));
        assert_eq!(s.limit, Some(100));
        assert_eq!(s.window, Some(WindowKind::Minute));

        let old = parse_rate_limit(
            &h(&[("ratelimit", "limit=100, remaining=5, reset=12")]),
            at(0),
        );
        assert_eq!(
            (old.limit, old.remaining, old.reset_after),
            (Some(100), Some(5), Some(Duration::from_secs(12)))
        );
    }

    #[test]
    fn retry_after_seconds_and_date() {
        assert_eq!(
            parse_rate_limit(&h(&[("retry-after", "7")]), at(0)).retry_after,
            Some(Duration::from_secs(7))
        );
        // Wed, 21 Oct 2015 07:28:00 GMT = 1445412480
        let s = parse_rate_limit(
            &h(&[("retry-after", "Wed, 21 Oct 2015 07:28:00 GMT")]),
            at(1_445_412_470),
        );
        assert_eq!(s.retry_after, Some(Duration::from_secs(10)));
        let past = parse_rate_limit(
            &h(&[("retry-after", "Wed, 21 Oct 2015 07:28:00 GMT")]),
            at(1_445_412_490),
        );
        assert_eq!(past.retry_after, Some(Duration::ZERO));
        assert_eq!(
            parse_rate_limit(&h(&[("retry-after", "soon")]), at(0)).retry_after,
            None
        );
    }

    #[test]
    fn no_headers_is_default() {
        assert_eq!(
            parse_rate_limit(&HeaderMap::new(), at(0)),
            RateLimitSnapshot::default()
        );
    }

    #[test]
    fn scrubs_url_parts_and_extra_secrets() {
        let c = HttpClient::new("alchemy", DEFAULT_TIMEOUT).with_secrets(["sk_extra_secret"]);
        let url =
            Redacted::new("https://eth-mainnet.g.alchemy.com/v2/abcdef0123456789".to_string());
        let out = c.scrub("bad key abcdef0123456789 and sk_extra_secret", &url);
        assert_eq!(out, "bad key *** and ***");
        let q =
            Redacted::new("https://mainnet.helius-rpc.com/?api-key=hel-0123456789abc".to_string());
        assert_eq!(c.scrub("key hel-0123456789abc", &q), "key ***");
    }
}
