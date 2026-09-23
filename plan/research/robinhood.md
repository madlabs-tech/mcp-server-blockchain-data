# Robinhood Chain & tokenized stocks research

_Date: 2026-09-23. Written by a web-research subagent. ⚠ / [U] mark claims that could not be verified._

## Robinhood Chain facts
| Fact | Confidence |
|---|---|
| Mainnet live **2026-07-01**, after a public testnet from ~Feb 2026 | High (Robinhood, Arbitrum, The Block) |
| Arbitrum Orbit L2 settling to Ethereum; ~100 ms blocks; gas paid in ETH | High |
| Chain IDs: mainnet **4663**, testnet **46630** | High (Robinhood docs, QuickNode, Goldsky, chainlist) |
| Public RPC `https://rpc.mainnet.chain.robinhood.com` / `https://rpc.testnet.chain.robinhood.com`; WS `wss://feed.{mainnet,testnet}.chain.robinhood.com`. Rate-limited, not for production | High |
| Explorer: `robinhoodchain.blockscout.com` (mainnet), `explorer.testnet.chain.robinhood.com` (testnet); Goldsky lists `robinscan.io` instead | Medium (sources disagree) |
| Bridges: Arbitrum canonical (7-day withdrawal), Stargate, Chainlink CCIP/Transporter, Relay, Across, LiFi | High |
| The sequencer drops txs linked to sanctioned addresses | Medium (QuickNode only) |
| Infra vendors named in Robinhood docs: Alchemy (recommended, `robinhood-mainnet.g.alchemy.com`), QuickNode (archive), Chainstack, Blockdaemon, dRPC, Validation Cloud, GlobalStake. **No evidence of Ankr support** | High |
| Indexers: Goldsky (Graph-compatible subgraphs, pipelines, RPC), Envio (HyperSync/HyperIndex), Allium (incl. RWA pricing), Dune, SQD (private enterprise dataset) | High |

## Two generations of stock tokens
- **Classic Stock Tokens** (Arbitrum One, Jun 2025, 213 contracts). EU-only, roughly 24/5 trading, transfers only between allowlisted wallets, no withdrawal, derivatives with Robinhood Europe. Confidence: medium-high.
- **New Stock Tokens** (Robinhood Chain). ERC-20 with 18 decimals plus **ERC-8056** ("UI multiplier"). Issued by Robinhood Assets (Jersey) Ltd; legally a debt instrument. Trade 24/7 on DEXs, self-custodiable, offered in 120+ countries but **not** US, UK, Canada, Switzerland or UAE. No documented on-chain allowlist. Confidence: high.
- **Corporate actions:** splits and reinvested dividends change `uiMultiplier()` (1e18 = 1.0); raw balances don't change. A pending change is visible via `newUIMultiplier()` and `effectiveAt()`. Events: `UIMultiplierUpdated`, `TransferWithScaledUI`. Confidence: high.
- **Oracle:** one Chainlink feed per token (8 decimals). The price already includes the multiplier. Updates **24/5**, not 24/7. `oraclePaused()` is true during corporate actions. Confidence: high.
- Classic tokens expected to migrate to Robinhood Chain [U].
- **Main stablecoin on Robinhood Chain is USDG** (see stablecoin.md).

## Capabilities to add
| Tool | P | What it does |
|---|---|---|
| `rwa_token_info` | P0 | Issuer, underlying ticker, standard, current and pending multiplier + effective time, `oraclePaused`, whether the address is on the issuer's official list. Robinhood says a token with the right ticker but a different address is not a Robinhood Stock Token |
| `rwa_balance` | P0 | Raw balance + share-equivalent (`balanceOfUI`, or raw × multiplier) |
| `rwa_price` | P0 | Chainlink `latestRoundData`, `updatedAt`, staleness flag, market session (regular/pre/post/overnight/closed with exchange-holiday calendar), `oraclePaused`, sequencer uptime |
| `rwa_corporate_actions` | P1 | `UIMultiplierUpdated` history + scheduled changes |
| `rwa_transfer_check` | P1 | Transfer eligibility via `eth_call` simulation + a per-issuer rules table (allowlists, Solana transfer hooks) |
| `rwa_trades` | P1 | Find real trades by grouping stock-token and USDG transfers that share a tx hash, plus DEX swaps |
| `rwa_find_by_underlying` | P2 | E.g. TSLA across Robinhood, xStocks, Ondo, Dinari |
| `rwa_premium` | P2 | DEX price vs oracle price |

## Data sources
- Chainlink tokenized-equity feeds.
- Robinhood docs token table (generated from an on-chain asset registry; registry address not found).
- `GET api.robinhood.com/rhj/assets` with `tradingCapabilities` per docs; fetch failed [U].
- Indexers: Allium, Goldsky, Envio, SQD.
- Other issuers:
  - **xStocks (Backed/Kraken):** Ethereum, Solana, Arbitrum, Mantle, TON, Ink. Permissionless; dividends/splits applied by **rebasing**.
  - **Ondo Global Markets:** Ethereum, BNB, Solana. Chainlink; Solana transfer hooks for compliance.
  - **Dinari dShares:** Arbitrum (main), Ethereum, Base, Avalanche. On-chain KYC allowlist.
- Pyth equity feeds on Robinhood Chain [U].

## Pitfalls
- **Stale prices outside market hours:** tokens trade 24/7 but feeds hold the last price on weekends/holidays with no heartbeat. Reject stale prices by market session, not a fixed timeout.
- **`oraclePaused` during corporate actions** → treat as "no price".
- **Raw balance ≠ share count:** multiply by the multiplier on Robinhood tokens; xStocks rebase instead.
- **Restricted transfers:** classic Robinhood tokens are allowlist-only; Dinari uses a KYC allowlist; Ondo on Solana uses transfer hooks; Robinhood Chain filters sanctioned addresses at the sequencer. Geo-eligibility is enforced off-chain, so it's the agent's job.
- **Fake tokens:** always check against the issuer's official list.
- **Topic collisions:** ERC-721 `Transfer` shares topic0 with ERC-20; drop 4-topic logs.
- **Not every transfer is a trade:** a `Transfer` alone can be an internal sweep.
- **Decimals differ:** stock tokens 18, USDG 6.
- **Canonical bridge exits take 7 days.**

## Sources
- https://docs.robinhood.com/chain/connecting · https://docs.robinhood.com/chain/stock-tokens/ · https://docs.robinhood.com/chain/building-with-stock-tokens/ · https://docs.robinhood.com/chain/oracles-and-price-feeds · https://docs.robinhood.com/chain/bridging · https://docs.robinhood.com/chain/contracts
- https://docs.chain.link/data-feeds/tokenized-equity-feeds/robinhood
- https://forum.arbitrum.foundation/t/arbitrumdao-factsheet-robinhood-chain-mainnet-launch/31041 · https://blog.arbitrum.io/robinhood-chain-mainnet/
- https://www.theblock.co/news/business/2026-07-01-robinhood-chain-goes-live-mainnet-alongside-24-7-tokenized-stocks-lighter-perps-planned-crypto-agentic-trading-406918
- https://www.quicknode.com/guides/robinhood/what-is-robinhood-chain
- https://sqd.dev/learn/robinhood-tokenized-stocks/ (its token addresses are **unverified**, e.g. TSLA `0x322F…2b3d`; Blockscout returned 403)
- https://docs.goldsky.com/chains/robinhood-chain · https://envio.dev/chains/robinhood · https://www.allium.so/blog/supporting-robinhood-chain-at-launch/ · https://dune.com/blockchains/robinhood
- https://docs.xstocks.fi/docs · https://en.cryptonomist.ch/2026/09/22/tokenized-stocks-access-ondo/ · https://www.coindesk.com/business/2026/08/04/dinari-brings-tokenized-u-s-stocks-to-american-investors-as-equity-race-heats-up · https://eco.com/support/en/articles/15083159-dinari-dshares-tokenized-equities (secondary)

## Impact on our plan
- Robinhood Chain is config only: `eip155:4663` in `chains.toml`, with public RPC + Alchemy + QuickNode. Ankr is marked unsupported.
- The ERC-8056 multiplier reader lives in `protocols`. `wallet_get_balances` returns a share-equivalent amount for these tokens.
- `rwa_price` staleness is judged by market session with a holiday calendar; `oraclePaused` → no price.
- `registry/rwa.toml` holds issuer official lists; any token not listed is flagged as a lookalike. Token addresses are only taken from issuer docs.
- R1 tools: `rwa_token_info`, `rwa_price`. The rest are R2/P2.
