# EVM MCP Server - Rust Implementation with Alloy

This is a Rust implementation of an EVM blockchain MCP server, rewritten from the TypeScript example using the Alloy library for Ethereum interaction.

## Overview

The implementation provides MCP tools for interacting with EVM-compatible blockchains, including:
- Getting wallet/contract balances
- Detecting if an address is a contract
- Fetching current gas prices

## Key Components

### 1. Chain Configuration (`src/core/chains.rs`)
- Defines supported EVM chains (Ethereum, Base, Arbitrum, Avalanche, BSC)
- Manages RPC endpoints (supports both QuickNode and custom RPC URLs)
- Provides chain metadata (network names, native tokens, decimals)

### 2. Alloy Provider Client (`src/core/services/clients.rs`)
- Uses Alloy 1.0's `RootProvider` for blockchain interaction
- Implements connection caching to avoid duplicate providers
- Type-safe provider using Ethereum network configuration

### 3. Main Server (`src/main.rs`)
- Implements MCP server using rmcp macros
- Provides three tools:
  - `eth_get_balance`: Get native token balance
  - `eth_get_code`: Check if address is a contract
  - `eth_gas_price`: Get current gas price

## Technology Stack

- **Alloy 1.0.41**: Modern Rust library for Ethereum, successor to ethers-rs
- **rmcp 0.8.3**: Official Rust SDK for Model Context Protocol
- **Tokio**: Async runtime
- **Serde**: JSON serialization

## Building and Running

```bash
# Build the project
cargo build --release

# Run the server
cargo run --release

# Set RPC URL
export RPC_URL="https://eth-mainnet.g.alchemy.com/v2/YOUR-API-KEY"
```

## Configuration

The server supports two RPC configuration methods:

1. **Simple RPC URL** (recommended for testing):
   ```bash
   export RPC_URL="https://your-rpc-endpoint"
   ```

2. **QuickNode Configuration**:
   ```bash
   export QN_ENDPOINT_NAME="https://xxxh.base-mainnet.quiknode.pro"
   export QN_TOKEN_ID=""
   ```

## Usage with Claude Desktop

Add to your Claude configuration:

```json
{
  "mcpServers": {
    "evm-blockchain": {
      "command": "/path/to/evm-mcp-server/target/release/evm-mcp-server",
      "env": {
        "RPC_URL": "https://your-rpc-endpoint"
      }
    }
  }
}
```

## Implementation Notes

### Alloy Provider Setup
The Alloy provider is initialized with:
```rust
let provider = RootProvider::<Http<Client>, Ethereum>::new_http(url);
```

This creates a type-safe provider that works with the Ethereum network configuration.

### Tool Implementation Pattern
Tools are implemented using rmcp's macro system:
```rust
#[tool(description = "Tool description")]
async fn tool_name(&self, param: String) -> Result<CallToolResult, McpError> {
    // Implementation
}
```

### Error Handling
The implementation uses `anyhow::Result` for error propagation and converts blockchain data to JSON for MCP responses.

## Compilation Challenges and Solutions

1. **rmcp API**: The rmcp crate uses specific macro patterns that differ from traditional Rust patterns
2. **Alloy Types**: Network-aware providers require explicit network type parameters
3. **Async Handling**: All blockchain operations are async and require proper await handling

## Future Enhancements

- Add more blockchain operations (transactions, smart contract calls)
- Implement ENS resolution
- Add token balance queries (ERC20, ERC721)
- Support more EVM chains
- Implement caching strategies