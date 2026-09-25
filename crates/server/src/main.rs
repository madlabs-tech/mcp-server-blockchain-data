#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )
)]
//! `onchain-data-mcp`: blockchain data aggregator.
//!
//! Usage:
//!   onchain-data-mcp [--config-dir DIR]        self-hosted: MCP over stdio (Claude Desktop); also
//!                                              serves REST + dashboard on `server.http_bind` if free
//!   onchain-data-mcp serve [--config-dir DIR]  HTTP only (self-hosted: `http_bind`; hosted:
//!                                              public router on `public_bind`, admin on `admin_bind`)
//!   onchain-data-mcp password [reset]          show (or replace) the dashboard URL and password
//!   onchain-data-mcp clients create <name>     create a client key (printed once) for hosted mode
//!   onchain-data-mcp clients list              list client keys (ids and names, never keys)
//!   onchain-data-mcp --version
//!
//! `mode = "hosted"` always runs HTTP (never stdio) and refuses to start without client keys.

mod wiring;

use anyhow::{bail, Context, Result};
use bdm_config::{ConfigLoader, Loaded, Mode, ServerSettings};
use bdm_store::{ClientKeyAuth, QuotaEngine, Store};
use bdm_transport_http::{
    admin_router, dashboard_base, ensure_dashboard_password,
    password::{self, Source},
    public_router, reset_dashboard_password, AdminState, HttpState,
};
use bdm_transport_mcp::McpServer;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

const DEFAULT_ADMIN_BIND: &str = "127.0.0.1:8788";
const DEFAULT_CONFIG_DIR: &str = "config";
/// Env var that sets the dashboard password (read through the config loader's env snapshot).
const PASSWORD_ENV: &str = "DASHBOARD_PASSWORD";

enum Command {
    Run { serve: bool },
    Password { reset: bool },
    ClientsCreate(String),
    ClientsList,
}

struct Args {
    command: Command,
    config_dir: PathBuf,
}

const USAGE: &str = "usage: onchain-data-mcp [serve | password [reset] | clients create <name> | clients list] [--config-dir DIR] [--version]";

fn parse_args() -> Result<Args> {
    let mut command = Command::Run { serve: false };
    let mut config_dir = PathBuf::from(DEFAULT_CONFIG_DIR);
    let mut it = std::env::args().skip(1).peekable();
    while let Some(a) = it.next() {
        match a.as_str() {
            "serve" => command = Command::Run { serve: true },
            "password" => {
                command = Command::Password {
                    reset: it.next_if_eq("reset").is_some(),
                }
            }
            "clients" => {
                command = match it.next().as_deref() {
                    Some("create") => {
                        Command::ClientsCreate(it.next().context("clients create needs a <name>")?)
                    }
                    Some("list") => Command::ClientsList,
                    Some("-h" | "--help") => {
                        eprintln!("{USAGE}");
                        std::process::exit(0);
                    }
                    _ => bail!("{USAGE}"),
                }
            }
            "--config-dir" => config_dir = it.next().context("--config-dir needs a value")?.into(),
            "-h" | "--help" => {
                eprintln!("{USAGE}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("onchain-data-mcp {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other => bail!("unknown argument '{other}' (try --help)"),
        }
    }
    Ok(Args {
        command,
        config_dir,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    // Logs go to stderr: stdout is the MCP stdio channel.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let args = parse_args()?;
    let (loader, loaded) = wiring::load_config(args.config_dir)?;
    match args.command {
        Command::ClientsCreate(name) => clients_create(&loaded, &name).await,
        Command::ClientsList => clients_list(&loaded),
        Command::Password { reset } => show_password(&loader, &loaded, reset),
        Command::Run { serve } => run(loader, loaded, serve).await,
    }
}

/// Canonical path if it exists, else absolute (for printing).
fn absolute(p: &Path) -> PathBuf {
    std::fs::canonicalize(p)
        .or_else(|_| std::path::absolute(p))
        .unwrap_or_else(|_| p.to_path_buf())
}

/// `password [reset]`: print the dashboard URL and password. Never starts a server.
fn show_password(loader: &ConfigLoader, loaded: &Loaded, reset: bool) -> Result<()> {
    let dir = &loader.dir.root;
    let env = loader.env.get(PASSWORD_ENV);
    let (pw, source) = if reset {
        if env.is_some() {
            bail!("The password comes from the {PASSWORD_ENV} setting. Change it there instead.");
        }
        let pw = reset_dashboard_password(dir).map_err(anyhow::Error::msg)?;
        let src = Source::File {
            created: true,
            migrated: false,
        };
        (pw, src)
    } else {
        ensure_dashboard_password(dir, env).map_err(anyhow::Error::msg)?
    };
    let srv = &loaded.settings.server;
    let base = dashboard_base(srv);
    let login: String = url::form_urlencoded::byte_serialize(pw.as_bytes()).collect();
    let abs = absolute(dir);
    if reset {
        println!("New password saved.");
    }
    println!("Dashboard: {base}/dashboard");
    println!("Password:  {pw}");
    println!("One-click login: {base}/dashboard#login={login}");
    println!("Config folder: {}", abs.display());
    match source {
        Source::Env => println!("Source: {PASSWORD_ENV} env var"),
        Source::File { .. } => println!("Source: {}", password::path(&abs).display()),
    }
    if srv.mode == Mode::SelfHosted && !srv.dashboard {
        println!(
            "Note: the dashboard is turned off (server.dashboard = false). Turn it on to use it."
        );
    }
    if reset {
        println!("Restart the server (or your AI app) to use the new password.");
    }
    Ok(())
}

/// Startup hint: where the dashboard is and how to get the password (never the password).
fn log_dashboard(srv: &ServerSettings, config_dir: &Path) {
    let cfg = if config_dir == Path::new(DEFAULT_CONFIG_DIR) {
        String::new()
    } else {
        let d = absolute(config_dir).display().to_string();
        if d.contains(' ') {
            format!(" --config-dir \"{d}\"")
        } else {
            format!(" --config-dir {d}")
        }
    };
    tracing::info!(
        "dashboard: {}/dashboard (run `onchain-data-mcp password{cfg}` to see the password)",
        dashboard_base(srv)
    );
}

async fn clients_create(loaded: &Loaded, name: &str) -> Result<()> {
    let path = loaded.settings.server.data_dir.join("bdm.db");
    let store = Store::open(&path).with_context(|| format!("opening {}", path.display()))?;
    let (rec, key) = store.create_client(name, None).await?;
    println!("client id: {}\nname:      {}\nkey:       {key}\n\nThe key is shown once and stored only as a SHA-256 hash.", rec.id, rec.name);
    Ok(())
}

fn clients_list(loaded: &Loaded) -> Result<()> {
    let path = loaded.settings.server.data_dir.join("bdm.db");
    let store = Store::open(&path).with_context(|| format!("opening {}", path.display()))?;
    for c in store.clients() {
        let state = if c.active() { "active" } else { "revoked" };
        println!(
            "{}\t{}\t{}\t{}",
            c.id,
            state,
            c.created_at.to_rfc3339(),
            c.name
        );
    }
    Ok(())
}

/// Admin state for the dashboard, or `None` (dashboard off, server keeps running) if there is no
/// usable dashboard password. Never logs the password.
fn admin_state(
    loader: ConfigLoader,
    built: &wiring::Built,
    quota: Arc<QuotaEngine>,
) -> Option<AdminState> {
    let dir = loader.dir.root.clone();
    match ensure_dashboard_password(&dir, loader.env.get(PASSWORD_ENV)) {
        Ok((token, source)) => {
            let path = password::path(&dir);
            match source {
                Source::File { migrated: true, .. } => tracing::info!(
                    path = %path.display(),
                    "dashboard password: renamed the old admin_token file (same password)"
                ),
                Source::File { created: true, .. } => {
                    tracing::info!(path = %path.display(), "dashboard password created")
                }
                _ => {}
            }
            Some(AdminState::new(
                built.app.clone(),
                built.store.clone(),
                quota,
                Arc::new(loader),
                {
                    let router = built.app.router().clone();
                    Arc::new(move |l: &bdm_config::Loaded| wiring::full_registry(l, &router))
                },
                token,
            ))
        }
        Err(e) => {
            tracing::error!("dashboard turned off (the server keeps running): {e}");
            None
        }
    }
}

/// Reload config on SIGHUP (same path as the dashboard's reload button).
fn spawn_sighup(admin: Option<AdminState>) {
    #[cfg(unix)]
    tokio::spawn(async move {
        let Some(admin) = admin else { return };
        let Ok(mut hup) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        else {
            return;
        };
        while hup.recv().await.is_some() {
            let a = admin.clone();
            match tokio::task::spawn_blocking(move || a.reload()).await {
                Ok(Ok(_)) => tracing::info!("SIGHUP: configuration reloaded"),
                Ok(Err(issues)) => tracing::error!(
                    "SIGHUP: reload refused, keeping the old config\n{}",
                    wiring::render(&issues)
                ),
                Err(e) => tracing::error!("SIGHUP: reload failed: {e}"),
            }
        }
    });
    #[cfg(not(unix))]
    drop(admin);
}

async fn run(loader: ConfigLoader, loaded: Loaded, serve: bool) -> Result<()> {
    let store = wiring::open_store(&loaded)?;
    let built = wiring::build(loaded, store)?;
    let settings = built.loaded.settings.server.clone();
    let config_dir = loader.dir.root.clone();
    let router = built.app.router().clone();
    let quota = QuotaEngine::new(router.clone(), built.store.clone());
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        tools = built.app.catalog().len(),
        mode = ?settings.mode,
        "starting onchain-data-mcp"
    );
    if settings.warmup {
        wiring::spawn_warmup(router.clone());
    }

    if settings.mode == Mode::Hosted {
        let public_bind = settings
            .public_bind
            .clone()
            .context("hosted mode requires server.public_bind")?;
        let admin_bind = settings
            .admin_bind
            .clone()
            .unwrap_or_else(|| DEFAULT_ADMIN_BIND.into());
        let auth = Arc::new(ClientKeyAuth::new(built.store.clone(), router));
        let public = public_router(HttpState {
            app: built.app.clone(),
            auth: Some(auth),
        });
        let admin = admin_state(loader, &built, quota.clone());
        spawn_sighup(admin.clone());
        quota.clone().spawn();

        let public_l = tokio::net::TcpListener::bind(&public_bind)
            .await
            .with_context(|| format!("binding public_bind {public_bind}"))?;
        tracing::info!(bind = %public_bind, "hosted: public REST /v1 + MCP /mcp (client keys required)");
        let admin_task = match admin {
            Some(a) => {
                let l = tokio::net::TcpListener::bind(&admin_bind)
                    .await
                    .with_context(|| format!("binding admin_bind {admin_bind}"))?;
                if !l.local_addr()?.ip().is_loopback() {
                    tracing::warn!(bind = %admin_bind, "admin_bind is not loopback: restrict it with a firewall / IP allowlist");
                }
                tracing::info!(bind = %admin_bind, "hosted: dashboard + admin API at /dashboard");
                log_dashboard(&settings, &config_dir);
                Some(tokio::spawn(async move {
                    axum::serve(l, admin_router(a)).await
                }))
            }
            None => None,
        };
        axum::serve(public_l, public).await?;
        if let Some(t) = admin_task {
            t.abort();
        }
        return Ok(());
    }

    // Self-hosted: public + admin on one localhost bind.
    let http = public_router(HttpState {
        app: built.app.clone(),
        auth: None,
    });
    quota.clone().spawn();

    let bind = settings.http_bind.clone();
    let listener = if serve || settings.dashboard {
        match tokio::net::TcpListener::bind(&bind).await {
            Ok(l) => Some(l),
            Err(e) if !serve => {
                tracing::warn!(bind = %bind, "HTTP not started ({e}); stdio only");
                None
            }
            Err(e) => return Err(e).with_context(|| format!("binding {bind}")),
        }
    } else {
        None
    };
    let mut admin = None;
    let http = match &listener {
        Some(l) => {
            if !l.local_addr()?.ip().is_loopback() {
                tracing::warn!(bind = %bind, "self-hosted HTTP is not on loopback and has no client auth; prefer 127.0.0.1 or mode = \"hosted\"");
            }
            admin = settings
                .dashboard
                .then(|| admin_state(loader, &built, quota.clone()))
                .flatten();
            match &admin {
                Some(a) => {
                    log_dashboard(&settings, &config_dir);
                    http.merge(admin_router(a.clone()))
                }
                None => http,
            }
        }
        None => http,
    };
    spawn_sighup(admin);

    if serve {
        let Some(l) = listener else {
            bail!("HTTP listener not bound");
        };
        tracing::info!(bind = %bind, "HTTP listening (REST /v1, MCP /mcp, dashboard /dashboard)");
        axum::serve(l, http).await?;
        return Ok(());
    }
    if let Some(l) = listener {
        tracing::info!(bind = %bind, "HTTP listening alongside stdio (dashboard /dashboard)");
        tokio::spawn(async move { axum::serve(l, http).await });
    }
    McpServer::new(Arc::clone(&built.app))
        .serve_stdio()
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(())
}
