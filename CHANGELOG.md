# Changelog

## 0.2.0 (unreleased)

The single-file EVM MCP server became **blockchain-data-mcp**: a chain- and provider-agnostic
blockchain data aggregator (MCP + REST) for payment, stablecoin, neobank and trading agents.

### Added
- Chains: Ethereum, Base, Arbitrum, Optimism, Polygon, Avalanche, BSC, Robinhood Chain (4663) and Solana. CAIP-2/10/19 identifiers with friendly aliases.
- 34 tools across chain, wallet, transactions, payments, stablecoin, compliance, neobank, market/trading and tokenized stocks; MCP over stdio and streamable HTTP (`/mcp`), REST (`/v1/<domain>/<tool>`), OpenAPI (`/openapi.json`).
- 28 vendor adapters (free tiers by default) behind user-ordered failover with retries, circuit breakers, quota guard (per-vendor limit / cap / reserve) and provenance on every response.
- On-chain readers over the routed RPC (`rpc` pseudo-vendor): Multicall3 balances, transfer scans, EIP-1559 + L2 data fees, Chainlink, ERC-8056, issuer freeze/blacklist checks, the Chainalysis sanctions oracle, SPL/Token-2022 parsing.
- Stablecoin registry (31 issuer-sourced entries) and tokenized-stock registry (Robinhood Chain).
- Dashboard with a 4-step setup wizard (providers & keys → routing → tools → connect), quota page, admin API; hosted mode with client keys and per-client limits (fails closed without a key).
- Docker image, docker-compose + Caddy, systemd unit.

### Changed
- Binary renamed `evm-mcp-server` → `blockchain-data-mcp` (no alias). Update MCP client configs.
- Config is layered: built-in registry < `config.toml` < `secrets.toml` < env (`BDM__<PATH>`); env-set values are locked in the dashboard.
- Legacy tools `eth_get_balance`, `eth_get_code`, `eth_gas_price`, `eth_get_transaction_by_hash` are kept as aliases with identical output for one release.

### Fixed
- `RPC_URL` now applies to Ethereum only; it used to override every chain.
- GoPlus authentication (`Authorization` takes the raw access token).
