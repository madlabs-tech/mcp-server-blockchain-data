//! `evm-mcp-server`: blockchain data aggregator.
//!
//! Usage:
//!   evm-mcp-server [--config-dir DIR]          MCP over stdio (Claude Desktop); also serves REST +
//!                                              dashboard on `server.http_bind` if that port is free
//!   evm-mcp-server serve [--config-dir DIR]    HTTP only: REST, MCP streamable HTTP (/mcp), admin

mod wiring;

use anyhow::{Context, Result};
use ems_transport_http::{admin_router, public_router, HttpState};
use ems_transport_mcp::McpServer;
use std::{path::PathBuf, sync::Arc};

struct Args {
    serve: bool,
    config_dir: PathBuf,
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        serve: false,
        config_dir: PathBuf::from("config"),
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "serve" => args.serve = true,
            "--config-dir" => {
                args.config_dir = it.next().context("--config-dir needs a value")?.into()
            }
            "-h" | "--help" => {
                eprintln!("usage: evm-mcp-server [serve] [--config-dir DIR]");
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown argument '{other}' (try --help)"),
        }
    }
    Ok(args)
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
    let (_loader, loaded) = wiring::load_config(args.config_dir)?;
    let built = wiring::build(loaded)?;
    let settings = &built.loaded.settings.server;
    let state = HttpState {
        app: built.app.clone(),
        auth: None,
    };
    let http = public_router(state.clone()).merge(admin_router(state));
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        tools = built.app.catalog().len(),
        "starting evm-mcp-server"
    );

    if args.serve {
        let listener = tokio::net::TcpListener::bind(&settings.http_bind)
            .await
            .with_context(|| format!("binding {}", settings.http_bind))?;
        tracing::info!(bind = %settings.http_bind, "HTTP listening (REST /v1, MCP /mcp)");
        axum::serve(listener, http).await?;
        return Ok(());
    }

    if settings.dashboard {
        match tokio::net::TcpListener::bind(&settings.http_bind).await {
            Ok(listener) => {
                tracing::info!(bind = %settings.http_bind, "HTTP listening alongside stdio");
                tokio::spawn(async move { axum::serve(listener, http).await });
            }
            Err(e) => {
                tracing::warn!(bind = %settings.http_bind, "HTTP not started ({e}); stdio only")
            }
        }
    }
    McpServer::new(Arc::clone(&built.app))
        .serve_stdio()
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(())
}
