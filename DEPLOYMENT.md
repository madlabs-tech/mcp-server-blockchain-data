# Put it on a server

This guide is for running onchain-data-mcp **for other people or apps** ("hosted mode").
If it's just for you, you don't need this: see the [README](README.md).

**How hosted mode works**

- The program runs on a server (a computer that is always on, for example a small cloud server, also called a VPS).
- Your customers connect over the internet, at an address like `https://api.example.com/mcp`.
- Each customer gets a **client key** (starts with `odm_`). No key, no access.
- Each client key has its own limits, so one busy customer can't use up everything.
- The program uses **your** provider keys (Alchemy, Helius…) for everyone.
- The dashboard is **never** on the internet. You reach it through a private tunnel (see [Open the dashboard](#open-the-dashboard)).

**What you need**

- A Linux server with about 1 GB of memory.
- A domain name (like `api.example.com`) that points to your server's IP address.
- About 30 minutes.

Pick **Option A** (Docker, easiest) or **Option B** (no Docker).

## Option A: Docker + Caddy (recommended)

Docker runs programs in sealed boxes. Caddy is a small web server that sits in front and gets a
free HTTPS certificate for your domain by itself. Only Caddy is reachable from the internet.

### 1. Get the files

On your server:

```bash
git clone https://github.com/madlabs-tech/onchain-data-mcp.git
cd onchain-data-mcp
cp .env.example .env
chmod 600 .env
```

### 2. Fill in your settings

Open `.env` in a text editor (for example `nano .env`) and fill in:

```bash
# Your provider keys (all optional, but recommended for a public server)
ALCHEMY_API_KEY=your-alchemy-key
HELIUS_API_KEY=your-helius-key

# Your dashboard password: at least 12 characters, no spaces
DASHBOARD_PASSWORD=pick-a-long-password-here
```

You don't need to set the mode or addresses. The Docker setup already turns on hosted mode.

### 3. Put in your domain

Replace `your.domain` with your real domain in the Caddy settings:

```bash
sed -i 's/your.domain/api.example.com/' deploy/Caddyfile
```

### 4. Create the first client key

Hosted mode won't start without at least one client key. Create one (the first time, this also
builds the program, which can take 10 to 20 minutes):

```bash
docker compose -f deploy/docker-compose.yml run --rm onchain-data-mcp clients create alice --config-dir /config
```

It prints the key **once**, like `key: odm_6924…`. Copy it somewhere safe. We only keep a
scrambled copy (a "hash"), so it can't be shown again. Lost it? Create a new one.

### 5. Start it

```bash
docker compose -f deploy/docker-compose.yml up -d
```

Check that it works (from any computer):

```bash
curl https://api.example.com/healthz
```

It should answer `ok`. To watch the logs: `docker compose -f deploy/docker-compose.yml logs -f onchain-data-mcp`.

## Option B: Without Docker (systemd)

systemd is the part of Linux that starts programs when the server boots.
Our service file runs the program as a locked-down user that can only write to its own folder.

### 1. Install the program and the files

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/madlabs-tech/onchain-data-mcp/releases/latest/download/onchain-data-mcp-installer.sh | sh
sudo cp ~/.cargo/bin/onchain-data-mcp /usr/local/bin/
git clone https://github.com/madlabs-tech/onchain-data-mcp.git
sudo cp onchain-data-mcp/deploy/onchain-data-mcp.service /etc/systemd/system/
```

### 2. Write the settings file

```bash
sudo mkdir -p /etc/onchain-data-mcp
sudo nano /etc/onchain-data-mcp/env
```

Paste this, and fill in your own values:

```bash
ODM__SERVER__MODE=hosted
ODM__SERVER__PUBLIC_BIND=127.0.0.1:8787
DASHBOARD_PASSWORD=pick-a-long-password-here
ALCHEMY_API_KEY=your-alchemy-key
HELIUS_API_KEY=your-helius-key
```

Then lock the file so only the admin can read it:

```bash
sudo chmod 600 /etc/onchain-data-mcp/env
```

Set the dashboard password here, in `DASHBOARD_PASSWORD`. The service runs as a temporary,
locked-down user, so `onchain-data-mcp password reset` can't be used for it.

### 3. Create the first client key

This runs the command inside the same locked-down setup the service uses:

```bash
sudo systemd-run --wait --pipe --collect -p DynamicUser=yes -p StateDirectory=onchain-data-mcp \
  -p EnvironmentFile=/etc/onchain-data-mcp/env -p Environment=ODM__SERVER__DATA_DIR=/var/lib/onchain-data-mcp/data \
  /usr/local/bin/onchain-data-mcp clients create alice --config-dir /var/lib/onchain-data-mcp/config
```

Copy the `odm_…` key it prints. It is shown only once.

### 4. Start it

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now onchain-data-mcp
sudo journalctl -u onchain-data-mcp -f
```

### 5. Add HTTPS in front

The program now listens only on the server itself (`127.0.0.1:8787`). Put a web server with
HTTPS in front of it. With [Caddy](https://caddyserver.com/docs/install), use
`deploy/Caddyfile` and change two things: your domain instead of `your.domain`, and
`127.0.0.1:8787` instead of `onchain-data-mcp:8787`. Never forward port 8788 (the dashboard).

## Open the dashboard

The dashboard runs on the server at `127.0.0.1:8788`, which the internet can't reach.
To open it, make a private tunnel from your own computer with SSH (the tool you use to log in
to your server):

```bash
ssh -N -L 8788:127.0.0.1:8788 you@your-server
```

Keep that window open, then visit `http://127.0.0.1:8788/dashboard` in your browser and log in
with your `DASHBOARD_PASSWORD`.

Forgot the password?

- Docker: `docker exec ems onchain-data-mcp password --config-dir /config`
- systemd: look in `/etc/onchain-data-mcp/env`

## Create client keys

One key per customer or app. Two ways:

- **Dashboard:** open **Clients**, type a name, click create. You can also change limits or cancel a key there; cancelling works right away.
- **Command line:** then restart the program so it sees the new key (the dashboard doesn't need a restart).
  - Docker: `docker exec ems onchain-data-mcp clients create bob --config-dir /config`, then `docker restart ems`
  - systemd: the `systemd-run` command from [step 3](#3-create-the-first-client-key) with a new name, then `sudo systemctl restart onchain-data-mcp`

To list keys (names and ids only, never the keys themselves): `clients list` instead of `clients create <name>`.

**Default limits per client key.** You can change them per key in the dashboard.

| Limit | Default | Setting to change the default |
|---|---|---|
| Requests per minute | 30 | `ODM__CLIENTS__DEFAULT__REQUESTS_PER_MINUTE` |
| Requests per day (resets at midnight UTC) | 1,000 | `ODM__CLIENTS__DEFAULT__DAILY_REQUESTS` |
| Provider credits per month (resets on the 1st) | 200,000 | `ODM__CLIENTS__DEFAULT__MONTHLY_CREDITS` |
| Tool set | `payments` | `ODM__CLIENTS__DEFAULT__TOOL_PROFILE` |

A customer over their limit gets a "too many requests" answer telling them when to try again.
Other customers are not affected.

## Give a customer their settings

Send them their key and this, with your domain filled in. It goes into their AI app's MCP
settings (for example `claude_desktop_config.json` or `~/.cursor/mcp.json`):

```json
{
  "mcpServers": {
    "onchain-data": {
      "url": "https://api.example.com/mcp",
      "headers": { "Authorization": "Bearer odm_their-key-here" }
    }
  }
}
```

For Claude Code:

```bash
claude mcp add --transport http onchain-data https://api.example.com/mcp --header "Authorization: Bearer odm_their-key-here"
```

Apps that don't use MCP can call the same tools as a normal web API. See
[docs/TECHNICAL.md](docs/TECHNICAL.md#rest-api).

## Update to a new version

**Docker:**

```bash
cd onchain-data-mcp
git pull
docker compose -f deploy/docker-compose.yml up -d --build
```

**systemd:** run the install command from Option B step 1 again, then:

```bash
sudo cp ~/.cargo/bin/onchain-data-mcp /usr/local/bin/
sudo systemctl restart onchain-data-mcp
```

Your settings, keys and usage numbers are kept.

## Backups

Two things hold everything:

- **The database** `bdm.db`: usage numbers, client keys (scrambled) and the call log.
- **The settings folder**: `config.toml`, `secrets.toml` (provider keys) and `dashboard_password`.

**Docker** (saves both into the current folder):

```bash
docker run --rm --volumes-from ems -v "$PWD":/out alpine \
  sh -c 'apk add -q sqlite && sqlite3 /data/bdm.db ".backup /out/bdm-$(date +%F).db" && tar -C /config -czf /out/config-$(date +%F).tgz .'
```

**systemd:**

```bash
sudo sqlite3 /var/lib/onchain-data-mcp/data/bdm.db ".backup /root/bdm-$(date +%F).db"
sudo tar -C /var/lib/onchain-data-mcp -czf /root/config-$(date +%F).tgz config
sudo cp /etc/onchain-data-mcp/env /root/env-$(date +%F)
```

(The systemd way needs the `sqlite3` tool: `sudo apt install sqlite3`.)

The backups contain secrets. Keep them somewhere private.

**To restore:** stop the program, put the database and the settings folder back, and start it
again. Client keys keep working.

## Security checklist

- [ ] Hosted mode is on. (The Docker setup does this for you. It won't start without a client key.)
- [ ] Only the web ports 80 and 443 are open to the internet. Test from another computer: `curl -m 5 http://api.example.com:8787/` and `curl -m 5 http://api.example.com:8788/` should both fail.
- [ ] `.env`, `/etc/onchain-data-mcp/env` and `secrets.toml` can only be read by the admin (`chmod 600`), and are never added to git.
- [ ] You set a strong `DASHBOARD_PASSWORD`, and only open the dashboard through the SSH tunnel.
- [ ] The server uses its own provider accounts, separate from the ones on your laptop, so their limits don't clash.
- [ ] Each provider has a budget below its real limit (dashboard, **Providers** page), so customers can't use up your whole plan.
- [ ] Client key limits start low. Raise them for customers you trust.
- [ ] You cancel any client key you think has leaked (dashboard, **Clients** page).
- [ ] Backups run on a schedule.
- [ ] You update when a new version comes out.
