# Agent trading research

_Date: 2026-09-23. Written by a web-research subagent. ⚠ / [U] mark claims that could not be verified (search budget ran out for some items)._

## Four findings that change the design
- **Odos is gone:** all services shut down 30 Jul 2026. Drop it.
- **GOAT SDK repo is archived** (read-only). Don't depend on it.
- **Flashbots Protect and MEV Blocker only cover Ethereum mainnet.** On Base, Arbitrum, Optimism and Polygon, slippage limits and intent-based orders (CoW) are the only protection.
- **Robinhood Chain** (4663, mainnet since 2026-07-01) is supported from day one by 0x and the Uniswap API; 1inch has deployed contracts there.

## Capabilities (primary → fallback)
| Tool | What it does | P | Primary → fallback | Why |
|---|---|---|---|---|
| `get_token_price` | USD price + source, timestamp, liquidity | P0 | EVM: Alchemy Prices → CoinGecko onchain; SOL: Jupiter Price v3 → Birdeye | Every trade decision |
| `get_token_metadata` | Decimals, symbol, verified flag | P0 | On-chain (`decimals()` / mint) → CoinGecko/Jupiter Tokens | Wrong decimals lose money |
| `check_token_risk` | Honeypot, tax, mint/freeze, owner flags → one verdict | P0 | EVM: GoPlus → honeypot.is (ETH/BSC/Base); SOL: RugCheck → GoPlus Solana | Stops scam buys |
| `get_swap_quote` | Route, minOut, price impact, TTL | P0 | EVM: 0x → 1inch → Velora (keyless) → Uniswap API; SOL: Jupiter → OKX DEX | Core trade path |
| `build_swap_tx` | Unsigned tx + needed approvals (AllowanceHolder/Permit2) | P0 | Same vendor as the quote | Non-custodial core |
| `estimate_fees` | EIP-1559; SOL priority fee + Jito tip | P0 | `eth_feeHistory` → 1inch Gas API; Helius `getPriorityFeeEstimate` → ⚠ | Txs must land |
| `broadcast_protected` | Private route per chain | P0 | ETH: Flashbots → MEV Blocker; SOL: Jito → Helius Sender; others: plain RPC + `no_private_mempool` flag | Sandwich protection |
| `simulate_tx` | Dry-run before signing | P1 | `eth_call`/`simulateTransaction` via RPC → Alchemy | Catches honeypots/reverts |
| `get_pools` | Pools, TVL, 24h volume | P1 | DexScreener (keyless) → GeckoTerminal | Liquidity check |
| `get_ohlcv` | Candles | P1 | GeckoTerminal/CoinGecko onchain → Birdeye/Moralis; Hyperliquid `candleSnapshot` | TA agents |
| `get_trending` / `get_new_pairs` | Discovery | P1 | GeckoTerminal `trending_pools`/`new_pools` → Birdeye, DexScreener | Memecoin agents (high risk) |
| `get_holders` | Top holders, concentration % | P1 | Moralis → Birdeye/Bitquery | Rug signal |
| `get_perp_markets` | Mark/oracle, funding, OI, book | P1 | Hyperliquid info API (keyless) → ⚠ Drift | Perps agents |
| `build_perp_order` | Hyperliquid EIP-712 action; user approves an agent wallet (`approveAgent`) | P2 | Hyperliquid exchange API | Non-custodial fit, complex signing |
| `create_limit_order` | Unsigned limit order / intent | P2 | SOL: Jupiter Trigger (min $10) → Manifest; EVM: CoW (EIP-712, no key) → 1inch LOP | Nice to have |
| `track_smart_money` | Labeled whale flows | P2 | Nansen (paid) → Arkham Intel API → Bitquery labels | Paid only |

## Keys and free tiers
- **No key:** DexScreener (300 req/min pair/token/search, 60 on profiles/boosts ⚠), Velora (anonymous = 1 bps fee), CoW, Hyperliquid info (weight 2 for `l2Book`/`allMids`, 20 for others), honeypot.is, Flashbots, MEV Blocker.
- **Key, free tier:** Jupiter (60 req/min), 1inch (1 req/s, 100k/month), GoPlus (30/min; MCP server needs key + secret), Birdeye (30K CU/month, 1 req/s), CoinGecko Demo (10k/month; rate 30 vs 100/min ⚠), Bitquery (1,000 points), Moralis (CU).
- **Key, free tier unclear:** 0x (key required, pricing lists only paid ⚠), OKX DEX (HMAC key + secret + passphrase ⚠).
- **Paid:** Nansen (1,000 trial credits), Arkham (enterprise ⚠).

## What existing toolkits cover
- **Solana Agent Kit v2:** Jupiter, Manifest limit orders, Drift/Adrena perps, Jito, Pyth, Birdeye, CoinGecko, RugCheck.
- **Coinbase AgentKit:** swaps via CDP Swap, 0x, Sushi, Enso, Jupiter; prices via Pyth + DefiLlama; no perps.
- **Alchemy MCP:** 168 tools incl. current/historical prices and simulation.
- **Hyperliquid MCPs:** community-built, read-only.

## Pitfalls
- **Stale prices:** return `as_of` + `source`; reject old ones. Jupiter v3 omits tokens failing checks or untraded for 7 days, so a missing price = "unknown", never 0.
- **Thin-liquidity manipulation:** Jupiter prices come from the last swap. Require minimum liquidity, cross-check ≥2 sources, flag disagreement. DexScreener boosts are paid ads.
- **Quote expiry:** 0x `/price` is indicative, only `/quote` is firm. Enforce TTL; re-quote before build; watch Solana blockhash lifetime and CoW `validTo`.
- **Slippage:** always put minOut in the tx; cap slippage by liquidity tier. On L2s it's the only protection.
- **Decimals:** read on-chain; amounts as base-unit strings (U256/u128), never floats. BSC stablecoins use 18. Native-token placeholder differs by vendor (`0xEeee…`, `111…1` for OKX Solana, wSOL for Jupiter).
- **Rate limits:** per-vendor limits + caching needed; 1 req/s tiers are common.

## Gaps and differentiation
- Parallel quotes from 2–4 vendors, return best + spread (a wide spread is itself a manipulation signal).
- One risk verdict from GoPlus, RugCheck and honeypot.is, showing which source said what.
- Every response records vendor, time, latency, and whether a fallback was used.
- MEV-aware broadcast routing that's explicit about chains with no protection.
- A usable zero-key baseline: DexScreener, GeckoTerminal, Velora, CoW, Hyperliquid, honeypot.is, MEV Blocker.
- Robinhood Chain is thinly covered: CoinGecko, DexScreener and GoPlus coverage ⚠. Bitquery lists it.

⚠ **Not verified:** QuickNode MCP; BSC private-tx relays; Solana priority-fee fallback; 1inch API on Robinhood; RugCheck key/limits; GeckoTerminal keyless rate (10 vs 30/min).

## Sources
- https://github.com/goat-sdk/goat · https://crypto.news/odos-shuts-down-july-30-as-defi-aggregator-ends-all-services/
- https://docs.flashbots.net/flashbots-protect/quick-start · https://docs.mevblocker.io/ · https://solana.com/developers/cookbook/transactions/mev-protection
- https://developers.jup.ag/docs/price · https://developers.jup.ag/docs/portal/rate-limits · https://dev.jup.ag/docs/trigger/create-order
- https://0x.org/docs/introduction/faq · https://0x.org/post/robinhood-chain · https://0x.org/pricing
- https://help.1inch.com/en/articles/15618946-robinhood-chain · https://business.1inch.com/pricing
- https://developers.velora.xyz/api/velora-api/velora-market-api/master/api-v6.2 · https://docs.cow.fi/cow-protocol/reference/apis/orderbook · https://web3.okx.com/build/dev-docs/wallet-api/dex-swap · https://developers.uniswap.org/docs/trading/swapping-api/supported-chains
- https://github.com/coinbase/agentkit/blob/main/typescript/agentkit/README.md · https://github.com/sendaifun/solana-agent-kit · https://www.alchemy.com/docs/alchemy-mcp-server
- https://www.helius.dev/docs/priority-fee-api · https://docs.coingecko.com/ai-integration/mcp-server · https://apiguide.geckoterminal.com/ · https://docs.dexscreener.com/api/reference
- https://bds-support.birdeye.so/hc/en-us/articles/48250867942425 · https://docs.moralis.com/web3-data-api/evm/reference/get-tokens-with-top-gainers · https://docs.bitquery.io/docs/mcp/mcp-server/
- https://docs.nansen.ai/getting-started/credits · https://arkm.com/api
- https://docs.gopluslabs.io/reference/api-overview · https://github.com/GoPlusSecurity/goplus-mcp · https://docs.honeypot.is/
- https://github.com/sendaifun/solana-agent-kit/pull/90 · https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/rate-limits-and-user-limits · https://chainstack.com/hyperliquid-agent-wallets-nonce-state-machine/ · https://github.com/kukapay/hyperliquid-info-mcp

## Impact on our plan
- Odos and GOAT are excluded. The default free `swap_quote` order is 1inch → Velora → CoW (+ Jupiter on Solana); 0x/Uniswap/OKX are optional until their free tiers are confirmed.
- `trade_get_swap_quote` uses the Aggregate strategy (parallel, best + spread). `trade_build_swap_tx` re-quotes and enforces TTL + minOut.
- `tx_broadcast` is MEV-aware per chain (Flashbots/MEV Blocker only on Ethereum) and returns an explicit `no_private_mempool` flag elsewhere.
- `token_check_risk` merges GoPlus + honeypot.is + RugCheck + on-chain authorities. A missing price is `unknown`.
- Per-vendor token buckets + a shared cache are required, because 1 req/s free tiers are common.
