//! Test fakes: JSON-RPC servers, vendor fixtures, port conformance suites, mocks.
//!
//! - [`FakeJsonRpc`]: in-process JSON-RPC node (EVM or Solana) with error injection.
//! - [`mocks`]: scripted port implementations for routing / app tests.
//! - [`CountingSink`]: metering sink that records usage for assertions.
//! - [`port_conformance`]: shared transport checks reused by every RPC adapter.
//! - [`vendor_fixture`]: load recorded vendor JSON (see `FIXTURES.md`).

mod fake_rpc;
pub mod mocks;
pub mod port_conformance;
mod sink;

pub use fake_rpc::{Failure, FakeJsonRpc};
pub use sink::CountingSink;
pub use wiremock;

use std::path::Path;

/// Load `<dir>/fixtures/<vendor>/<case>.json`. Panics with a clear message if missing/invalid.
pub fn vendor_fixture(dir: impl AsRef<Path>, vendor: &str, case: &str) -> serde_json::Value {
    let path = dir
        .as_ref()
        .join("fixtures")
        .join(vendor)
        .join(format!("{case}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("fixture {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("fixture {}: {e}", path.display()))
}
