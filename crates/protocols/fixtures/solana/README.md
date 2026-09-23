# Solana `getTransaction` fixtures

`getTransaction` results (`encoding: jsonParsed`, `maxSupportedTransactionVersion: 0`), the
`result` object only. Used by `crates/protocols/src/solana/{tx,rpc_vendor}.rs`.

| File | Origin | What it exercises |
|---|---|---|
| `mainnet_jupiter_pyusd_usdc.json` | **Recorded** 2026-09-23 from `https://api.mainnet-beta.solana.com`, tx `2dyb2hr9fzY2fmMh36krHww4BG9rRpRg5TpQ1bvS1mJHXeGq8qX4sLouTJpa2cW9jmS7hEB2YW2SXHvyUD6pmM4K` (slot 449687808). `meta.logMessages` removed. | Real Jupiter route PYUSD (Token-2022) → USDC (legacy), all transfers in CPIs |
| `usdc_transfer_to_new_ata.json` | **Hand-built** | Transfer into a brand-new ATA (no pre-balance → 0), `createIdempotent` CPIs, rent excluded from native deltas |
| `multi_transfer_cpi.json` | **Hand-built** | Several transfers in one tx: outer `transferChecked`, CPI `transfer` (no mint), CPI `transferChecked`, CPI SOL transfer; two token accounts of one owner summed |
| `token2022_transfer_fee.json` | **Hand-built** | `transferCheckedWithFee` with a hypothetical 10 bps fee (PYUSD's live fee is 0): net amount + `withheld_fee` |
| `pyusd_confidential_transfer.json` | **Hand-built** | Token-2022 confidential transfer: public balances unchanged → `Finality::Unverifiable` |

Hand-built fixtures follow the documented RPC shapes
(<https://solana.com/docs/rpc/http/gettransaction>) and the field layout of the recorded mainnet
tx above (`accountKeys` objects, `pre/postTokenBalances` with `owner`/`programId`,
`uiTokenAmount.amount` strings, Token-2022 instructions parsed as `program: "spl-token"` and told
apart by `programId`). Addresses are real base58 keys reused from that tx and from mint docs;
their relationships (who pays whom, balances, signatures) are synthetic. The ATAs
`5MjBG9…` (USDC) and `3Rvy7A…` (PYUSD) of `Go5EVX…` are real and checked by
`spl::tests::ata_uses_the_mints_token_program`. The confidential-transfer instruction `type`
(`confidentialTransfer`) is not taken from a recorded tx; the parser also matches unparsed
instructions by their Token-2022 tag (27/37), which a test covers.

Regenerate hand-built files: they are small; edit the JSON directly and keep the test
assertions in sync.
