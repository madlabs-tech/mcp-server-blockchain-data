# onchain-data-mcp

Chain- and provider-agnostic blockchain data for AI agents that move money: payments, stablecoin, neobank and trading. One binary, one config, exposed as **MCP** (stdio or streamable HTTP) and **REST**.

- **Chains:** Ethereum, Base, Arbitrum, Optimism, Polygon, Avalanche, BSC, **Robinhood Chain** and **Solana** (`registry/chains.toml`; add your own with `[[extra_chains]]`).
- **Vendors:** ~30 data providers, all usable on their free tier. You pick the order per capability; the router fails over, meters quota locally and stops routing to a vendor before it runs out.
- **Zero-key start:** with no API keys it runs on public RPCs and keyless vendors. Tools that need a key answer `UNSUPPORTED_CAPABILITY` with a hint.
- **Non-custodial:** the server never holds keys. `tx_build_transfer` / `trade_build_swap_tx` return unsigned transactions; `tx_broadcast` takes an already-signed one.
- **Two modes:** `self_hosted` (you, your keys, localhost) or `hosted` (an operator serving others with client keys, per-client limits and a separate admin port).

## Quick start

```bash
cargo build --release -p bdm-server        # binary: target/release/onchain-data-mcp (Rust 1.90)
./target/release/onchain-data-mcp serve      # REST + MCP + dashboard on http://127.0.0.1:8787
curl -s http://127.0.0.1:8787/v1/tools | head -c 400
curl -s -X POST http://127.0.0.1:8787/v1/chain/list -H 'content-type: application/json' -d '{}'
```

No keys needed for the above. To add vendors, `cp .env.example .env`, fill in the keys you have, and `set -a; source .env; set +a` (or use `config/secrets.toml`, or the dashboard).

**Claude Desktop (MCP over stdio).** Running the binary without a subcommand serves MCP on stdio; logs go to stderr.

```json
{
  "mcpServers": {
    "onchain-data": {
      "command": "/path/to/target/release/onchain-data-mcp",
      "args": ["--config-dir", "/path/to/config"],
      "env": {
        "ODM__SERVER__DATA_DIR": "/path/to/data",
        "ALCHEMY_API_KEY": "…",
        "HELIUS_API_KEY": "…"
      }
    }
  }
}
```

While Claude Desktop runs it, the dashboard is also up at `http://127.0.0.1:8787/dashboard` (if the port is free). Use absolute paths: Claude Desktop starts the process with cwd `/`, and without a writable data dir the usage counters live in memory only.

**MCP over HTTP.** Point any streamable-HTTP MCP client at `http://127.0.0.1:8787/mcp`.

**CLI**

```
onchain-data-mcp [--config-dir DIR]           MCP on stdio + HTTP on server.http_bind (self-hosted)
onchain-data-mcp serve [--config-dir DIR]     HTTP only
onchain-data-mcp clients create <name>        hosted mode: mint a client key (printed once)
onchain-data-mcp clients list                 hosted mode: ids, status, names (never keys)
```

`--config-dir` defaults to `./config`.

## Configuration

Layered, later wins:

```
built-in registry (registry/*.toml, compiled in)
  < config/config.toml      (the dashboard writes this; start from config/config.example.toml)
  < config/secrets.toml     (API keys, mode 0600, written by the dashboard)
  < environment             (ODM__<PATH> with "__" between segments, plus the vendor key vars)
```

Anything set by env is shown as **locked by env** in the dashboard and cannot be edited there. `POST /admin/api/reload` or `kill -HUP <pid>` re-reads the files.

**Env mapping.** `ODM__SERVER__HTTP_BIND=127.0.0.1:8787` sets `[server] http_bind`; `ODM__VENDORS__ALCHEMY__CAP__MONTHLY_CREDITS=15000000` sets `[vendors.alchemy.cap] monthly_credits`; `ODM__ROUTING__DEFAULTS__EVM_RPC=alchemy,quicknode,public` sets an order. Vendor keys use their own names (`ALCHEMY_API_KEY`, `QN_ENDPOINT_NAME`, …; see the vendor table).

**Server** (`[server]`): `mode` (`self_hosted` | `hosted`), `http_bind` (self-hosted, default `127.0.0.1:8787`), `public_bind` + `admin_bind` (hosted, admin defaults to `127.0.0.1:8788`), `dashboard` (bool), `tool_profile`, `enabled_tools` / `disabled_tools`, `data_dir` (default `./data`, holds `bdm.db`), `cache_max_entries`, `quota_poll_secs`, `warmup` (default `true`: right after startup, a background task sends one `eth_chainId` to every active EVM RPC and logs endpoints that serve the wrong chain; never blocks startup).

**Vendor budget** (`[vendors.<id>]`):

| Field | Meaning |
|---|---|
| `enabled` | on/off (unverified free tiers ship disabled) |
| `limit.{rps,per_minute,daily,monthly}` | the vendor's real quota; defaults to its free tier from `registry/vendors.toml` |
| `cap.{…}` | your own budget below the limit (`monthly_credits` / `daily_requests` are accepted aliases) |
| `reserve_pct` | stop routing to the vendor at `(100 − reserve_pct)%` of the budget |
| `on_exhausted` | `skip` (next vendor) or `allow_overage` |
| `costs.<method>` | override the per-call cost estimate |
| `alert_pct` | thresholds logged once per window (default 75/90) |

Effective budget per window = `min(cap, limit × (1 − reserve_pct/100))`.

**Routing order.** A list per capability = primary, then fallbacks. Most specific wins:

```
[operations.<op>.order]  <capability> = [...]   # one tool
  > [routing.chains."<caip2>"]  <capability> = [...]   # one chain
  > [routing.defaults]  <capability> = [...]           # everywhere
  > built-in order in registry/vendors.toml (global, then per chain)
```

Capabilities: `evm_rpc`, `solana_rpc`, `token_balances`, `transfer_history`, `fee_estimate`, `simulate`, `broadcast`, `private_relay`, `price`, `price_history`, `token_metadata`, `token_risk`, `swap_quote`, `sanctions`, `fx`. Two pseudo-vendors always exist: `public` (keyless RPCs from `chains.toml`) and `rpc` (generic on-chain implementations over whatever RPC is routed).

Per operation you can also set `strategy` (`failover` | `quorum` | `aggregate` | `fan_out` | `hedged`), `quorum`, `fan_out`, `hedge_delay_ms`, `cache_ttl_secs`, `enabled`.

**Tool profiles.** `server.tool_profile` picks which tools an MCP client sees: `payments`, `trading`, `neobank`, `defi`, `all`, or `custom` (then list them in `enabled_tools`). `disabled_tools` always applies. In hosted mode each client key carries its own profile.

**Custom RPC / chains.** `[custom_rpc.<name>] chain = "eip155:1", url = "…"` adds an endpoint as a vendor named `<name>`; `[chain_overrides."eip155:56"] enabled = false`; `[[extra_chains]]` uses the schema of `registry/chains.toml`. Every EVM endpoint is checked against `eth_chainId` on first use and disabled on mismatch.

## Tools

34 tools. REST path = `/v1/<domain>/<name without the domain prefix>` (`wallet_get_balances` → `POST /v1/wallet/get_balances`); tools that don't carry the prefix keep their full name (`POST /v1/wallet/address_validate`). `GET /v1/tools` lists what the caller can see, `GET /openapi.json` has the schemas. MCP and REST return identical JSON.

| Tool | Domain | What it does | Profiles |
|---|---|---|---|
| `chain_list` | chain | List the enabled chains (CAIP-2 id, aliases, native asset, block time, finality policy) and, per chain, which capabilities are available and through which vendors in the user's configured order | all |
| `chain_finality` | chain | Current head and finality checkpoints of a chain | all |
| `provider_health` | chain | Read-only health of every data vendor | all |
| `wallet_get_balances` | wallet | Native coin and token balances of one wallet across chains (all EVM chains for a 0x address, Solana for a base58 address, or the `chains` you list) | all |
| `wallet_get_transfers` | wallet | Incoming and/or outgoing native and token transfers of a wallet on one chain, newest first, paginated with `cursor` (pass `next_cursor` back unchanged) | payments, neobank, trading |
| `address_validate` | wallet | Check a recipient address before sending funds on a chain | all |
| `tx_get` | tx | Fetch one transaction, normalized across EVM and Solana | all |
| `tx_status` | tx | Lightweight status of a transaction | all |
| `tx_estimate_fee` | tx | Current network fee tiers (slow / standard / fast) for a simple transfer on a chain, with the estimated total in the native coin and in fiat | all |
| `tx_simulate` | tx | Dry-run an unsigned transaction without broadcasting it | all |
| `tx_build_transfer` | tx | Build an UNSIGNED transfer of the native coin or a token (ERC-20 on EVM; SPL / Token-2022 on Solana) for the user to sign with their own wallet, then simulate it | payments, neobank, trading |
| `tx_broadcast` | tx | Broadcast an already-SIGNED transaction to every configured provider at once (the hash is computed locally, so duplicate sends are safe) and return the hash plus which providers accepted it | all |
| `payments_verify_transfer` | payments | Verify that one on-chain transaction paid an expected amount of a canonical stablecoin to a recipient (EVM or Solana) | payments, neobank |
| `payments_list_deposits` | payments | List incoming canonical-stablecoin deposits to up to 20 addresses on one chain, with per-address cursors for polling | payments, neobank |
| `payments_build_request` | payments | Build a payment request for a canonical stablecoin (EIP-681, Solana Pay, x402) | payments, neobank |
| `stablecoin_resolve` | stablecoin | Resolve a stablecoin against the verified registry | payments, neobank |
| `stablecoin_check_restrictions` | stablecoin | Check issuer controls for an address on-chain | payments, neobank |
| `stablecoin_peg` | stablecoin | Check whether a canonical stablecoin is holding its peg | payments, neobank, trading |
| `compliance_screen_address` | compliance | Screen an address before paying it or accepting its funds | payments, neobank |
| `neobank_card_funding_status` | neobank | Explain whether a non-custodial stablecoin card (Bridge / Stripe, Baanx and similar just-in-time programs) can be funded, and why an authorization was declined | neobank |
| `neobank_get_ledger` | neobank | Bank-style statement for a wallet on one chain | neobank |
| `fiat_get_fx_rate` | neobank | Fiat exchange rate between two ISO 4217 currencies, latest or for a date (YYYY-MM-DD), as an exact decimal | neobank, payments |
| `market_get_price` | market | Current price of a token from several independent sources | trading, defi, neobank |
| `market_get_price_at` | market | Historical price of a token at a point in time, from the first source in the price_history order that has data (CoinGecko Demo covers the last 365 days) | trading, defi, neobank |
| `token_get_metadata` | market | Token decimals, symbol, name and logo | all |
| `token_check_risk` | market | Scam and honeypot check before buying a token | trading |
| `trade_get_swap_quote` | trade | Swap quotes from several aggregators in parallel (default 1inch, Velora, CoW; Jupiter on Solana) | trading |
| `trade_build_swap_tx` | trade | Build an unsigned swap transaction for an external signer (non-custodial) | trading |
| `rwa_token_info` | rwa | Tokenized-stock facts | trading |
| `rwa_price` | rwa | Oracle price of a tokenized stock from its Chainlink equity feed (updates 24/5) | trading |
| `eth_get_balance` | legacy | Get the ETH/native token balance of an address | all |
| `eth_get_code` | legacy | Detect whether an address is a contract or wallet | all |
| `eth_gas_price` | legacy | Get the current gas price on the specified chain | all |
| `eth_get_transaction_by_hash` | legacy | Get transaction details by hash | all |

"all" = payments, trading, neobank, defi. The `defi` domain is reserved and empty. Full descriptions and input schemas: `GET /v1/tools`.

## Vendors

From `registry/vendors.toml`. "Free tier" is the built-in `limit`; "powers" is the capability the built-in routing order uses the vendor for. Vendors with `free_tier_verified = false` are off by default (enable in `[vendors.<id>]`).

| Vendor | Key | Env var(s) | Free tier (built-in limit) | Powers |
|---|---|---|---|---|
| `public` (keyless RPCs from `chains.toml`) | no | – | 5 rps, self-limit | evm_rpc, solana_rpc, broadcast (last resort) |
| `rpc` (on-chain via routed RPC) | no | – | metered on the underlying RPC | token_balances, transfer_history, fee_estimate, simulate, token_metadata |
| Alchemy | yes | `ALCHEMY_API_KEY` | 30M credits/month | evm_rpc, token_balances, transfer_history, broadcast |
| QuickNode | yes | `QN_ENDPOINT_NAME`, `QN_TOKEN_ID` | 10M credits/month, 15 rps | evm_rpc, solana_rpc, broadcast, fee_estimate (Solana) |
| Helius | yes | `HELIUS_API_KEY` | 1M credits/month, 10 rps | solana_rpc, token_balances, fee_estimate, broadcast, token_metadata (Solana) |
| Moralis | yes | `MORALIS_API_KEY` | 40k CU/day | token_balances, transfer_history (EVM) |
| Ankr Advanced API | yes | `ANKR_API_KEY` | unverified, disabled | not in the built-in order |
| CoinGecko (Demo) | yes | `COINGECKO_API_KEY` | 10k/month, 30/min | price, price_history, token_metadata |
| GeckoTerminal | no | – | 10/min | price |
| DefiLlama | no | – | 60/min (self-limit) | price, price_history |
| DexScreener | no | – | 300/min | price |
| Birdeye | yes | `BIRDEYE_API_KEY` | 30k credits/month, 1 rps | price (Solana) |
| Jupiter | optional | `JUPITER_API_KEY` | 30/min keyless, 60/min with key | price, token_metadata, swap_quote (Solana) |
| Pyth (Hermes / Benchmarks) | yes | `PYTH_API_KEY` | 60/min | price_history (native coins) |
| GoPlus Security | optional | `GOPLUS_APP_KEY`, `GOPLUS_APP_SECRET` | 30/min | token_risk |
| honeypot.is | no | – | 30/min (self-limit) | token_risk (Ethereum, BSC, Base) |
| RugCheck | optional | `RUGCHECK_API_KEY` | 60/min, unverified | token_risk (Solana) |
| 1inch | yes | `ONEINCH_API_KEY` | 100k/month, 1 rps | swap_quote |
| Velora (ParaSwap) | no | – | 60/min | swap_quote |
| CoW Protocol | no | – | 60/min | swap_quote (quotes only) |
| 0x Swap API | yes | `ZEROEX_API_KEY` | no free tier, disabled | not in the built-in order |
| Uniswap Trading API | yes | `UNISWAP_API_KEY` | 6 rps, unverified, disabled | not in the built-in order |
| OKX DEX | yes | `OKX_API_KEY`, `OKX_SECRET_KEY`, `OKX_PASSPHRASE` | unverified, disabled | not in the built-in order |
| Flashbots Protect | no | – | keyless | private_relay (Ethereum) |
| MEV Blocker | no | – | keyless | private_relay (Ethereum) |
| Jito block engine | no | – | 1 rps | private_relay (Solana) |
| Helius Sender | no | – | 50 rps | private_relay (Solana) |
| Chainalysis sanctions oracle | no | – | on-chain, metered on RPC | sanctions (EVM, not Robinhood Chain) |
| TRM Labs sanctions API | optional | `TRM_API_KEY` | 100/day, 1 rps keyless | sanctions |
| Frankfurter (ECB) | no | – | 60/min (self-limit) | fx |
| Open Exchange Rates | yes | `OPENEXCHANGERATES_APP_ID` | 1,000/month | fx |

Signup URLs and per-method cost tables are in `registry/vendors.toml`. Quota shown in the dashboard is **estimated** from local metering unless the vendor exposes a usage API.

## Dashboard and admin API

`/dashboard` (self-hosted: on `http_bind`; hosted: on `admin_bind` only). Sign in with the token from `<config-dir>/admin_token`, generated on first start (mode 0600) and printed once in the log. Pages: overview (live call log), vendors (keys are write-only), routing, quota (limit / cap / used per vendor, source badge `vendor API` / `headers` / `estimated`, burn rate, CSV export), chains, tools, clients (hosted).

Every `/admin/api/*` call needs `Authorization: Bearer <admin token>` **and** `X-BDM-Admin: 1` (CSRF guard). No response ever contains a key.

| Endpoint | Purpose |
|---|---|
| `GET /admin/api/health` | vendor breaker/latency/usage, per-tool call stats |
| `GET /admin/api/config` | settings (secrets scrubbed), locked-by-env map, key status, effective orders, tools, chains |
| `PUT /admin/api/config` `{"edits":[{"path":[…],"value":…}]}` | validate, write atomically (`.bak` kept), hot-swap routing; `null` removes; env-locked paths → 422 |
| `POST /admin/api/config/validate` | same checks, nothing written |
| `POST /admin/api/reload` | re-read the files (same as `SIGHUP`) |
| `POST /admin/api/vendors/{id}/test` | one cheap call against the vendor |
| `POST /admin/api/vendors/{id}/budget` `{"which":"cap","window":"monthly","value":15000000}` | set or clear (`null`) one limit/cap window |
| `GET /admin/api/quota`, `GET /admin/api/quota.csv`, `POST /admin/api/quota/refresh` | quota report, CSV, poll vendor usage APIs now |
| `GET/POST /admin/api/clients`, `PATCH/DELETE /admin/api/clients/{id}` | list, create (key returned once), set limits, revoke |
| `GET /admin/api/calls?limit=N`, `GET /admin/api/calls/stream` | recent calls, live SSE stream |

Public router (both modes): `POST /v1/<domain>/<tool>`, `POST /v1/tools/<name>`, `GET /v1/tools`, `GET /openapi.json`, `/mcp`, `GET /healthz`, `GET /metrics` (Prometheus text).

## Hosted mode

`mode = "hosted"` turns the server into a shared instance run by an operator:

- HTTP only (never stdio). Public REST + `/mcp` on `public_bind`, dashboard + admin API on `admin_bind` (default `127.0.0.1:8788`, never the public port).
- Every public request needs `Authorization: Bearer <client key>`; unauthenticated requests get 401. Keys are stored as SHA-256 hashes and shown once.
- **Fails closed:** refuses to start until at least one client key exists (`onchain-data-mcp clients create <name>`).
- Per-client limits from `[clients.default]`, overridable per key: `requests_per_minute` (token bucket), `daily_requests` (reset 00:00 UTC), `monthly_credits` (vendor credits spent on the client's behalf, reset on the 1st), `tool_profile`. Over the limit → HTTP 429 `QUOTA_EXCEEDED` with `Retry-After`.
- Vendor limits/caps still apply on top; set a `cap` below every `limit` the public instance uses.

Client config:

```json
{ "mcpServers": { "onchain-data": {
  "url": "https://your.domain/mcp",
  "headers": { "Authorization": "Bearer odm_…" } } } }
```

Deployment (Docker + Caddy, systemd, backups, security checklist): **[DEPLOYMENT.md](DEPLOYMENT.md)**.

## Legacy compatibility

- The binary is still named `onchain-data-mcp`, so existing MCP client configs keep working.
- The four original tools (`eth_get_balance`, `eth_get_code`, `eth_gas_price`, `eth_get_transaction_by_hash`) are kept as aliases in the `legacy` domain with unchanged chain names; prefer `wallet_get_balances`, `address_validate`, `tx_estimate_fee`, `tx_get`.
- `RPC_URL` is deprecated and now applies to **Ethereum (`eip155:1`) only** (it becomes `[custom_rpc.rpc_url]`, locked by env, with a startup warning). It used to be applied to every chain. Use vendor keys or `[custom_rpc]` per chain instead.

## Development

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo hack check -p bdm-adapters --each-feature --no-dev-deps   # every vendor builds alone
```

Workspace crates: `domain` (types), `ports` (vendor traits), `config` (registry + layering), `routing` (orders, budgets, breakers), `protocols` (on-chain implementations: Multicall3, getLogs, oracles, Solana wire), `adapters` (one module per vendor, **one cargo feature per vendor**; default = verified free tiers), `app` (tool catalog, `crates/app/src/ops/*`), `store` (sqlite: usage, clients, call log), `transport-mcp`, `transport-http` (REST + dashboard), `server` (binary), `testkit`.

To add a vendor: a module in `crates/adapters/src/vendors/`, a feature in `crates/adapters/Cargo.toml`, an entry in `registry/vendors.toml`. To add a tool: an `Operation` in `crates/app/src/ops/<domain>.rs` with `NAME`, `DOMAIN`, `DESCRIPTION`, `PROFILES`, registered in that module's `register`.

## License

MIT
