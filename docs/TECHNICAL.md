# Technical reference

For developers and operators. The [README](../README.md) covers everyday use and
[DEPLOYMENT.md](../DEPLOYMENT.md) covers running it on a server.

- [Architecture](#architecture)
- [Configuration](#configuration)
- [Routing](#routing)
- [Tool profiles](#tool-profiles)
- [REST API](#rest-api)
- [MCP](#mcp)
- [Admin API](#admin-api)
- [Hosted mode](#hosted-mode)
- [CLI](#cli)
- [Metrics](#metrics)
- [Building and testing](#building-and-testing)
- [Releasing](#releasing)
- [Legacy compatibility](#legacy-compatibility)

## Architecture

A Rust workspace (Rust 1.90, edition 2021) with a hexagonal layout: pure domain types in the
middle, ports (traits) around them, vendor adapters and transports at the edge. One binary,
`onchain-data-mcp`, built from the `onchain-data-mcp` package in `crates/server`.

| Crate | Package | Role |
|---|---|---|
| `crates/domain` | `bdm-domain` | Pure types: CAIP identifiers, amounts, transfers, finality, provenance, error codes. No I/O. |
| `crates/ports` | `bdm-ports` | Capability traits implemented by vendor adapters, plus provider errors. |
| `crates/config` | `bdm-config` | Layered configuration (registry, `config.toml`, `secrets.toml`, env) and routing-order resolution. |
| `crates/routing` | `bdm-routing` | Provider registry, routing strategies, retries, circuit breakers, quota guard, health. |
| `crates/protocols` | `bdm-protocols` | On-chain readers: ERC-20, Multicall3, getLogs scans, issuer controls, Chainlink, ERC-8056, Solana/SPL wire formats. |
| `crates/adapters` | `bdm-adapters` | One module per vendor, **one cargo feature per vendor** (default = verified free tiers). |
| `crates/app` | `bdm-app` | Tools as `Operation`s (`crates/app/src/ops/*.rs`), the catalog and profiles. |
| `crates/store` | `bdm-store` | SQLite (`bdm.db`): usage counters, call log, client keys; quota engine; hosted client auth and limits. |
| `crates/transport-mcp` | `bdm-transport-mcp` | MCP over stdio and streamable HTTP, generated from the catalog. |
| `crates/transport-http` | `bdm-transport-http` | REST, OpenAPI, admin API and the embedded dashboard (`src/dashboard/`). |
| `crates/server` | `onchain-data-mcp` | The binary: CLI, wiring, startup. |
| `crates/testkit` | `bdm-testkit` | Test fakes: JSON-RPC servers, vendor fixtures, port conformance suites. |

Built-in data lives in `registry/` and is compiled in: `chains.toml` (chains and public RPCs),
`vendors.toml` (vendors, free-tier limits, tiers, per-method costs, default routing orders),
plus the stablecoin and tokenized-stock registries.

Production code denies `unwrap`, `expect`, indexing, `panic!` and `unreachable!` (workspace
clippy lints): vendor data never crashes the process.

**Adding a vendor:** a module in `crates/adapters/src/vendors/`, a feature in
`crates/adapters/Cargo.toml`, an entry in `registry/vendors.toml` (with `tier`), and a row in
`docs/VENDORS.md`.

**Adding a tool:** an `Operation` in `crates/app/src/ops/<domain>.rs` with `NAME`, `DOMAIN`,
`DESCRIPTION` and `PROFILES`, registered in that module's `register`. Set `READ_ONLY = false`
only for tools that change state (today only `tx_broadcast`).

## Configuration

Layered, later wins:

```text
built-in registry (registry/*.toml, compiled in)
  < <config-dir>/config.toml     (the dashboard writes this; start from config/config.example.toml)
  < <config-dir>/secrets.toml    (API keys, mode 0600, written by the dashboard)
  < environment                  (ODM__<PATH>, "__" between segments, plus vendor key names)
```

- `--config-dir` defaults to `./config`, relative to the working directory. Claude Desktop starts
  processes with cwd `/`, so pass absolute paths.
- `server.data_dir` defaults to `./data` (also relative to the working directory) and holds
  `bdm.db`. If it isn't writable, self-hosted mode keeps counters in memory and warns.
- The deprecated `BDM__` prefix is still read (with a startup warning); `ODM__` wins when both are set.
- The binary does **not** read `.env` files. Docker Compose (`env_file`) and systemd
  (`EnvironmentFile`) load `.env` for you; `.env.example` lists every variable.
- Anything set by env shows as **locked by env** in the dashboard and can't be edited there.
- `POST /admin/api/reload` or `kill -HUP <pid>` re-reads the files. An invalid config is
  refused and the old one kept.

**Env mapping examples:**

| Env var | Sets |
|---|---|
| `ODM__SERVER__MODE=hosted` | `[server] mode` |
| `ODM__SERVER__HTTP_BIND=127.0.0.1:8787` | `[server] http_bind` |
| `ODM__VENDORS__ALCHEMY__CAP__MONTHLY_CREDITS=15000000` | `[vendors.alchemy.cap] monthly_credits` |
| `ODM__ROUTING__DEFAULTS__EVM_RPC=alchemy,quicknode,public` | `[routing.defaults] evm_rpc` |
| `ALCHEMY_API_KEY`, `HELIUS_API_KEY`, … | vendor keys (names in [VENDORS.md](VENDORS.md)) |
| `DASHBOARD_PASSWORD` | dashboard password (≥ 12 printable ASCII characters, no spaces; blank = unset) |

**`[server]`:**

| Key | Default | Meaning |
|---|---|---|
| `mode` | `self_hosted` | `self_hosted` or `hosted` |
| `http_bind` | `127.0.0.1:8787` | self-hosted: REST, `/mcp` and dashboard |
| `public_bind` | none (required in hosted) | hosted: public REST + `/mcp` |
| `admin_bind` | `127.0.0.1:8788` | hosted: dashboard + admin API |
| `dashboard` | `true` | serve the dashboard and admin API |
| `tool_profile` | `all` | `payments`, `trading`, `neobank`, `defi`, `all` or `custom` |
| `enabled_tools` / `disabled_tools` | `[]` | tool list for `custom`; tools always hidden |
| `data_dir` | `./data` | where `bdm.db` lives |
| `cache_max_entries`, `quota_poll_secs` | | cache size; how often vendor usage APIs are polled |
| `warmup` | `true` | one background `eth_chainId` per active EVM RPC at startup; mismatches are logged and disabled |

**`[vendors.<id>]`:**

| Key | Meaning |
|---|---|
| `enabled` | on/off (Tier 4 vendors ship off) |
| `limit.{rps,per_minute,daily,monthly}` | the vendor's real quota; defaults to its free tier from `registry/vendors.toml` |
| `cap.{…}` | your own budget below the limit (`monthly_credits` / `daily_requests` are accepted aliases) |
| `reserve_pct` | stop routing to the vendor at `(100 − reserve_pct)%` of the budget |
| `on_exhausted` | `skip` (next vendor) or `allow_overage` |
| `costs.<method>` | override the per-call cost estimate |
| `alert_pct` | thresholds logged once per window (default 75/90) |

Effective budget per window = `min(cap, limit × (1 − reserve_pct/100))`. Usage is metered
locally; the dashboard labels each number `vendor API`, `headers` or `estimated`.

**Custom RPCs and chains:** `[custom_rpc.<name>] chain = "eip155:1", url = "…"` adds an endpoint
as a vendor named `<name>`; `[chain_overrides."eip155:56"] enabled = false`; `[[extra_chains]]`
uses the schema of `registry/chains.toml`. Every EVM endpoint is checked against `eth_chainId`.

The full, commented reference is [`config/config.example.toml`](../config/config.example.toml).

## Routing

Each capability has an ordered vendor list: primary first, then fallbacks. Most specific wins:

```text
[operations.<tool>.order]  <capability> = [...]      # one tool
  > [routing.chains."<caip2>"]  <capability> = [...]  # one chain
  > [routing.defaults]  <capability> = [...]          # everywhere
  > built-in order in registry/vendors.toml ([default_order], then [default_order_chain."<caip2>"])
```

**Built-in orders** (`registry/vendors.toml`):

| Capability | Default | Solana |
|---|---|---|
| `evm_rpc` | alchemy, quicknode, public | |
| `solana_rpc` | helius, quicknode, public | |
| `token_balances` | alchemy, moralis, rpc | helius, rpc |
| `transfer_history` | alchemy, moralis, rpc | rpc |
| `fee_estimate` | rpc | helius, quicknode, rpc |
| `simulate` | rpc | |
| `broadcast` | alchemy, quicknode, public | helius, quicknode, public |
| `private_relay` | flashbots, mev_blocker | jito, helius_sender |
| `price` | coingecko, defillama, geckoterminal, dexscreener | jupiter, birdeye, defillama, coingecko |
| `price_history` | coingecko, defillama, pyth | |
| `token_metadata` | rpc, coingecko | rpc, helius, jupiter |
| `token_risk` | goplus, honeypot_is, rugcheck | rugcheck, goplus |
| `swap_quote` | oneinch, velora, cow | jupiter |
| `sanctions` | chainalysis_oracle, trm | trm |
| `fx` | frankfurter, openexchangerates | |

Disabled vendors and vendors without a key are skipped. Two pseudo-vendors always exist:
`public` (keyless RPCs from `chains.toml`) and `rpc` (generic on-chain implementations over
whatever RPC is routed).

**Strategies** (`[operations.<tool>] strategy = …`): `failover` (default: first that works),
`quorum` (with `quorum = N`, N sources must agree), `aggregate` (median + spread across
`fan_out` vendors), `fan_out` (ask all at once, e.g. broadcast), `hedged` (start the next vendor
after `hedge_delay_ms`). Also per tool: `cache_ttl_secs`, `enabled`.
`config.example.toml` sets `payments_verify_transfer` to quorum 2, `market_get_price` to
aggregate over 3, and `tx_broadcast` to fan-out.

## Tool profiles

`server.tool_profile` picks which tools a client sees: `payments`, `trading`, `neobank`,
`defi`, `all` (self-hosted default) or `custom` (then list tools in `enabled_tools`).
`disabled_tools` always applies. In hosted mode each client key carries its own profile
(default `payments`, from `[clients.default]`). The per-tool profiles are listed in the
[README](../README.md#available-tools); `GET /v1/tools` shows the effective list.

## REST API

REST mirrors the tools exactly: same inputs, same JSON output as MCP.

| Route | Purpose |
|---|---|
| `GET /v1/tools` | tools visible to the caller, with descriptions and input schemas |
| `POST /v1/tools/{name}` | call a tool by name, JSON body = tool input |
| `POST /v1/{domain}/{tool}` | same, by domain: `wallet_get_balances` → `/v1/wallet/get_balances`; tools without the domain prefix keep their full name (`/v1/wallet/address_validate`) |
| `GET /openapi.json` | OpenAPI document |
| `/mcp` | MCP over streamable HTTP |
| `GET /healthz` | returns `ok` |
| `GET /metrics` | Prometheus text, see [Metrics](#metrics) |

```bash
curl -s -X POST http://127.0.0.1:8787/v1/chain/list -H 'content-type: application/json' -d '{}'
```

Errors come back as `{"error":{"code":"…","message":"…","hint":"…"}}`. Codes include
`INVALID_INPUT`, `UNSUPPORTED_CHAIN`, `UNSUPPORTED_CAPABILITY` (usually a missing key),
`NOT_FOUND`, `ALL_PROVIDERS_FAILED`, `RATE_LIMITED`, `QUOTA_EXCEEDED`, `UNAUTHORIZED`.

## MCP

- **stdio:** run the binary with no subcommand. Logs go to stderr; stdout is the MCP channel.
  In self-hosted mode it also serves HTTP on `http_bind` if the port is free (otherwise it logs
  `HTTP not started (…); stdio only` and carries on).
- **Streamable HTTP:** `/mcp` on `http_bind` (self-hosted) or `public_bind` (hosted, client key required).

## Admin API

Served with the dashboard: on `http_bind` in self-hosted mode, on `admin_bind` only in hosted
mode. Every `/admin/api/*` call needs **both** headers:

```text
Authorization: Bearer <dashboard password>
X-BDM-Admin: 1
```

The second header is CSRF protection (browsers can't send it cross-site). Missing password →
401 `missing or invalid dashboard password`; missing header → 403. No response ever contains a
vendor key or client key (except the one-time key on client creation).

| Endpoint | Purpose |
|---|---|
| `GET /admin/api/health` | vendor breaker/latency/usage, per-tool call stats |
| `GET /admin/api/connect` | facts for client snippets (mode, URLs, binary path, config dir) |
| `GET /admin/api/config` | settings (secrets scrubbed), locked-by-env map, key status, effective orders, tools, chains |
| `PUT /admin/api/config` `{"edits":[{"path":[…],"value":…}]}` | validate, write atomically (`.bak` kept), hot-swap; `null` removes; env-locked paths → 422 |
| `POST /admin/api/config/validate` | same checks, nothing written |
| `POST /admin/api/reload` | re-read the files (same as `SIGHUP`) |
| `POST /admin/api/vendors/{id}/test` | one cheap call against the vendor |
| `POST /admin/api/vendors/{id}/budget` `{"which":"cap","window":"monthly","value":15000000}` | set or clear (`null`) one limit/cap window |
| `GET /admin/api/quota`, `GET /admin/api/quota.csv`, `POST /admin/api/quota/refresh` | quota report, CSV, poll vendor usage APIs now |
| `GET /admin/api/clients`, `POST /admin/api/clients` `{"name":"bob"}` | list; create (key returned once) |
| `PATCH /admin/api/clients/{id}`, `DELETE /admin/api/clients/{id}` | set limits/profile; revoke |
| `GET /admin/api/calls?limit=N`, `GET /admin/api/calls/stream` | recent calls; live SSE stream |

```bash
PW=$(onchain-data-mcp password --config-dir ~/.onchain-data-mcp | sed -n 's/^Password: *//p')
curl -s -H "Authorization: Bearer $PW" -H 'X-BDM-Admin: 1' http://127.0.0.1:8787/admin/api/quota
```

The dashboard password is `DASHBOARD_PASSWORD` if set, else `<config-dir>/dashboard_password`
(generated on first start, mode 0600). A pre-0.2.0 `admin_token` file is renamed to it once.
An invalid `DASHBOARD_PASSWORD` turns the dashboard off (logged as `dashboard turned off`); the
rest of the server keeps running.

## Hosted mode

`mode = "hosted"`:

- HTTP only (never stdio). Public REST + `/mcp` on `public_bind`; dashboard + admin API on
  `admin_bind` (default `127.0.0.1:8788`), never on the public port.
- Every public request needs `Authorization: Bearer <client key>`, else 401
  `missing or invalid client key`. Keys (`odm_…`) are stored as SHA-256 hashes and shown once.
- **Fails closed:** refuses to start without at least one active client key, and config
  validation requires `public_bind`.
- Client keys are loaded at startup; keys created with the CLI while the server runs need a
  restart (keys created in the dashboard work immediately).
- Per-client limits from `[clients.default]`, overridable per key: `requests_per_minute`
  (token bucket), `daily_requests` (resets 00:00 UTC), `monthly_credits` (vendor credits spent
  for the client, resets on the 1st), `tool_profile`. Over a limit → HTTP 429 `QUOTA_EXCEEDED`
  with `Retry-After`.
- Vendor limits and caps apply on top. Set a `cap` below every `limit` a public instance uses.

## CLI

```text
onchain-data-mcp [--config-dir DIR]                 self-hosted: MCP on stdio, plus HTTP on http_bind if free
onchain-data-mcp serve [--config-dir DIR]           HTTP only (hosted: public_bind + admin_bind)
onchain-data-mcp password [--config-dir DIR]        print dashboard URL, password, one-click login link, config folder, source
onchain-data-mcp password reset [--config-dir DIR]  new random password (restart needed; refused if DASHBOARD_PASSWORD is set)
onchain-data-mcp clients create <name>              create a client key (printed once)
onchain-data-mcp clients list                       ids, status, created date, names (never keys)
onchain-data-mcp --version | --help
```

`--config-dir` can go before or after the subcommand. `clients` commands use `server.data_dir`,
so pass the same `ODM__SERVER__DATA_DIR` as the server.

## Metrics

`GET /metrics` (Prometheus text), per vendor:

- `bdm_vendor_ok_total{vendor}`: successful calls (counter)
- `bdm_vendor_failed_total{vendor}`: failed calls (counter)
- `bdm_vendor_month_used{vendor}`: units used this month (gauge)

The Caddy config in `deploy/` doesn't expose `/metrics` publicly.

## Building and testing

```bash
cargo build --release -p onchain-data-mcp          # target/release/onchain-data-mcp
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo hack check -p bdm-adapters --each-feature --no-dev-deps   # every vendor builds alone
```

CI (`.github/workflows/ci.yml`) runs these on Linux and the tests on Windows. `.gitattributes`
forces LF line endings so Windows checkouts match.

For a quick local binary, use `cargo build --profile fast -p onchain-data-mcp` (output in
`target/fast/`): it skips link-time optimisation, so it builds much faster but runs a little
slower. If a build uses too much memory, limit parallel jobs with `CARGO_BUILD_JOBS=2` (the
Dockerfile defaults to 2; override with `--build-arg CARGO_BUILD_JOBS=4`). Release binaries are
built by `dist` with `[profile.dist]`, not by these commands.

## Releasing

Releases use [cargo-dist](https://opensource.axo.dev/cargo-dist/) (`dist-workspace.toml`,
dist 0.33.0). After editing `dist-workspace.toml`, run `dist generate` to refresh
`.github/workflows/release.yml` (never edit that file by hand).

**To release:** bump `version` in `Cargo.toml`, `mcpb/manifest.json` and `server.json`, update
`CHANGELOG.md`, then push a tag like `v0.2.0` on a commit of `main`. The release workflow:

1. runs `ci.yml` as a gate and builds 5 targets: macOS arm64 and x64, Linux x64 and arm64
   (glibc ≥ 2.35, built on ubuntu-22.04), Windows x64;
2. builds the shell and PowerShell installers and checksums (install path `CARGO_HOME`, i.e. `~/.cargo/bin`);
3. `build-mcpb.yml`: the Claude Desktop bundle `onchain-data-mcp.mcpb` (universal macOS + Windows);
4. creates the GitHub Release;
5. publishes the Homebrew formula to `madlabs-tech/homebrew-tap` and the Docker image
   `ghcr.io/madlabs-tech/onchain-data-mcp` (`publish-docker.yml`: version, major.minor, `latest`);
6. `publish-registry.yml`: fills the version and `.mcpb` SHA-256 into `server.json` and
   publishes `io.github.madlabs-tech/onchain-data-mcp` to the official MCP Registry.

**One-time setup:**

- Create the repo `madlabs-tech/homebrew-tap` and add a `HOMEBREW_TAP_TOKEN` secret (a token
  that can push to it) to this repo.
- After the first image push, make the ghcr package `onchain-data-mcp` **public**.
- The MCP Registry uses GitHub OIDC (no secret); the Docker image carries the
  `io.modelcontextprotocol.server.name` label the registry checks.

**npm later:** append `"npm"` to `installers` and `publish-jobs` in `dist-workspace.toml`, set
`npm-scope`, add an `NPM_TOKEN` secret, then run `dist generate`.

## Legacy compatibility

- `eth_get_balance`, `eth_get_code`, `eth_gas_price` and `eth_get_transaction_by_hash` remain as
  aliases in the `legacy` domain with unchanged chain names. Prefer `wallet_get_balances`,
  `address_validate`, `tx_estimate_fee` and `tx_get`.
- `RPC_URL` is deprecated and applies to **Ethereum (`eip155:1`) only** (it becomes
  `[custom_rpc.rpc_url]`, locked by env, with a startup warning).
- `BDM__` env vars still work but are deprecated; rename them to `ODM__`.
