use alloy::primitives::{Address, Bytes, FixedBytes, U256};
use anyhow::{anyhow, Result};

/// Parse Ethereum address from string (supports ENS names via resolution in services)
pub fn parse_address(address_str: &str) -> Result<Address> {
    // Check if it's an ENS name (contains .eth or no 0x prefix)
    if address_str.ends_with(".eth") || !address_str.starts_with("0x") {
        // ENS resolution will be handled at service layer
        return Err(anyhow!("ENS resolution required for: {}", address_str));
    }

    // Parse as regular address
    address_str
        .parse::<Address>()
        .map_err(|e| anyhow!("Invalid address {}: {}", address_str, e))
}

/// Parse transaction hash from string
pub fn parse_tx_hash(hash_str: &str) -> Result<FixedBytes<32>> {
    hash_str
        .parse::<FixedBytes<32>>()
        .map_err(|e| anyhow!("Invalid transaction hash {}: {}", hash_str, e))
}

/// Parse block hash from string
pub fn parse_block_hash(hash_str: &str) -> Result<FixedBytes<32>> {
    hash_str
        .parse::<FixedBytes<32>>()
        .map_err(|e| anyhow!("Invalid block hash {}: {}", hash_str, e))
}

/// Convert Wei to ETH (or other native token)
pub fn wei_to_eth(wei: U256) -> f64 {
    let wei_f64 = wei.to_string().parse::<f64>().unwrap_or(0.0);
    wei_f64 / 1e18
}

/// Convert ETH to Wei
pub fn eth_to_wei(eth: f64) -> U256 {
    let wei = (eth * 1e18) as u128;
    U256::from(wei)
}

/// Format Wei as human readable string with token symbol
pub fn format_token_amount(amount: U256, decimals: u8, symbol: &str) -> String {
    let divisor = U256::from(10).pow(U256::from(decimals));
    let whole = amount / divisor;
    let fraction = amount % divisor;

    // Convert to decimal representation
    let whole_str = whole.to_string();
    let fraction_str = format!("{:0>width$}", fraction.to_string(), width = decimals as usize);

    // Trim trailing zeros from fraction
    let fraction_trimmed = fraction_str.trim_end_matches('0');

    if fraction_trimmed.is_empty() {
        format!("{} {}", whole_str, symbol)
    } else {
        format!("{}.{} {}", whole_str, fraction_trimmed, symbol)
    }
}

/// Parse token amount with decimals
pub fn parse_token_amount(amount_str: &str, decimals: u8) -> Result<U256> {
    // Remove any commas and trim whitespace
    let cleaned = amount_str.replace(',', "").trim().to_string();

    // Check if it contains a decimal point
    if let Some(dot_pos) = cleaned.find('.') {
        let whole = &cleaned[..dot_pos];
        let fraction = &cleaned[dot_pos + 1..];

        // Validate fraction doesn't exceed decimals
        if fraction.len() > decimals as usize {
            return Err(anyhow!(
                "Too many decimal places. Token has {} decimals",
                decimals
            ));
        }

        // Parse whole and fraction parts
        let whole_val = whole
            .parse::<u128>()
            .map_err(|e| anyhow!("Invalid amount: {}", e))?;
        let fraction_val = fraction
            .parse::<u128>()
            .map_err(|e| anyhow!("Invalid fraction: {}", e))?;

        // Calculate total value in smallest unit
        let multiplier = 10u128.pow(decimals as u32);
        let fraction_multiplier = 10u128.pow((decimals as usize - fraction.len()) as u32);

        let total = whole_val * multiplier + fraction_val * fraction_multiplier;
        Ok(U256::from(total))
    } else {
        // No decimal point, parse as whole number
        let whole_val = cleaned
            .parse::<u128>()
            .map_err(|e| anyhow!("Invalid amount: {}", e))?;
        let multiplier = 10u128.pow(decimals as u32);
        Ok(U256::from(whole_val * multiplier))
    }
}

/// Encode function selector from signature
pub fn encode_function_selector(signature: &str) -> FixedBytes<4> {
    use alloy::primitives::keccak256;

    let hash = keccak256(signature.as_bytes());
    FixedBytes::<4>::from_slice(&hash[..4])
}

/// Create ERC20 transfer call data
pub fn encode_erc20_transfer(to: Address, amount: U256) -> Bytes {
    use alloy::sol_types::SolValue;

    let selector = encode_function_selector("transfer(address,uint256)");
    let params = (to, amount).abi_encode();

    let mut data = Vec::new();
    data.extend_from_slice(&selector[..]);
    data.extend_from_slice(&params);

    Bytes::from(data)
}

/// Create ERC20 approve call data
pub fn encode_erc20_approve(spender: Address, amount: U256) -> Bytes {
    use alloy::sol_types::SolValue;

    let selector = encode_function_selector("approve(address,uint256)");
    let params = (spender, amount).abi_encode();

    let mut data = Vec::new();
    data.extend_from_slice(&selector[..]);
    data.extend_from_slice(&params);

    Bytes::from(data)
}

/// Create ERC721 transferFrom call data
pub fn encode_erc721_transfer_from(from: Address, to: Address, token_id: U256) -> Bytes {
    use alloy::sol_types::SolValue;

    let selector = encode_function_selector("transferFrom(address,address,uint256)");
    let params = (from, to, token_id).abi_encode();

    let mut data = Vec::new();
    data.extend_from_slice(&selector[..]);
    data.extend_from_slice(&params);

    Bytes::from(data)
}

/// Create ERC721 safeTransferFrom call data
pub fn encode_erc721_safe_transfer_from(from: Address, to: Address, token_id: U256) -> Bytes {
    use alloy::sol_types::SolValue;

    let selector = encode_function_selector("safeTransferFrom(address,address,uint256)");
    let params = (from, to, token_id).abi_encode();

    let mut data = Vec::new();
    data.extend_from_slice(&selector[..]);
    data.extend_from_slice(&params);

    Bytes::from(data)
}

/// Create ERC1155 safeTransferFrom call data
pub fn encode_erc1155_safe_transfer_from(
    from: Address,
    to: Address,
    id: U256,
    amount: U256,
    data: Bytes,
) -> Bytes {
    use alloy::sol_types::SolValue;

    let selector = encode_function_selector("safeTransferFrom(address,address,uint256,uint256,bytes)");
    let params = (from, to, id, amount, data).abi_encode();

    let mut call_data = Vec::new();
    call_data.extend_from_slice(&selector[..]);
    call_data.extend_from_slice(&params);

    Bytes::from(call_data)
}

/// Validate Ethereum address checksum
pub fn is_valid_address(address: &str) -> bool {
    address.parse::<Address>().is_ok()
}

/// Convert block number string to appropriate type
pub fn parse_block_number(block: &str) -> Result<u64> {
    match block {
        "latest" | "pending" | "earliest" => {
            Err(anyhow!("Use specific block tags, not: {}", block))
        }
        _ => block
            .parse::<u64>()
            .map_err(|e| anyhow!("Invalid block number {}: {}", block, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_token_amount() {
        let amount = U256::from(1500000000000000000u128); // 1.5 ETH
        assert_eq!(format_token_amount(amount, 18, "ETH"), "1.5 ETH");

        let amount = U256::from(1000000); // 1 USDC (6 decimals)
        assert_eq!(format_token_amount(amount, 6, "USDC"), "1 USDC");
    }

    #[test]
    fn test_parse_token_amount() {
        let amount = parse_token_amount("1.5", 18).unwrap();
        assert_eq!(amount, U256::from(1500000000000000000u128));

        let amount = parse_token_amount("1", 6).unwrap();
        assert_eq!(amount, U256::from(1000000));
    }
}