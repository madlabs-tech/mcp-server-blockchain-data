# Plan: Free-tier-first, chain- & provider-agnostic blockchain data aggregator (MCP + REST + config dashboard)

> Approved 2026-09-23. Source of truth for scope and architecture. Task status lives in [TASKS.md](TASKS.md); vendor keys in [VENDORS.md](VENDORS.md); hosting in [DEPLOYMENT.md](DEPLOYMENT.md); sourced findings in [research/](research/).

## Context
Today the repo is `evm-mcp-server`, a small Rust MCP server:
- 4 EVM tools in `src/main.rs`
- one Alloy client per chain
- a hardcoded `Chain` enum
- lots of dead code (below)

**Bug:** setting `RPC_URL` makes every chain use one URL (`src/core/chains.rs:62`).

**Goal:** a self-hosted, non-custodial aggregator.
- **Chains:** EVM (Ethereum, Base, Arbitrum, Optimism, Polygon, Avalanche, BSC, Robinhood Chain 4663) and Solana.
- **Interfaces:** MCP (stdio + streamable HTTP) and REST.
- **Focus:** payments, stablecoins and neobanks first; trading, DeFi and tokenized stocks second.

**User requirements (latest):**
1. **Free-quota vendors only** in the default build and routing. Paid-only vendors are left out, and the ports allow adding them later.
2. **User-controlled vendor order** per capability, per chain and per tool: primary → fallback 1 → fallback 2…
3. A **configuration dashboard** for the MCP itself.
4. **Delete unused files.**
5. Create a **`plan/` folder with plan and tasks markdown** in the repo.
6. The work is executed with **Claude Agent Teams**.
7. A **vendor list in `plan/`** so the user can create API keys and test.
8. **Two deployment modes:**
   - **self-hosted**: any user runs it with their own keys.
   - **hosted**: the user runs it on a small VPS with their own vendor accounts, and other people use it.

   In hosted mode the operator sets per-vendor **limits, usage and quota caps** through the dashboard **or env**, plus per-client limits, so shared free quotas can't be drained.

**Positioning (from 8 research agents):** existing MCPs wrap one vendor per action, and most toolkits that sign transactions hold keys. Our edge:
- The same payment verification semantics on EVM and Solana.
- A canonical stablecoin registry with freeze/pause checks.
- Quotes and prices from several vendors, returning the spread between them.
- Provenance on every answer.
- A useful **zero-key mode** plus **free-quota stretching**: the router spreads load across several free tiers and moves to the next vendor before one runs out.

**Decisions made:**
- Vendors are the primary data source; direct on-chain reads are the source of truth where money is at stake.
- The Graph only for event-history needs (its free 100K queries/month).
- Bring your own keys; non-custodial. We build unsigned txs and broadcast signed ones.
- x402: we verify payments; settlement is forwarded to the user's own facilitator (CDP has a free 1K tx/month tier).
- Keep the binary name `evm-mcp-server` so existing Claude Desktop configs keep working.

## Step 0: housekeeping (first commit of execution)

**Delete (all verified unused by grep):**
- `src/core/services/{balance,blocks,contracts,ens,tokens,transactions,transfer}.rs`: 0-byte stubs, plus their `mod`/`pub use` lines.
- `src/core/services/utils.rs`: 227 lines, nothing references it, and it has f64 money helpers (`wei_to_eth`) that break our money rule. Alloy's `parse_units`/`format_units` replace it.
- Dead functions `clear_client_cache` (`clients.rs:53`) and `get_supported_chains` (`chains.rs:175`), plus the re-export at `core/mod.rs:4`.
- `IMPLEMENTATION.md` and `README_RUST.md`: stale duplicates of README. README is rewritten in Phase 2.
- `test_server.sh`: depends on the Alchemy `demo` key. Replaced by the documented Inspector command and `cargo test`.

After deleting, `cargo build` passes and the 4 tools behave the same (checked by the characterization tests in T0.3).

**Create `plan/`:**
```
plan/
  PLAN.md              # this plan (architecture, config, dashboard, tool catalog, decisions)
  TASKS.md             # master task list: ID, phase, owner, deps, acceptance criteria, [ ] status
  VENDORS.md           # API-KEY CHECKLIST + free-tier matrix + default routing order + excluded paid vendors
  DEPLOYMENT.md        # self-hosted vs hosted (VPS) setup, limits via env/dashboard, reverse proxy, backups
  research/            # sourced summaries from the 8 research agents (kept for future reference)
    evm.md solana.md robinhood.md trading.md defi.md payments.md neobank.md stablecoin.md
.env.example           # every env var: vendor keys + per-vendor limit/cap overrides + client defaults
config/config.example.toml
```

**`plan/VENDORS.md` API-key checklist.** One row per vendor:
- signup link (taken from the research sources and checked with a fetch when written)
- env var name(s)
- free tier (quota, RPS)
- what it powers (tools/capabilities)
- setup notes (e.g. turn on Robinhood Chain and Base in the Alchemy app; QuickNode needs a multichain endpoint or one per chain)
- a `[ ] key created` checkbox

The rows are grouped:
- **Tier A: needed to test R1 (create now).** Alchemy, Helius, QuickNode (a 2nd RPC for failover tests), CoinGecko Demo, GoPlus, 1inch, TRM sanctions. Optional: Jupiter key, Birdeye, Moralis, Open Exchange Rates.
- **Tier B: R2.** Zerion, The Graph, Coinbase CDP (x402 facilitator + Onramp), Pimlico, Pyth Benchmarks, Safe API key.
- **Keyless: nothing to create.** Public RPCs (incl. Robinhood), DefiLlama, DexScreener, GeckoTerminal, Velora, CoW, Flashbots, MEV Blocker, Jito, Helius Sender, honeypot.is, Chainalysis oracle, Circle Iris, Frankfurter, Hyperliquid, Morpho, Lido/Jito APIs.
- **Free tier unconfirmed: optional, disabled by default.** Ankr, 0x, Uniswap API, OKX DEX, RugCheck, LI.FI, Across.

## Architecture: Hexagonal (Ports & Adapters), Cargo workspace

Dependency rule: `domain ← ports ← {protocols, app} ← {adapters, transports}`. `server` is the only composition root.

```
crates/
  domain/        # pure types: ChainId(CAIP-2) AccountId(CAIP-10) AssetId(CAIP-19), Amount{raw:U256,decimals},
                 #   Fiat(rust_decimal)+as_of+source, Finality{Pending|Confirmed(n)|Safe|Finalized|Unverifiable},
                 #   Transfer, Tx, Fee, Price, Quote, RiskReport, Provenance, DomainError(codes). No I/O.
  ports/         # small capability traits (Interface Segregation) + ProviderError{Transient|RateLimited|
                 #   QuotaExhausted|Unsupported|NotFound|Invalid|Fatal}. Capability ids = config keys.
  config/        # schema, loading (file + env + secrets), validation, order resolution, built-in defaults
  routing/       # ProviderRegistry, strategies, resilience, QUOTA GUARD, health; hot-swappable via ArcSwap
  protocols/     # contract/program readers over any RPC (multicall3, erc20, erc4626, erc8056, issuer
                 #   controls, chainlink, chainalysis oracle, ens, op/arbitrum fee oracles, spl/token-2022,
                 #   solana-pay, sns). They get failover for free through the RPC ports.
  app/           # Operation (Command pattern) per tool + Catalog + tool profiles
  adapters/      # one module per vendor behind a cargo feature (Adapter + Anti-Corruption Layer + Factory)
  transport-mcp/ # rmcp ServerHandler list_tools/call_tool generated from the Catalog (verified in rmcp 0.8.3)
  transport-http/# REST /v1/<domain>/<op> + OpenAPI + MCP HTTP (/mcp) + admin API (/admin/api) + dashboard
  store/         # sqlite (rusqlite, bundled): quota usage counters, call log, later cursors/subscriptions
  server/        # bin `evm-mcp-server`: config → factories → registry → catalog → transports
  testkit/       # fake JSON-RPC/HTTP servers, recorded fixtures, port conformance suites, mocks
registry/        # built-in data: chains.toml, stablecoins.toml, rwa.toml, vendors.toml (free-tier limits)
```

**Patterns:**
- Hexagonal
- Interface Segregation (ports)
- Adapter + Anti-Corruption Layer (vendor DTOs never leave the adapter)
- Abstract Factory + Registry (config decides the vendors)
- Strategy (failover / quorum / aggregate / fan-out / hedged)
- Chain of Responsibility (fallback chain)
- Decorator (resilience + quota + cache + metrics around ports and operations)
- Command (one Operation drives MCP + REST + OpenAPI + docs)
- Observer (call-log SSE, later deposit watchers)
- Repository (store)
- Data-driven registry (adding a chain or stablecoin needs no code)

## Vendor ordering: config model

Files live in `--config-dir`, which defaults to `./config/`:
- `config.toml` holds the settings.
- `secrets.toml` holds API keys. It is written with mode 0600, is git-ignored, and environment variables override it.
- The dashboard edits the same files.

```toml
[server]
http_bind    = "127.0.0.1:8787"
dashboard    = true            # also available alongside stdio mode (Claude Desktop) on localhost
tool_profile = "payments"      # payments | trading | neobank | defi | all | custom (keeps agent tool lists small)

[vendors.alchemy]              # key: env ALCHEMY_API_KEY or secrets.toml
enabled = true
[vendors.alchemy.quota]        # defaults come from registry/vendors.toml (free-tier limits); override here
monthly_credits = 30_000_000
reserve_pct     = 10           # stop routing at 90% used → next vendor
on_exhausted    = "skip"       # skip | allow_overage

[routing.defaults]             # order = primary, then fallbacks
evm_rpc    = ["alchemy", "quicknode", "public"]
solana_rpc = ["helius", "quicknode", "public"]
price      = ["coingecko", "defillama", "geckoterminal", "dexscreener"]
token_risk = ["goplus", "honeypot_is", "rugcheck"]
swap_quote = ["oneinch", "velora", "cow"]
fx         = ["frankfurter", "openexchangerates"]

[routing.chains."eip155:4663"]       # Robinhood Chain override
evm_rpc = ["alchemy", "quicknode", "public"]
[routing.chains."solana:mainnet"]
price   = ["jupiter", "birdeye", "defillama"]

[operations.payments_verify_transfer]
strategy = "quorum"               # failover | quorum | aggregate | fan_out | hedged
quorum   = 2
[operations.market_get_price]
strategy = "aggregate"            # median + spread across the first N available
fan_out  = 3
cache_ttl_secs = 15
```

**How the order is resolved (most specific wins):**
1. `operations.<op>.order`
2. `routing.chains.<caip2>.<capability>`
3. `routing.defaults.<capability>`
4. Built-in default: keyless and free vendors first.

Then vendors are filtered out if they are disabled, have no key when one is needed, don't support the chain (e.g. Ankr on Robinhood), have an open breaker, or have hit the quota guard. Each response's `meta.providersTried` shows the path actually taken.

**Env overrides (for headless VPS setup):**
- Any config key can be set as `EMS__<PATH>` with `__` between path segments, e.g.:
  - `EMS__VENDORS__ALCHEMY__CAP__MONTHLY_CREDITS=15000000`
  - `EMS__ROUTING__DEFAULTS__EVM_RPC=quicknode,alchemy,public`
  - `EMS__CLIENTS__DEFAULT__DAILY_REQUESTS=500`
- API keys also accept the plain `<VENDOR>_API_KEY` names.
- Layering is: built-in registry < `config.toml` (what the dashboard writes) < env. Settings fixed by env show in the dashboard as **"locked by env"** and are read-only, so a dashboard edit is never silently overridden.
- Loaded with `figment` (TOML + env).

**Validation:** unknown vendor, capability or chain names, or a vendor that doesn't implement the capability, show up as startup warnings and dashboard warnings. The config is never applied half-valid.

**Hot reload:** the dashboard saves, or you send SIGHUP → validate → atomic write (temp file + rename, keeping a `.bak`) → swap the routing table with `ArcSwap`. Requests already in flight finish on the old table.

## Quota tracking and guard (every vendor's quota is visible and makes free tiers last)

**Where the quota numbers come from.** Every vendor always has at least one source. Each number carries a badge saying where it came from.

1. **Vendor-reported:** the adapter optionally implements a `QuotaReporter` port that calls the vendor's own usage/key endpoint:

   ```rust
   async fn usage() -> VendorUsage {
       plan, windows: [{ kind, used, limit, resets_at }]
   }
   ```

   It is polled on a configurable interval, 5 min by default, and only calls usage endpoints that don't cost credits. Candidates to check: CoinGecko `/key`, QuickNode Console API usage, Alchemy admin/usage API, Helius credits, Zerion, The Graph billing ⚠. Unconfirmed ones fall back to sources 2 and 3.
2. **Header-reported:** the shared HTTP client reads `X-RateLimit-Limit/Remaining/Reset`, IETF `RateLimit-*` and `Retry-After` from **every** response, giving live remaining counts for per-second, per-minute and per-day windows.
3. **Locally metered:** always available. Every call is counted in credits using the per-method cost table in `registry/vendors.toml` (e.g. Helius DAS = 10 credits, Ankr = 700 per call). Counts go into `store` (sqlite), so they survive restarts, broken down by vendor × window × method × chain × tool.

**Limits vs caps (per vendor, set in the dashboard or env):**
- **`limit`** is the vendor's real quota. Defaults to the free tier in `registry/vendors.toml`: RPS, per-minute/daily/monthly credits, reset rule (calendar vs rolling), plan limits such as the `getLogs` range. Override it when you upgrade a plan.
- **`cap`** is the operator's own budget, set **below** the limit. Example: the hosted instance may only spend 50% of Alchemy's monthly credits, keeping the rest for your own testing. It can be set per window (rps / daily / monthly).
- **The budget the router actually uses** is `min(cap, limit × (1 − reserve_pct))`.
- **Shared response cache** (moka, with a TTL per operation) is the biggest quota saver in hosted mode, because identical requests from different clients hit the vendor only once.

**Guard (routing):**
- Each vendor has a token-bucket limiter (`governor`).
- At `reserve_pct` the vendor is skipped. The router uses the most pessimistic of the three sources.
- A 429 or quota error marks the vendor exhausted until `resets_at`, and the router moves to the next vendor in the user's order.
- Optional alert thresholds (e.g. 75%/90%) surface in the dashboard, logs and `provider_health`.

## Dashboard (`/dashboard`, same binary, localhost)
- **Overview:** status per vendor (breaker state, p50/p95 latency, error rate), a compact quota bar per vendor, and a live call stream over SSE (op, chain, provider picked, whether a fallback was used, latency).
- **Quota page** (one card for **each** vendor, including keyless ones that only have rate limits):
  - plan name, and for each window (per second/minute, daily, monthly): used / limit / remaining, %, reset time
  - a source badge (vendor API · headers · estimated) and the gap between our estimate and the vendor's number
  - burn rate and the **projected run-out date** at the current rate
  - for each window, three numbers side by side: **vendor limit**, **operator cap** and **effective budget**, each editable unless locked by env
  - state: ok / warning / reserve reached / exhausted until X
  - a breakdown of which tools, methods and chains use the quota
  - a 30-day usage chart
  - a "refresh from vendor" button
  - CSV export
  - vendors whose usage API isn't confirmed are labeled "estimated only"
- **Vendors:**
  - enable/disable
  - key status (set / missing / invalid, via a "Test" button)
  - **write-only** key input (saved to `secrets.toml`; keys are never returned)
  - quota overrides
  - signup link and free-tier notes
- **Routing:** drag-and-drop ordered list per capability, a per-chain override tab, the **effective order** after filtering, and warnings.
- **Tools:** tool profile, enable/disable per tool, strategy and cache TTL per operation.
- **Chains:** enable/disable, finality policy, custom RPC URLs, and adding an EVM chain from a registry entry.
- **Clients page (hosted mode):**
  - create or revoke client API keys (shown once, stored as SHA-256 hashes)
  - per-client limits: requests per minute, daily requests, monthly credits (vendor credits spent on their behalf), allowed tool profile or tools
  - per-client usage, top tools and a "throttled" count
  - defaults in `[clients.default]`
- **How it's built:** static HTML + vanilla JS embedded with `include_str!` (no Node build) over a JSON admin API at `/admin/api/*`. The same API is scriptable.
- **Security:**
  - binds to localhost by default
  - admin bearer token generated on first run (file mode 0600)
  - admin calls must send a custom header, which blocks CSRF
  - secrets and key-bearing URLs are redacted in every response and log
  - request size limits
- **Admin is not exposed over MCP,** so agents can't reroute their own data sources. MCP gets only the read-only `provider_health` tool.

## Deployment modes → `plan/DEPLOYMENT.md`
| | `mode = "self_hosted"` (default) | `mode = "hosted"` (operator's VPS) |
|---|---|---|
| Who runs it | each user, with their own vendor keys | you, with your vendor accounts, serving other people |
| Transports | stdio (Claude Desktop) + HTTP on 127.0.0.1 | MCP HTTP (`/mcp`) + REST on `public_bind`, behind a TLS reverse proxy |
| Client auth | none (local) | **required**: `Authorization: Bearer <client key>`. The server **refuses to start** in hosted mode with no client keys configured (fail closed) |
| Limits | vendor limit/cap only | vendor limit/cap **plus** per-client rate limits and quotas |
| Dashboard / admin API | localhost | on a **separate `admin_bind`** (default `127.0.0.1:8788`), reached over an SSH tunnel, or through the proxy with admin token + IP allowlist. Never on the public port |

**VPS files:**
- multi-stage `Dockerfile` (small image: static binary + sqlite)
- `deploy/docker-compose.yml` with Caddy (automatic TLS)
- `deploy/evm-mcp-server.service` (systemd) as the alternative
- a sqlite volume and a backup note
- a low-memory profile for a small VPS: moka cache size cap, sqlite WAL

## Free-tier vendor matrix (default build) → `plan/VENDORS.md`
| Area | Vendor (key? · free quota) | Notes |
|---|---|---|
| EVM RPC | public RPCs (keyless, rate-limited) · Alchemy (key · 30M CU/mo) · QuickNode (key · 10M credits/mo, 15 RPS ⚠) · Moralis (key · 40k CU/day) · Ankr (key · free credits ⚠, not on Robinhood) · any custom URL (dRPC/Chainstack ⚠) | Free-tier `getLogs` caps (Alchemy 10 blocks, QuickNode 5) → adaptive chunking |
| Solana | public RPC (100 req/10s/IP) · Helius (key · 1M credits/mo, 10 RPS; DAS 2 RPS) · QuickNode · Helius Sender (keyless) · Jito (keyless) | Helius `getTransfersByAddress` is paid → our own parser over plain RPC |
| Prices | CoinGecko Demo (key · 10k/mo, 365 days history) · GeckoTerminal (keyless) · DefiLlama (keyless, non-Pro endpoints) · DexScreener (keyless · 300/min) · Jupiter (keyless 0.5 RPS / free key 1 RPS) · Birdeye (key · 30K CU/mo) · Pyth Hermes (keyless) / Benchmarks (free key) · Chainlink (on-chain) | |
| Swap / MEV | 1inch (key · 1 rps, 100k/mo) · Velora (keyless) · CoW (keyless) · Jupiter · 0x / Uniswap API / OKX (key, free tier ⚠, optional) · Flashbots & MEV Blocker (keyless, Ethereum only) | Odos is shut down; GOAT repo archived |
| Risk / compliance | GoPlus (key · 30/min) · honeypot.is (keyless) · RugCheck (⚠) · Chainalysis sanctions oracle (on-chain, EVM) · TRM (key · 100/day) | |
| Cross-chain | Circle Iris CCTP v2 (keyless · 40 rps) · LayerZero OFT (on-chain) · LI.FI / Across (⚠) | CCTP V1 paused 2026-12-01 |
| Fiat / ramps | Frankfurter (keyless, ECB) · Open Exchange Rates (key · 1k/mo) · Coinbase Onramp quotes (CDP key) | |
| DeFi | DefiLlama yields · Zerion (key · 2k/day) · Morpho GraphQL (keyless · 750/min) · AaveKit / Kamino (keyless ⚠) · Lido / Jito APIs · Hyperliquid info (keyless) · The Graph (key · 100K/mo) | |
| Payments / infra | CDP x402 facilitator (key · 1K tx/mo) · Safe Tx Service (keyless · 5k/mo) · Pimlico (⚠) · Alchemy/QuickNode `eth_simulateV1` | |

**Excluded (no usable free tier; backlog as optional paid adapters):** Tenderly API, DeBank, Nansen, Arkham, Chainalysis KYT / Elliptic, Triton, Goldsky, Transak/MoonPay (partner contract), DefiLlama Pro, Helius paid methods.

⚠ = the research could not confirm it; check during implementation. Unconfirmed vendors ship disabled until they're checked.

## Cross-cutting rules (from specific pitfalls in the research)
- **Failover triggers:** only `Transient`, `RateLimited` or `QuotaExhausted`. A `NotFound` on a payment lookup is re-checked with a 2nd provider.
- **Envelope:** `{data, meta:{chain, provider, providersTried, block|slot, blockHash, finality, source, cached, latencyMs, asOf}}`.
- **Finality:** per-chain policy in `chains.toml`. EVM uses the `safe`/`finalized` tags, since L2 `latest` is only the sequencer's word. Solana commitment levels are configured to allow for Alpenglow (activating from 2026-09-28).
- **Payments:**
  - Use the **recipient's balance change**, not the instruction or event amount.
  - Match tokens by contract/mint through the registry, never by symbol. Reject spoofed `Transfer` logs.
  - Idempotency key is (chain, tx, logIndex).
  - PYUSD confidential transfers → `Unverifiable`.
- **Solana:** incoming SPL transfers are indexed on the token account. Query both token programs; derive ATAs with the program ID.
- **EVM logs:** cap `toBlock` at the same node's head, handle `removed:true`, drop 4-topic `Transfer` logs, and pin Multicall3 reads to one block.
- **Money:** integer base units only. Fiat is `rust_decimal` plus the valuation time (block time) and source. A missing price is `unknown`, never 0. Tokenized-equity prices are stale outside the market session, and `oraclePaused` means "no price".
- **Security:** reject key-like inputs; redact secrets everywhere; body limits; SSRF checks on any outbound URL the user supplies.
- **Compatibility:** the 4 legacy tool names stay as aliases with the same output for one release. `RPC_URL` maps to **Ethereum only**, with a deprecation warning (bug fix). `QN_*` variables still build QuickNode providers.

## Tool catalog (chain-agnostic; P0 = Release 1, P1 = R2, P2 = backlog)
Each tool uses **free sources only**, in the user's order.

| Domain | P0 (R1) | P1 (R2) | P2 |
|---|---|---|---|
| chain/infra | `chain_list`, `chain_finality`, `provider_health` | | |
| wallet/address | `wallet_get_balances`, `wallet_get_transfers`, `address_validate` | `address_resolve_name` (ENS/Basenames/SNS), `address_verify_ownership`, `wallet_get_approvals`, `wallet_get_portfolio` | |
| transactions | `tx_get`, `tx_status`, `tx_estimate_fee` (incl. L1 data fee; Solana priority + Jito tip), `tx_simulate` (`eth_simulateV1` → `debug_traceCall` → `eth_call`; Solana `simulateTransaction`), `tx_build_transfer`, `tx_broadcast` (fan-out, MEV-aware) | `tx_userop_status` | `safe_pending_txs` |
| payments | `payments_verify_transfer`, `payments_list_deposits`, `payments_build_request` (EIP-681 / Solana Pay / x402 requirements) | `payments_reconcile`, `payments_watch` (webhooks as hints + cursor polling), `x402_verify`, `x402_settle`/`x402_supported` (user's facilitator), `payments_build_authorization` | `payments_gas_sponsorship`, `payments_build_refund` |
| stablecoin/x-chain | `stablecoin_resolve`, `stablecoin_check_restrictions`, `stablecoin_peg` | `stablecoin_supply`, `bridge_cctp_status/quote` (v2), `bridge_oft_status/quote` | `bridge_quote`, `stablecoin_restriction_events`, `stablecoin_reserves` |
| compliance | `compliance_screen_address` (Chainalysis oracle + TRM + issuer freeze) | | |
| neobank | `neobank_card_funding_status`, `neobank_get_ledger`, `fiat_get_fx_rate` | `neobank_export_ledger`, `defi_earn_rates` | `neobank_onramp_quotes` (Coinbase only) |
| market/trading | `market_get_price` (median + spread), `market_get_price_at`, `token_get_metadata`, `token_check_risk` (merged verdict), `trade_get_swap_quote` (parallel, best + spread), `trade_build_swap_tx` | `market_get_pools`, `market_get_ohlcv`, `market_get_trending`/`new_pairs`, `token_get_holders`, `perps_get_markets` | `perps_build_order`, `trade_create_limit_order` |
| RWA (Robinhood first) | `rwa_token_info`, `rwa_price` (session-aware staleness) | `rwa_corporate_actions`, `rwa_transfer_check` | `rwa_find_by_underlying`, `rwa_premium` |
| defi | | `defi_get_positions` (Zerion → Moralis → on-chain), `defi_lending_health` (on-chain), `defi_vault_info` | `defi_staking_rates`, `defi_lp_positions` (The Graph), `defi_protocol_risk` |

R1 = 29 tools + 4 legacy aliases. **Tool profiles** keep each MCP client's tool list small; `all` exposes everything.

**Stablecoin registry fields:**
- `asset_id`, `issuer_entity`, `chain`, `address`
- `decimals` per deployment
- `issuance` (native / oft / bridged / exchange_peg)
- `freeze_check` method, `pause_check`, `fee_params`
- Solana `token_program`, `extensions`, `permanent_delegate`
- `cctp_domain`, `oft_adapter`
- price feed ids
- `mica_emt`
- **`source_url`, `verified_at`**: CI rejects rows without a source or with a non-checksummed address. Only addresses from issuer docs; nothing invented.

## Execution with Agent Teams → `plan/TASKS.md`
Each task has an ID, owner, deps, acceptance criteria and a checkbox.

**Phase 0 (lead, sequential):**
- T0.1 Create `plan/` (PLAN, TASKS, VENDORS with the API-key checklist, DEPLOYMENT, research/*), plus `.env.example` and `config/config.example.toml`. **The user can start creating Tier A keys right away.**
- T0.2 Delete unused files (Step 0). Build is green.
- T0.3 Characterization tests for the 4 legacy tools against a fake JSON-RPC server.
- T0.4 Workspace skeleton + CI checks.
- T0.5 `domain`.
- T0.6 `ports`.
- T0.7 `config`: schema, env/secrets, order resolution, validation, `registry/vendors.toml` defaults.
- T0.8 `routing`: strategies, resilience, quota guard, health, ArcSwap.
- T0.9 `app` Operation, Catalog and profiles.
- T0.10 `transport-mcp` (stdio + HTTP) + REST skeleton.
- T0.11 `evm_rpc` + `solana_rpc` base adapters + `chains.toml`.
- T0.12 `testkit`.
- T0.13 Port the legacy aliases and delete `src/core`. Characterization tests stay green → **contract freeze**: after this, `domain`, `ports` and `config` change only through the lead.

**Phase 1 (6 teammates in parallel, each owns its own modules, tests against testkit mocks):**

| Teammate | Owns |
|---|---|
| **evm** | EVM ports on `evm_rpc` (Multicall3 balances, adaptive `getLogs`, finality, fees incl. L1 data fee, simulate, build, broadcast + Flashbots/MEV Blocker); `alchemy`, `quicknode`, `moralis`, `ankr`; erc20/erc8056/multicall/fee-oracle readers |
| **solana** | Solana ports on `solana_rpc` (both token programs, ATA-aware balance-change parser, Token-2022 extensions, simulate, priority fee, build, send + resend); `helius` (DAS, priority fee, Sender), `jito`, `jupiter` |
| **payments-stablecoin** | `registry/stablecoins.toml` (verified), issuer-control readers, Chainalysis oracle, `trm`; `payments_*`, `stablecoin_*`, `compliance_screen_address` |
| **market-trading** | `coingecko`, `geckoterminal`, `defillama`, `dexscreener`, `birdeye`, `pyth`, Chainlink reader, `goplus`, `honeypot_is`, `rugcheck`, `oneinch`, `velora`, `cow` (+ optional 0x/uniswap/okx); `market_*`, `token_*`, `trade_*`, `rwa_*`; `registry/rwa.toml` |
| **neobank-wallet** | `frankfurter`, `openexchangerates`; `chain_*`, `wallet_*`, `tx_*`, `address_validate`, `neobank_card_funding_status`, `neobank_get_ledger`, `fiat_get_fx_rate` |
| **platform-dashboard** | hosted mode (client keys, per-client limits, separate admin bind, fail-closed start); env layering + "locked by env"; limit/cap/effective budget; `store` (usage counters per vendor and per client, call log); quota engine (header parsing in the shared HTTP client, local metering, `QuotaReporter` polling, aggregation, run-out projection, alerts); admin API (config read/validate/write/reload, vendor test, health, quota, SSE); dashboard UI incl. the Quota page; admin token/CSRF/redaction; SIGHUP reload |

Each adapter owner also implements `QuotaReporter` for their vendors where a confirmed usage endpoint exists, and fills in the per-method cost table in `registry/vendors.toml`.

**Phase 2 (lead):**
- T2.1 Wire everything together in `server`.
- T2.2 Full verification.
- T2.3 Rewrite README (config and dashboard guide, generated tool catalog, `VENDORS.md` signup links).
- T2.3b Deploy files (Dockerfile, compose + Caddy, systemd unit), then a hosted-mode smoke test on the user's VPS using their Tier A keys.
- T2.4 Release 1.

R2/R3 reuse the same team layout for the P1/P2 tools.

**After plan approval (before T0.1):** save a feedback memory. The user prefers modular, pattern-based architecture, domain research via parallel subagents, free-tier vendors, user-configurable routing, and plans kept in the repo under `plan/`.

## Verification
- **Build checks:** `cargo fmt --check`, `cargo clippy --workspace --all-features -D warnings`, `cargo test --workspace`. Each feature builds on its own.
- **Compatibility:** the T0.3 characterization tests pass before and after (same JSON for the 4 legacy tools).
- **Routing and config unit tests:**
  - order resolution precedence
  - inactive-vendor filtering
  - failover per error kind
  - quota guard (reserve reached → next vendor; 429 → exhausted until the window resets; counters survive restart)
  - breaker states
  - quorum disagreement
  - aggregate median/spread
  - adaptive `getLogs` split
  - hot reload mid-request (in-flight requests use the old table)
  - invalid config rejected without being applied
  - secret redaction
- **Adapter conformance:** every adapter passes its port suite against recorded fixtures (wiremock).
- **Pitfall golden tests:**
  - spoofed `Transfer` log
  - ERC-721 4-topic log
  - `removed:true` log
  - BSC 18-decimal stablecoin
  - Token-2022 transfer fee net amount
  - PYUSD confidential → unverifiable
  - transfer to a token account found when querying the wallet
  - several transfers in one tx
  - weekend stale equity feed
  - ERC-8056 scaling
  - under/overpaid
  - missing price → unknown
- **Parity:** MCP `list_tools` matches the active profile. The same input over MCP and REST returns identical JSON.
- **Quota tests:**
  - header parsing (X-RateLimit, IETF RateLimit, Retry-After)
  - local metering applies the method cost table
  - window reset (calendar vs rolling)
  - the guard uses the most pessimistic source
  - a `QuotaReporter` fixture per confirmed vendor
  - run-out projection math
- **Dashboard (driven with the Chrome tools):**
  - the Quota page shows a card for **every enabled vendor**, each with a source badge
  - after N calls, the local count goes up by N × cost
  - a mocked 429 flips the card to "exhausted until X" and routing skips that vendor
  - reorder Base `evm_rpc` to QuickNode first, save, and the next `wallet_get_balances` shows `meta.provider = quicknode`
  - disable a vendor and it drops out of the effective order
  - key input is never echoed back
  - calls without the admin token return 401
- **Hosted mode:**
  - startup fails with no client keys
  - requests without a key, or with a bad one, get 401
  - a client over its daily or credit limit gets `QUOTA_EXCEEDED` with a reset time, while other clients are unaffected
  - an operator cap below the vendor limit makes routing move to the next vendor at the cap
  - an env-set cap shows as "locked by env" in the dashboard, and an edit through the admin API is refused
  - the admin port is unreachable from the public interface
- **Zero-key mode:** the server boots with only public and keyless vendors. Tools that need keys return `UNSUPPORTED_CAPABILITY` with a hint to add a key.
- **Live smoke tests** (`cargo test -- --ignored`, free keys only):
  - verify a USDC transfer on Base and on Solana
  - USDG balance on Robinhood Chain
  - `rwa_price`
  - swap quote spread on Base
  - failover drill: bad Alchemy key → next vendor, and the dashboard shows the breaker open

## Open items (research could not confirm; check during implementation)
- USDC/EURC addresses and CCTP v2 domains.
- USDT0 freeze method.
- Decimals of Binance-Peg USDC on BSC.
- Native USDC on Robinhood Chain.
- Alpenglow timing.
- rmcp structured output.
- Free tiers: QuickNode credit amount, Ankr, 0x, Uniswap API, OKX, RugCheck, LI.FI, Pimlico, AaveKit/Kamino.
- Robinhood stock-token registry address.
