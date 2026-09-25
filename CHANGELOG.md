# Changelog

## 0.2.0 — 2026-09-25

The first public release under the name **onchain-data-mcp**.

### New name

- The project is now called **onchain-data-mcp** (it was briefly called blockchain-data-mcp).
- Settings now start with `ODM__`, for example `ODM__SERVER__MODE=hosted`. The old `BDM__`
  settings still work for now, but the program warns you to rename them.
- New client keys start with `odm_`. Old keys keep working.

### Dashboard password

- The dashboard is protected by a **dashboard password** (it used to be called the "admin token").
- It is made for you on first start. See it any time with `onchain-data-mcp password`.
  That command also prints a one-click login link.
- Set your own with `DASHBOARD_PASSWORD` (at least 12 characters, no spaces).
- Make a new one with `onchain-data-mcp password reset`.
- The program prints the dashboard address every time it starts (never the password).
- If you have an old `admin_token` file, it is renamed to `dashboard_password` automatically.
  Your password stays the same.

### Brand-new dashboard

- A new look, with a side menu and eight screens: Login, Setup guide, Overview, Providers,
  Routing, Tools & Chains, Clients and Connect.
- The Setup guide has three steps: add keys, pick tools, connect your AI app.
- The Connect screen gives ready-to-copy settings for Claude Desktop, Claude Code and Cursor.
- Fonts are included, so the dashboard works without internet access.

### Data providers re-checked (2026-09-25)

- Every provider now has a **tier** (1 to 4), from "free, no sign-up" to "paid only".
  See [docs/VENDORS.md](docs/VENDORS.md).
- **Moralis**, **QuickNode** and **Pyth** no longer have a lasting free plan, so they are
  **off by default**. Turn them on if you pay for them.
- CoinGecko's free plan now allows 100 requests a minute (was 30).
- Velora's free plan is now 1 request a second and 5,000 a day.
- Alchemy's cost per request was lowered to match its real prices, so your free credits go further.
- RugCheck keys are now sent the right way.
- RugCheck scam checks for Solana are now on by default (no key needed).

### Easy install

- Ready-made installers for Mac, Linux and Windows, plus Homebrew
  (`brew install madlabs-tech/tap/onchain-data-mcp`) and a Docker image.
- One-click install for Claude Desktop: download the `.mcpb` file and double-click it.
- Listed in the official MCP Registry.
- `onchain-data-mcp --version` shows the version.

### Other

- Windows is now fully supported and tested.
- Added the MIT license and a security policy ([SECURITY.md](SECURITY.md)).
- The program no longer crashes on strange data from providers; it reports an error and moves on.
- On start, it quietly checks that every blockchain connection points at the right chain.

### The big rebuild (technical details)

Before 0.2.0 this was a small Ethereum-only MCP server. It grew into a full blockchain data
service for AI agents. The rest of this section is for developers.

#### Added
- Chains: Ethereum, Base, Arbitrum, Optimism, Polygon, Avalanche, BSC, Robinhood Chain (4663) and Solana. CAIP-2/10/19 identifiers with friendly aliases.
- 30 tools (plus 4 legacy aliases) across chain, wallet, transactions, payments, stablecoin, compliance, neobank, market/trading and tokenized stocks; MCP over stdio and streamable HTTP (`/mcp`), REST (`/v1/<domain>/<tool>`), OpenAPI (`/openapi.json`).
- About 30 provider adapters (free tiers by default) behind user-ordered failover with retries, circuit breakers, quota guard (per-provider limit / cap / reserve) and provenance on every response.
- On-chain readers over the routed RPC (`rpc` pseudo-provider): Multicall3 balances, transfer scans, EIP-1559 + L2 data fees, Chainlink, ERC-8056, issuer freeze/blacklist checks, the Chainalysis sanctions oracle, SPL/Token-2022 parsing.
- Stablecoin registry (31 issuer-sourced entries) and tokenized-stock registry (Robinhood Chain).
- Dashboard with a setup wizard, quota page and admin API; hosted mode with client keys and per-client limits (refuses to start without a key).
- Docker image, docker-compose + Caddy, systemd unit.

#### Changed
- The program was renamed from `evm-mcp-server`. Update your MCP app settings.
- Settings are layered: built-in defaults < `config.toml` < `secrets.toml` < environment variables; values set by environment variables are locked in the dashboard.
- The original tools `eth_get_balance`, `eth_get_code`, `eth_gas_price` and `eth_get_transaction_by_hash` are kept as legacy aliases with the same output.

#### Fixed
- `RPC_URL` now applies to Ethereum only; it used to override every chain.
- GoPlus sign-in (`Authorization` takes the raw access token).
