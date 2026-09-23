//! EVM readers. Owner: `evm` (Phase 1). Block tags are strings: "latest" | "safe" | "finalized" | "0x…".

pub mod chainlink;
pub mod erc20;
pub mod erc8056;
pub mod fees;
pub mod logs;
pub mod multicall3;
pub mod rpc_vendor;
pub mod tx;
