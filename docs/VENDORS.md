# Data providers

A data provider is an outside service that this server asks for blockchain data. Examples are
token prices, wallet balances and scam checks. The server asks several providers. If one is
slow, busy or down, it asks the next one.

You don't need to sign up for anything. The server works out of the box with the Tier 1
providers below.

Some providers need an **API key**. An API key is a free password the provider gives you when
you sign up. Adding a few free keys makes the server faster and more complete. For example,
wallet balances and transaction history work best with an Alchemy key.

Providers are grouped into four tiers:

- **Tier 1** — free, no signup.
- **Tier 2** — free key, big limit.
- **Tier 3** — free key, small limit.
- **Tier 4** — paid or trial only. These are off unless you turn them on.

Checked on 2026-09-25. Providers change their plans, so check their site too.

## Tier 1: Free, no signup

These work right away. A few accept an optional free key that raises the limit.

| Provider | Needs a key? | Setting name | Free limit | What we use it for | Sign up |
|---|---|---|---|---|---|
| Flashbots Protect | No | — | No set limit | Sending transactions privately on Ethereum, so bots can't jump ahead of you | Not needed |
| MEV Blocker | No | — | No set limit | Sending transactions privately on Ethereum | Not needed |
| Helius Sender | No | — | 50 requests a second | Sending transactions fast on Solana | Not needed |
| Public blockchain connections | No | — | 5 requests a second (our safety limit) | Basic blockchain reads and sending transactions, as a last backup | Not needed |
| DexScreener | No | — | 300 requests a minute | Token prices from trading pools | Not needed |
| DefiLlama | No | — | 60 requests a minute (our safety limit) | Prices and past prices | Not needed |
| CoW Protocol | No | — | 60 requests a minute (our safety limit) | Swap prices | Not needed |
| Frankfurter | No | — | 60 requests a minute (our safety limit) | Currency exchange rates (for example USD to EUR) | Not needed |
| Jito | No | — | 1 request a second | Sending transactions privately on Solana | Not needed |
| Jupiter | Optional | `JUPITER_API_KEY` | 30 requests a minute (60 with a free key) | Solana prices, swap prices and token details | [portal.jup.ag](https://portal.jup.ag/) |
| GoPlus Security | Optional | `GOPLUS_APP_KEY`, `GOPLUS_APP_SECRET` | 30 requests a minute | Scam checks for tokens | [gopluslabs.io](https://gopluslabs.io/security-api) |
| honeypot.is | No | — | 30 requests a minute (our safety limit) | Scam checks (tokens you can buy but not sell) on Ethereum, BNB Chain and Base | Not needed |
| RugCheck | Optional | `RUGCHECK_API_KEY` | We keep it to 30 a minute (they don't publish a limit) | Checking Solana tokens for scams | Not needed |
| GeckoTerminal | No | — | 10 requests a minute | Token prices from trading pools | Not needed |
| Velora (ParaSwap) | No | — | 5,000 requests a day, 1 a second | Swap prices | Not needed |
| TRM Labs | Optional | `TRM_API_KEY` | 100 checks a day (100,000 with a free key) | Sanctions checks (is this address on a blocked list?) | [trmlabs.com](https://www.trmlabs.com/products/sanctions) |
| Chainalysis sanctions list | No | — | Uses your blockchain connection's limit | Sanctions checks on Ethereum-style chains | Not needed |
| On-chain reads | No | — | Uses your blockchain connection's limit | Wallet balances, transaction history, fees and token details read straight from the blockchain | Not needed |

## Tier 2: Free key, big limit

A free key with at least 1 million units a month. These give the biggest boost.

| Provider | Needs a key? | Setting name | Free limit | What we use it for | Sign up |
|---|---|---|---|---|---|
| Alchemy | Yes | `ALCHEMY_API_KEY` | 30 million credits a month | Blockchain reads, wallet balances, transaction history and sending transactions (Ethereum-style chains and Solana) | [dashboard.alchemy.com](https://dashboard.alchemy.com/signup) |
| Helius | Yes | `HELIUS_API_KEY` | 1 million credits a month | Solana reads, wallet balances, fees and sending transactions | [dashboard.helius.dev](https://dashboard.helius.dev/signup) |

## Tier 3: Free key, small limit

A free key, but the monthly limit is small. Still worth adding.

| Provider | Needs a key? | Setting name | Free limit | What we use it for | Sign up |
|---|---|---|---|---|---|
| 1inch | Yes | `ONEINCH_API_KEY` | 100,000 requests a month | Swap prices | [business.1inch.com](https://business.1inch.com/portal) |
| Birdeye | Yes | `BIRDEYE_API_KEY` | 30,000 credits a month | Solana token prices | [bds.birdeye.so](https://bds.birdeye.so) |
| CoinGecko (Demo) | Yes | `COINGECKO_API_KEY` | 10,000 requests a month | Prices, past prices and token details | [coingecko.com](https://www.coingecko.com/en/developers/dashboard) |
| Open Exchange Rates | Yes | `OPENEXCHANGERATES_APP_ID` | 1,000 requests a month | Currency exchange rates (backup) | [openexchangerates.org](https://openexchangerates.org/signup/free) |

## Tier 4: Paid or trial only — off unless you turn it on

These have no lasting free plan, we could not confirm one, or (like Ankr) the free plan needs a
signup key. The server skips them unless you turn them on. Turn one on only if you pay for it (or want to use its trial).

| Provider | Needs a key? | Setting name | Free limit | What we use it for | Sign up |
|---|---|---|---|---|---|
| Ankr | Yes | `ANKR_API_KEY` | Free plan exists (200M credits/month, 50 requests/minute) but needs a signup key; off by default — turn it on in config if you add ANKR_API_KEY. | Wallet balances | [ankr.com](https://www.ankr.com/rpc/) |
| QuickNode | Yes | `QN_ENDPOINT_NAME`, `QN_TOKEN_ID` | Trial only: 10 million credits for 1 month | Blockchain reads, fees and sending transactions (backup) | [dashboard.quicknode.com](https://dashboard.quicknode.com/signup) |
| Uniswap Trading API | Yes | `UNISWAP_API_KEY` | Not confirmed as free | Swap prices | [developers.uniswap.org](https://developers.uniswap.org/dashboard/welcome) |
| OKX DEX | Yes | `OKX_API_KEY`, `OKX_SECRET_KEY`, `OKX_PASSPHRASE` | Trial only: 60 days | Swap prices | [web3.okx.com](https://web3.okx.com/build/dev-portal) |
| Pyth | Yes | `PYTH_API_KEY` | Short free trial, then paid | Past prices of main coins (ETH, SOL…) | [pythdata.app](https://pythdata.app/signup) |
| Moralis | Yes | `MORALIS_API_KEY` | None (paid plans from $149 a month) | Wallet balances and transaction history (backup) | [admin.moralis.com](https://admin.moralis.com/register) |
| 0x | Yes | `ZEROEX_API_KEY` | None (paid plans from $1,000 a month) | Swap prices | [dashboard.0x.org](https://dashboard.0x.org) |

## How to add a key

**In the dashboard (easiest):**

1. Open the dashboard and go to **Providers**.
2. Find the provider and paste your key.
3. Click **Test** to check it works.

**Or with a setting:** set it under its setting name, for example `ALCHEMY_API_KEY=your-key-here`.
Put it in the `env` block of your AI app's config, in your Docker `.env` file, or in your server's
environment file. The program does not read a `.env` file on its own. Then restart the server.

**To turn on a Tier 4 provider:** switch it on in the dashboard, or add this to `config.toml`:

```toml
[vendors.moralis]
enabled = true
```

## What changed

Checked on 2026-09-25:

- **Moralis** ended its free plan on 2026-09-01. It is now off by default.
- **QuickNode** now offers a 1-month trial instead of a free plan. It is now off by default.
- **Pyth** now needs a key, with a free trial and then paid plans. It is now off by default.
- CoinGecko's free plan allows 100 requests a minute (up from the 30 we used before).
- Velora's free plan is 5,000 requests a day and 1 a second.
- **RugCheck** is now on — it needs no key.
- **Ankr** has a free plan, but it needs a signup key, so it stays off by default.

Nothing breaks when a provider turns off. The server simply asks the next one.
