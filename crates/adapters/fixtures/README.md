# Adapter fixtures

Loaded by `bdm_testkit::vendor_fixture(env!("CARGO_MANIFEST_DIR"), "<vendor>", "<case>")` in each
vendor module's tests (served through wiremock; tests never touch the network).

## Rules

- Path: `fixtures/<vendor>/<case>.json`.
- Strip API keys, key-bearing URLs, emails, IPs and any other PII before committing.
- Keep fixtures minimal: trim arrays to the entries the test asserts on.
- When a vendor changes its schema, add a new case instead of editing the old one.

## Provenance

The market-trading fixtures (`coingecko`, `geckoterminal`, `defillama`, `dexscreener`, `birdeye`,
`pyth`, `goplus`, `honeypot_is`, `rugcheck`, `oneinch`, `velora`, `cow`, `zeroex`, `uniswap_api`,
`okx_dex`) are **recorded-shape**, not live recordings. They were written on 2026-09-23 from each
vendor's documented response schema (field names, nesting, string-vs-number types), trimmed to the
fields the adapters read. The numbers are illustrative; token and contract addresses are real
public addresses (USDC, USDT, wSOL, Robinhood TSLA, 1inch/Velora routers) or obvious placeholders
(`0x…0001`).

Schema sources (checked 2026-09-23):

| Vendor | Docs |
|---|---|
| coingecko | https://docs.coingecko.com/demo/reference/onchain-simple-price , `/simple/price`, `/coins/{id}/market_chart/range`, `/onchain/networks/{network}/tokens/{address}` |
| geckoterminal | https://apiguide.geckoterminal.com/ (same on-chain schema as CoinGecko) |
| defillama | https://api-docs.defillama.com/ (`coins.llama.fi/prices/current`, `/prices/historical`) |
| dexscreener | https://docs.dexscreener.com/api/reference (`/tokens/v1/{chainId}/{addresses}`) |
| birdeye | https://docs.birdeye.so/ (`/defi/price`, `/defi/history_price`) |
| pyth | https://docs.pyth.network/ (Hermes `/v2/price_feeds`, `/v2/updates/price/latest`) |
| goplus | https://docs.gopluslabs.io/reference/api-overview (`/api/v1/token`, `/api/v1/token_security/{chain_id}`) |
| honeypot_is | https://docs.honeypot.is/ishoneypot |
| rugcheck | https://api.rugcheck.xyz/swagger/index.html (`/v1/tokens/{mint}/report/summary`) |
| oneinch | https://business.1inch.com/portal/documentation/apis/swap/classic-swap/quick-start |
| velora | https://velora.xyz/docs/api/velora-api/velora-market-api/get-rate-for-a-token-pair |
| cow | https://github.com/cowprotocol/services/blob/main/crates/orderbook/openapi.yml |
| zeroex | https://docs.0x.org/api-reference/evm-ap-is/swap/allowanceholder-getquote.md |
| uniswap_api | https://developers.uniswap.org/docs/trading/swapping-api (`/v1/quote`) |
| okx_dex | https://web3.okx.com/onchainos/dev-docs (aggregator v6 `/quote`) |

When a real recording replaces one of these, keep the file name, strip keys/PII, and note the
recording date here.
