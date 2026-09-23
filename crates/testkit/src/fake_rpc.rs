use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

type Handler = Arc<dyn Fn(&Value) -> Result<Value, (i64, String)> + Send + Sync>;

/// Injected failure for the next request(s).
#[derive(Debug, Clone)]
pub enum Failure {
    /// HTTP status with an optional `Retry-After` (seconds).
    Http(u16, Option<u64>),
    /// Delay the response (use longer than the client timeout).
    Timeout(Duration),
    /// JSON-RPC error object with HTTP 200.
    JsonRpc(i64, String),
}

#[derive(Default)]
struct Inner {
    handlers: Mutex<HashMap<String, Handler>>,
    failures: Mutex<VecDeque<Failure>>,
    calls: Mutex<Vec<(String, Value)>>,
}

/// In-process JSON-RPC 2.0 server on `127.0.0.1:<random>`. Accepts any path/query, so URLs with
/// keys embedded (`/v2/<key>`, `?api-key=`) work. Unknown methods answer `-32601`.
pub struct FakeJsonRpc {
    url: String,
    inner: Arc<Inner>,
    task: tokio::task::JoinHandle<()>,
}

impl FakeJsonRpc {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let inner = Arc::new(Inner::default());
        let app = Router::new().fallback(handle).with_state(inner.clone());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { url, inner, task }
    }

    /// Base URL, e.g. `http://127.0.0.1:41234`.
    pub fn url(&self) -> String {
        self.url.clone()
    }

    /// Answer `method` with a fixed `result`.
    pub fn on(&self, method: &str, result: Value) -> &Self {
        self.on_fn(method, move |_| Ok(result.clone()))
    }

    /// Answer `method` with a function of its params; `Err((code, msg))` is a JSON-RPC error.
    pub fn on_fn<F>(&self, method: &str, f: F) -> &Self
    where
        F: Fn(&Value) -> Result<Value, (i64, String)> + Send + Sync + 'static,
    {
        self.inner
            .handlers
            .lock()
            .unwrap()
            .insert(method.to_owned(), Arc::new(f));
        self
    }

    /// Apply `failure` to the next `n` HTTP requests (a batch counts as one).
    pub fn fail_next(&self, n: usize, failure: Failure) -> &Self {
        let mut q = self.inner.failures.lock().unwrap();
        q.extend(std::iter::repeat_n(failure, n));
        self
    }

    /// Every JSON-RPC call received (method, params), in order.
    pub fn calls(&self) -> Vec<(String, Value)> {
        self.inner.calls.lock().unwrap().clone()
    }
}

impl Drop for FakeJsonRpc {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle(State(inner): State<Arc<Inner>>, body: Bytes) -> Response {
    let failure = inner.failures.lock().unwrap().pop_front();
    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid json").into_response(),
    };
    match failure {
        Some(Failure::Http(status, retry_after)) => {
            let mut resp =
                (StatusCode::from_u16(status).unwrap(), "injected error").into_response();
            if let Some(s) = retry_after {
                resp.headers_mut().insert(
                    "retry-after",
                    HeaderValue::from_str(&s.to_string()).unwrap(),
                );
            }
            return resp;
        }
        Some(Failure::Timeout(d)) => tokio::time::sleep(d).await,
        Some(Failure::JsonRpc(code, msg)) => {
            let err = |r: &Value| json!({"jsonrpc": "2.0", "id": r["id"], "error": {"code": code, "message": msg}});
            return Json(match &req {
                Value::Array(b) => Value::Array(b.iter().map(err).collect()),
                one => err(one),
            })
            .into_response();
        }
        None => {}
    }
    let one = |r: &Value| {
        let method = r["method"].as_str().unwrap_or_default().to_owned();
        let params = r.get("params").cloned().unwrap_or(Value::Null);
        inner
            .calls
            .lock()
            .unwrap()
            .push((method.clone(), params.clone()));
        let handler = inner.handlers.lock().unwrap().get(&method).cloned();
        let outcome = match handler {
            Some(h) => h(&params),
            None => Err((-32601, format!("the method {method} does not exist"))),
        };
        match outcome {
            Ok(result) => json!({"jsonrpc": "2.0", "id": r["id"], "result": result}),
            Err((code, message)) => {
                json!({"jsonrpc": "2.0", "id": r["id"], "error": {"code": code, "message": message}})
            }
        }
    };
    Json(match &req {
        Value::Array(batch) => Value::Array(batch.iter().map(one).collect()),
        single => one(single),
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn post(url: &str, body: Value) -> (u16, Value) {
        // Minimal client without reqwest: raw HTTP/1.1 over TCP.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let addr = url.trim_start_matches("http://");
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        let b = body.to_string();
        let req = format!(
            "POST /v2/key HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
            b.len()
        );
        s.write_all(req.as_bytes()).await.unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        let status = out[9..12].parse().unwrap();
        let body = out.split("\r\n\r\n").nth(1).unwrap_or("");
        (status, serde_json::from_str(body).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn batch_errors_and_injection() {
        let f = FakeJsonRpc::start().await;
        f.on("a", json!(1))
            .on_fn("b", |p| Err((-32602, format!("bad {p}"))));
        let (st, v) = post(
            &f.url(),
            json!([{"id":1,"method":"a","params":[]},{"id":2,"method":"b","params":[7]}]),
        )
        .await;
        assert_eq!(st, 200);
        assert_eq!(v[0]["result"], json!(1));
        assert_eq!(v[1]["error"]["code"], json!(-32602));
        f.fail_next(1, Failure::Http(429, Some(3)));
        assert_eq!(post(&f.url(), json!({"id":1,"method":"a"})).await.0, 429);
        let (_, v) = post(&f.url(), json!({"id":1,"method":"zzz"})).await;
        assert_eq!(v["error"]["code"], json!(-32601));
        assert_eq!(f.calls().len(), 3);
    }
}
