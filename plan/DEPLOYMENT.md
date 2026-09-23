# Deployment

The same binary (`evm-mcp-server`) runs in two modes. Configuration is layered:

**built-in registry < `config/config.toml` (the dashboard writes this) < `config/secrets.toml` < environment variables**

Settings fixed by env appear in the dashboard as **locked by env** and are read-only there.

> Status: target design for Release 1 (see [PLAN.md](PLAN.md)). The Docker/systemd files are delivered in task T2.3b.

## Modes at a glance

| | `self_hosted` (default) | `hosted` (operator VPS) |
|---|---|---|
| Who runs it | each user, with their own vendor keys | the operator, with their vendor accounts, serving other people |
| Transports | stdio (Claude Desktop) + HTTP on `127.0.0.1` | MCP HTTP (`/mcp`) + REST (`/v1/*`) on `public_bind`, behind a TLS reverse proxy |
| Client auth | none (local only) | **required**: `Authorization: Bearer <client key>` |
| Startup guard | none | **refuses to start** if no client keys exist (fail closed) |
| Limits | per-vendor limit / cap | per-vendor limit / cap **plus** per-client limits |
| Dashboard / admin API | `http://127.0.0.1:8787/dashboard` | separate `admin_bind` (default `127.0.0.1:8788`), **never** on the public port |

## Self-hosted

```bash
cp .env.example .env               # fill in the keys you created from plan/VENDORS.md (all optional)
cp config/config.example.toml config/config.toml
cargo run --release -p ems-server  # binary: target/release/evm-mcp-server
```

Claude Desktop (stdio). The binary name and path are unchanged from earlier versions:

```json
{
  "mcpServers": {
    "blockchain-data": {
      "command": "/path/to/target/release/evm-mcp-server",
      "env": { "ALCHEMY_API_KEY": "…", "HELIUS_API_KEY": "…" }
    }
  }
}
```

The dashboard is at `http://127.0.0.1:8787/dashboard` (also while Claude Desktop runs the server over stdio). Sign in with the admin token from `config/admin_token`, which is generated on first start (mode 0600) and printed once in the log. Usage counters live in `<data_dir>/ems.db` (default `./data`). If that directory isn't writable, for example when Claude Desktop starts the binary with cwd `/`, the server keeps counters in memory and logs a warning; set `EMS__SERVER__DATA_DIR` and `--config-dir` to absolute paths to persist them.

**Zero-key mode:** with no keys at all, the server uses public RPCs and keyless vendors (DefiLlama, DexScreener, GeckoTerminal, CoW, Velora, Frankfurter, the Chainalysis oracle…). Tools that need a key return `UNSUPPORTED_CAPABILITY` with a hint.

## Hosted (small VPS)

### 1. Limits the operator controls
Every value can be set in the dashboard **or** by env (`EMS__` prefix, `__` between path segments).

**Per vendor:**

| Setting | Meaning | Env example |
|---|---|---|
| `limit.*` | The vendor's real quota (defaults to its free tier from `registry/vendors.toml`). Raise it if you upgrade a plan. | `EMS__VENDORS__ALCHEMY__LIMIT__MONTHLY_CREDITS=30000000` |
| `cap.*` | Your own budget, **below** the limit, e.g. keep half for your own testing. Windows: `rps`, `per_minute`, `daily`, `monthly_credits`. | `EMS__VENDORS__ALCHEMY__CAP__MONTHLY_CREDITS=15000000` |
| `reserve_pct` | Stop routing to this vendor at `(100 - reserve_pct)%` of the budget | `EMS__VENDORS__ALCHEMY__RESERVE_PCT=10` |
| `on_exhausted` | `skip` (move to the next vendor) or `allow_overage` | `EMS__VENDORS__ALCHEMY__ON_EXHAUSTED=skip` |
| `enabled` | Turn the vendor on or off | `EMS__VENDORS__MORALIS__ENABLED=false` |

The router uses **`effective budget = min(cap, limit × (1 − reserve_pct/100))`** per window.

**Per client.** Defaults come from `[clients.default]`. Each field can be overridden per client, and the most specific value wins: the override set for that key in the dashboard or `PATCH /admin/api/clients/{id}` (stored in sqlite), then `[clients.overrides.<id>]`, then `[clients.default]`.

| Setting | Env example |
|---|---|
| requests per minute | `EMS__CLIENTS__DEFAULT__REQUESTS_PER_MINUTE=30` |
| daily requests | `EMS__CLIENTS__DEFAULT__DAILY_REQUESTS=1000` |
| monthly vendor credits spent on the client's behalf | `EMS__CLIENTS__DEFAULT__MONTHLY_CREDITS=200000` |
| allowed tool profile | `EMS__CLIENTS__DEFAULT__TOOL_PROFILE=payments` |

A client over its limit gets HTTP 429 `QUOTA_EXCEEDED`, with `retry_after_secs` and a `Retry-After` header. The requests-per-minute limit is a token bucket, daily requests reset at 00:00 UTC, and monthly credits reset on the 1st (UTC). Other clients are not affected.

**Routing order**, also settable by env: `EMS__ROUTING__DEFAULTS__EVM_RPC=alchemy,quicknode,public`.

### 2. Seeing usage and quota
The dashboard **Quota** page shows one card per vendor:
- limit / cap / effective budget, and used / remaining for each window
- a source badge: `vendor API` (the vendor's own usage endpoint), `headers` (rate-limit response headers) or `estimated` (our local metering from the per-method cost table)
- burn rate and projected run-out date
- which tools, methods, chains and clients use the quota
- a 30-day chart and CSV export
- the alert thresholds you crossed (`alert_pct`, default 75/90). Each one is also logged once per window.

The guard uses the most pessimistic source: after each `QuotaReporter` poll (every `server.quota_poll_secs`, default 300), local counters are raised to the vendor-reported usage when it is higher. A rate-limit header with `remaining = 0` marks the vendor exhausted until the reset.

The **Clients** page shows usage per client key. The same data is available at `GET /admin/api/quota` (and `/admin/api/quota.csv`) and `GET /admin/api/clients`.

**Admin API** (scriptable; every call needs `Authorization: Bearer <admin token>` **and** `X-EMS-Admin: 1`):

| Endpoint | Purpose |
|---|---|
| `GET /admin/api/health` | vendor breaker/latency/usage, per-tool call stats |
| `GET /admin/api/config` | settings (secrets removed), locked-by-env map, vendor key status, effective orders per capability and chain, tools, chains |
| `PUT /admin/api/config` `{"edits":[{"path":[…],"value":…}]}` | validate, write atomically (`.bak` kept), hot-swap routing; `value: null` removes a key; env-locked paths are refused (422) |
| `POST /admin/api/config/validate` | the same checks, nothing written |
| `POST /admin/api/reload` | re-read the files (same as `kill -HUP <pid>`) |
| `POST /admin/api/vendors/{id}/test` | one cheap call (usage endpoint, `eth_blockNumber`, `getSlot` or an FX rate) |
| `POST /admin/api/vendors/{id}/budget` `{"which":"cap","window":"monthly","value":15000000}` | set or clear (`null`) one limit/cap window |
| `GET /admin/api/quota`, `GET /admin/api/quota.csv`, `POST /admin/api/quota/refresh` | quota report, CSV export, poll vendor usage APIs now |
| `GET/POST /admin/api/clients`, `PATCH/DELETE /admin/api/clients/{id}` | list, create (key returned once), set limits, revoke |
| `GET /admin/api/calls?limit=N`, `GET /admin/api/calls/stream` | recent calls, live Server-Sent Events stream |

Keys are write-only (`{"path":["keys","alchemy","api_key"],"value":"…"}` goes to `secrets.toml`, mode 0600), and no response ever contains a key.

### 3. Run it (Docker + Caddy, delivered in T2.3b)

```bash
# on the VPS
git clone <repo> && cd <repo>
cp .env.example .env && chmod 600 .env        # operator keys + EMS__SERVER__MODE=hosted
docker compose -f deploy/docker-compose.yml up -d
```

- `deploy/docker-compose.yml` runs `evm-mcp-server` plus **Caddy**, which gets TLS certificates automatically for your domain. Only Caddy's ports 80/443 are public.
- The sqlite database (`<data_dir>/ems.db`, e.g. `/data/ems.db` with `EMS__SERVER__DATA_DIR=/data`: usage counters, client keys, call log) lives on a named volume. Back it up with `sqlite3 /data/ems.db ".backup '/data/backup.db'"` or copy the volume while stopped.
- **Low-memory profile for a small VPS:** set `EMS__SERVER__CACHE_MAX_ENTRIES` (moka size cap). sqlite runs in WAL mode.
- **Alternative without Docker:** `deploy/evm-mcp-server.service` (systemd) plus any reverse proxy.

### 4. Create client keys
Hosted mode refuses to start until at least one client key exists, so create the first one from the shell:

```bash
evm-mcp-server clients create alice --config-dir config   # prints the key once
evm-mcp-server clients list --config-dir config           # ids, status, names (never keys)
```

(With Docker: `docker compose -f deploy/docker-compose.yml run --rm evm-mcp-server clients create alice`.)

After that, manage keys in the dashboard. Open it through an SSH tunnel: `ssh -L 8788:127.0.0.1:8788 you@vps`, then go to `http://127.0.0.1:8788/dashboard`.
- The admin token is generated on first start and saved to `config/admin_token` (mode 0600). It is also printed once in the logs.
- **Clients → Create key.** The key is shown **once** and stored as a SHA-256 hash. **Revoke** takes effect immediately.

Clients then connect with:

```json
{ "mcpServers": { "blockchain-data": { "url": "https://your.domain/mcp",
  "headers": { "Authorization": "Bearer <client key>" } } } }
```

or over REST: `curl -H "Authorization: Bearer <client key>" -X POST https://your.domain/v1/wallet/get_balances -d '{…}'`.

### 5. Security checklist
- [ ] `EMS__SERVER__MODE=hosted` (without client keys, the server refuses to start)
- [ ] the admin port is not reachable publicly (`curl https://your.domain:8788` fails)
- [ ] `.env`, `config/secrets.toml` and `config/admin_token` are mode 0600 and not in git
- [ ] separate vendor accounts/keys for the VPS and for local testing, so quotas don't collide
- [ ] a vendor `cap` is set below its `limit` for every vendor the public instance uses
- [ ] conservative `clients.default` limits, raised per trusted client
