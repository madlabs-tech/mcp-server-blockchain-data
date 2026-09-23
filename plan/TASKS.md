# Tasks: Release 1

Status: `[ ]` todo · `[~]` in progress · `[x]` done · `[!]` blocked. Owners are Agent Teams teammate names (or `lead`).
Rules:
- **Ownership.** Only touch files your task owns. `domain`, `ports` and `config` are **frozen after T0.13**, and changes to them go through `lead` (request them via message).
- **Definition of done:**
  - acceptance criteria met
  - `cargo fmt --check` passes
  - `cargo clippy --workspace --all-features -- -D warnings` passes
  - `cargo test --workspace` passes
  - no invented contract addresses or URLs; every registry row has a `source_url`
- **Money.** Integer base units only. No `f64` for amounts.
- **Secrets.** Never log keys or key-bearing URLs (use the `Redacted` wrapper from T0.7).

---

## Phase 0: Foundation (owner: `lead`, sequential)

> **Status 2026-09-23: complete.**
> - `cargo fmt --check` and `clippy --workspace --all-targets --all-features -D warnings` pass. 88 tests pass across 12 crates.
> - The legacy characterization tests pass unchanged on the new stack.
> - Zero-key smoke test: live balances returned on Ethereum, Robinhood Chain and Base via public RPC.
> - **Contract freeze is in effect for `domain`, `ports` and `config`.**
> - Deviations from the plan:
>   - T0.11/T0.12 were built by teammate `foundation-adapters` in parallel.
>   - EVM chain-id checks are lazy (on first use, `ChainIdGuard` in `crates/server/src/wiring.rs`) rather than blocking at startup.
>   - Hosted mode fails closed until T1.D3.
>   - The `phase0-freeze` tag is pending the user's go-ahead to commit.

- [x] **T0.1 Plan folder + key checklist.**
  - Deps: none.
  - Deliverables:
    - `plan/{PLAN,TASKS,VENDORS,DEPLOYMENT}.md`
    - `plan/research/*.md` (8)
    - `.env.example`
    - `config/config.example.toml`
  - Accept:
    - VENDORS.md lists Tier A/B/keyless/unconfirmed/excluded vendors, with env var names that match `.env.example`
    - the user can create Tier A keys from it
- [x] **T0.2 Delete unused files.**
  - Deps: none.
  - Delete:
    - `src/core/services/{balance,blocks,contracts,ens,tokens,transactions,transfer,utils}.rs`
    - `clear_client_cache`, `get_supported_chains` and their re-export
    - `IMPLEMENTATION.md`, `README_RUST.md`, `test_server.sh`
  - Accept:
    - `cargo build` is green
    - `grep` finds no references
    - `.gitignore` covers `config/secrets.toml`, `*.db`, `.env`
- [x] **T0.3 Characterization tests (legacy behavior lock).**
  - Deps: T0.2.
  - What: a fake JSON-RPC server (axum, in-process) that answers `eth_getBalance`, `eth_getCode`, `eth_gasPrice`, `eth_getTransactionByHash`, `eth_chainId`.
  - Tests call the 4 legacy tools and snapshot the JSON shape: key names, types, formatting (`gasPriceGwei` 2dp).
  - Accept: tests pass on the current code. These exact tests must still pass after T0.13.
- [x] **T0.4 Workspace skeleton + CI.**
  - Deps: T0.3.
  - What:
    - Cargo `[workspace]` with the `crates/*` from PLAN.md
    - shared `[workspace.dependencies]`
    - `rust-toolchain.toml` (1.90)
    - `.github/workflows/ci.yml` (fmt, clippy `-D warnings`, test, feature-powerset for adapters)
  - Accept: empty crates build, and the legacy binary still builds from `crates/server`.
- [x] **T0.5 `domain` crate.**
  - Deps: T0.4.
  - Types:
    - `ChainId` / `AccountId` / `AssetId`: CAIP-2/10/19, with parse/display + friendly aliases (`base` → `eip155:8453`, `solana` → `solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp`)
    - `ChainFamily`
    - `Amount{raw: U256, decimals}` with exact `format_units`/`parse_units`
    - `Fiat` (rust_decimal + `as_of` + `source`)
    - `Finality`, `Transfer`, `Tx`, `Fee`, `Price`, `Quote`, `RiskReport`, `Provenance`
    - `DomainError` with stable string codes
  - Accept: round-trip and property tests (parse ∘ format = id, 6/8/18 decimals); no I/O dependencies.
- [x] **T0.6 `ports` crate.**
  - Deps: T0.5.
  - What:
    - capability traits (async-trait, `Send + Sync`) per PLAN.md
    - `ProviderError{Transient,RateLimited{retry_after},QuotaExhausted{resets_at},Unsupported,NotFound,Invalid,Fatal}`
    - `Capability` enum, whose ids equal the config keys (`evm_rpc`, `solana_rpc`, `price`, `token_risk`, `swap_quote`, `fx`, …)
    - `QuotaReporter` trait
    - `VendorMeta{id, requires_key, chains, capabilities}`
  - Accept: compiles; every PLAN.md P0 tool maps to ≥1 capability (the table is in rustdoc).
- [x] **T0.7 `config` crate.**
  - Deps: T0.6.
  - What:
    - figment layering: `registry/vendors.toml` defaults < `config.toml` < `secrets.toml` < env (`EMS__…`, plus `<VENDOR>_API_KEY` aliases, plus legacy `RPC_URL` → Ethereum only with a warning, and `QN_*`)
    - schema for `server{mode,http_bind,admin_bind,public_bind,tool_profile,dashboard}`, `vendors.<id>{enabled,limit,cap,reserve_pct,on_exhausted}`, `routing.defaults`/`routing.chains`, `operations.<op>{strategy,quorum,fan_out,order,cache_ttl_secs}`, `clients`
    - order resolution (op > chain > default > built-in)
    - validation that returns all errors and warnings, never a half-applied config
    - a provenance map (which layer set each key) → "locked by env"
    - `Redacted<T>` for secrets
    - atomic writer (temp + rename + `.bak`)
  - Accept: unit tests for precedence, env parsing (lists, numbers), locked-by-env, validation errors, redaction in `Debug`/`Display`/serde.
- [x] **T0.8 `routing` crate.**
  - Deps: T0.7.
  - What:
    - `ProviderRegistry` (chain × capability → ordered `Arc<dyn Port>` + VendorMeta)
    - strategies: `PriorityFailover`, `Hedged(δ)`, `Quorum(n)`, `Aggregate(n)` (median + spread), `FanOut`
    - resilience: timeout, retry + jitter (only `Transient`), circuit breaker (closed/open/half-open)
    - quota guard: governor token bucket per vendor, window counters (calendar/rolling), effective budget `min(cap, limit×(1−reserve))`, exhausted-until on 429/quota errors; the counter store sits behind a trait (in-memory now, sqlite in T1.D1)
    - health snapshot
    - `ArcSwap<RoutingTable>` hot swap
    - `providersTried` trail into `Provenance`
  - Accept: unit tests for every rule in PLAN.md "Routing and config unit tests" that concerns routing or the quota guard.
- [x] **T0.9 `app` crate: Operation + Catalog.**
  - Deps: T0.8.
  - What:
    - `Operation` trait (name, description, domain, Input/Output with `JsonSchema`, `read_only`, default strategy, cache TTL, `execute(ctx, input)`)
    - `DynOperation` blanket impl (JSON in/out)
    - `Catalog`
    - tool profiles (`payments`, `trading`, `neobank`, `defi`, `all`, `custom`)
    - response envelope `{data, meta}`
    - moka cache decorator
    - metrics/tracing decorator
    - `Ctx` (routing table handle, client identity, request id)
  - Accept: a test Operation runs through the cache and envelope, and profiles filter the Catalog.
- [x] **T0.10 Transports.**
  - Deps: T0.9.
  - What:
    - `transport-mcp`: rmcp `ServerHandler` with `list_tools`/`call_tool` from the Catalog (+ tool annotations), over stdio and streamable HTTP (`/mcp`)
    - `transport-http`: axum REST `POST /v1/<domain>/<op>`, OpenAPI JSON from the Catalog, body limits, tracing, `/healthz`, `/metrics`
    - admin router mount point (empty; T1.D2 fills it)
  - Accept: a parity test (same input over MCP vs REST → identical JSON); MCP Inspector lists the tools.
- [x] **T0.11 Base chain adapters + chain registry.**
  - Deps: T0.6.
  - What:
    - `registry/chains.toml` (8 EVM + Solana: CAIP-2, chain id, native asset, finality policy, block time, multicall3, public RPC, explorers)
    - `adapters/evm_rpc` (alloy; any URL; asserts `eth_chainId` at startup; implements the chain ports at a basic level)
    - `adapters/solana_rpc` (reqwest JSON-RPC; basic `getBalance`/`getSlot`/`getTransaction`)
    - `adapters/public` (public RPC URLs from the registry)
    - base shared HTTP client (redaction, timeouts, and a hook for the rate-limit header parser)
  - Accept: conformance suites pass against the testkit fakes.
- [x] **T0.12 `testkit` crate.**
  - Deps: T0.6.
  - What:
    - fake EVM JSON-RPC and Solana RPC servers (scriptable responses, error injection: 429/5xx/timeouts)
    - wiremock helpers for vendor REST fixtures
    - a `port_conformance!` macro per port
    - mock ports
    - fixture recording guidelines (`fixtures/<vendor>/<case>.json`, secrets stripped)
  - Accept: used by T0.8, T0.10 and T0.11 tests.
- [x] **T0.13 Port the legacy tools + contract freeze.**
  - Deps: T0.3, T0.9–T0.12.
  - What:
    - `eth_get_balance`, `eth_get_code`, `eth_gas_price`, `eth_get_transaction_by_hash` as alias Operations on the new stack
    - `crates/server` composition root (config → factories → registry → catalog → transports)
    - delete `src/`
  - Accept:
    - **T0.3 tests pass unchanged**
    - `RPC_URL` affects only Ethereum (new test)
    - binary name is still `evm-mcp-server`
    - tag the commit `phase0-freeze`

---

## Phase 1: Release 1 in parallel (6 teammates)

Each teammate works in its own modules, tests against `testkit`, and implements `QuotaReporter` plus cost-table rows in `registry/vendors.toml` for its vendors.

### evm
- [~] **T1.E1** EVM chain ports on `evm_rpc`:
  - `NativeBalance`/`TokenBalances` via Multicall3 `aggregate3`, pinned to one block
  - `ChainHead` + finality tags (`safe`/`finalized`)
  - `TxLookup` (receipt + decoded ERC-20 `Transfer`s; drop 4-topic logs; handle `removed`)
  - `LogScan`: adaptive chunking from the vendor plan's `getLogs` limit, split on error, `toBlock` capped at the same node's head

  Accept: golden tests (spoofed token log, 4-topic log, removed log, BSC 18-decimal token).
- [~] **T1.E2** `FeeOracle` EVM:
  - `eth_feeHistory` tiers
  - OP-stack L1 fee (`GasPriceOracle.getL1Fee` + operator fee)
  - Arbitrum `NodeInterface.gasEstimateL1Component`
  - USD conversion via the price port

  Accept: fixtures for Base, Arbitrum, Ethereum.
- [~] **T1.E3** `Simulator` (`eth_simulateV1` → `debug_traceCall` → `eth_call`), `TxBuilder` (EIP-1559 native/ERC-20), `Broadcaster` (fan-out; tx hash computed locally, so resends are safe), `PrivateRelay` (Flashbots Protect, MEV Blocker; Ethereum only; other chains flagged `no_private_mempool`).
- [~] **T1.E4** Vendor adapters `alchemy` (RPC, Portfolio/Token balances, `alchemy_getAssetTransfers`, Prices, `eth_simulateV1`), `quicknode` (RPC via `QN_*`), `moralis` (balances, transfers), `ankr` (disabled by default, flagged ⚠).

  Accept: conformance + fixtures; Alchemy Robinhood Chain URL mapping.
- [~] **T1.E5** `protocols` readers: `erc20`, `multicall3`, `erc8056` (UI multiplier), OP/Arbitrum fee oracles, Chainlink aggregator (`latestRoundData`, `getRoundData`).

### solana
- [~] **T1.S1** Solana chain ports on `solana_rpc`:
  - balances (`getBalance` + `getTokenAccountsByOwner` for BOTH token programs; sum non-ATA accounts)
  - ATA derivation with the program id
  - transfer history over the wallet and every token account (`getSignaturesForAddress` pagination, skip failed txs)
  - parser from `pre/postTokenBalances` changes (inner instructions, multiple transfers, Token-2022 transfer-fee net, confidential → `Unverifiable`)
  - scaled-UI support

  Accept: golden tests for each case.
- [~] **T1.S2** `FeeOracle` (priority-fee percentiles over `getRecentPrioritizationFees` + Jito tip), `Simulator` (`simulateTransaction` with `replaceRecentBlockhash`, `innerInstructions`), `TxBuilder` (SOL/SPL/Token-2022 transfer: ATA create, memo, compute price, transfer-hook extra accounts), `Broadcaster` (send to all + resend until `lastValidBlockHeight`; signature computed locally), commitment/finality per `chains.toml` (Alpenglow-ready).
- [~] **T1.S3** `helius` adapter (RPC, DAS `getAssetsByOwner`, `getPriorityFeeEstimate`, Sender with tip checks, credits `QuotaReporter` if confirmed). Paid methods are **not** used by default.
- [~] **T1.S4** `jito` (bundle/tip endpoints) and `jupiter` (Price v3, Swap v2 quote/build) adapters.

### payments-stablecoin
- [~] **T1.P1** `registry/stablecoins.toml`, schema per PLAN.md: USDC, EURC, USDT, USDT0, PYUSD, USDG, RLUSD, DAI/USDS on our chains, **only from issuer docs with `source_url` + `verified_at`**. A CI check (test) rejects missing sources and non-checksummed addresses. Resolve the open items (USDC list, USDT0 freeze method, BSC decimals, Robinhood USDC).
- [~] **T1.P2** `protocols` issuer controls (`isBlacklisted`, `isBlackListed`/`getBlackListStatus`, `isFrozen`, `paused`, `deprecated`/`upgradedAddress`, Solana account `state` + permanent delegate), the Chainalysis sanctions oracle (per-chain address from docs), and the `trm` adapter.
- [~] **T1.P3** Operations `payments_verify_transfer` (recipient balance change, registry token match, min finality, quorum option, `underpaid|overpaid|wrong_token|unverifiable`, idempotency key), `payments_list_deposits` (cursor, canonical tokens only), `payments_build_request` (EIP-681, Solana Pay with a fresh reference, x402 `PaymentRequirements`).
- [~] **T1.P4** Operations `stablecoin_resolve`, `stablecoin_check_restrictions` (report the block used), `stablecoin_peg` (oracle vs DEX vs CoinGecko), `compliance_screen_address` (combined verdict that lists each source).

### market-trading
- [~] **T1.M1** Price adapters `coingecko` (Demo header; `/key` `QuotaReporter` if confirmed), `geckoterminal`, `defillama`, `dexscreener`, `birdeye`, `pyth` (Hermes; Benchmarks with a key), and the Chainlink reader. Operations `market_get_price` (Aggregate: median + spread + `as_of` + liquidity; missing → `unknown`) and `market_get_price_at`.
- [~] **T1.M2** `token_get_metadata` (on-chain decimals first) and `token_check_risk` (`goplus`, `honeypot_is`, `rugcheck` ⚠ + on-chain authorities: mint/freeze authority, permanent delegate, Token-2022 extensions, proxy admin → merged verdict listing each source).
- [~] **T1.M3** Swap adapters `oneinch`, `velora`, `cow`, plus `jupiter` (from T1.S4); optional `zeroex`, `uniswap_api`, `okx_dex`, disabled until the free tier is confirmed. Operations `trade_get_swap_quote` (parallel, best + spread, TTL, minOut) and `trade_build_swap_tx` (re-quote, then unsigned tx + required approvals).
- [~] **T1.M4** `registry/rwa.toml` (Robinhood official list source, xStocks/Ondo/Dinari placeholders marked unverified), `rwa_token_info` (multiplier current/pending, `oraclePaused`, official-list check), `rwa_price` (Chainlink equity feed, market-session calendar incl. US holidays, staleness by session, sequencer uptime).

### neobank-wallet
- [x] **T1.N1** Operations `chain_list`, `chain_finality`, `provider_health` (read-only), `wallet_get_balances` (cross-chain, stablecoin filter, share-equivalent for ERC-8056/scaled-UI), `wallet_get_transfers`.
- [x] **T1.N2** Operations `tx_get`, `tx_status`, `tx_estimate_fee`, `tx_simulate`, `tx_build_transfer`, `tx_broadcast`, all over the chain ports (EVM and Solana behind the same Operation).
- [x] **T1.N3** `address_validate` (checksum/base58, EOA/contract/smart account, Solana owner vs ATA, token exists on chain, native vs bridged via the registry).
- [x] **T1.N4** Adapters `frankfurter` and `openexchangerates`. Operations `fiat_get_fx_rate` (business date labeled), `neobank_card_funding_status` (min(balance, allowance / delegatedAmount) for the issuer spender + decline reason), `neobank_get_ledger` (credit/debit rows, block-time fiat valuation + source, par vs market).

### platform-dashboard
- [~] **T1.D1** `store` (rusqlite bundled, WAL): usage counters (vendor × window × method × chain × tool × client), call log ring, client keys (SHA-256), migrations. Implements the routing counter-store trait so counters survive restarts.
- [~] **T1.D2** Quota engine:
  - rate-limit header parser in the shared HTTP client (`X-RateLimit-*`, IETF `RateLimit-*`, `Retry-After`)
  - local metering with the cost table
  - `QuotaReporter` polling scheduler (default 5 min)
  - aggregation (the most pessimistic source wins for the guard)
  - burn rate + run-out projection
  - alert thresholds
- [~] **T1.D3** Hosted mode:
  - `mode = hosted` → client bearer auth on `/mcp` and `/v1/*`
  - per-client limits (rpm, daily, monthly credits, allowed profile/tools) → `QUOTA_EXCEEDED` with a reset time
  - separate `admin_bind`
  - **fail closed** when there are no client keys
- [~] **T1.D4** Admin API `/admin/api/*`:
  - config read (with provenance / locked-by-env), validate, write (atomic), reload (ArcSwap) + SIGHUP
  - vendor test
  - health, quota (incl. CSV export)
  - clients CRUD
  - SSE call stream
  - security: admin bearer token generated on first run (0600), required custom header (CSRF), redaction, body limits
- [~] **T1.D5** Dashboard UI (static HTML + vanilla JS, `include_str!`), pages:
  - Overview
  - **Quota** (a card per vendor: limit / cap / effective budget, used/remaining per window, source badge, burn rate, run-out date, breakdown, 30-day chart, refresh, CSV)
  - Vendors (write-only keys, test)
  - Routing (drag-and-drop order, per-chain tab, effective order + warnings)
  - Tools (profiles, per-op strategy/TTL)
  - Chains
  - Clients

  Accept: the dashboard checks in PLAN.md Verification, driven via Chrome.

---

## Phase 2 follow-ups (collected from Phase 1 reports)
- [ ] **F1** (from neobank-wallet)
  - `wallet_get_balances` share-equivalent amounts for ERC-8056 and scaled-UI tokens. Needs `evm::erc8056` (evm).
  - `tx_build_transfer` on Solana: add compute-budget / priority-fee instructions.
- [ ] **F2** (contract requests from neobank-wallet):
  - `ChainEntry.no_private_mempool`. Currently inferred from "no private relay registered".
  - `StablecoinEntry.issuance` / `peg_currency`. Peg currently guessed from the symbol. Check against the payments merge.
  - Memo / system / ATA instruction builders in `protocols::solana::spl`.
  - Method-dispatching RPC mocks in testkit.
- [ ] **F3** Wire `App::with_observer` (CallObserver, f577fb2) when merging platform-dashboard.

## Phase 2: Integration (owner: `lead`)
- [ ] **T2.1** Wire all factories in `crates/server`. Default build = free-tier features; zero-key boot works.
- [ ] **T2.2** Full PLAN.md Verification: unit, conformance, golden, parity, quota, hosted, zero-key, and live smoke with Tier A keys (`cargo test -- --ignored`).
- [ ] **T2.3** README rewrite (quick start, config, dashboard, tool catalog generated from the Catalog, links to VENDORS/DEPLOYMENT).
- [ ] **T2.3b** `Dockerfile` (multi-stage), `deploy/docker-compose.yml` (+ Caddy TLS), `deploy/evm-mcp-server.service`, then a hosted-mode smoke test on the operator VPS.
- [ ] **T2.4** Release 1: tag `v0.2.0`, changelog, migration notes (legacy aliases, `RPC_URL` → Ethereum-only).
