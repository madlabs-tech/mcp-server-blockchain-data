//! ERC-20 reads. Used by payments, neobank (allowance), wallet.

use super::{eth_call, malformed};
use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{sol, SolCall};
use bdm_ports::{EvmRpc, PortResult, ProviderError};

sol! {
    interface IERC20 {
        function balanceOf(address owner) external view returns (uint256);
        function allowance(address owner, address spender) external view returns (uint256);
        function decimals() external view returns (uint8);
        function symbol() external view returns (string);
        function name() external view returns (string);
        function transfer(address to, uint256 amount) external returns (bool);
        function approve(address spender, uint256 amount) external returns (bool);
        event Transfer(address indexed from, address indexed to, uint256 value);
    }
}

/// `transfer(to, amount)` calldata.
pub fn transfer_calldata(to: Address, amount: U256) -> Vec<u8> {
    IERC20::transferCall { to, amount }.abi_encode()
}

/// `(spender, amount)` of exactly one `approve(spender, amount)` call, else `None`.
pub fn decode_approve(data: &[u8]) -> Option<(Address, U256)> {
    if data.len() != 4 + 64 {
        return None;
    }
    let c = IERC20::approveCall::abi_decode(data).ok()?;
    Some((c.spender, c.amount))
}

async fn call<C: SolCall>(
    rpc: &dyn EvmRpc,
    token: Address,
    c: C,
    block: &str,
) -> PortResult<Vec<u8>> {
    let out = eth_call(rpc, token, c.abi_encode(), block).await?;
    if out.is_empty() {
        // An EOA or missing contract returns empty data instead of reverting.
        return Err(ProviderError::NotFound);
    }
    Ok(out)
}

pub async fn balance_of(
    rpc: &dyn EvmRpc,
    token: Address,
    owner: Address,
    block: &str,
) -> PortResult<U256> {
    let out = call(rpc, token, IERC20::balanceOfCall { owner }, block).await?;
    IERC20::balanceOfCall::abi_decode_returns(&out).map_err(|_| malformed("balanceOf"))
}

pub async fn allowance(
    rpc: &dyn EvmRpc,
    token: Address,
    owner: Address,
    spender: Address,
    block: &str,
) -> PortResult<U256> {
    let out = call(rpc, token, IERC20::allowanceCall { owner, spender }, block).await?;
    IERC20::allowanceCall::abi_decode_returns(&out).map_err(|_| malformed("allowance"))
}

/// On-chain decimals. `NotFound` when `token` is not a contract.
pub async fn decimals(rpc: &dyn EvmRpc, token: Address) -> PortResult<u8> {
    let out = call(rpc, token, IERC20::decimalsCall {}, "latest").await?;
    decode_decimals(&out).ok_or_else(|| malformed("decimals"))
}

/// Symbol, or `None` when the token has none (reverts / empty / undecodable).
/// Handles both `string` and legacy `bytes32` symbols (e.g. MKR).
pub async fn symbol(rpc: &dyn EvmRpc, token: Address) -> PortResult<Option<String>> {
    match eth_call(rpc, token, IERC20::symbolCall {}.abi_encode(), "latest").await {
        Ok(out) => Ok(decode_str(&out)),
        Err(ProviderError::Invalid(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// `uint8` decimals; some tokens return a wider word, so accept any value that fits.
pub(crate) fn decode_decimals(out: &[u8]) -> Option<u8> {
    u8::try_from(U256::from_be_slice(out.get(..32)?)).ok()
}

pub(crate) fn decode_str(out: &[u8]) -> Option<String> {
    if let Ok(s) = IERC20::symbolCall::abi_decode_returns(out) {
        return (!s.is_empty()).then_some(s);
    }
    if out.len() == 32 {
        let b = B256::from_slice(out);
        let text: Vec<u8> = b.iter().copied().take_while(|&c| c != 0).collect();
        return String::from_utf8(text).ok().filter(|s| !s.is_empty());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_sol_types::SolValue;

    #[test]
    fn symbol_string_and_bytes32() {
        assert_eq!(
            decode_str(&"USDC".to_string().abi_encode()).as_deref(),
            Some("USDC")
        );
        let mut mkr = [0u8; 32];
        mkr[..3].copy_from_slice(b"MKR");
        assert_eq!(decode_str(&mkr).as_deref(), Some("MKR"));
        assert_eq!(decode_str(&[]), None);
    }

    #[test]
    fn decimals_word() {
        assert_eq!(decode_decimals(&U256::from(18u8).abi_encode()), Some(18));
        assert_eq!(decode_decimals(&U256::from(300u16).abi_encode()), None);
        assert_eq!(decode_decimals(&[1, 2]), None);
    }
}
