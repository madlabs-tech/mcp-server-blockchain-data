#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
//! T1.D3 hosted mode, black-box against the real binary: fail-closed start, `clients create`,
//! 401 without / with a bad key, per-client daily quota → 429 while another client is unaffected,
//! admin API only on `admin_bind`. No network: every RPC endpoint is a closed local port.

use serde_json::{json, Value};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::process::Command;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn bin(dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_onchain-data-mcp"));
    c.arg("--config-dir")
        .arg(dir)
        .env("RUST_LOG", "error")
        .env("ODM__SERVER__WARMUP", "false")
        .env_remove("RPC_URL")
        .env_remove("ALCHEMY_API_KEY")
        .env_remove("QN_ENDPOINT_NAME")
        .env_remove("QN_TOKEN_ID")
        .env_remove("ODM__SERVER__MODE")
        .env_remove("DASHBOARD_PASSWORD")
        .kill_on_drop(true);
    c
}

fn write_config(dir: &Path, public: u16, admin: u16) {
    let data = dir.join("data");
    std::fs::write(
        dir.join("config.toml"),
        format!(
            r#"
[server]
mode = "hosted"
public_bind = "127.0.0.1:{public}"
admin_bind = "127.0.0.1:{admin}"
data_dir = '{}'  # literal string: Windows paths have backslashes

[chain_overrides.ethereum]
public_rpc = ["http://127.0.0.1:9"]

[clients.default]
requests_per_minute = 100
daily_requests = 1
tool_profile = "all"
"#,
            data.display()
        ),
    )
    .unwrap();
}

async fn create_client(dir: &Path, name: &str) -> String {
    let out = bin(dir)
        .args(["clients", "create", name])
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("key:"))
        .expect("key line")
        .trim()
        .to_owned()
}

async fn wait_up(port: u16) {
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("server did not start on {port}");
}

#[tokio::test]
async fn hosted_mode_fails_closed_without_client_keys() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), free_port(), free_port());
    let out = tokio::time::timeout(
        Duration::from_secs(20),
        bin(dir.path()).arg("serve").stderr(Stdio::piped()).output(),
    )
    .await
    .expect("must exit, not serve")
    .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("client key"), "{err}");
}

#[tokio::test]
async fn hosted_mode_auth_limits_and_admin_bind() {
    let dir = tempfile::tempdir().unwrap();
    let (public, admin) = (free_port(), free_port());
    write_config(dir.path(), public, admin);
    let key_a = create_client(dir.path(), "a").await;
    let key_b = create_client(dir.path(), "b").await;
    assert_ne!(key_a, key_b);

    let _child = bin(dir.path())
        .arg("serve")
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_up(public).await;
    wait_up(admin).await;
    let http = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{public}");

    // 401 without a key and with a bad key (REST and MCP)
    for auth in [None, Some("odm_not_a_key")] {
        let mut r = http.get(format!("{base}/v1/tools"));
        if let Some(k) = auth {
            r = r.bearer_auth(k);
        }
        assert_eq!(r.send().await.unwrap().status(), 401);
        let mut r = http.post(format!("{base}/mcp")).json(&json!({}));
        if let Some(k) = auth {
            r = r.bearer_auth(k);
        }
        assert_eq!(r.send().await.unwrap().status(), 401);
    }
    let r = http
        .get(format!("{base}/v1/tools"))
        .bearer_auth(&key_a)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    // daily_requests = 1: a's second call is refused; b is unaffected.
    // (Invalid input fails after admission, so no RPC is attempted.)
    let call = |key: &str| {
        http.post(format!("{base}/v1/legacy/eth_get_balance"))
            .bearer_auth(key.to_owned())
            .json(&json!({ "address": "not-an-address", "chain": "ethereum" }))
            .send()
    };
    assert_eq!(call(&key_a).await.unwrap().status(), 400);
    let r = call(&key_a).await.unwrap();
    assert_eq!(r.status(), 429);
    assert!(r.headers().contains_key("retry-after"));
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["error"]["code"], "QUOTA_EXCEEDED");
    assert!(body["error"]["retry_after_secs"].as_u64().unwrap() >= 1);
    assert_eq!(
        call(&key_b).await.unwrap().status(),
        400,
        "other clients are unaffected"
    );

    // admin API lives only on admin_bind, and needs the dashboard password
    let r = http
        .get(format!("{base}/admin/api/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        401,
        "public port must not serve admin (auth layer rejects first)"
    );
    let r = http
        .get(format!("{base}/admin/api/health"))
        .bearer_auth(&key_a)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404, "no admin routes on the public port");
    let admin_url = format!("http://127.0.0.1:{admin}/admin/api/health");
    assert_eq!(http.get(&admin_url).send().await.unwrap().status(), 401);
    let token = std::fs::read_to_string(dir.path().join("dashboard_password")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("dashboard_password"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let r = http
        .get(&admin_url)
        .bearer_auth(token.trim())
        .header("X-BDM-Admin", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let health: Value = r.json().await.unwrap();
    assert_eq!(health["mode"], "hosted");
    assert_eq!(health["clients"], 2);
}

#[tokio::test]
async fn password_command_prints_admin_url_and_login_link() {
    let dir = tempfile::tempdir().unwrap();
    let admin = free_port();
    write_config(dir.path(), free_port(), admin);
    let out = bin(dir.path()).arg("password").output().await.unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let pw = std::fs::read_to_string(dir.path().join("dashboard_password")).unwrap();
    assert!(
        stdout.contains(&format!(
            "http://127.0.0.1:{admin}/dashboard#login={}",
            pw.trim()
        )),
        "{stdout}"
    );
    assert!(stdout.contains("Source: "), "{stdout}");
}

#[tokio::test]
async fn password_reset_refuses_when_set_by_env() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), free_port(), free_port());
    let out = bin(dir.path())
        .args(["password", "reset"])
        .env("DASHBOARD_PASSWORD", "from-the-env-123")
        .output()
        .await
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("DASHBOARD_PASSWORD setting"));
    assert!(!dir.path().join("dashboard_password").exists());
}
