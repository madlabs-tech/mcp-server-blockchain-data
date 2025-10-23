use anyhow::Result;
use std::env;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod core;
mod server;

use server::{http_server, server};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "evm_mcp_server=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Get command line arguments
    let args: Vec<String> = env::args().collect();

    // Determine server mode
    let mode = if args.len() > 1 {
        args[1].as_str()
    } else {
        "stdio"
    };

    // Get RPC URL from environment
    let rpc_url = env::var("RPC_URL").unwrap_or_else(|_| {
        warn!("RPC_URL not set, using default Ethereum mainnet endpoint");
        "https://eth-mainnet.g.alchemy.com/v2/demo".to_string()
    });

    info!("Starting EVM MCP Server in {} mode", mode);
    info!("RPC URL configured: {}", if rpc_url.contains("demo") {
        "Demo endpoint (limited)"
    } else {
        "Custom endpoint"
    });

    match mode {
        "http" => {
            info!("Starting HTTP server with SSE on port 3000");
            http_server::run_http_server(rpc_url).await
        }
        "stdio" | _ => {
            info!("Starting stdio server for MCP communication");
            server::run_stdio_server(rpc_url).await
        }
    }
}