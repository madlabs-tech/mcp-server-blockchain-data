# Vendor fixtures

Recorded vendor responses used by adapter tests (`ems_testkit::vendor_fixture`).

- Path: `<crate>/fixtures/<vendor>/<case>.json` (e.g. `fixtures/coingecko/simple_price_usdc.json`).
- Record from a real call; **never invent data**. Put the recording date and the request
  (method/endpoint + params, keys removed) in a sibling `<case>.md` or a top-level `_meta` field.
- Strip API keys, key-bearing URLs, emails, IPs and any other PII before committing.
- Keep fixtures minimal: trim arrays to the entries the test asserts on.
- When a vendor changes its schema, add a new case instead of editing the old one.
