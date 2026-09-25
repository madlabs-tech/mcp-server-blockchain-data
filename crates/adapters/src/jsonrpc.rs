//! Generic JSON-RPC 2.0 client over [`HttpClient`] with vendor error → routing taxonomy mapping.

use crate::http::HttpClient;
use bdm_config::Redacted;
use bdm_ports::{metering, PortResult, ProviderError};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

/// `v[key]` as an array, or `Transient` when the vendor answered with a different shape (an
/// empty list would silently stand for "nothing held"; a fail-over is the honest answer).
#[cfg(any(feature = "alchemy", feature = "helius"))]
pub(crate) fn array_field<'a>(v: &'a Value, key: &str, what: &str) -> PortResult<&'a Vec<Value>> {
    v.get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| ProviderError::Transient(format!("malformed {what}: {key} is not an array")))
}

pub struct JsonRpcClient {
    http: HttpClient,
    url: Redacted<String>,
    next_id: AtomicU64,
}

impl JsonRpcClient {
    pub fn new(http: HttpClient, url: Redacted<String>) -> Self {
        Self {
            http,
            url,
            next_id: AtomicU64::new(1),
        }
    }

    pub fn vendor(&self) -> &str {
        self.http.vendor()
    }

    /// One call. Returns `result` (a `null` result stays `Ok(Null)`; callers decide NotFound).
    pub async fn request(&self, method: &str, params: Value) -> PortResult<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let resp = self.http.post_json(&self.url, method, &body).await?;
        self.unwrap_response(resp)
    }

    /// Batch call; one result per request, in request order. Every method is metered.
    #[allow(clippy::indexing_slicing)] // serde_json::Value[..] reads return Null, never panic
    pub async fn batch(&self, calls: &[(&str, Value)]) -> PortResult<Vec<PortResult<Value>>> {
        let Some((first, _)) = calls.first() else {
            return Ok(Vec::new());
        };
        let base = self
            .next_id
            .fetch_add(calls.len() as u64, Ordering::Relaxed);
        let body: Vec<Value> = calls
            .iter()
            .enumerate()
            .map(|(i, (m, p))| json!({"jsonrpc": "2.0", "id": base + i as u64, "method": m, "params": p}))
            .collect();
        for (m, _) in calls.iter().skip(1) {
            metering::record_request(self.vendor(), m);
        }
        let resp = self
            .http
            .post_json(&self.url, first, &Value::Array(body))
            .await?;
        let Value::Array(items) = resp else {
            return Err(ProviderError::Transient(
                "batch response is not an array".into(),
            ));
        };
        let mut out: Vec<PortResult<Value>> = (0..calls.len())
            .map(|_| Err(ProviderError::Transient("missing batch item".into())))
            .collect();
        for item in items {
            if let Some(i) = item["id"].as_u64().and_then(|id| id.checked_sub(base)) {
                if let Some(slot) = out.get_mut(i as usize) {
                    *slot = self.unwrap_response(item);
                }
            }
        }
        Ok(out)
    }

    fn unwrap_response(&self, mut resp: Value) -> PortResult<Value> {
        if let Some(err) = resp.get("error").filter(|e| !e.is_null()) {
            let code = err["code"].as_i64().unwrap_or(0);
            let msg = self
                .http
                .scrub(err["message"].as_str().unwrap_or(""), &self.url);
            return Err(map_rpc_error(code, &msg));
        }
        match resp.get_mut("result") {
            Some(r) => Ok(r.take()),
            None => Err(ProviderError::Transient(
                "JSON-RPC response has no result".into(),
            )),
        }
    }
}

/// Map a JSON-RPC error into the routing taxonomy (message must already be scrubbed).
pub fn map_rpc_error(code: i64, message: &str) -> ProviderError {
    let m = message.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| m.contains(n));
    // Deterministic request problems: never fail over.
    if has(&[
        "execution reverted",
        "nonce too low",
        "insufficient funds",
        "already known",
        "replacement transaction underpriced",
    ]) {
        return ProviderError::Invalid(message.to_owned());
    }
    // getLogs range/size limits: plan-specific, callers split the range.
    if has(&["block range", "response size", "more than"]) && !has(&["rate"]) {
        return ProviderError::Invalid(message.to_owned());
    }
    match code {
        -32601 => return ProviderError::Unsupported(message.to_owned()),
        -32602 | -32600 => return ProviderError::Invalid(message.to_owned()),
        _ => {}
    }
    if has(&["credit", "quota", "monthly", "capacity"]) {
        return ProviderError::QuotaExhausted { resets_at: None };
    }
    if matches!(code, -32005 | -32029 | 429)
        || has(&["rate limit", "too many requests", "exceeded"])
    {
        return ProviderError::RateLimited { retry_after: None };
    }
    ProviderError::Transient(format!("rpc error {code}: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_mapping() {
        use ProviderError::*;
        assert!(matches!(map_rpc_error(-32005, "limit"), RateLimited { .. }));
        assert!(matches!(
            map_rpc_error(-32000, "Too Many Requests"),
            RateLimited { .. }
        ));
        assert!(matches!(
            map_rpc_error(-32000, "monthly capacity reached"),
            QuotaExhausted { .. }
        ));
        assert!(matches!(
            map_rpc_error(-32000, "out of compute credits"),
            QuotaExhausted { .. }
        ));
        assert!(matches!(
            map_rpc_error(-32601, "method not found"),
            Unsupported(_)
        ));
        assert!(matches!(map_rpc_error(-32602, "bad params"), Invalid(_)));
        assert!(matches!(
            map_rpc_error(3, "execution reverted: nope"),
            Invalid(_)
        ));
        assert!(matches!(map_rpc_error(-32000, "nonce too low"), Invalid(_)));
        assert!(matches!(
            map_rpc_error(
                -32600,
                "Log response size exceeded. use up to a 2K block range"
            ),
            Invalid(_)
        ));
        assert!(matches!(
            map_rpc_error(-32603, "internal error"),
            Transient(_)
        ));
        assert!(matches!(
            map_rpc_error(-32000, "header not found"),
            Transient(_)
        ));
    }
}
