# Deployment

One binary, two modes. Configuration is layered: **built-in registry < `config/config.toml` < `config/secrets.toml` < environment** (`EMS__<PATH>`, `__` between segments). Values set by env show as **locked by env** in the dashboard.

| | `self_hosted` (default) | `hosted` (operator VPS) |
|---|---|---|
| Who | you, with your own vendor keys | an operator serving other people with their vendor accounts |
| Transports | stdio (Claude Desktop) + HTTP on `http_bind` (`127.0.0.1:8787`) | REST + `/mcp` on `public_bind`, behind a TLS proxy |
| Client auth | none (local only) | `Authorization: Bearer <client key>` on every request |
| Startup | – | refuses to start with no client keys (fail closed) |
| Limits | per vendor (limit / cap / reserve) | per vendor **and** per client |
| Dashboard | `http://127.0.0.1:8787/dashboard` | `admin_bind` (`127.0.0.1:8788`), never on the public port |

## Self-hosted

```bash
cargo build --release -p ems-server
cp .env.example .env                              # all keys optional
cp config/config.example.toml config/config.toml  # optional; the dashboard edits this file
set -a; source .env; set +a
./target/release/evm-mcp-server serve             # or without `serve` for MCP on stdio
```

- Dashboard: `http://127.0.0.1:8787/dashboard`, token in `config/admin_token` (created on first start, mode 0600, printed once in the log).
- State: `<data_dir>/ems.db` (default `./data`): usage counters, client keys, call log. If the directory isn't writable (Claude Desktop starts the binary with cwd `/`) the server keeps counters in memory and warns; pass absolute `--config-dir` and `EMS__SERVER__DATA_DIR`.
- Zero keys: public RPCs plus keyless vendors (DefiLlama, DexScreener, GeckoTerminal, CoW, Velora, Frankfurter, GoPlus, TRM keyless tier, Chainalysis oracle, Flashbots, Jito…). Tools that need a key answer `UNSUPPORTED_CAPABILITY`.
- Self-hosted has **no client auth**. Keep `http_bind` on loopback; the server warns if it isn't.

Docker, self-hosted (the image binds `0.0.0.0:8787` inside the container; publish it on loopback only):

```bash
docker build -t evm-mcp-server .
docker run -d --name ems -p 127.0.0.1:8787:8787 --env-file .env \
  -v ems-data:/data -v ems-config:/config evm-mcp-server
docker exec ems cat /config/admin_token
```

## Hosted (small VPS)

### 1. Configure

```bash
git clone <repo> && cd <repo>
cp .env.example .env && chmod 600 .env
```

In `.env` set at least:

```
EMS__SERVER__MODE=hosted
EMS__SERVER__PUBLIC_BIND=0.0.0.0:8787      # inside the container / behind the proxy
EMS__SERVER__ADMIN_BIND=0.0.0.0:8788       # container: published to 127.0.0.1 on the host only
ALCHEMY_API_KEY=…                          # the operator's vendor keys
```

Everything below can be set by env **or** later in the dashboard (env wins and locks the field).

**Per vendor** (`EMS__VENDORS__<ID>__…`):

| Setting | Meaning | Example |
|---|---|---|
| `LIMIT__<window>` | the vendor's real quota (defaults to its free tier) | `EMS__VENDORS__ALCHEMY__LIMIT__MONTHLY_CREDITS=30000000` |
| `CAP__<window>` | your budget below the limit; windows `RPS`, `PER_MINUTE`, `DAILY`, `MONTHLY` (`MONTHLY_CREDITS`, `DAILY_REQUESTS` accepted) | `EMS__VENDORS__ALCHEMY__CAP__MONTHLY_CREDITS=15000000` |
| `RESERVE_PCT` | stop routing at `(100 − reserve)%` of the budget | `EMS__VENDORS__ALCHEMY__RESERVE_PCT=10` |
| `ON_EXHAUSTED` | `skip` or `allow_overage` | `EMS__VENDORS__ALCHEMY__ON_EXHAUSTED=skip` |
| `ENABLED` | on/off | `EMS__VENDORS__MORALIS__ENABLED=false` |

Effective budget per window = `min(cap, limit × (1 − reserve_pct/100))`.

**Per client** (`EMS__CLIENTS__DEFAULT__…`, overridable per key in the dashboard or `PATCH /admin/api/clients/{id}`):

| Setting | Example |
|---|---|
| requests per minute (token bucket) | `EMS__CLIENTS__DEFAULT__REQUESTS_PER_MINUTE=30` |
| daily requests (reset 00:00 UTC) | `EMS__CLIENTS__DEFAULT__DAILY_REQUESTS=1000` |
| monthly vendor credits spent for the client (reset on the 1st) | `EMS__CLIENTS__DEFAULT__MONTHLY_CREDITS=200000` |
| tool profile | `EMS__CLIENTS__DEFAULT__TOOL_PROFILE=payments` |

Over a limit → HTTP 429 `QUOTA_EXCEEDED` with `retry_after_secs` and a `Retry-After` header; other clients are unaffected.

**Routing order:** `EMS__ROUTING__DEFAULTS__EVM_RPC=alchemy,quicknode,public` (see README for precedence).

### 2. Create the first client key

Hosted mode refuses to start without one:

```bash
# Docker
docker compose -f deploy/docker-compose.yml run --rm evm-mcp-server clients create alice
# bare metal
evm-mcp-server clients create alice --config-dir config
evm-mcp-server clients list --config-dir config        # ids, status, names; never keys
```

The key (`ems_…`) is printed once and stored as a SHA-256 hash.

### 3. Run: Docker + Caddy

```bash
sed -i 's/your.domain/api.example.com/' deploy/Caddyfile
docker compose -f deploy/docker-compose.yml up -d
docker compose -f deploy/docker-compose.yml logs -f evm-mcp-server
```

`deploy/docker-compose.yml` runs the server and Caddy. Caddy obtains TLS certificates automatically and is the only service with public ports (80/443). The server's public port is reachable only from Caddy; the admin port is published to `127.0.0.1:8788` on the host only.

### 3b. Run: systemd (no Docker)

```bash
sudo cp target/release/evm-mcp-server /usr/local/bin/
sudo mkdir -p /etc/evm-mcp-server && sudo cp .env /etc/evm-mcp-server/env && sudo chmod 600 /etc/evm-mcp-server/env
sudo cp deploy/evm-mcp-server.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable evm-mcp-server
```

The unit runs as a `DynamicUser` with `ProtectSystem=strict`; the only writable path is `/var/lib/evm-mcp-server` (`config/` with `admin_token`, `config.toml`, `secrets.toml`; `data/` with `ems.db`). Set `EMS__SERVER__PUBLIC_BIND=127.0.0.1:8787` in the env file, put a TLS reverse proxy (Caddy, nginx) in front of it and leave `127.0.0.1:8788` unproxied.

Hosted mode needs a client key before the first start. Create it inside the unit's sandbox (same state directory and dynamic user), then start:

```bash
sudo systemd-run --wait --pipe --collect -p DynamicUser=yes -p StateDirectory=evm-mcp-server \
  -p EnvironmentFile=/etc/evm-mcp-server/env -p Environment=EMS__SERVER__DATA_DIR=/var/lib/evm-mcp-server/data \
  /usr/local/bin/evm-mcp-server clients create alice --config-dir /var/lib/evm-mcp-server/config
sudo systemctl start evm-mcp-server
sudo journalctl -u evm-mcp-server -f
```

### 4. Dashboard over an SSH tunnel

The admin port is never public. From your machine:

```bash
ssh -N -L 8788:127.0.0.1:8788 you@vps
# then open http://127.0.0.1:8788/dashboard
docker exec ems cat /config/admin_token            # Docker
sudo cat /var/lib/evm-mcp-server/config/admin_token # systemd
```

Clients page: create keys (shown once), set per-client limits, revoke (immediate). Quota page: limit / cap / used per vendor, source badge (`vendor API` / `headers` / `estimated`), burn rate, CSV.

Admin API for scripts: every call needs `Authorization: Bearer <admin token>` and `X-EMS-Admin: 1`:

```bash
curl -s -H "Authorization: Bearer $TOKEN" -H 'X-EMS-Admin: 1' http://127.0.0.1:8788/admin/api/quota
curl -s -H "Authorization: Bearer $TOKEN" -H 'X-EMS-Admin: 1' -X POST http://127.0.0.1:8788/admin/api/clients \
  -H 'content-type: application/json' -d '{"name":"bob"}'
```

### 5. Client configuration

```json
{ "mcpServers": { "blockchain-data": {
  "url": "https://api.example.com/mcp",
  "headers": { "Authorization": "Bearer ems_…" } } } }
```

```bash
curl -s -H 'Authorization: Bearer ems_…' -X POST https://api.example.com/v1/wallet/get_balances \
  -H 'content-type: application/json' -d '{"address":"0x…"}'
```

## Backups

Everything durable is in two places:

- `ems.db` (sqlite, WAL mode): usage counters, client keys (hashes + limits), call log. Back it up live with sqlite's online backup so the WAL is included:
  ```bash
  # Docker: copy the volume from a throwaway container
  docker run --rm -v ems-data:/data -v "$PWD":/out alpine \
    sh -c 'apk add -q sqlite && sqlite3 /data/ems.db ".backup /out/ems-$(date +%F).db"'
  # systemd
  sudo sqlite3 /var/lib/evm-mcp-server/data/ems.db ".backup /root/ems-$(date +%F).db"
  ```
  Or stop the service and copy `ems.db`, `ems.db-wal`, `ems.db-shm` together.
- the config dir: `config.toml`, `secrets.toml`, `admin_token` (all three are secret-bearing; keep the backup at mode 0600).

Restore = put both back and start. Client keys keep working since only hashes are stored.

## Security checklist

- [ ] `EMS__SERVER__MODE=hosted`; the server refuses to start without client keys.
- [ ] Only 80/443 are reachable from the internet: `curl -m 5 https://api.example.com:8788/` and `:8787` fail.
- [ ] `.env`, `secrets.toml`, `admin_token` are mode 0600 and outside git.
- [ ] Separate vendor accounts for the VPS and for local testing so quotas don't collide.
- [ ] A `cap` below the `limit` for every vendor the public instance uses, and `reserve_pct` set.
- [ ] Conservative `clients.default` limits; raise per trusted client in the dashboard.
- [ ] Dashboard only through the SSH tunnel; the admin token is never sent over plain HTTP off-host.
- [ ] `docker compose pull`/rebuild and `sudo systemctl restart` after a version bump; `POST /admin/api/reload` (or `SIGHUP`) after editing files by hand.
- [ ] Backups of `ems.db` and the config dir on a schedule.
