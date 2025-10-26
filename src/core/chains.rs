use anyhow::{anyhow, Result};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fmt;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainConfig {
    pub network: String,
    pub rpc: String,
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Chain {
    Ethereum,
    Base,
    Arbitrum,
    Avalanche,
    Bsc,
}

impl fmt::Display for Chain {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Chain::Ethereum => write!(f, "ethereum"),
            Chain::Base => write!(f, "base"),
            Chain::Arbitrum => write!(f, "arbitrum"),
            Chain::Avalanche => write!(f, "avalanche"),
            Chain::Bsc => write!(f, "bsc"),
        }
    }
}

impl std::str::FromStr for Chain {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "ethereum" => Ok(Chain::Ethereum),
            "base" => Ok(Chain::Base),
            "arbitrum" => Ok(Chain::Arbitrum),
            "avalanche" => Ok(Chain::Avalanche),
            "bsc" => Ok(Chain::Bsc),
            _ => Err(anyhow!("Unsupported chain: {}", s)),
        }
    }
}

// Helper function to validate environment variables
fn validate_env_var(name: &str) -> Result<String> {
    env::var(name).map_err(|_| {
        anyhow!(
            "Environment variable {} is not set. Please check your .env file.",
            name
        )
    })
}

// Build RPC URL based on network name
fn build_rpc_url(network_name: &str) -> Result<String> {
    // Try to get RPC_URL first (for simple setups)
    if let Ok(rpc_url) = env::var("RPC_URL") {
        return Ok(rpc_url);
    }

    // Otherwise, try QuickNode setup
    let endpoint_name = validate_env_var("QN_ENDPOINT_NAME")?;
    let token_id = validate_env_var("QN_TOKEN_ID")?;

    let url = match network_name {
        "mainnet" => format!("https://{}.quiknode.pro/{}/", endpoint_name, token_id),
        "avalanche-mainnet" => format!(
            "https://{}.{}.quiknode.pro/{}/ext/bc/C/rpc",
            endpoint_name, network_name, token_id
        ),
        _ => format!(
            "https://{}.{}.quiknode.pro/{}/",
            endpoint_name, network_name, token_id
        ),
    };

    Ok(url)
}

// Lazy static for chain configurations
pub static CHAINS: Lazy<HashMap<Chain, ChainConfig>> = Lazy::new(|| {
    let mut chains = HashMap::new();

    // Ethereum
    chains.insert(
        Chain::Ethereum,
        ChainConfig {
            network: "mainnet".to_string(),
            rpc: build_rpc_url("mainnet").unwrap_or_else(|_| {
                "https://eth-mainnet.g.alchemy.com/v2/your-api-key".to_string()
            }),
            name: "Ethereum".to_string(),
            symbol: "ETH".to_string(),
            decimals: 18,
        },
    );

    // Base
    chains.insert(
        Chain::Base,
        ChainConfig {
            network: "base-mainnet".to_string(),
            rpc: build_rpc_url("base-mainnet").unwrap_or_else(|_| {
                "https://mainnet.base.org".to_string()
            }),
            name: "Base".to_string(),
            symbol: "ETH".to_string(),
            decimals: 18,
        },
    );

    // Arbitrum
    chains.insert(
        Chain::Arbitrum,
        ChainConfig {
            network: "arbitrum-mainnet".to_string(),
            rpc: build_rpc_url("arbitrum-mainnet").unwrap_or_else(|_| {
                "https://arb1.arbitrum.io/rpc".to_string()
            }),
            name: "Arbitrum".to_string(),
            symbol: "ETH".to_string(),
            decimals: 18,
        },
    );

    // Avalanche
    chains.insert(
        Chain::Avalanche,
        ChainConfig {
            network: "avalanche-mainnet".to_string(),
            rpc: build_rpc_url("avalanche-mainnet").unwrap_or_else(|_| {
                "https://api.avax.network/ext/bc/C/rpc".to_string()
            }),
            name: "Avalanche".to_string(),
            symbol: "AVAX".to_string(),
            decimals: 18,
        },
    );

    // BSC
    chains.insert(
        Chain::Bsc,
        ChainConfig {
            network: "bsc".to_string(),
            rpc: build_rpc_url("bsc").unwrap_or_else(|_| {
                "https://bsc-dataseed.binance.org".to_string()
            }),
            name: "Binance Smart Chain".to_string(),
            symbol: "BNB".to_string(),
            decimals: 18,
        },
    );

    chains
});

// Get a chain configuration by Chain enum
pub fn get_chain(chain: Chain) -> Result<&'static ChainConfig> {
    CHAINS
        .get(&chain)
        .ok_or_else(|| anyhow!("Chain {:?} not supported", chain))
}

// Get list of all supported chains
pub fn get_supported_chains() -> Vec<String> {
    CHAINS
        .keys()
        .map(|chain| chain.to_string())
        .collect()
}