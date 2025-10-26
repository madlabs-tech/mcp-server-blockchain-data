# EVM MCP Server - Rust Implementation with Alloy

## 🎉 Features

- ✅ **Get Balance** - Query wallet/contract balances on any supported chain
- ✅ **Contract Detection** - Check if an address is a smart contract or EOA
- ✅ **Gas Price** - Get current gas prices on supported networks
- ✅ **Multi-chain Support** - Ethereum, Base, Arbitrum, Avalanche, BSC

## 🚀 Quick Start

### Prerequisites

- Rust 1.70+ (install from [rustup.rs](https://rustup.rs))
- An RPC endpoint (QuickNode, Alchemy, or any EVM RPC provider)

### Installation

```bash
# Clone the repository
git clone <repository>
cd evm-mcp-server

# Build the project
cargo build --release
```

### Running the Server

```bash
# Set your RPC endpoint
export RPC_URL="https://eth-mainnet.g.alchemy.com/v2/YOUR-API-KEY"

# Run the server
cargo run --release

# Or run the compiled binary
./target/release/evm-mcp-server
```

### Testing with MCP Inspector

```bash
# Install MCP Inspector
npm install -g @modelcontextprotocol/inspector

# Run with inspector
npx @modelcontextprotocol/inspector cargo run --release
```

## 📦 Configuration

### Environment Variables

- `RPC_URL` - Your EVM RPC endpoint URL
- `QN_ENDPOINT_NAME` - QuickNode endpoint name (optional)
- `QN_TOKEN_ID` - QuickNode token ID (optional)
- `RUST_LOG` - Logging level (default: info)

### Claude Desktop Integration

Add to `~/Library/Application Support/Claude/claude_desktop_config.json`:

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

{
  "mcpServers": {
    "evm-blockchain": {
      "command": "/Users/hryer/Code/evm-mcp-server/target/release/evm-mcp-server",
      "env": {
        "RPC_URL": "https://tame-autumn-borough.base-mainnet.quiknode.pro/78a89ee24220b94adbab00acaa1401741381bee2",
        "QN_ENDPOINT_NAME": "https://tame-autumn-borough.base-mainnet.quiknode.pro",
        "QN_TOKEN_ID": "78a89ee24220b94adbab00acaa1401741381bee2"
      }
    }
  }
}

```

## 🛠️ Tools Available

### 1. `eth_get_balance`
Get the native token balance of an address.

**Parameters:**
- `address` (string) - The Ethereum address to check
- `chain` (string) - The chain to check (ethereum, base, arbitrum, avalanche, bsc)

**Response:**
```json
{
  "address": "0x...",
  "chain": "Ethereum",
  "balanceWei": "1000000000000000000",
  "symbol": "ETH",
  "decimals": 18
}
```

### 2. `eth_get_code`
Check if an address is a smart contract or regular wallet.

**Parameters:**
- `address` (string) - The Ethereum address to check
- `chain` (string) - The chain to check

**Response:**
```json
{
  "address": "0x...",
  "chain": "Ethereum",
  "isContract": true,
  "bytecodeSize": 24576
}
```

### 3. `eth_gas_price`
Get the current gas price on a specific chain.

**Parameters:**
- `chain` (string) - The chain to check

**Response:**
```json
{
  "chain": "Ethereum",
  "gasPriceWei": "30000000000",
  "gasPriceGwei": "30.00",
  "timestamp": "2024-01-01T00:00:00Z"
}
```

## 🏗️ Architecture

```
src/
├── main.rs                 # MCP server implementation
├── core/
│   ├── chains.rs          # Chain configurations
│   └── services/
│       └── clients.rs     # Alloy provider management
```

### Key Components

1. **MCP Server** (`main.rs`)
   - Uses rmcp 0.8.3 with `tool_router` macro
   - Implements three blockchain tools
   - Structured parameter handling with `Parameters<T>`

2. **Chain Management** (`chains.rs`)
   - Supports 5 major EVM chains
   - Configurable RPC endpoints
   - Chain metadata (symbols, decimals)

3. **Provider Client** (`clients.rs`)
   - Alloy 1.0.41 provider with caching
   - Type-safe blockchain interactions
   - Connection pooling

## 📊 Performance

- **Native Rust Performance** - Compiled binary with no runtime overhead
- **Connection Caching** - Reuses providers across requests
- **Async/Await** - Non-blocking I/O with Tokio
- **Type Safety** - Compile-time guarantees prevent runtime errors

## 🐛 Troubleshooting

### Common Issues

1. **Connection errors**
   - Verify your RPC_URL is correct
   - Check network connectivity
   - Ensure the RPC endpoint supports the chain

2. **Invalid address**
   - Addresses must be valid hex strings (0x...)
   - ENS names not supported (yet)

3. **Chain not supported**
   - Currently supports: ethereum, base, arbitrum, avalanche, bsc
   - Add new chains in `src/core/chains.rs`

### Debugging

```bash
# Enable debug logging
RUST_LOG=debug cargo run

# Check server info
echo '{"jsonrpc":"2.0","method":"initialize","params":{},"id":1}' | cargo run
```

## 📚 Dependencies

- **alloy** (1.0.41) - Modern Ethereum library for Rust
- **rmcp** (0.8.3) - MCP protocol implementation
- **tokio** (1.48) - Async runtime
- **serde** (1.0) - Serialization
- **anyhow** (1.0) - Error handling

## 🎯 Future Enhancements

- [ ] ENS name resolution
- [ ] ERC20 token balances
- [ ] Transaction sending
- [ ] Event log filtering
- [ ] Contract interaction
- [ ] Batch requests
- [ ] WebSocket support

## 📄 License

MIT

## 🙏 Acknowledgments

- Built with [Alloy](https://alloy.rs) - The next generation Ethereum library
- Uses [rmcp](https://github.com/modelcontextprotocol/rust-sdk) - Official Rust MCP SDK
- Inspired by the TypeScript example using Viem

## ✅ Status

**WORKING** - The server compiles and runs successfully. All three tools are functional and can be used with Claude Desktop or any MCP client.