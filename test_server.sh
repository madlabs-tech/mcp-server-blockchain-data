#!/bin/bash

# Test script for EVM MCP Server

echo "Starting EVM MCP Server test..."

# Set RPC URL (using public demo endpoint)
export RPC_URL="https://eth-mainnet.g.alchemy.com/v2/demo"

# Test with MCP inspector if available
if command -v npx &> /dev/null; then
    echo "Testing with MCP Inspector..."
    echo "Run: npx @modelcontextprotocol/inspector cargo run --release"
    npx @modelcontextprotocol/inspector cargo run --release
else
    echo "Running server directly..."
    cargo run --release
fi