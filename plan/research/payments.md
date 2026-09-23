# Agent payments research

_Date: 2026-09-23. Written by a web-research subagent. ⚠ / [unverified] mark claims that could not be verified (search quota ran out; Circle developer docs would not load)._

## Capabilities and data sources
| Tool | What it does | Pri | Why | Data source |
|---|---|---|---|---|
| `payment_verify_transfer` | Checks one tx against expected chain, token, recipient, amount, memo/reference/nonce. Returns amount actually received, confirmations, finality | **P0** | Main job of a payment agent. Stripe's x402 flow records payments by tx hash ("transaction_verification") | RPC receipt logs; Solana pre/post balances; indexers as fallback |
| `payment_list_deposits` | Incoming token transfers to an address since a cursor, with finality | **P0** | Deposit detection; cursor polling works on any RPC, no vendor lock-in | `eth_getLogs` / Solana `getSignaturesForAddress`. Webhooks (Alchemy, Helius, QuickNode) at P1 |
| `payment_build_request` | EIP-681 URI, Solana Pay URL (with `reference`), or x402 `PaymentRequirements` | **P0** | Pure computation; prevents decimals/chain mix-ups | Token decimals via RPC |
| `stablecoin_resolve` | Correct stablecoin contract per chain (native USDC vs bridged, USDT, USDG), decimals, flags: EIP-3009 / 2612 / Permit2, fee-on-transfer, Token-2022 extensions | **P0** | Wrong-token mistakes are the most common payment failure | Static registry + on-chain checks (e.g. `authorizationState`) |
| `chain_finality` | Latest/safe/finalized block per chain; Solana commitment | **P0** | Every verification depends on it | `eth_getBlockByNumber("finalized")`, Solana commitment |
| `x402_verify` | Keyless x402 check: signature recovery, nonce unused, balance, time window, `eth_call` simulation; Solana instruction layout + token account | P1 | Makes us an x402 verifier with no keys | RPC + simulation |
| `x402_settle` / `x402_supported` | Forward to user's own facilitator (CDP, Circle, self-hosted x402-rs) | P1 | Facilitator = another BYO-key vendor behind the router | Facilitator APIs |
| `build_transfer_authorization` | EIP-712 typed data for EIP-3009 or Permit2, `nonce = keccak(invoice_id)` | P1 | Ties payment to invoice on EVM (no memo field) | Token EIP-712 domain via RPC |
| `gas_sponsorship_info` | Who pays gas on chain X for token Y: x402 fee payers (`/supported` signers), ERC-7677 paymasters, Circle Paymaster | P1 | Agents often have no ETH/SOL | Facilitator `/supported`; ERC-7677 `pm_getPaymasterStubData` |
| `payment_reconcile` | Stateless: invoice + candidate txs → pending/partial/paid/overpaid/expired/reorged | P1 | Handles partial/over-payment without a DB | The P0 tools |
| `cctp_transfer_status` | Cross-chain USDC transfer status | P2 | Mostly neobank flows | Circle Iris API [unverified] |
| `x402_discover` | Proxy x402 service directory ("Bazaar", `/discovery/resources`) | P2 | Nice to have | Facilitator |
| `refund_build` | Unsigned refund to the payer recovered from the tx (never a caller-claimed payer) | P2 | Reuses transfer builder | — |

## Where x402 and AP2 fit
**How x402 works**
- Server replies 402 with a `PAYMENT-REQUIRED` header listing `accepts[]`: `scheme`, `network` (CAIP-2), `amount` (smallest units), `asset`, `payTo`, `maxTimeoutSeconds`.
- Client pays and retries with a `PAYMENT-SIGNATURE` header; server calls facilitator `/verify` and `/settle`.
- EVM: EIP-3009 where supported, else Permit2 via proxy at `0x402085c2…0001`, or ERC-7710.
- Solana: partially signed tx (compute budget + TransferChecked + required Memo); facilitator co-signs as fee payer.
- **CDP facilitator:** Base, Polygon, Arbitrum, World, Solana. Does **not** list Ethereum, OP, Avalanche, BSC. Free 1,000 tx/month, then $0.001/tx.
- **Circle facilitator:** USDC on Arc, Base, Polygon, launched 2026-09-16 [secondary only].

**Should we be a facilitator?**
- **Verifier: yes.** `/verify` is pure data work and fits non-custodial.
- **Settler: no.** `/settle` needs a gas-paying hot wallet or Solana fee-payer key, which conflicts with "never holds keys". Settlement = BYO-key adapter behind the router (CDP, Circle, self-hosted [x402-rs](https://github.com/x402-rs/x402-rs), which is Rust and could supply our types).
- After settlement, `payment_verify_transfer` checks the returned tx hash at finality instead of trusting `success: true`.

**AP2**
- Signed-mandate (W3C credential) authorization layer. v0.2 donated to FIDO on 2026-04-28. Mandates renamed: v0.1 Intent/Cart/Payment → v0.2 Checkout/Payment (open or closed).
- Stablecoin payments run through x402 (A2A x402 extension). We don't sign mandates; at most a P2 check that a settled tx matches a mandate's amount, payee, token.

**Others**
- Stripe ACP uses Shared Payment Tokens (fiat). Stripe's stablecoin route is x402/MPP on **Tempo, Base, Solana** (Tempo not on our chain list).
- Visa/Mastercard: card/token side, nothing on-chain to index.
- Crossmint, Skyfire, Payman, Nevermined, Coinbase AgentKit / Payments MCP, Circle MCPs (community-built) all hold wallets: clients of our tools, not data sources.

## Pitfalls
- **Under/overpayment:** compare what the recipient actually received (balance change), not the amount requested. x402 `exact` requires exact match; invoices need tolerance + partial state; refund overpayment to the recovered payer.
- **Wrong token/chain:** match contract + chain ID, never symbol. USDC.e ≠ USDC. Robinhood Chain uses **USDG**, not USDC [secondary]. USDC on BSC is Binance-pegged, likely no EIP-3009 → Permit2 [unverified].
- **Reorgs:** report `confirmed` separately from `finalized` (safe/finalized tags, Solana commitment). Per-chain finality times from memory [unverified].
- **Fee-on-transfer:** USDT has a dormant fee switch; Token-2022 has a transfer-fee extension. `Transfer` amount can differ from what arrives → use balance change.
- **Memo matching:** EVM has no memo → unique deposit address, EIP-3009 nonce (`AuthorizationUsed` event), or Permit2 witness. Solana Pay `reference` is findable via `getSignaturesForAddress`. Memos are spoofable; always check amount + recipient. Solana `payTo` is the owner; funds land in its ATA, so derive it.
- **Idempotency:** key on (chain, tx hash, log index). EIP-3009 nonces can't be replayed, but the verifier must still check `authorizationState`.

## Sources
- x402: https://github.com/coinbase/x402 · https://raw.githubusercontent.com/coinbase/x402/main/specs/x402-specification-v2.md · https://raw.githubusercontent.com/coinbase/x402/main/specs/schemes/exact/scheme_exact_evm.md · https://raw.githubusercontent.com/coinbase/x402/main/specs/schemes/exact/scheme_exact_svm.md · https://docs.cdp.coinbase.com/x402/network-support
- Stripe: https://docs.stripe.com/payments/machine/x402
- AP2: https://ap2-protocol.org/ · https://blog.google/products-and-platforms/platforms/google-pay/agent-payments-protocol-fido-alliance/ · https://github.com/google-agentic-commerce/a2a-x402
- Standards: https://docs.solanapay.com/spec · https://eips.ethereum.org/EIPS/eip-681 · https://eips.ethereum.org/EIPS/eip-3009 (Draft) · https://eips.ethereum.org/EIPS/eip-7677 (in review)
- https://www.coinbase.com/developer-platform/discover/launches/payments-mcp · https://usa.visa.com/about-visa/newsroom/press-releases.releaseId.22491.html
- [Secondary only]: https://cryptobriefing.com/circle-x402-facilitator-service-arc-launch/ · https://across.to/blog/bridge-to-robinhood-chain-with-across · https://blog.arbitrum.io/robinhood-chain-mainnet/ ; Mastercard Agent Pay, Visa Trusted Agent Protocol, Skyfire, Payman, Nevermined (vendor/aggregator blogs)
- [Unverified]: CCTP v2 and Circle Paymaster chain lists.

## Impact on our plan
- R1 payment core: `payments_verify_transfer` (balance-change based, status set incl. underpaid/overpaid/wrong_token/unverifiable, optional quorum), `payments_list_deposits` (stateless cursor polling), `payments_build_request`.
- x402: `x402_verify` is keyless (R2). `x402_settle`/`x402_supported` forward to the user's facilitator (CDP free 1K tx/month); we never hold a fee-payer key.
- Idempotency key is (chain, tx, logIndex); tokens are matched via the stablecoin registry, never by symbol.
- Webhooks are hints; cursor polling is the source of truth (`payments_watch`, R2, with `store`).
- Evaluate reusing `x402-rs` types before writing our own.
