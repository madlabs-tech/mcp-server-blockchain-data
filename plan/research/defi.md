# Agent DeFi research

_Date: 2026-09-23. Written by a web-research subagent. ⚠ / [UNVERIFIED] mark claims that could not be verified._

## Capabilities
| Tool | What it does | P | Why |
|---|---|---|---|
| `defi_positions` | Wallet → normalized positions (supply, borrow, stake, LP, rewards) across protocols and chains | P0 | Every earn/neobank balance screen starts here |
| `lending_health` | Health factor, LTV, liquidation price, distance-to-liquidation | P0 | The only signal that warns of losing funds; check before any borrow/withdraw |
| `yield_search` | Stablecoin pools: base vs reward APY, TVL, chain | P0 | Core of "earn" (best USDC rate) |
| `vault_info` | ERC-4626 / Morpho vaults / Kamino kVaults / sUSDS: live APY, share price, curator, allocations, withdrawable liquidity | P0 | What users actually deposit into; low liquidity = stuck withdrawals |
| `approvals` | ERC-20 + Permit2 allowances, spender labels, risk flags | P1 | Agent safety before/after txs |
| `protocol_risk` | TVL + trend, hacks, ratings | P1 | Filter risky yields before suggesting them |
| `staking_rates` | LST APR + exchange rate (stETH, rETH, JitoSOL, mSOL) | P1 | Non-stablecoin earn option; common loop collateral |
| `lp_positions` | Range, in-range status, uncollected fees | P2 | Mostly trading agents |
| `yield_history` | APY over time | P2 | Explains rate moves |

## Sources
| Capability | Primary | Fallback | Auth / free tier |
|---|---|---|---|
| defi_positions | **Zerion** (EVM + Solana in one schema; Robinhood Chain from day one) | DeBank Cloud (EVM); Moralis (EVM only, fewer protocols); Octav | Zerion: API key, free 2K req/day at 3 RPS; DeFi endpoints capped at 25% of quota, no overage. DeBank: AccessKey header, prepaid units, 14-day trial. Moralis free tier conflicting [UNVERIFIED]. Octav ~$0.02/credit |
| lending_health | AaveKit GraphQL (`api.v3.aave.com`, v3 + v4); Morpho GraphQL (`api.morpho.org/graphql`); Kamino REST (`api.kamino.finance`) | Direct reads: Aave/Spark `getUserAccountData`; Compound v3 `isLiquidatable` + `borrowBalanceOf`; Kamino obligation accounts | Aave, Kamino free/no key [UNVERIFIED]. Morpho 750 req/min, no SLA. MarginFi moved to Project 0; public API [UNVERIFIED] |
| yield_search, yield_history | DefiLlama `/pools`, `/chart/{pool}` | Protocol APIs above | DefiLlama free, no key. `poolsBorrow`, `lsdRates`, `chartLendBorrow` need Pro ($300/mo) |
| vault_info | Morpho API, Kamino API, AaveKit | ERC-4626 `convertToAssets` on-chain | As above |
| staking_rates | Lido `eth-api.lido.fi/v1/protocol/steth/apr/sma`; Jito `kobe.mainnet.jito.network/api/v1/stake_pool_stats` | Exchange-rate change on-chain; DefiLlama | Free. Marinade, Rocket Pool APIs [UNVERIFIED] |
| approvals | Moralis `/wallets/{addr}/approvals` | Scan `Approval` logs + Permit2 events, then `allowance()` (how revoke.cash works); GoPlus approval risk | revoke.cash is open source, no public API found |
| lp_positions | Meteora `dlmm.datapi.meteora.ag` (no key, 30 RPS); Aerodrome LpSugar contract | Uniswap subgraphs; Orca `orca_whirlpools` Rust crate; Zerion | |
| protocol_risk | DefiLlama TVL | Credora, Exponential, Sentora | Rating APIs' terms [UNVERIFIED] |

## The Graph vs vendor APIs vs direct on-chain reads
- **Vendor:** `defi_positions`. Nobody can maintain thousands of protocol adapters in-house.
- **On-chain is the source of truth:** `lending_health` and vault APY/liquidity before a deposit. Subgraphs and vendors lag, and a stale number here costs money.
- **The Graph / Goldsky:** only where data comes from past events and can't be read from current state: finding Uniswap v4 positions (PositionManager can't list a wallet's positions), per-user history and P&L, full-chain approval scans.
  - The Graph: $2 per 100K queries after 100K free/month (fits BYO key).
  - Goldsky Mirror (~$0.16/hour + $1.50/GB) streams into your own Postgres.
  - Never use subgraphs for liquidation-critical reads.
- **DefiLlama:** yield discovery and TVL. Always re-check the vault on-chain before recommending it.

## Pitfalls
- **Health factors mean different things.** Aave has HF (v4 has a target HF per spoke); Compound v3 has no HF, only liquidity + `isLiquidatable`; Kamino combines LTV with a borrow factor; Morpho markets each have their own LLTV. Normalize to "% to liquidation" and keep the native value.
- **APY semantics differ.** Separate base and reward APY. Lido = 7-day average; Marinade = 30-day average of 14-day APY. DefiLlama's `stablecoin` flag says nothing about risk.
- **Don't mix price sources in one response.**
- **Zerion's DeFi quota can't go over its cap.** Cache aggressively. Morpho's API has no SLA.
- **Robinhood Chain** (live 2026-07-01): DeBank/Moralis support [UNVERIFIED].
- **Permit2 has two layers:** ERC-20 approval to Permit2, then a per-spender allowance with expiry. Signed permits are invisible until used.
- **Solana position discovery needs `getProgramAccounts`,** which many RPCs throttle.
- **Uniswap v4 hooks** can make fee/value calculations non-standard.

## Existing DeFi MCPs and agent kits
Mostly single-protocol or write-oriented. Our gap: normalized, read-only, cross-protocol data with failover.

| Kit | Exposes |
|---|---|
| Aave MCP (`mcp.aave.com`, launched 2026-09-09) | ~40 tools: market data, positions/HF, risk simulation, tx prep. v3 on 21 chains, v4 on Ethereum + Avalanche. Auth [UNVERIFIED] |
| Morpho Agents (beta) | CLI + MCP, 17 read/simulate/write tools, Ethereum + Base |
| DefiLlama MCP | 23 tools; premium subscription |
| Zerion | Hosted MCP + CLI (alpha) |
| The Graph | Subgraph MCP + Token API MCP |
| GOAT | 200+ protocol integrations (repo now archived; see trading.md) |
| Solana Agent Kit v2 | Plugin-based, 60+ actions |
| Coinbase AgentKit | Action providers incl. Morpho, Compound, Moonwell |
| Sugar | CLI + Claude skill for Aerodrome/Velodrome |

## Sources
- https://zerion.io/api/ · https://zerion.io/blog/zerion-api-supports-robinhood-chain-from-day-one/ · https://github.com/zeriontech/zerion-ai
- https://docs.cloud.debank.com/en/readme/api-pro-reference · https://docs.moralis.com/data-api/evm/defi/overview · https://docs.moralis.com/web3-data-api/evm/reference/get-wallet-token-approvals · https://octav.fi/api
- https://api-docs.defillama.com/ · https://defillama.com/mcp
- https://aave.com/docs/aave-v3/getting-started/graphql · https://aave.com/docs/aave-v4/getting-started/graphql · https://cryptobriefing.com/aave-mcp-server-ai-agents/ · https://www.theblock.co/post/395617/aave-v4-launches-ethereum-mainnet
- https://docs.morpho.org/tools/offchain/api/get-started/ · https://morpho.org/blog/introducing-morpho-agents-beta-interface-built-for-ai-agents/
- https://api.kamino.finance/documentation/ · https://docs.compound.finance/liquidation/
- https://docs.lido.fi/integrations/api/ · https://www.jito.network/docs/jitosol/jitosol-liquid-staking/for-developers/stake-pool-api/
- https://docs.uniswap.org/sdk/v4/guides/liquidity/position-fetching · https://github.com/velodrome-finance/sugar · https://meteora.mintlify.app/api-reference/dlmm/overview · https://docs.rs/orca_whirlpools
- https://github.com/RevokeCash/revoke.cash · https://gopluslabs.io/approval-security-api
- https://thegraph.com/studio-pricing/ · https://thegraph.com/blog/querying-blockchain-data-natural-language-mcp-skills/ · https://docs.goldsky.com/pricing/summary
- https://www.credora.network/ · https://app.marginfi.com/ · https://github.com/coinbase/agentkit · https://kit.sendai.fun/

## Impact on our plan
- DeFi tools are R2 (P1). `defi_get_positions` goes Zerion (free 2k/day) → Moralis → on-chain readers. DeBank is excluded (prepaid).
- `defi_lending_health` and vault checks are **read on-chain** (protocol readers in `protocols`), normalized to "% to liquidation" with the native value kept.
- The Graph is an optional adapter only for event-history needs (`defi_lp_positions`), using its 100K free queries/month. Goldsky is excluded (paid).
- `defi_earn_rates` (neobank "earn") = DefiLlama `/pools` + on-chain ERC-4626 re-check. DefiLlama Pro endpoints are excluded.
- Zerion's hard DeFi quota cap → aggressive caching plus the quota guard.
