# EVM Blockchain MCP Server

A Model Context Protocol (MCP) server built in Rust for interacting with EVM-compatible blockchains through Common RPCs like QuickNode or Alchemy or other RPC providers.

## 📋 Contents

- [Overview](#overview)
- [Features](#features)
- [Supported Networks](#supported-networks)
- [Prerequisites](#prerequisites)
- [Installation](#installation)
- [Server Configuration](#server-configuration)
- [Usage](#usage)
- [API Reference](#api-reference)
  - [Tools](#tools)
  - [Resources](#resources)
- [Security Considerations](#security-considerations)
- [Project Structure](#project-structure)
- [Development](#development)
- [License](#license)

## 🔭 Overview

The MCP EVM Server leverages the Model Context Protocol to provide blockchain services to AI agents. It supports a wide range of services including:

- Reading blockchain state (balances, transactions, blocks, etc.)
- Interacting with smart contracts
- Transferring tokens (native, ERC20, ERC721, ERC1155)
- Querying token metadata and balances
- Chain-specific services across 10+ EVM networks
- **ENS name resolution** for all address parameters (use human-readable names like 'vitalik.eth' instead of addresses)

All services are exposed through a consistent interface of MCP tools and resources, making it easy for AI agents to discover and use blockchain functionality. **Every tool that accepts Ethereum addresses also supports ENS names**, automatically resolving them to addresses behind the scenes.

## Features

This MCP server provides the following tools for AI assistants to interact with EVM blockchains:

### Blockchain Data Access

- **Multi-chain support** for 30+ EVM-compatible networks
- **Chain information** including blockNumber, chainId, and RPCs
- **Block data** access by number, hash, or latest
- **Transaction details** and receipts with decoded logs
- **Address balances** for native tokens and all token standards
- **ENS resolution** for human-readable Ethereum addresses (use 'vitalik.eth' instead of '0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045')

### Token services

- **ERC20 Tokens**
  - Get token metadata (name, symbol, decimals, supply)
  - Check token balances
  - Transfer tokens between addresses
  - Approve spending allowances

- **NFTs (ERC721)**
  - Get collection and token metadata
  - Verify token ownership
  - Transfer NFTs between addresses
  - Retrieve token URIs and count holdings

- **Multi-tokens (ERC1155)**
  - Get token balances and metadata
  - Transfer tokens with quantity
  - Access token URIs

### Smart Contract Interactions

- **Read contract state** through view/pure functions
- **Write services** with private key signing
- **Contract verification** to distinguish from EOAs
- **Event logs** retrieval and filtering

### Comprehensive Transaction Support

- **Native token transfers** across all supported networks
- **Gas estimation** for transaction planning
- **Transaction status** and receipt information
- **Error handling** with descriptive messages

## 🌐 Supported Networks

### Mainnets
- Ethereum (ETH)
- Optimism (OP)
- Arbitrum (ARB)
- Arbitrum Nova
- Base
- Polygon (MATIC)
- Polygon zkEVM
- Avalanche (AVAX)
- Binance Smart Chain (BSC)
- zkSync Era
- Linea
- Filecoin (FIL)
- Mantle
- Hyperliquid Mainnet

### Testnets
- Sepolia
- Optimism Sepolia
- Arbitrum Sepolia
- Base Sepolia
- Polygon Amoy
- Avalanche Fuji
- BSC Testnet
- zkSync Sepolia
- Linea Sepolia
- Mantle Sepolia
- Filecoin Calibration
- Hyperliquid Testnet

## 🛠️ Prerequisites

- Rust 1.70+ installed ([rustup.rs](https://rustup.rs))
- A QuickNode account and endpoint (or any other EVM RPC provider)

## Setup

### 1. Get Your QuickNode Endpoint

1. Sign up at [QuickNode](https://www.quicknode.com/)
2. Create a new endpoint for your desired EVM chain (Ethereum, Polygon, BSC, etc.)
3. Copy your endpoint URL (format: `https://your-endpoint.quiknode.pro/your-token/`)

### 2. Clone and Build

```bash
# Clone this repository (or copy the files)
git clone <your-repo>
cd evm-mcp-server

# Build the project
cargo build --release
```

### 3. Configure Environment

Set your RPC URL as an environment variable:

```bash
# For QuickNode
export RPC_URL="https://your-endpoint.quiknode.pro/your-token/"

# Or for other providers
export RPC_URL="https://mainnet.infura.io/v3/YOUR-PROJECT-ID"
```

### 4. Run the Server

```bash
cargo run --release
```

Or run the built binary:

```bash
./target/release/evm-mcp-server
```

## Using with Claude Desktop

To use this MCP server with Claude Desktop, add it to your Claude configuration:

### macOS/Linux

Edit `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "evm-blockchain": {
      "command": "/path/to/evm-mcp-server/target/release/evm-mcp-server",
      "env": {
        "RPC_URL": "https://your-endpoint.quiknode.pro/your-token/"
      }
    }
  }
}
```

### Windows

Edit `%APPDATA%\Claude\claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "evm-blockchain": {
      "command": "C:\\path\\to\\evm-mcp-server\\target\\release\\evm-mcp-server.exe",
      "env": {
        "RPC_URL": "https://your-endpoint.quiknode.pro/your-token/"
      }
    }
  }
}
```

Restart Claude Desktop after making changes.

## Example Usage

Once configured, you can ask Claude questions like:

- "What's the latest Ethereum block number?"
- "Get me the balance of Vitalik's address: 0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"
- "Show me details for block 18000000"
- "What's the current gas price?"
- "Get transaction details for 0xabc123..."
- "What chain am I connected to?"
- "Is this address a smart contract?"
- "Estimate gas for sending 1 ETH from address A to address B"


## Architecture

```
┌─────────────────┐
│  Claude/AI App  │
└────────┬────────┘
         │ MCP Protocol
         │ (stdio/JSON-RPC)
┌────────▼────────┐
│  MCP Server     │
│  (Rust + Alloy) │
└────────┬────────┘
         │ HTTP/WebSocket
         │ (JSON-RPC)
┌────────▼────────┐
│  QuickNode or   │
│  RPC Provider   │
└────────┬────────┘
         │
┌────────▼────────┐
│  EVM Blockchain │
│ (ETH/Polygon/+) │
└─────────────────┘
```

## Error Handling

The server includes comprehensive error handling for:
- Invalid addresses/hashes
- Network errors
- Missing blocks/transactions
- RPC provider issues

## Performance Considerations

- All requests are async for maximum concurrency using Tokio
- Alloy provides efficient connection pooling and request batching
- Consider rate limits of your RPC provider
- QuickNode offers higher rate limits than public nodes
- Alloy's modular design results in smaller binary sizes and faster compilation

## 🔒 Security Considerations

- Never commit your RPC URL with tokens to version control
- Use environment variables for sensitive data
- Consider using separate endpoints for development and production
- Monitor your QuickNode / Alchemy / other RPC usage to avoid unexpected costs

## 📁 Project Structure

```
mcp-evm-server/
├── src/
│   ├── core/
│   │   ├── chains.rs           # Chain definitions and utilities
│   │   ├── resources.rs        # MCP resources implementation
│   │   ├── tools.rs            # MCP tools implementation
│   │   ├── prompts.rs          # MCP prompts implementation
│   │   └── services/           # Core blockchain services
│   │       ├── mod.rs        # Operation exports
│   │       ├── balance.rs      # Balance services
│   │       ├── transfer.rs     # Token transfer services
│   │       ├── utils.rs        # Utility functions
│   │       ├── tokens.rs       # Token metadata services
│   │       ├── contracts.rs    # Contract interactions
│   │       ├── transactions.rs # Transaction services
│   │       └── blocks.rs       # Block services
│   │       └── clients.rs      # RPC client utilities
├── main.rs
└── README.md
```

## Resources

- [MCP Documentation](https://modelcontextprotocol.io)
- [rmcp Rust SDK](https://github.com/modelcontextprotocol/rust-sdk)
- [Alloy Documentation](https://alloy.rs)
- [Alloy GitHub](https://github.com/alloy-rs/alloy)
- [QuickNode Docs](https://www.quicknode.com/docs)
- [Ethereum JSON-RPC Spec](https://ethereum.org/en/developers/docs/apis/json-rpc/)

## License

MIT

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.
