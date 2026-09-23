//! `ankr` vendor adapter. Owner: `evm` (T1.E4).
//!
//! Advanced API `ankr_getAccountBalance` → `token_balances`
//! (https://www.ankr.com/docs/advanced-api/token-methods/). Disabled by default: the free tier is
//! unconfirmed (Ankr's pages disagree on Freemium access; 700 credits per call). Not on Robinhood.

use crate::{
    http::{HttpClient, DEFAULT_TIMEOUT},
    jsonrpc::JsonRpcClient,
};
use alloy_primitives::U256;
use async_trait::async_trait;
use bdm_config::{ChainEntry, Loaded, Redacted, VendorStatus};
use bdm_domain::{AccountAddress, Amount, AssetId, AssetRef};
use bdm_ports::{
    PortHandle, PortResult, ProviderError, Registration, TokenBalance, TokenBalances, VendorMeta,
};
use serde_json::{json, Value};
use std::sync::Arc;

const VENDOR: &str = "ankr";
/// Multichain Advanced API endpoint; the key is the last path segment (scrubbed from errors).
const URL_PREFIX: &str = "https://rpc.ankr.com/multichain/";

/// Ankr `blockchain` names for our chains (same docs page). Robinhood Chain is not supported.
fn blockchain(chain_id: u64) -> Option<&'static str> {
    Some(match chain_id {
        1 => "eth",
        8453 => "base",
        42161 => "arbitrum",
        10 => "optimism",
        137 => "polygon",
        43114 => "avalanche",
        56 => "bsc",
        _ => return None,
    })
}

/// Push this vendor's registration if it is active (`loaded.vendor_status("ankr")`).
pub fn register(loaded: &Loaded, out: &mut Vec<Registration>) {
    if loaded.vendor_status(VENDOR) != VendorStatus::Active {
        return;
    }
    let (Some(entry), Some(key)) = (
        loaded.registry.vendors.get(VENDOR),
        loaded.key(VENDOR, "api_key"),
    ) else {
        return;
    };
    let http = HttpClient::new(VENDOR, DEFAULT_TIMEOUT).with_secrets(loaded.secret_values());
    let mut reg = Registration::new(VendorMeta {
        id: VENDOR.into(),
        display_name: entry.display_name.clone(),
        requires_key: entry.requires_key,
        signup_url: entry.signup_url.clone(),
        rpc_features: Default::default(),
    });
    for chain in loaded.registry.chains.enabled() {
        let Some(name) = chain.id.evm_chain_id().and_then(blockchain) else {
            continue;
        };
        let url = Redacted::new(format!("{URL_PREFIX}{key}"));
        let api = Arc::new(Ankr {
            rpc: JsonRpcClient::new(http.clone(), url),
            chain: chain.clone(),
            blockchain: name,
        });
        reg = reg.chain_port(chain.id.clone(), PortHandle::TokenBalances(api));
    }
    if !reg.ports.is_empty() {
        out.push(reg);
    }
}

struct Ankr {
    rpc: JsonRpcClient,
    chain: ChainEntry,
    blockchain: &'static str,
}

#[async_trait]
impl TokenBalances for Ankr {
    /// Native + whitelisted ERC-20 balances (first page; `onlyWhitelisted` filters spam).
    async fn balances(
        &self,
        owner: &AccountAddress,
        assets: Option<&[AssetId]>,
    ) -> PortResult<Vec<TokenBalance>> {
        let AccountAddress::Evm(who) = owner else {
            return Err(ProviderError::Invalid("expected an EVM address".into()));
        };
        let params = json!({
            "walletAddress": format!("{who:#x}"),
            "blockchain": [self.blockchain],
            "onlyWhitelisted": assets.is_none(),
        });
        let v = self.rpc.request("ankr_getAccountBalance", params).await?;
        let out = v["assets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| self.parse(a))
            .filter(|b| assets.is_none_or(|l| l.contains(&b.asset)))
            .collect();
        Ok(out)
    }
}

impl Ankr {
    /// Uses `balanceRawInteger` (integer string), never the formatted `balance`.
    fn parse(&self, a: &Value) -> Option<TokenBalance> {
        let asset = match a["tokenType"].as_str()? {
            "NATIVE" => AssetId::native(self.chain.id.clone(), self.chain.native.slip44),
            "ERC20" => AssetId {
                chain: self.chain.id.clone(),
                asset: AssetRef::Erc20(a["contractAddress"].as_str()?.parse().ok()?),
            },
            _ => return None,
        };
        let raw = U256::from_str_radix(a["balanceRawInteger"].as_str()?, 10).ok()?;
        let decimals = u8::try_from(a["tokenDecimals"].as_u64()?).ok()?;
        Some(TokenBalance {
            asset,
            amount: Amount::new(raw, decimals),
            symbol: a["tokenSymbol"].as_str().map(str::to_owned),
            token_account: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bdm_config::{ConfigDir, ConfigLoader, EnvSource, Registry};
    use bdm_testkit::{vendor_fixture, FakeJsonRpc};

    #[tokio::test]
    async fn account_balance_uses_raw_integers() {
        let server = FakeJsonRpc::start().await;
        server.on(
            "ankr_getAccountBalance",
            vendor_fixture(env!("CARGO_MANIFEST_DIR"), VENDOR, "get_account_balance")["response"]
                .clone(),
        );
        let api = Ankr {
            rpc: JsonRpcClient::new(
                HttpClient::new(VENDOR, DEFAULT_TIMEOUT),
                Redacted::new(server.url()),
            ),
            chain: Registry::builtin()
                .unwrap()
                .chains
                .resolve("ethereum")
                .unwrap()
                .clone(),
            blockchain: "eth",
        };
        let owner: AccountAddress = "0xd8da6bf26964af9d7eed9e03e53415d37aa96045"
            .parse()
            .unwrap();
        let b = api.balances(&owner, None).await.unwrap();
        assert_eq!(b.len(), 2);
        assert!(b[0].asset.is_native());
        assert_eq!(b[0].amount.format_units(), "1.5");
        assert_eq!(b[1].amount.format_units(), "7.000001");
        let sent = &server.calls()[0].1;
        assert_eq!(sent["blockchain"], json!(["eth"]));
        assert_eq!(sent["onlyWhitelisted"], json!(true));
    }

    #[test]
    fn disabled_by_default_and_never_on_robinhood() {
        let load = |cfg: &str| {
            ConfigLoader::new(
                ConfigDir::new("/nonexistent"),
                EnvSource::from_pairs([("ANKR_API_KEY", "ankr_key_123456")]),
            )
            .unwrap()
            .load_texts(cfg, "")
            .unwrap()
        };
        let mut out = Vec::new();
        register(&load(""), &mut out);
        assert!(out.is_empty(), "free tier unverified → off by default");
        register(&load("[vendors.ankr]\nenabled = true\n"), &mut out);
        let chains: Vec<String> = out[0]
            .ports
            .iter()
            .map(|(c, _)| c.as_ref().unwrap().to_string())
            .collect();
        assert_eq!(chains.len(), 7);
        assert!(!chains.contains(&"eip155:4663".to_string()));
    }
}
