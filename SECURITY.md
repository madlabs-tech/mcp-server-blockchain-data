# Security

## Report a vulnerability

Please do not open a public issue for security problems.

Report it privately with a GitHub Security Advisory:
[github.com/madlabs-tech/onchain-data-mcp/security/advisories/new](https://github.com/madlabs-tech/onchain-data-mcp/security/advisories/new)
(or the repo's **Security** tab, then **Report a vulnerability**).

Include what you found, how to reproduce it, and which version you use
(`onchain-data-mcp --version` or the release tag). We will reply as soon as we can,
fix the issue, and credit you in the release notes if you want.

Only the latest release gets security fixes.

## Keep your setup safe

- **Dashboard password.** The dashboard and its admin API can change your config,
  save vendor API keys and create client keys. Set `DASHBOARD_PASSWORD` so nobody
  else can open it.
- **Keep the dashboard on localhost.** By default it listens on `127.0.0.1:8787`,
  so only your own computer can reach it. Do not bind it to `0.0.0.0` or open the port
  on your router. For a server, use hosted mode and keep `admin_bind` on `127.0.0.1`
  (reach it with an SSH tunnel), or put it behind a firewall.
- **Never share vendor API keys.** Keys for Alchemy, Helius, CoinGecko and others are
  tied to your account and quota. Do not paste them into issues, logs, screenshots or
  chats. The dashboard stores them in `secrets.toml` (readable only by your user on
  macOS and Linux). If a key leaks, rotate it in the vendor's dashboard.
- **Client keys** (`odm_...`) for hosted mode are secrets too. Revoke any key you
  think has leaked.
