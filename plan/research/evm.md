# EVM research

_Date: 2026-09-23. Written by a web-research subagent. ⚠ / [U] mark claims that could not be verified._

## Two findings that change the design
- **`alchemy_simulateAssetChanges` shuts down on 2026-09-30.**
- **Free-tier `eth_getLogs` allows almost no block range:** Alchemy 10 blocks, QuickNode 5.

**Chain IDs:** Ethereum 1, Base 8453, Arbitrum 42161, Optimism 10, Polygon 137, Avalanche 43114, BSC 56, Robinhood Chain 4663 (testnet 46630).

## Capabilities, sources, auth
| Tool | Pri | What it does (why) | Primary → fallback |
|---|---|---|---|
| `evm_verify_payment` | P0 | Checks a tx/log: correct token, amount and recipient, status==1, finality level | Receipt + decoded `Transfer` log + `finalized` tag over any RPC, checked by a 2nd vendor |
| `evm_watch_deposits` | P0 | Detects incoming transfers to addresses | Alchemy Address Activity webhooks / QuickNode Webhooks or Streams → own chunked `eth_getLogs` poller |
| `evm_get_balances` | P0 | Native + ERC-20 balances | Multicall3 `aggregate3` at `0xcA11…CA11` (also on Robinhood) → Alchemy Portfolio/Token API → Ankr `ankr_getAccountBalance` → Moralis |
| `evm_estimate_fees` | P0 | Total fee including the L1 data part | `eth_feeHistory`; OP-stack `GasPriceOracle.getL1Fee` + `getOperatorFee`; Arbitrum `NodeInterface.gasEstimateL1Component` |
| `evm_build_tx` / `evm_broadcast` | P0 | Builds unsigned EIP-1559 tx; sends signed raw tx to several vendors | Any RPC. Duplicate sends are safe because the tx hash is the same |
| `evm_simulate_tx` | P0 | Shows effects before signing | `eth_simulateV1` (Alchemy, QuickNode) → `debug_traceCall` callTracer → Tenderly simulate-bundle |
| `evm_get_transfers` | P1 | Wallet history, including internal ETH | `alchemy_getAssetTransfers` → Ankr `ankr_getTokenTransfers` → `eth_getLogs` |
| `evm_token_price` / `_metadata` | P1 | USD value, decimals | Alchemy Prices API → Ankr `ankr_getTokenPrice` → Moralis; decimals via Multicall |
| `evm_resolve_name` | P1 | ENS / Basenames, both directions | ENS Universal Resolver + CCIP-Read; ENSIP-19 for Base primary names |
| `evm_userop_status` | P1 | `eth_getUserOperationReceipt`, send/estimate userOps | Pimlico → Alchemy bundler |
| `evm_safe_pending` | P2 | Pending multisig txs and signatures | Safe Transaction Service |
| `evm_get_logs` | P2 | Raw log queries | Any RPC |

**Auth and free tiers**
- **Alchemy:** key in the URL path. 30M compute units/month free. Free `eth_getLogs` is capped at 10 blocks. Pay-as-you-go allows unlimited range on major chains, with a 150 MB response cap.
- **QuickNode:** token in the endpoint URL. 10M credits and 15 RPS free per its pricing page (a third-party site says 50M ⚠). `eth_getLogs` capped at 5 blocks free, 10k paid. Each webhook delivery costs 30 credits.
- **Ankr Advanced API:** key in the URL. Every call costs 700 credits. Does not support Robinhood Chain.
- **Moralis:** `X-API-Key` header. 40k compute units/day free.
- **Tenderly:** the free tier has no API access.
- **Safe Transaction Service:** `Authorization` header. Without a key: 2 req/s and 5k/month.

**Vendor coverage:** Alchemy and QuickNode cover all 8 chains. Ankr covers all except Robinhood.

## Correctness pitfalls
- **Finality differs by chain; use the tag, not a confirmation count:**
  - OP-stack and Base: `latest` is the sequencer head and can be reorged. `safe` = batch on L1 (~5–10 min). `finalized` ≈ 15–30 min.
  - Arbitrum and Robinhood: similar. Block numbers are not a proxy for time.
  - Polygon: finality in 2–5 s since Heimdall v2 (Jul 2025); old "wait 128 blocks" advice is out of date.
  - BSC: 0.45 s blocks and ~1.1 s finality since Fermi (Jan 2026).
  - Avalanche: finality in under a second.
- **Load-balanced RPCs can lag at the chain head.** `getLogs` can return empty and deposits get missed silently. Cap `toBlock` at the head the same node reports. Store and re-check the block hash.
- **Reorgs:** handle `removed:true` logs. Webhooks are at-least-once and can have gaps; backfill by polling.
- **A `Transfer` log is not proof of payment.** Anyone can deploy a fake token that emits one, so allowlist canonical stablecoin contracts. Watch for fee-on-transfer and rebasing tokens. Internal ETH transfers emit no log and need traces.
- **Blocklists and freezes:** USDC and USDT can block addresses. Robinhood stock tokens have per-address blocklists, a global pause, and active transaction filtering (blog post [U]).
- **Fees:** `feeHistory` leaves out the L1 data fee. On Arbitrum, `eth_estimateGas` already includes the L1 part, so it moves with L1 prices.
- **Multicall3:** `msg.sender` is the Multicall contract, so caller-dependent reads are wrong. Pin all calls to one block.
- **ENS:** always forward-check a reverse lookup. Turn on CCIP-Read. ENSv2 is moving to L1 (Namechain L2 dropped Feb 2026), so resolver addresses will change; hardcode only the Universal Resolver.
- **ERC-4337:** a userOp hash is not the tx hash. The receipt only exists after bundling.
- **Chain ID:** assert `eth_chainId` for every adapter at startup.

## Differentiation
- One finality model across chains: `pending / confirmed(n) / safe / finalized`, returned with the block hash as proof.
- Quorum payment verification: two vendors must agree on the receipt's block hash.
- `getLogs` chunking that adapts to each vendor's plan limits and splits a range on failure.
- One fee quote per chain covering L2 execution, L1 data and operator fees, in USD.
- Simulation behind a port, so vendor shutdowns don't break callers.
- A canonical stablecoin registry per chain, to catch spoofed tokens.
- Routing that accounts for each vendor's credit cost per method.

## Shut down or retired; unverified
- **Retired:** `alchemy_simulateAssetChanges` on 2026-09-30, no replacement named. Some Alchemy NFT endpoints also end 2026-09-30. QuickNode QuickAlerts ended 2025-07-31 (replaced by Webhooks).
- **Unverified:** QuickNode free credits (10M vs 50M); Moralis/Tenderly/Safe support for Robinhood; Robinhood mainnet date (reported 2026-07-01); Alchemy simulation chain coverage; Arbitrum ignoring priority fee (memory); EntryPoint v0.8/v0.9 addresses.

## Sources
- https://www.alchemy.com/docs/reference/eth-getlogs · https://www.alchemy.com/docs/reference/simulation-asset-changes · https://www.alchemy.com/docs/changelog · https://www.alchemy.com/pricing · https://www.alchemy.com/docs/data/portfolio-apis/portfolio-api-endpoints/portfolio-api-endpoints/get-tokens-by-address · https://www.alchemy.com/docs/robinhood-chain/robinhood-chain-api-overview · https://www.alchemy.com/docs/reference/eth_simulatev1
- https://www.quicknode.com/docs/ethereum/eth_getLogs · https://www.quicknode.com/pricing · https://www.quicknode.com/docs/changelog · https://www.quicknode.com/docs/ethereum/eth_simulateV1
- https://www.ankr.com/docs/advanced-api/overview/ · https://docs.moralis.com/get-started/pricing · https://docs.tenderly.co/simulations/asset-balance-changes · https://docs.tenderly.co/faq/simulations
- https://docs.robinhood.com/chain/connecting · https://xroot.dev/blog/robinhood-chain-read-directly
- https://docs.optimism.io/op-stack/transactions/fees · https://docs.optimism.io/op-stack/transactions/transaction-finality · https://docs.arbitrum.io/build-decentralized-apps/how-to-estimate-gas · https://docs.arbitrum.io/arbitrum-ethereum-differences
- https://docs.polygon.technology/pos/concepts/finality/finality · https://docs.bnbchain.org/announce/fermi-bsc/ · https://build.avax.network/docs/rpcs/c-chain/eth/eth_getBlockByNumber
- https://github.com/mds1/multicall3 · https://docs.ens.domains/ensip/19/ · https://blog.base.dev/basenames-ensip-19 · https://www.theblock.co/post/388932/ens-labs-scraps-namechain-l2-shifts-ensv2-fully-ethereum-mainnet
- https://docs.pimlico.io/references/bundler/usage · https://docs.safe.global/core-api/transaction-service-overview · https://docs.safe.global/core-api/how-to-use-api-keys
- https://github.com/fystack/multichain-indexer/issues/114

## Impact on our plan
- `simulateAssetChanges` retired 2026-09-30 → `tx_simulate` uses `eth_simulateV1` → `debug_traceCall` → `eth_call` behind the `Simulator` port. Tenderly is excluded (no free API).
- Free-tier `getLogs` caps of 10 and 5 blocks → the router keeps a plan limit per vendor and chunks ranges adaptively. Enhanced transfer APIs come first.
- Finality differs per chain → `chains.toml` finality policy, using `safe`/`finalized` tags. `payments_verify_transfer` supports quorum on block hash.
- `tx_estimate_fee` adds the OP/Arbitrum L1 data fee. `eth_chainId` is asserted at adapter startup.
- Ankr is not on Robinhood → the registry marks per-chain vendor support, and the routing filter skips it there.
