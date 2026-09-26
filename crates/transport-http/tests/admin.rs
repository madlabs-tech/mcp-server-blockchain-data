#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
//! Admin API (T1.D4): auth + CSRF header, locked-by-env edits refused, secrets never echoed,
//! reorder → router.swap → the next call uses the new primary, clients CRUD, quota, call log.
//! No network: the "vendors" are in-process fake RPC ports.

use async_trait::async_trait;
use bdm_app::{App, Catalog, Ctx, Domain, OpOutput, Operation, Profile};
use bdm_config::{ConfigDir, ConfigLoader, EnvSource, Loaded};
use bdm_domain::DomainError;
use bdm_ports::{Capability, EvmRpc, PortHandle, PortResult, Registration, VendorMeta};
use bdm_routing::{ProviderRegistry, Router, RouterOptions, RoutingTable};
use bdm_store::{QuotaEngine, Store};
use bdm_transport_http::{admin_router, public_router, AdminState, HttpState};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";

struct Named(&'static str);

#[async_trait]
impl EvmRpc for Named {
    fn chain_id(&self) -> u64 {
        1
    }
    async fn request(&self, _method: &str, _params: Value) -> PortResult<Value> {
        Ok(json!(self.0))
    }
}

fn registry(_: &Loaded) -> ProviderRegistry {
    let eth: bdm_domain::ChainId = "eip155:1".parse().unwrap();
    let reg = |id: &'static str| {
        Registration::new(VendorMeta {
            id: id.into(),
            display_name: id.into(),
            ..Default::default()
        })
        .chain_port(eth.clone(), PortHandle::EvmRpc(Arc::new(Named(id))))
    };
    ProviderRegistry::new(vec![reg("alpha"), reg("beta")])
}

#[derive(Deserialize, JsonSchema)]
struct NoIn {}

#[derive(Serialize, JsonSchema)]
struct Head {
    answer: Value,
}

/// Routes one `evm_rpc` request on Ethereum and reports who answered.
struct WhoAnswers;

#[async_trait]
impl Operation for WhoAnswers {
    type Input = NoIn;
    type Output = Head;
    const NAME: &'static str = "chain_who_answers";
    const DOMAIN: Domain = Domain::Chain;
    const DESCRIPTION: &'static str = "test";
    const PROFILES: &'static [Profile] = &[Profile::Payments];

    async fn execute(&self, ctx: &Ctx, _: NoIn) -> Result<OpOutput<Head>, DomainError> {
        let eth = ctx.chain("ethereum")?.id.clone();
        let r = ctx
            .router()
            .failover::<dyn EvmRpc, _, _, _>(
                ctx.route(Capability::EvmRpc).chain(eth),
                |p| async move { p.request("eth_blockNumber", json!([])).await },
            )
            .await
            .map_err(|e| e.error)?;
        Ok(OpOutput::from_routed(r, |answer| Head { answer }))
    }
}

const CONFIG: &str = r#"
[server]
tool_profile = "all"

[custom_rpc.alpha]
chain = "eip155:1"
url = "http://127.0.0.1:9/alpha"

[custom_rpc.beta]
chain = "eip155:1"
url = "http://127.0.0.1:9/beta"

[routing.chains."eip155:1"]
evm_rpc = ["alpha", "beta"]
"#;

struct Harness {
    url: String,
    _dir: tempfile::TempDir,
    http: reqwest::Client,
}

impl Harness {
    async fn start() -> Self {
        Self::start_with(CONFIG).await
    }

    async fn start_with(config: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), config).unwrap();
        let loader = ConfigLoader::new(
            ConfigDir::new(dir.path()),
            EnvSource::from_pairs([("ODM__VENDORS__ALCHEMY__CAP__MONTHLY", "100")]),
        )
        .unwrap();
        let loaded = loader.load().unwrap();
        let store = Store::open_in_memory().unwrap();
        let router = Router::new(
            RoutingTable {
                registry: registry(&loaded),
                config: Arc::new(loaded),
            },
            Arc::new(store.clone()),
            RouterOptions::default(),
        );
        let mut catalog = Catalog::new();
        catalog.register(WhoAnswers);
        let app =
            Arc::new(App::new(catalog, router.clone(), 100).with_observer(Arc::new(store.clone())));
        let admin = AdminState::new(
            app.clone(),
            store.clone(),
            QuotaEngine::new(router, store.clone()),
            Arc::new(loader),
            Arc::new(registry),
            TOKEN,
        );
        let http = public_router(HttpState { app, auth: None }).merge(admin_router(admin));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, http).await });
        Self {
            url,
            _dir: dir,
            http: reqwest::Client::new(),
        }
    }

    fn admin(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.url))
            .bearer_auth(TOKEN)
            .header("X-BDM-Admin", "1")
    }

    async fn edit(&self, edits: Value) -> (u16, String) {
        let r = self
            .admin(reqwest::Method::PUT, "/admin/api/config")
            .json(&json!({ "edits": edits }))
            .send()
            .await
            .unwrap();
        (r.status().as_u16(), r.text().await.unwrap())
    }

    async fn who(&self) -> Value {
        self.http
            .post(format!("{}/v1/chain/who_answers", self.url))
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn admin_requires_token_and_csrf_header() {
    let h = Harness::start().await;
    let url = format!("{}/admin/api/health", h.url);
    let r = h
        .http
        .get(&url)
        .header("X-BDM-Admin", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let r = h
        .http
        .get(&url)
        .bearer_auth("wrong")
        .header("X-BDM-Admin", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let r = h.http.get(&url).bearer_auth(TOKEN).send().await.unwrap();
    assert_eq!(r.status(), 403, "CSRF header required");
    let r = h
        .admin(reqwest::Method::GET, "/admin/api/health")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    // the dashboard shell itself is static and public (it holds no data)
    let r = h
        .http
        .get(format!("{}/dashboard", h.url))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .contains("default-src 'self'"));
}

#[tokio::test]
async fn dashboard_fonts_are_served_by_name_only() {
    let h = Harness::start().await;
    for name in ["orbitron", "jetbrains-mono", "share-tech-mono"] {
        let r = h
            .http
            .get(format!("{}/dashboard/fonts/{name}.woff2", h.url))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "{name}");
        assert_eq!(r.headers()["content-type"], "font/woff2");
        assert_eq!(r.headers()["x-content-type-options"], "nosniff");
        assert!(r.bytes().await.unwrap().starts_with(b"wOF2"));
    }
    for bad in ["nope.woff2", "..%2Fapp.js", "%2E%2E%2F%2E%2E%2FCargo.toml"] {
        let r = h
            .http
            .get(format!("{}/dashboard/fonts/{bad}", h.url))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 404, "{bad}");
    }
    let r = h
        .http
        .get(format!("{}/dashboard", h.url))
        .send()
        .await
        .unwrap();
    assert!(r.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .contains("default-src 'self'"));
}

#[tokio::test]
async fn locked_by_env_edit_is_refused() {
    let h = Harness::start().await;
    let (status, body) = h
        .edit(json!([{ "path": ["vendors", "alchemy", "cap", "monthly"], "value": 5 }]))
        .await;
    assert_eq!(status, 422);
    assert!(
        body.contains("ODM__VENDORS__ALCHEMY__CAP__MONTHLY"),
        "{body}"
    );
    // budget endpoint too
    let r = h
        .admin(reqwest::Method::POST, "/admin/api/vendors/alchemy/budget")
        .json(&json!({ "which": "cap", "window": "monthly", "value": 7 }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 422);
    // and the config view reports the lock
    let cfg: Value = h
        .admin(reqwest::Method::GET, "/admin/api/config")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(cfg["locked"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["env"] == "ODM__VENDORS__ALCHEMY__CAP__MONTHLY"));
}

#[tokio::test]
async fn secrets_are_never_echoed() {
    let h = Harness::start().await;
    let secret = "hel_secret_value_987654";
    let (status, body) = h
        .edit(json!([{ "path": ["keys", "helius", "api_key"], "value": secret }]))
        .await;
    assert_eq!(status, 200, "{body}");
    assert!(!body.contains(secret));
    let cfg = h
        .admin(reqwest::Method::GET, "/admin/api/config")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!cfg.contains(secret), "config view leaked a key");
    assert!(
        !cfg.contains("http://127.0.0.1:9/alpha"),
        "custom RPC URLs are secrets too"
    );
    let cfg: Value = serde_json::from_str(&cfg).unwrap();
    let helius = cfg["vendors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == "helius")
        .unwrap();
    assert_eq!(helius["keys"][0]["set"], true);
    assert_eq!(helius["tier"], 2);
    for v in cfg["vendors"].as_array().unwrap() {
        assert!(
            (1..=4).contains(&v["tier"].as_u64().unwrap_or(0)),
            "vendor {} has no tier",
            v["id"]
        );
    }
    assert!(cfg["settings"].get("keys").is_none());
    for path in [
        "/admin/api/quota",
        "/admin/api/health",
        "/admin/api/quota.csv",
    ] {
        let t = h
            .admin(reqwest::Method::GET, path)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(!t.contains(secret), "{path} leaked a key");
    }
    // validate-only never writes
    let r = h
        .admin(reqwest::Method::POST, "/admin/api/config/validate")
        .json(
            &json!({ "edits": [{ "path": ["routing", "defaults", "price"], "value": ["nope"] }] }),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 422);
}

#[tokio::test]
async fn reorder_swaps_router_and_next_call_uses_new_primary() {
    let h = Harness::start().await;
    let first = h.who().await;
    assert_eq!(first["meta"]["provider"], "alpha", "{first}");
    let (status, body) = h
        .edit(json!([{ "path": ["routing", "chains", "eip155:1", "evm_rpc"], "value": ["beta", "alpha"] }]))
        .await;
    assert_eq!(status, 200, "{body}");
    let next = h.who().await;
    assert_eq!(next["meta"]["provider"], "beta", "{next}");
    assert_eq!(next["data"]["answer"], "beta");

    // effective order in the config view reflects the swap
    let cfg: Value = h
        .admin(reqwest::Method::GET, "/admin/api/config")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let eth = &cfg["orders"]["evm_rpc"]["chains"]["eip155:1"];
    assert_eq!(eth["vendors"], json!(["beta", "alpha"]));
    assert_eq!(eth["level"], "chain");
    assert_eq!(eth["effective"][0]["usable"], true);

    // REST calls land in the call log
    let calls: Value = h
        .admin(reqwest::Method::GET, "/admin/api/calls")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rows = calls["calls"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["provider"], "beta");
    assert_eq!(rows[0]["chain"], "eip155:1");
}

#[tokio::test]
async fn clients_crud_and_quota_views() {
    let h = Harness::start().await;
    let r = h
        .admin(reqwest::Method::POST, "/admin/api/clients")
        .json(&json!({ "name": "acme" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let created: Value = r.json().await.unwrap();
    let key = created["key"].as_str().unwrap().to_owned();
    let id = created["client"]["id"].as_str().unwrap().to_owned();
    assert!(key.starts_with("odm_"));

    let r = h
        .admin(reqwest::Method::PATCH, &format!("/admin/api/clients/{id}"))
        .json(&json!({ "limits": { "daily_requests": 5 } }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let list = h
        .admin(reqwest::Method::GET, "/admin/api/clients")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!list.contains(&key), "keys are shown once only");
    let list: Value = serde_json::from_str(&list).unwrap();
    assert_eq!(list["clients"][0]["effective_limits"]["daily_requests"], 5);
    assert_eq!(
        list["clients"][0]["effective_limits"]["requests_per_minute"],
        30
    );

    let r = h
        .admin(reqwest::Method::DELETE, &format!("/admin/api/clients/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let list: Value = h
        .admin(reqwest::Method::GET, "/admin/api/clients")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["clients"][0]["active"], false);

    let q: Value = h
        .admin(reqwest::Method::GET, "/admin/api/quota")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let vendors = q["vendors"].as_array().unwrap();
    assert!(
        vendors.iter().any(|v| v["vendor"] == "public"),
        "keyless vendors have a card"
    );
    let alchemy = vendors.iter().find(|v| v["vendor"] == "alchemy").unwrap();
    assert_eq!(alchemy["estimated_only"], true);
    let monthly = alchemy["windows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["kind"] == "monthly")
        .unwrap();
    assert_eq!(monthly["cap"], 100);
    assert_eq!(monthly["effective"], 100);
    assert_eq!(
        monthly["cap_locked_by"],
        "ODM__VENDORS__ALCHEMY__CAP__MONTHLY"
    );

    let r = h
        .admin(reqwest::Method::GET, "/admin/api/quota.csv")
        .send()
        .await
        .unwrap();
    assert!(r.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/csv"));
    assert!(r.text().await.unwrap().starts_with("vendor,window,"));
}

#[tokio::test]
async fn admin_body_limit() {
    let h = Harness::start().await;
    let huge = "x".repeat(bdm_transport_http::ADMIN_BODY_LIMIT + 1);
    let r = h
        .admin(reqwest::Method::POST, "/admin/api/clients")
        .header("content-type", "application/json")
        .body(format!("{{\"name\":\"{huge}\"}}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);
}

#[tokio::test]
async fn connect_facts_self_hosted_and_hosted() {
    let h = Harness::start().await;
    let text = h
        .admin(reqwest::Method::GET, "/admin/api/connect")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        !text.contains("127.0.0.1:9/"),
        "custom RPC URL leaked: {text}"
    );
    assert!(!text.contains(TOKEN));
    let c: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(c["mode"], "self_hosted");
    assert_eq!(c["http_url"], "http://127.0.0.1:8787");
    assert_eq!(c["mcp_url"], "http://127.0.0.1:8787/mcp");
    assert!(c["public_url"].is_null());
    assert_eq!(c["tool_profile"], "all");
    assert_eq!(c["sample_tool"], "chain_who_answers");
    assert!(c["binary_path"].as_str().unwrap().len() > 1);
    assert_eq!(
        c["config_dir"],
        h._dir.path().canonicalize().unwrap().to_str().unwrap()
    );

    let hosted = Harness::start_with(
        "[server]\nmode = \"hosted\"\npublic_bind = \"0.0.0.0:9001\"\nadmin_bind = \"127.0.0.1:9002\"\ntool_profile = \"payments\"\n",
    )
    .await;
    let c: Value = hosted
        .admin(reqwest::Method::GET, "/admin/api/connect")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(c["mode"], "hosted");
    assert_eq!(c["http_url"], "http://127.0.0.1:9002");
    assert_eq!(
        c["public_url"], "http://127.0.0.1:9001",
        "wildcard bind becomes loopback"
    );
    assert_eq!(c["mcp_url"], "http://127.0.0.1:9001/mcp");
    assert_eq!(c["tool_profile"], "payments");
}
