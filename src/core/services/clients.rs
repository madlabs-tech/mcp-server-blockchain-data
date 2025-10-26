use alloy::providers::{Provider, ProviderBuilder};
use anyhow::Result;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::core::chains::{get_chain, Chain};

// Type alias for our provider
type AlloyProvider = Arc<dyn Provider + Send + Sync>;

// Cache for Alloy providers to avoid creating duplicate clients
static CLIENT_CACHE: Lazy<RwLock<HashMap<Chain, AlloyProvider>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

/// Creates or retrieves a cached Alloy provider for the specified chain
///
/// # Arguments
/// * `chain` - The chain identifier
///
/// # Returns
/// An Alloy provider configured for the specified chain
pub async fn get_public_client(chain: Chain) -> Result<AlloyProvider> {
    // Check cache first
    {
        let cache = CLIENT_CACHE.read().await;
        if let Some(client) = cache.get(&chain) {
            return Ok(client.clone());
        }
    }

    // Get chain configuration
    let chain_config = get_chain(chain)?;

    // Create new provider using Alloy's ProviderBuilder
    let provider = ProviderBuilder::new()
        .on_builtin(&chain_config.rpc)
        .await?;

    let provider: AlloyProvider = Arc::new(provider) as AlloyProvider;

    // Cache for future use
    {
        let mut cache = CLIENT_CACHE.write().await;
        cache.insert(chain, provider.clone());
    }

    Ok(provider)
}

/// Clear the client cache (useful for testing or reconnection scenarios)
pub async fn clear_client_cache() {
    let mut cache = CLIENT_CACHE.write().await;
    cache.clear();
}