# Vendors & API-key checklist

_Last updated 2026-09-23. Links were checked with an HTTP fetch on that date. ⚠ means the link returned a bot-block (403) or a redirect or error, so it was not confirmed loading; it is still the vendor's official domain. Free-tier numbers come from `plan/research/*` and may change; the dashboard's Quota page shows live values once keys are in._

## How keys are supplied
You can supply each key in any of three ways. When a key is set in more than one place, the higher one in this list wins.
1. **Environment variables** (names below). Best for the VPS / Docker. A field set by env shows as **"locked by env"** in the dashboard.
2. **`config/secrets.toml`**. Written with mode `0600` and git-ignored:
   ```toml
   [keys]
   alchemy = "…"
   ```
3. **Dashboard → Vendors → key field**. Write-only: once saved, the key is never shown again. It is stored in `secrets.toml`.

Limits and caps can also be set per vendor via env, for example `EMS__VENDORS__ALCHEMY__CAP__MONTHLY_CREDITS=15000000`. See `plan/DEPLOYMENT.md` and `.env.example`.

**Hosted mode tip:** create **separate accounts/keys for the VPS and for local testing**. Then your tests can't drain the quota that the hosted instance serves to clients, and each quota card in the dashboard reflects one environment.

**Legacy:** `RPC_URL` still works but now applies to **Ethereum only** and logs a deprecation warning. Previously it silently applied to every chain.

---

## Tier A: needed to test Release 1 (create these now)
| ✓ | Vendor | Signup link | Env var(s) | Free tier (quota / rate) | Powers | Setup notes |
|---|---|---|---|---|---|---|
| [ ] | **Alchemy** | https://dashboard.alchemy.com/signup | `ALCHEMY_API_KEY` | 30M CU/month; free `eth_getLogs` capped at **10 blocks** | EVM RPC (primary `evm_rpc`), Portfolio/Token balances, Transfers API, Prices, `eth_simulateV1`, Address Activity webhooks (R2), bundler (R2) | Create one app and **turn on every chain we use**: Ethereum, Base, Arbitrum, Optimism, Polygon, Avalanche, BSC, **Robinhood Chain**. The key goes in the URL path. Note: `alchemy_simulateAssetChanges` shuts down 2026-09-30 and we don't use it. |
| [ ] | **Helius** | https://dashboard.helius.dev/signup | `HELIUS_API_KEY` | 1M credits/month, 10 RPS; DAS = 10 credits at **2 RPS**; priority-fee API = 1 credit | Solana RPC (primary `solana_rpc`), DAS balances/metadata, priority fees, webhooks (R2) | The free plan covers DAS + the priority-fee API. **Helius Sender needs no key.** `getTransfersByAddress` / `getTransactionsForAddress` are paid, so we don't use them. |
| [ ] | **QuickNode** | https://dashboard.quicknode.com/signup | `QN_ENDPOINT_NAME` + `QN_TOKEN_ID` | 10M credits/month, 15 RPS (a third party says 50M ⚠); free `eth_getLogs` capped at **5 blocks** | 2nd EVM + Solana RPC (fallback; needed for **failover tests**), `qn_estimatePriorityFees` add-on | Create a **multichain endpoint** and enable our EVM chains **plus Solana**. The URL pattern is `https://{name}.{network}.quiknode.pro/{token}/`. `QN_ENDPOINT_NAME` is `{name}` and `QN_TOKEN_ID` is `{token}`. |
| [ ] | **CoinGecko (Demo)** | https://www.coingecko.com/en/developers/dashboard ⚠ (docs: https://docs.coingecko.com/) | `COINGECKO_API_KEY` | 10k calls/month; rate 30–100/min ⚠; 365 days history | `market_get_price` (primary), `market_get_price_at`, GeckoTerminal on-chain endpoints | Choose the **Demo** (free) plan. The key goes in the **`x-cg-demo-api-key`** header, not the Pro header. |
| [ ] | **GoPlus** | https://gopluslabs.io/security-api | `GOPLUS_APP_KEY` + `GOPLUS_APP_SECRET` | 30 req/min | `token_check_risk` (primary), approval risk | Create an app to get the key + secret pair. Docs: https://docs.gopluslabs.io/reference/api-overview |
| [ ] | **1inch** | https://portal.1inch.dev/ | `ONEINCH_API_KEY` | 1 req/s, 100k/month | `trade_get_swap_quote` / `trade_build_swap_tx` (primary EVM), gas API | Pricing: https://business.1inch.com/pricing. Robinhood Chain contracts are deployed; API coverage there ⚠. |
| [ ] | **TRM Labs (sanctions)** | https://www.trmlabs.com/products/sanctions | `TRM_API_KEY` | **100 req/day**, 1 req/s | `compliance_screen_address` (alongside the free on-chain Chainalysis oracle) | Free sanctions-screening API. Docs: https://docs.sanctions.trmlabs.com/. Results are proprietary, so don't redistribute them in bulk. |

### Tier A: optional (improves fallbacks)
| ✓ | Vendor | Signup link | Env var(s) | Free tier | Powers | Setup notes |
|---|---|---|---|---|---|---|
| [ ] | **Jupiter** | https://portal.jup.ag/ | `JUPITER_API_KEY` | Keyless 0.5 RPS; free key 1 RPS (60/min), per organization | Solana price (primary for `solana:mainnet` price), Swap v2 quotes | Works without a key at a lower rate. `lite-api.jup.ag` is being retired. |
| [ ] | **Birdeye** | https://docs.birdeye.so/ (portal `bds.birdeye.so` ⚠) | `BIRDEYE_API_KEY` | 30K CU/month, 1 req/s | Solana price fallback, OHLCV / holders (R2) | |
| [ ] | **Moralis** | https://admin.moralis.com/register ⚠ | `MORALIS_API_KEY` | 40k CU/day | EVM balances/transfers fallback, approvals, holders, DeFi fallback (R2) | The key goes in the `X-API-Key` header. Robinhood support ⚠. |
| [ ] | **Open Exchange Rates** | https://openexchangerates.org/signup/free | `OPENEXCHANGERATES_APP_ID` | 1k req/month, hourly rates, USD base only | `fiat_get_fx_rate` fallback (Frankfurter is primary and keyless) | |

---

## Tier B: needed for Release 2
| ✓ | Vendor | Signup link | Env var(s) | Free tier | Powers | Setup notes |
|---|---|---|---|---|---|---|
| [ ] | **Zerion** | https://dashboard.zerion.io/ | `ZERION_API_KEY` | 2K req/day, 3 RPS; DeFi endpoints ≤25% of quota, no overage | `defi_get_positions`, `wallet_get_portfolio` | Supports Robinhood Chain. Cache aggressively. |
| [ ] | **The Graph** | https://thegraph.com/studio/ | `THEGRAPH_API_KEY` | 100K queries/month free, then $2 per 100K | `defi_lp_positions` (event history only) | |
| [ ] | **Coinbase CDP** | https://portal.cdp.coinbase.com/ | `CDP_API_KEY_ID` + `CDP_API_KEY_SECRET` | x402 facilitator: 1,000 tx/month free, then $0.001/tx; Onramp quote 10 req/s | `x402_settle` / `x402_supported`, `neobank_onramp_quotes` | The CDP facilitator covers Base, Polygon, Arbitrum, World, Solana (not Ethereum, OP, Avalanche, BSC). |
| [ ] | **Pimlico** | https://dashboard.pimlico.io/sign-in | `PIMLICO_API_KEY` | Free tier ⚠ | `tx_userop_status` (ERC-4337 bundler) | Docs: https://docs.pimlico.io/ |
| [ ] | **Pyth Benchmarks** | https://docs.pyth.network/price-feeds/core/use-historical-price-data | `PYTH_API_KEY` | 10 req/10s; key required since 2026-08-26 | `market_get_price_at` fallback | Hermes (live prices) stays keyless. |
| [ ] | **Safe Transaction Service** | https://developer.safe.global/ | `SAFE_API_KEY` | Keyless 2 req/s, 5k/month; a key raises limits | `safe_pending_txs` (P2) | https://docs.safe.global/core-api/how-to-use-api-keys |

---

## Keyless: nothing to create
| Vendor | Endpoint / docs | Rate limit | Powers |
|---|---|---|---|
| Public EVM RPCs | e.g. `https://mainnet.base.org`, `https://arb1.arbitrum.io/rpc`, `https://api.avax.network/ext/bc/C/rpc`, `https://bsc-dataseed.binance.org` | Varies; not for production | Last-resort `evm_rpc` |
| Robinhood Chain public RPC | `https://rpc.mainnet.chain.robinhood.com` (checked: `eth_chainId` = `0x1237` = 4663), WS `wss://feed.mainnet.chain.robinhood.com` | Rate-limited, not for production | Robinhood fallback RPC |
| Solana public RPC | `https://api.mainnet-beta.solana.com` | 100 req/10s per IP, 40 per method | Last-resort `solana_rpc` |
| DefiLlama | https://api-docs.defillama.com/ | Free, non-Pro endpoints only | Prices, yields (`/pools`), stablecoins, TVL |
| DexScreener | https://docs.dexscreener.com/api/reference | 300/min (pairs/search), 60/min (profiles/boosts) ⚠ | Price fallback, pools |
| GeckoTerminal | https://apiguide.geckoterminal.com/ | 10–30/min ⚠ | Price fallback, pools, OHLCV, trending |
| Velora (ParaSwap) | https://developers.velora.xyz/ | Anonymous use carries a 1 bps fee | Swap-quote fallback |
| CoW Protocol | https://docs.cow.fi/cow-protocol/reference/apis/orderbook | — | Swap quotes / intents (MEV-protected on L2s) |
| Flashbots Protect | https://docs.flashbots.net/flashbots-protect/quick-start | — | Private broadcast (Ethereum only) |
| MEV Blocker | https://docs.mevblocker.io/ | — | Private broadcast fallback (Ethereum only) |
| Jito block engine | https://solana.com/developers/cookbook/transactions/mev-protection | — | Solana bundles / tips |
| Helius Sender | https://www.helius.dev/docs/sending-transactions/sender | 50 TPS; tip ≥0.001 SOL (or ≥0.000005 SOL in SWQOS-only mode) | Solana broadcast |
| honeypot.is | https://docs.honeypot.is/ | — | `token_check_risk` (ETH/BSC/Base) |
| Chainalysis sanctions oracle | https://go.chainalysis.com/chainalysis-oracle-docs.html | On-chain call via our RPC (different address on Base; no Solana) | `compliance_screen_address` |
| Circle Iris (CCTP v2) | https://developers.circle.com/cctp/references/technical-guide | **40 req/s**; exceeding it blocks you for 5 min | `bridge_cctp_status/quote` (R2) |
| Frankfurter (ECB) | https://frankfurter.dev/ | Free; daily rates ~16:00 CET | `fiat_get_fx_rate` (primary) |
| Hyperliquid info API | https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/rate-limits-and-user-limits | Weighted (2 for `l2Book`/`allMids`, 20 for others) | `perps_get_markets` (R2) |
| Morpho GraphQL | https://docs.morpho.org/tools/offchain/api/get-started/ | 750 req/min, no SLA | `defi_vault_info`, lending (R2) |
| Lido / Jito stake APIs | https://docs.lido.fi/integrations/api/ · https://www.jito.network/docs/jitosol/jitosol-liquid-staking/for-developers/stake-pool-api/ | — | `defi_staking_rates` (P2) |
| Pyth Hermes | https://hermes.pyth.network/v2/price_feeds | — | Peg / price cross-check |

---

## Free tier unconfirmed: disabled by default
Enable these once you've confirmed the free tier. They're already wired as optional adapters.

| Vendor | Link | Env var(s) | What's unknown |
|---|---|---|---|
| Ankr Advanced API | https://www.ankr.com/rpc/ | `ANKR_API_KEY` | Free credit amount; 700 credits per call; **not on Robinhood Chain** |
| 0x Swap API | https://0x.org/docs/introduction/faq (dashboard `dashboard.0x.org` ⚠) | `ZEROEX_API_KEY` | Key required, pricing page lists only paid plans |
| Uniswap Trading API | https://developers.uniswap.org/ | `UNISWAP_API_KEY` | Key/free tier |
| OKX DEX API | https://web3.okx.com/build/dev-docs/wallet-api/dex-swap ⚠ (404 on check) | `OKX_API_KEY` + `OKX_SECRET_KEY` + `OKX_PASSPHRASE` | HMAC auth; free tier |
| RugCheck | https://api.rugcheck.xyz/swagger/index.html | `RUGCHECK_API_KEY` | Auth and limits (~60/min reported) |
| LI.FI | https://portal.li.fi/ ⚠ (redirects) | `LIFI_API_KEY` | Free-tier limits |
| Across | https://docs.across.to/ | — | Rate limits |

## Excluded: no usable free tier (possible future paid adapters)
- **Tenderly API:** the free tier has no API access.
- **DeBank Cloud:** prepaid units, 14-day trial only.
- **Nansen:** 1,000 trial credits.
- **Arkham:** enterprise.
- **Chainalysis KYT / Elliptic:** contract; Elliptic ~$50K/year.
- **Triton:** no free tier.
- **Goldsky:** paid.
- **Transak / MoonPay:** partner contract.
- **DefiLlama Pro:** $300/month (`poolsBorrow`, `lsdRates`, `chartLendBorrow`).
- **Helius paid methods:** `getTransfersByAddress`, `getTransactionsForAddress`.
- **Shut down / archived:** Odos (Jul 30 2026), GOAT SDK (archived).

---

## Default routing order (built-in; change it in the dashboard or `config.toml`)
First entry = primary, the rest are fallbacks in order. Vendors with no key or an exhausted quota are skipped automatically.

| Capability | Order |
|---|---|
| `evm_rpc` | alchemy → quicknode → public |
| `solana_rpc` | helius → quicknode → public |
| `price` | coingecko → defillama → geckoterminal → dexscreener |
| `price` on `solana:mainnet` | jupiter → birdeye → defillama |
| `token_risk` | goplus → honeypot_is → rugcheck |
| `swap_quote` | oneinch → velora → cow |
| `fx` | frankfurter → openexchangerates |

---

## Before running live smoke tests
From the Verification section of `plan/PLAN.md` (`cargo test -- --ignored`):

| Smoke test | Keys needed |
|---|---|
| Verify a USDC transfer on **Base** | `ALCHEMY_API_KEY` (or QuickNode) |
| Verify a USDC transfer on **Solana** | `HELIUS_API_KEY` (or QuickNode) |
| USDG balance on **Robinhood Chain** | `ALCHEMY_API_KEY` with Robinhood enabled (public RPC works as a fallback) |
| `rwa_price` (Chainlink equity feed) | Any Robinhood RPC (Alchemy or public) |
| Swap quote spread on Base | `ONEINCH_API_KEY` (Velora + CoW are keyless) |
| `token_check_risk` | `GOPLUS_APP_KEY` + `GOPLUS_APP_SECRET` |
| `compliance_screen_address` | `TRM_API_KEY` (the Chainalysis oracle needs only an RPC) |
| Price aggregation | `COINGECKO_API_KEY` (DefiLlama/GeckoTerminal keyless) |
| **Failover drill** (bad Alchemy key → next vendor, breaker opens in dashboard) | `ALCHEMY_API_KEY` **and** `QN_ENDPOINT_NAME`/`QN_TOKEN_ID` |
| Zero-key mode | none |
