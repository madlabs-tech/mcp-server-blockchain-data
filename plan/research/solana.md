# Solana research

_Date: 2026-09-23. Written by a web-research subagent. ⚠ / [U] mark claims that could not be verified._

## Four findings that change the design
- **Alpenglow mainnet activation starts 2026-09-28** (Agave 4.3) and rolls into October. After it, `confirmed` and `finalized` both land in ~150 ms, and Solana advises new code to use `finalized`. Treat finality as a setting, not a hardcoded 32 slots / 12.8 s.
- **Helius Enhanced Transactions API is deprecated** (no new parsers). The replacements, `getTransfersByAddress` and `getTransactionsForAddress`, are paid-plan only.
- **Incoming SPL transfers are indexed on the token account (ATA), not the wallet.** `getSignaturesForAddress(wallet)` misses them.
- **PYUSD has confidential transfers enabled.** For those transfers, balances show no change.

## Capabilities, sources, auth
| Tool | P | Does | Primary → fallback | Auth / free tier |
|---|---|---|---|---|
| `sol_get_balances` | P0 | SOL plus all SPL and Token-2022 balances | `getBalance` + `getTokenAccountsByOwner` once per token program (Tokenkeg…, TokenzQd…) → Helius DAS `getAssetsByOwner` | Key in URL. Helius free: 1M credits, 10 RPS. DAS costs 10 credits, 2 RPS on free |
| `sol_verify_payment` | P0 | reference/recipient + mint + amount → pending/confirmed/finalized/failed/expired/unverifiable | `getSignaturesForAddress(reference or ATA)` + `getTransaction` jsonParsed | Any RPC |
| `sol_get_transfers` | P0 | Parsed in/out history with cursor | Helius `getTransfersByAddress` (10 credits, Developer+) → `getTransactionsForAddress` (100 credits, paid) → `getSignaturesForAddress`+`getTransaction` over wallet and each ATA | Neither Helius method is on the free plan |
| `sol_get_transaction` | P0 | Normalized tx: inner instructions, balance deltas | `getTransaction` → Triton archive | Helius archival calls cost 10 credits. Triton charges per query, no free tier |
| `sol_build_transfer` | P0 | Unsigned SOL/SPL/Token-2022 transfer (+ATA create, memo, compute price) | Built locally + `getLatestBlockhash` + mint `getAccountInfo` | — |
| `sol_simulate` | P0 | Compute units used, errors, balance deltas | `simulateTransaction` (`sigVerify:false`, `replaceRecentBlockhash`, `innerInstructions:true`) | Any RPC |
| `sol_priority_fee` | P0 | Fee per compute unit, by percentile | Helius `getPriorityFeeEstimate` (1 credit) → QuickNode `qn_estimatePriorityFees` (add-on) → `getRecentPrioritizationFees` + own percentile | — |
| `sol_send_signed` | P0 | Send, then resend until `lastValidBlockHeight` | Helius Sender → `sendTransaction` to every configured RPC | Sender: free, no credits, no key, 50 TPS. Needs a tip (≥0.001 SOL, or ≥0.000005 SOL in SWQOS-only mode) plus a compute-price instruction |
| `sol_tx_status` | P0 | Status / finality | `getSignatureStatuses` | — |
| `sol_payment_request` | P1 | Solana Pay URL with a fresh reference key | Built locally from the spec | — |
| `sol_resolve_name` | P1 | .sol ↔ owner | SNS Rust SDK over our RPC → Bonfida sdk-proxy REST | Proxy URL/limits [U] |
| `sol_price` | P1 | USD price | Jupiter Price v3 (`api.jup.ag/price/v3`, ≤50 ids per call) → Helius DAS `price_info` | `x-api-key`. No key 0.5 RPS; free key 1 RPS, per organization |
| `sol_swap_quote` | P1 | Quote plus unsigned swap tx | Jupiter Swap v2 `/order` or `/build` | Same limits as Price |
| `sol_token_risk` | P1 | Mint/freeze authorities, Token-2022 extensions, holder concentration | Parse the mint on-chain + `getTokenLargestAccounts` → RugCheck `/v1/tokens/{mint}/report` | RugCheck auth/limit (~60/min) [U] |
| `sol_watch_address` | P1 | Deposit push notifications | Helius webhooks (≤100k addresses each, 1 credit/event) → Yellowstone gRPC → WebSocket `logsSubscribe` | — |
| `sol_get_assets` | P2 | NFTs / compressed NFTs | Helius DAS → Triton DAS | — |

## Correctness pitfalls
- **Token accounts:** the ATA address is derived using the token program ID, so deriving PYUSD's ATA with the legacy program gives the wrong address. One owner can hold several non-ATA accounts for the same mint; add them up.
- **Measuring amounts:** use the difference between `preTokenBalances` and `postTokenBalances` on the recipient's token account (missing pre = 0), not the instruction's `amount`. This handles:
  - transfer fees (PYUSD's is 0 today but can change; withheld fees can't be spent)
  - transfers inside other programs (inner/CPI instructions: Jupiter, Squads)
  - several transfers in one tx (@solana/pay `validateTransfer` got this wrong up to 0.2.0)
- **Confidential transfers:** balances don't change, so return `unverifiable`, never 0.
- **PYUSD's other extensions:**
  - A transfer hook is set up with no program yet. If one is set later, the builder must include its extra accounts.
  - It has a permanent delegate, so the issuer can move funds without the owner signing; show this in risk.
  - Other mints: memo-required recipients need a memo instruction; some mints create accounts frozen.
  - Always simulate.
- **Scaled UI / interest-bearing mints:** `uiAmount` can be null or depend on the clock. Store raw amount + decimals; for history, use the multiplier at block time.
- **`getSignaturesForAddress`:** newest first, ≤1,000 per call (page with `before`/`until`), no `processed` commitment, includes failed txs (`err`≠null, filter them), and non-archive nodes prune old history.
- **Versioned txs:** set `maxSupportedTransactionVersion:0`, or one versioned tx fails the whole `getBlock`. Use jsonParsed to get lookup-table keys.
- **Webhooks:** Helius fires after confirmation, retries 3× one second apart, then drops the event. Treat webhooks as hints and reconcile by polling with a stored cursor.
- **Broadcasting:** tx ID = first signature, so compute it locally for repeat-safe sends. Only call a tx expired after block height passes `lastValidBlockHeight`. Send to Sender only if the tx has a tip.
- **Rate limits:** public RPC allows 100 req/10s per IP and 40 per method; not for production, last resort only. Helius free: `sendTransaction` 1/s. `getRecentPrioritizationFees` returns per-slot minimums, often zero.
- **Deposit detection in practice:** exchanges poll `getBlock` at `finalized`. For a few addresses, use signatures + `getTransaction`. Mark "seen" at `confirmed` and "settled" at `finalized`.

## Differentiation
- One `verify_payment` with the same status set on EVM and Solana, returning net amount, withheld fee and payer.
- Token-2022-aware transfer parsing over plain RPC, so free-tier users don't depend on Helius paid methods; Helius is only an accelerator.
- A safety check before building a transfer: mint extensions, memo-required recipient, simulated balance change, and the resolved .sol owner shown back.
- Finality settings per chain, aware of Alpenglow.
- Routing that accounts for credit cost (1 vs 10 vs 100 credits per call).
- Webhooks plus polling reconciliation, with saved cursors.

## Unverified or recently changed
- Alpenglow date is a schedule, not confirmed.
- Helius Parsed Events: free beta ended 2026-09-21; pricing not announced. Whether Helius "enhanced" webhooks are deprecated too is unclear.
- Jupiter `lite-api.jup.ag` is being retired (date unknown). Swap v1 `/quote` is replaced by v2.
- Triton archive price conflicts ($10 vs $25 per million queries).
- RugCheck auth/limits from third-party blogs.
- Sender `skipPreflight`: current docs say optional, older sources say required.
- QuickNode free trial (10M credits, 15 RPS): Solana credit multipliers not checked.
- Helius DAS price freshness not checked.

## Sources
- Helius: https://www.helius.dev/docs/faqs/enhanced-transactions · https://www.helius.dev/blog/introducing-gettransfersbyaddress · https://www.helius.dev/blog/introducing-gettransactionsforaddress · https://www.helius.dev/pricing · https://www.helius.dev/docs/sending-transactions/sender · https://www.helius.dev/docs/faqs/webhooks · https://www.helius.dev/docs/das-api · https://www.helius.dev/docs/priority-fee-api
- Solana: https://solana.com/docs/references/clusters · https://solana.com/developers/guides/advanced/exchange · https://solana.com/docs/rpc/http/getsignaturesforaddress · https://solana.com/docs/tokens/extensions/scaled-ui-amount/integration-guide · https://solana.com/upgrades/alpenglow · https://cryptoticker.io/en/solana-alpenglow-activation-date-validator-check/
- https://developer.paypal.com/community/blog/pyusd-solana-token-extensions/ · https://docs.solanapay.com/core/transfer-request/merchant-integration · https://github.com/solana-foundation/pay/security/advisories/GHSA-j47c-j42c-mwqq
- Jupiter: https://developers.jup.ag/docs/price · https://developers.jup.ag/docs/portal/rate-limits · https://developers.jup.ag/docs/swap
- https://www.quicknode.com/pricing · https://www.quicknode.com/docs/solana/qn_estimatePriorityFees · https://docs.triton.one/chains/solana/old-faithful-historical-archive-1 · https://api.rugcheck.xyz/swagger/index.html · https://github.com/Bonfida/sns-sdk

## Impact on our plan
- Alpenglow → Solana finality lives in the `chains.toml` policy, not hardcoded.
- Helius paid history methods → the free path is our own balance-change parser over plain RPC, querying the wallet **and** every token account for both token programs. Helius is an optional accelerator.
- PYUSD confidential transfers → `Finality::Unverifiable`. Token-2022 extensions are surfaced in `token_check_risk` and handled by `tx_build_transfer`.
- Webhooks are hints only → `payments_watch` (R2) reconciles with cursor polling in `store`.
- `tx_broadcast` on Solana: local signature ID, Helius Sender (keyless, tip required) + fan-out, resend until `lastValidBlockHeight`.
