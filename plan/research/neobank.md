# Crypto neobank & card programs research

_Date: 2026-09-23. Written by a web-research subagent. ⚠ / [U] mark claims that could not be verified._

## Main finding
The capability that matters most is a **non-custodial card funding check**.
- Bridge/Stripe cards spend "just-in-time" from a wallet. The user approves Bridge's spender (ERC-20 approve, or SPL delegate on Solana), and Bridge pulls funds when the card is authorized.
- Bridge declines if the approval is inactive or too small, or the balance is short.
- Baanx (delegation) and Gnosis Pay (Safe Roles module) work similarly.
- A self-hosted read-only MCP can answer the support question "why was this declined?".

## Capabilities and data sources
| Tool | What it does | P | Why | Source / cost | Contract needed? |
|---|---|---|---|---|---|
| `get_card_funding_status` | Stablecoin balance, allowance or SPL `delegatedAmount` for the issuer's spender, spendable = min of the two | P0 | Explains card declines | Own RPC (`balanceOf`/`allowance`), free | No (issuer supplies spender address) |
| `list_incoming_transfers` | Deposits to a set of addresses with finality | P0 | Crediting deposits | RPC logs. Optional BYO webhooks: Alchemy Address Activity (≤100K addresses/webhook), Helius, Privy `wallet.funds_deposited` | No |
| `get_ledger` | Normalized rows: raw amount + decimals, fee, counterparty, block timestamp, fiat value + price source | P0 | Bank-like statements | RPC + price tools | No |
| `get_price_at` | Token price at a timestamp, with source + staleness | P0 | Fiat value at tx time | CoinGecko Demo: free, 100/min, 10K/month, 365 days history. Pyth Benchmarks: API key required since 2026-08-26, 10 req/10s. Chainlink `getRoundData` on-chain [U] | No |
| `get_fx_rate` | Fiat FX at a date | P0 | Local-currency display | Frankfurter (ECB): free, daily ~16:00 CET, ~30 currencies. Open Exchange Rates: free 1K/month, hourly, USD base only | No |
| `screen_address` | Sanctions + issuer freeze status | P0 | Block deposits/withdrawals with sanctioned/frozen addresses | Chainalysis oracle `isSanctioned`: free, no customer relationship, EVM only, different address on Base, no Solana. TRM free API: 1 req/s, 100/day, key. USDC `isBlacklisted` / USDT `isBlackListed` | No |
| `validate_address` | Checksum, EOA vs contract, Solana owner vs ATA, token exists on chain, native vs bridged (USDC vs USDC.e) | P0 | Network mismatch loses funds. Stripe requires owner address, not ATA | Local + RPC | No |
| `verify_wallet_ownership` | Signed-message check (EIP-191, EIP-1271 for Safe/4337, ed25519 Solana) | P1 | EU travel rule: self-hosted wallet transfers ≥€1,000 need ownership proof | Local + RPC | No |
| `get_onramp_quotes` | Buy/sell quotes from several providers | P1 | Ramp comparison | Coinbase `/onramp/v1/buy/quote`: CDP key, 10 req/s. Transak: partner key via sales. MoonPay: API key [U] | Yes |
| `get_yield` | Earn APY + position | P1 | Yield on balances (Plasma One, ether.fi) | DefiLlama yields (free, no auth); ERC-4626 `convertToAssets` via RPC | No |
| `export_ledger` | CSV: UTC ISO-8601, raw + decimal amounts, fiat value, price source | P1 | Accounting/tax tools | From `get_ledger` | No |
| `get_reserve_status` | Chainlink PoR feeds, stablecoin supply, issuer attestation links | P2 | Issuer-risk dashboards | Chainlink on-chain. Circle monthly (Deloitte). Tether quarterly (BDO). DefiLlama stablecoins | No |
| `kyt_risk` | Pass-through tx risk scoring with operator's key | P2 | Some operators want it | Chainalysis KYT / TRM / Elliptic (Elliptic ~$50K/year on AWS Marketplace [U]) | Yes |

## Fit for a self-hosted, non-custodial data MCP
**In scope:** every read-only tool above (stateless, operator's keys, no personal data).

**Out of scope:**
- Card authorization decisions and issuing (Stripe/Bridge, Rain, Baanx own that; Gnosis Pay answers Visa within ~2 s).
- Custody, signing, KYC.
- Travel-rule messaging (personal data; needs TRUST or Notabene membership).
- Selling KYT as our own product (licensing).
- A neobank's own proof of reserves.
- Tax lots and 1099-DA (broker obligation; basis reporting starts with 2026 txs; CARF exchanges start 2027).
- Statement PDFs (Bridge already generates them).

**Chain gap:** card programs use chains outside our list, so the chain registry must be config-driven.
- Gnosis Chain (Gnosis Pay)
- Scroll (ether.fi Cash)
- Plasma (Plasma One)
- Tempo, World Chain, Linea (Bridge's non-custodial EVM chains: Tempo, Base, World Chain, Linea)
- TRON (large USDT volume; KAST supports it [U])

## Pitfalls
- **Valuation timestamp:** use block time, not ingestion or card capture time. One Bridge card authorization can cause several on-chain pulls (incremental auths, overcapture, top-up at capture). Refunds arrive later, possibly batched. The ledger must link on-chain txs to authorizations.
- **Staleness:** return price timestamp + source. ECB rates on weekends/holidays are the last business day's; label it.
- **Stablecoins aren't always $1:** return par and market value.
- **Precision:** USDC 6 decimals, DAI 18, USDT on BSC 18 [U]. Store raw integers (u128/U256), `rust_decimal`, never f64. Round only for display, with a rounding mode per currency (JPY has 0 decimals).
- **Finality:** confirmation depth per chain; L2 soft confirmation ≠ L1 finality; Solana confirmed ≠ finalized.
- **Smart-account deposits:** deposits into ERC-4337/Safe wallets happen as internal calls. Detect from Transfer logs, not `tx.to`.
- **Allowance races:** the user can revoke at any time. Gnosis Pay's Delay module adds a 3-minute delay.
- **Network mismatch:** the same 0x address exists on every EVM chain. Freeze status is per token and per chain.
- **Licensing / free tiers:**
  - The Chainalysis oracle disclaims accuracy and may lag official lists.
  - TRM's free API is proprietary.
  - Caching or sharing KYT results across tenants is probably restricted [U].
  - Etherscan free covers only Ethereum, Polygon, Arbitrum; Robinhood Chain free until 2026-10-15.

## Sources
- https://docs.stripe.com/issuing/bridge-stablecoin-cards · https://docs.stripe.com/issuing/stablecoin-cards · https://apidocs.bridge.xyz/platform/cards/overview/noncustodial
- https://www.gnosis.io/blog/a-hackers-guide-to-gnosis-pay · https://docs.baanx.com/guides/introduction · https://www.rain.xyz/cards
- https://www.coindesk.com/business/2025/09/22/plasma-unveils-first-stablecoin-native-neobank-targeting-emerging-markets · https://help.ether.fi/en/articles/326983-understanding-your-cash-card-borrow-mode-vs-direct-pay-mode
- https://go.chainalysis.com/chainalysis-oracle-docs.html · https://docs.sanctions.trmlabs.com/
- https://docs.privy.io/api-reference/webhooks/wallet/funds_deposited · https://www.alchemy.com/docs/reference/address-activity-webhook · https://www.helius.dev/docs/webhooks
- https://docs.pyth.network/price-feeds/core/use-historical-price-data · https://www.coingecko.com/en/api/pricing · https://frankfurter.dev/
- https://docs.cdp.coinbase.com/api-reference/rest-api/onramp-offramp/create-buy-quote · https://docs.transak.com/api/public/get-price · https://api-docs.defillama.com/
- https://docs.chain.link/data-feeds/proof-of-reserve · https://www.circle.com/transparency · https://docs.etherscan.io/supported-chains
- https://notabene.id/post/overview-of-eu-crypto-travel-rule-compliance-for-casps · https://www.irs.gov/pub/irs-prior/i1099da--2026.pdf
- https://stablecoininsider.org/how-to-check-usdc-blacklist-status/ · https://blog.arbitrum.io/robinhood-chain-mainnet/
- **Unverified / secondary:** MoonPay quote auth (404), Chainlink `getRoundData` history (not fetched), Helius pricing, KAST chain list, RedotPay Fireblocks custody, Elliptic pricing, BSC USDT decimals, Rain API (behind sales), Coinbase card and Revolut crypto (not researched).

## Impact on our plan
- R1 neobank tools: `neobank_card_funding_status` (balance + allowance/delegate for the issuer spender), `neobank_get_ledger` (block-time fiat valuation + source), `fiat_get_fx_rate` (Frankfurter → Open Exchange Rates, business-date labeled).
- `compliance_screen_address` = Chainalysis oracle (EVM, free) + TRM free API + issuer freeze → combined verdict with sources. KYT is a paid backlog pass-through.
- `address_validate` covers checksum, EOA/contract/smart account, owner vs ATA, and native vs bridged.
- Chains live in `chains.toml`, so Gnosis, Scroll, Plasma, Tempo, World and Linea can be added without code.
- Onramp quotes: Coinbase only (free CDP key). Transak/MoonPay are excluded (partner contract).
