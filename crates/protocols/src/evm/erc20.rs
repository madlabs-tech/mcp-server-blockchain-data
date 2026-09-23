//! ERC-20 reads. Owner: `evm` (T1.E5). Used by payments, neobank (allowance), wallet.

use crate::not_yet;
use alloy_primitives::{Address, U256};
use ems_ports::{EvmRpc, PortResult};

pub async fn balance_of(
    _rpc: &dyn EvmRpc,
    _token: Address,
    _owner: Address,
    _block: &str,
) -> PortResult<U256> {
    Err(not_yet("T1.E5 erc20::balance_of"))
}

pub async fn allowance(
    _rpc: &dyn EvmRpc,
    _token: Address,
    _owner: Address,
    _spender: Address,
    _block: &str,
) -> PortResult<U256> {
    Err(not_yet("T1.E5 erc20::allowance"))
}

pub async fn decimals(_rpc: &dyn EvmRpc, _token: Address) -> PortResult<u8> {
    Err(not_yet("T1.E5 erc20::decimals"))
}

pub async fn symbol(_rpc: &dyn EvmRpc, _token: Address) -> PortResult<Option<String>> {
    Err(not_yet("T1.E5 erc20::symbol"))
}
