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
cargo run --release -p server      # binary: target/release/evm-mcp-server
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

**Per client** (defaults in `[clients.default]`, overridable per client key in the dashboard):

| Setting | Env example |
|---|---|
| requests per minute | `EMS__CLIENTS__DEFAULT__REQUESTS_PER_MINUTE=30` |
| daily requests | `EMS__CLIENTS__DEFAULT__DAILY_REQUESTS=1000` |
| monthly vendor credits spent on the client's behalf | `EMS__CLIENTS__DEFAULT__MONTHLY_CREDITS=200000` |
| allowed tool profile | `EMS__CLIENTS__DEFAULT__TOOL_PROFILE=payments` |

A client over its limit gets `QUOTA_EXCEEDED` with a reset time. Other clients are not affected.

**Routing order**, also settable by env: `EMS__ROUTING__DEFAULTS__EVM_RPC=alchemy,quicknode,public`.

### 2. Seeing usage and quota
The dashboard **Quota** page shows one card per vendor:
- limit / cap / effective budget, and used / remaining for each window
- a source badge: `vendor API` (the vendor's own usage endpoint), `headers` (rate-limit response headers) or `estimated` (our local metering from the per-method cost table)
- burn rate and projected run-out date
- which tools, methods, chains and clients use the quota
- a 30-day chart and CSV export

The **Clients** page shows usage per client key. The same data is available at `GET /admin/api/quota` and `GET /admin/api/clients`.

### 3. Run it (Docker + Caddy, delivered in T2.3b)

```bash
# on the VPS
git clone <repo> && cd <repo>
cp .env.example .env && chmod 600 .env        # operator keys + EMS__SERVER__MODE=hosted
docker compose -f deploy/docker-compose.yml up -d
```

- `deploy/docker-compose.yml` runs `evm-mcp-server` plus **Caddy**, which gets TLS certificates automatically for your domain. Only Caddy's ports 80/443 are public.
- The sqlite database (`/data/ems.db`: usage counters, client keys, call log) lives on a named volume. Back it up with `sqlite3 /data/ems.db ".backup '/data/backup.db'"` or copy the volume while stopped.
- **Low-memory profile for a small VPS:** set `EMS__SERVER__CACHE_MAX_ENTRIES` (moka size cap). sqlite runs in WAL mode.
- **Alternative without Docker:** `deploy/evm-mcp-server.service` (systemd) plus any reverse proxy.

### 4. Create client keys
Open the dashboard through an SSH tunnel: `ssh -L 8788:127.0.0.1:8788 you@vps`, then go to `http://127.0.0.1:8788/dashboard`.
- The admin token is generated on first start and saved to `config/admin_token` (mode 0600). It is also printed once in the logs.
- **Clients → New key.** The key is shown **once** and stored as a SHA-256 hash.

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
