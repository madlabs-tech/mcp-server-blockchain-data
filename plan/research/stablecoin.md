# Stablecoin payments research

_Date: 2026-09-23. Written by a web-research subagent. ⚠ / (unverified) mark claims that could not be verified. Circle's developer site would not load, so **no USDC contract addresses are listed here**._

## 1. Registry schema (one row per chain + stablecoin)
| Field | Notes |
|---|---|
| `asset_id`, `symbol`, `name`, `issuer_entity`, `peg` (USD/EUR), `backing` (fiat/crypto/synthetic) | Issuer entity matters: Circle Mint France ≠ Circle Internet Financial |
| `chain` (CAIP-2), `address` (CAIP-19), **`decimals`** (per deployment) | Never assume 6 |
| `issuance`: native / oft_lock_mint / ntt / canonical_bridge / third_party_bridged / exchange_peg | Separates USDC from USDC.e and Binance-Peg tokens |
| `canonical_of` / `upgraded_from` | USDT0 on Arbitrum and Polygon kept the old USDT addresses (upgraded in place) |
| `proxy`, `impl`, `impl_version` | Watch `Upgraded` events; the ABI can change |
| `freeze_check` (method + meaning), `pause_check`, `can_wipe`, `fee_params` | Differ per token |
| Solana: `token_program`, `extensions[]`, `freeze_authority`, `permanent_delegate` | |
| `cctp_domain`, `cctp_version`, `oft_adapter`, `lz_eid`, `ntt_manager` | Cross-chain routing |
| `chainlink_feed`, `pyth_feed_id`, `defillama_id` | Price and supply |
| `mica_emt` {authorized, entity, regulator, as_of}, `genius_status`, `attestation_url` | Regulation |
| `status` (active/sunsetting), `source_url`, `verified_at` | |

**Starting rows** (from issuer docs; addresses abbreviated here, full values must be copied from the source pages):
- **USDT (Tether), 6 decimals:** Ethereum `0xdac17f95…1ec7`, Avalanche `0x9702230a…8c7`, Solana `Es9vMFrz…nwNYB`. Checks: `isBlackListed` / `getBlackListStatus`, `paused`, `deprecated` / `upgradedAddress`.
- **USDT0 (Everdawn Labs, LayerZero OFT), 6 decimals:** Arbitrum `0xFd086bC7…CbB9`, Optimism `0x01bFF417…1071`, Polygon `0xc2132D05…8e8F`. On Ethereum the OFT adapter `0x6C96dE32…1dee` locks the original USDT. Docs list no USDT0 on Base, BSC, Avalanche or Solana.
- **PYUSD (Paxos):** Ethereum `0x6c3ea903…A0e8`, Arbitrum `0x46850aD6…6984`, Polygon `0x99aF3EeA…0750`, Solana `2b1kV6Dk…4GXo`. Check: `isFrozen(address)`.
- **USDG (Paxos), LayerZero OFT on EVM:** Ethereum `0xe3431676…491D`, Arbitrum `0x004B5068…9bbC`, **Robinhood Chain `0x5fc5360D…1d168`**, Solana mint `2u1tszSe…jGWH`. Check: `isFrozen`.
- **RLUSD (Ripple):** Ethereum `0x8292bb45…17ed`; Base and Optimism share `0x8d58c0c6…9258`. Decimals 18 (unverified).
- **USDC and EURC (Circle), 6 decimals:** checks `isBlacklisted(address)` + public `paused`. Load addresses from Circle's contract-address page.
- **USDe, DAI, USDS:** 18 decimals, no issuer freeze on base token (unverified). **FDUSD, USD1:** 18 decimals (unverified).

## 2. Capabilities
| Tool | What it does | Priority, why |
|---|---|---|
| `stablecoin_resolve` | Address → issuer, native/bridged, decimals; flags lookalikes | **P0**, everything depends on it |
| `stablecoin_check_restrictions` | Address frozen/blacklisted? Token paused/deprecated? Across issuers | **P0**, stops payments to frozen addresses |
| `stablecoin_balances` | Cross-chain balances normalized by decimals | **P0**, basic payments/neobank need |
| `stablecoin_peg` | Oracle vs DEX price, depeg flag | **P0**, agents otherwise assume 1 = 1 USD |
| `cctp_transfer_status` | Iris `/v2/messages/{srcDomain}?transactionHash=` | **P1**, main native USDC bridge |
| `cctp_quote` | `/v2/burn/USDC/fees`, `/v2/fastBurn/USDC/allowance` | **P1**, shows whether Fast Transfer falls back to standard |
| `oft_transfer_status` / `oft_quote` | USDT0, USDG, PYUSD via LayerZero (`quoteOFT`, `quoteSend`) | **P1** |
| `stablecoin_supply` | Supply per chain (DefiLlama + on-chain `totalSupply`) | **P1** |
| `stablecoin_restriction_events` | Stream of blacklist/freeze/pause events | **P2**, monitoring |
| Regulation/attestation info | Registry fields, not a tool | **P2** |

## 3. Data sources
- **On-chain RPC (user's keys):** source of truth for freeze and pause checks.
- **Circle Iris** (`iris-api.circle.com`): 40 req/s; exceeding blocks all requests for 5 minutes with HTTP 429.
- **CCTP finality thresholds:** 1000 = fast, 2000 = standard. **CCTP V1 capacity cuts start Oct 31, 2026; V1 paused Dec 1, 2026** → support V2 only.
- **DefiLlama** (`stablecoins.llama.fi`): `/stablecoins`, `/stablecoincharts/{chain}`, `/stablecoinprices`.
- **Pyth Hermes** `/v2/price_feeds`: resolve feed IDs at runtime; USDC/USD is `eaa020c6…c94a`.
- **Chainlink price feeds:** not verified in this pass.
- **Supply:** USDC ~$75B; USDT ~$183B (single source). Tron holds the largest USDT share (out of scope for v1); DefiLlama lists Tron first; exact figure unverified.
- **MiCA (EU):** authorized: USDC, EURC, USDG (Paxos Issuance Europe), EURCV, EURI, EURQ/USDQ, EURR, EUROe. Not authorized: USDT (transition ended Jul 1, 2026), PYUSD, DAI, USDe, FDUSD. From Eco (secondary); confirm on the ESMA register.
- **GENIUS Act (US):** proposed rules only (OCC Mar 2026, Treasury Aug 2026). Effective at the earlier of Jan 18, 2027 or 120 days after final rules.

## 4. Pitfalls
- **The same address can change what it is.** Arbitrum/Polygon USDT became USDT0 in place, and the freeze method name changed (USDT0 thought to use `isBlocked`, unverified). Store the freeze-check method per deployment.
- **Bridged vs native:** Circle's bridged standard names them "Bridged USDC (X)", symbol `USDC.e`. Symbols can be copied; resolve against the registry only.
- **Decimals:** USDT and USDC on BSC are Binance-Peg with 18 decimals (USDC unverified). RLUSD and USDe use 18.
- **Fee-on-transfer:** Ethereum USDT has `basisPointsRate` and `maximumFee` (believed 0 now, unverified). Read them. On Solana, check the Token-2022 transfer-fee extension.
- **Blacklist race:** an address can be frozen between check and landing. Re-check right before sending and report the block used.
- **Solana:** freezes apply to token accounts, not the owner wallet. A permanent delegate lets the issuer move funds out of any account. PYUSD/USDG Token-2022 extensions unverified.
- **Legacy USDT:** Ethereum USDT `transfer` returns no bool. If `deprecated`, calls go to `upgradedAddress`.
- **Pegged prices:** stablecoin feeds update rarely and look stale; compare against a DEX price.
- **Robinhood Chain:** main stablecoin USDG (~68% of stablecoin supply there). Native Circle USDC unverified.

## 5. Sources
- https://docs.usdt0.to/technical-documentation/deployments · https://docs.usdt0.to/technical-documentation/developer
- https://docs.paxos.com/guides/stablecoin/usdg/mainnet · https://docs.paxos.com/guides/stablecoin/pyusd/mainnet · https://github.com/paxosglobal/usdg-contract · https://github.com/paxosglobal/pyusd-contract
- https://docs.ripple.com/products/stablecoin/overview/token-addresses · https://tether.to/en/supported-protocols · https://etherscan.io/address/0xdac17f958d2ee523a2206206994597c13d831ec7#code
- https://github.com/circlefin/stablecoin-evm/blob/master/contracts/v1/Blacklistable.sol · https://github.com/circlefin/stablecoin-evm/blob/master/contracts/v1/Pausable.sol · https://github.com/circlefin/stablecoin-evm/blob/master/doc/bridged_USDC_standard.md
- https://developers.circle.com/cctp/references/technical-guide
- https://developers.circle.com/stablecoins/usdc-contract-addresses (**did not load**)
- https://developers.circle.com/cctp/concepts/supported-chains-and-domains (**did not load**; only Ethereum = 0, Base = 6 confirmed)
- https://www.circle.com/blog/cctp-version-updates · https://cryptoslate.com/circle-gives-legacy-usdc-apps-95-days-before-old-cross-chain-transfer-routes-stop-working/
- https://api-docs.defillama.com/ · https://hermes.pyth.network/v2/price_feeds
- https://eco.com/support/en/articles/15192006-mica-compliant-stablecoins-2026-full-list-with-issuers (secondary)
- https://www.federalregister.gov/documents/2026/03/02/2026-04089/implementing-the-guiding-and-establishing-national-innovation-for-us-stablecoins-act-for-the · https://www.federalregister.gov/documents/2026/08/18/2026-16796/genius-act-regulations-on-payment-stablecoin-issuance-offer-and-sale
- https://www.coindesk.com/business/2026/07/01/robinhood-rolls-out-public-blockchain-as-it-expands-deeper-into-crypto

## Impact on our plan
- `registry/stablecoins.toml` uses the schema above, with `source_url` + `verified_at` required (CI-enforced). Full addresses come only from the issuer pages listed; USDC/EURC wait until Circle's page loads.
- `stablecoin_check_restrictions` uses a freeze-check method stored per deployment (`isBlacklisted` / `isBlackListed` / `isFrozen` / Solana account state) and reports the block number used.
- Bridges: CCTP **v2 only** (V1 paused 2026-12-01), with the Iris 40 rps limit in `vendors.toml`. OFT via on-chain LayerZero quotes.
- Decimals per deployment (BSC 18) are enforced by golden tests. Matching is never by symbol.
- `mica_emt` / attestation fields power EU filtering later; not a separate tool.
