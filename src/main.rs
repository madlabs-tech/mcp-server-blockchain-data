mod core;

use anyhow::Result;
use rmcp::{
    ErrorData as McpError,
    handler::server::{tool::ToolRouter, ServerHandler, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router,
    ServiceExt,
    schemars,
};
use serde::Deserialize;
use serde_json::json;
use std::str::FromStr;
use tracing_subscriber;

use core::chains::{get_chain, Chain};
use core::services::clients::get_public_client;

// Define parameter structs for our tools
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetBalanceParams {
    /// The Ethereum address to check
    pub address: String,
    /// The chain to check (ethereum, base, arbitrum, avalanche, bsc)
    pub chain: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetCodeParams {
    /// The Ethereum address to check
    pub address: String,
    /// The chain to check (ethereum, base, arbitrum, avalanche, bsc)
    pub chain: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetGasPriceParams {
    /// The chain to check (ethereum, base, arbitrum, avalanche, bsc)
    pub chain: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetTransactionByHashParams {
    // hash
    pub hash: String,
    // chain
    pub chain: String,
}


#[derive(Clone)]
pub struct EvmMcpServer {
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl EvmMcpServer {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Get the ETH/native token balance of an address")]
    async fn eth_get_balance(
        &self,
        Parameters(params): Parameters<GetBalanceParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.get_balance(params.address, params.chain).await {
            Ok(result) => Ok(CallToolResult::success(vec![Content::text(result)])),
            Err(e) => Err(McpError::internal_error(
                format!("Failed to get balance: {}", e),
                None,
            )),
        }
    }

    #[tool(description = "Detect whether an address is a contract or wallet")]
    async fn eth_get_code(
        &self,
        Parameters(params): Parameters<GetCodeParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.get_code(params.address, params.chain).await {
            Ok(result) => Ok(CallToolResult::success(vec![Content::text(result)])),
            Err(e) => Err(McpError::internal_error(
                format!("Failed to get code: {}", e),
                None,
            )),
        }
    }

    #[tool(description = "Get the current gas price on the specified chain")]
    async fn eth_gas_price(
        &self,
        Parameters(params): Parameters<GetGasPriceParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.get_gas_price(params.chain).await {
            Ok(result) => Ok(CallToolResult::success(vec![Content::text(result)])),
            Err(e) => Err(McpError::internal_error(
                format!("Failed to get gas price: {}", e),
                None,
            )),
        }
    }

    #[tool(description = "Get transaction details by hash")]
    async fn eth_get_transaction_by_hash(
        &self,
        Parameters(params): Parameters<GetTransactionByHashParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.get_transaction_by_hash(params.hash, params.chain).await {
            Ok(result) => Ok(CallToolResult::success(vec![Content::text(result)])),
            Err(e) => Err(McpError::internal_error(
                format!("Failed to get transaction: {}", e),
                None,
            )),
        }
    }

}

// Implementation methods
impl EvmMcpServer {
    async fn get_balance(&self, address: String, chain_str: String) -> Result<String> {
        use alloy::primitives::Address;
        use alloy::providers::Provider;

        // Parse chain
        let chain = Chain::from_str(&chain_str)?;
        let chain_config = get_chain(chain)?;

        // Parse address
        let address = Address::from_str(&address)?;

        // Get provider
        let provider = get_public_client(chain).await?;

        // Get balance in wei
        let balance_wei = provider.get_balance(address).await?;

        // Format response as JSON
        let result = json!({
            "address": address.to_string(),
            "chain": chain_config.name,
            "balanceWei": balance_wei.to_string(),
            "symbol": chain_config.symbol,
            "decimals": chain_config.decimals,
        });

        Ok(serde_json::to_string_pretty(&result)?)
    }

    async fn get_code(&self, address: String, chain_str: String) -> Result<String> {
        use alloy::primitives::Address;
        use alloy::providers::Provider;

        // Parse chain
        let chain = Chain::from_str(&chain_str)?;
        let chain_config = get_chain(chain)?;

        // Parse address
        let address = Address::from_str(&address)?;

        // Get provider
        let provider = get_public_client(chain).await?;

        // Get code at the address
        let code = provider.get_code_at(address).await?;

        let is_contract = !code.is_empty();
        let bytecode_size = code.len();

        let result = json!({
            "address": address.to_string(),
            "chain": chain_config.name,
            "isContract": is_contract,
            "bytecodeSize": bytecode_size,
        });

        Ok(serde_json::to_string_pretty(&result)?)
    }

    async fn get_gas_price(&self, chain_str: String) -> Result<String> {
        use alloy::providers::Provider;

        // Parse chain
        let chain = Chain::from_str(&chain_str)?;
        let chain_config = get_chain(chain)?;

        // Get provider
        let provider = get_public_client(chain).await?;

        // Get gas price
        let gas_price = provider.get_gas_price().await?;

        let result = json!({
            "chain": chain_config.name,
            "gasPriceWei": gas_price.to_string(),
            "gasPriceGwei": format!("{:.2}", gas_price as f64 / 1e9),
            "timestamp": chrono::Utc::now().to_rfc3339(),
        });

        Ok(serde_json::to_string_pretty(&result)?)
    }

    async fn get_transaction_by_hash(&self, hash: String, chain_str: String) -> Result<String> {
        use alloy::primitives::B256;
        use alloy::providers::Provider;

        // Parse chain
        let chain = Chain::from_str(&chain_str)?;
        let chain_config = get_chain(chain)?;

        // Parse transaction hash
        let tx_hash = B256::from_str(&hash)?;

        // Get provider
        let provider = get_public_client(chain).await?;

        // Get transaction details
        let tx = provider.get_transaction_by_hash(tx_hash).await?;
        
        let result = json!({
            "chain": chain_config.name,
            "transaction": tx,
        });

        Ok(serde_json::to_string_pretty(&result)?)
    }
}

#[tool_handler]
impl ServerHandler for EvmMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .build(),
            server_info: Implementation::from_build_env(),
            instructions: Some(
                "A server for LLM agents to access EVM blockchain data. \
                Tools: eth_get_balance (get wallet/contract balance), \
                eth_get_code (check if address is contract), \
                eth_gas_price (get current gas price)".to_string(),
            ),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing subscriber for logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    tracing::info!("Starting EVM MCP Server v0.1.0");

    // Create the server
    let server = EvmMcpServer::new();

    // Start the server on stdio
    let service = server.serve(rmcp::transport::stdio()).await?;

    tracing::info!("MCP Server started and listening on stdio");

    // Wait for the service to complete
    service.waiting().await?;

    Ok(())
}