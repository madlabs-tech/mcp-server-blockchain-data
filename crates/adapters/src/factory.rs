//! Base registrations: chain RPC (+ broadcast) for every active vendor with an endpoint.

use crate::chain_rpc::{EvmRpcClient, SolanaRpcClient};
use bdm_config::{Loaded, VendorStatus};
use bdm_domain::ChainFamily;
use bdm_ports::{PortHandle, Registration};
use std::sync::Arc;

/// Register `EvmRpc`/`SolanaRpc` + `Broadcast` for every enabled chain and every active vendor
/// that has an RPC URL for it: registry vendors with `rpc_urls` (alchemy, quicknode, helius…),
/// operator `custom_rpc` endpoints (incl. legacy `rpc_url`) and `public`.
pub fn base_registrations(loaded: &Loaded) -> Vec<Registration> {
    let registry_vendors = loaded
        .registry
        .vendors
        .iter()
        .filter(|(id, e)| !e.rpc_urls.is_empty() || id.as_str() == "public")
        .map(|(id, _)| id.clone());
    let custom = loaded.settings.custom_rpc.keys().cloned();

    registry_vendors
        .chain(custom)
        .filter(|v| loaded.vendor_status(v) == VendorStatus::Active)
        .filter_map(|vendor| {
            let http = crate::vendors::util::http(loaded, &vendor);
            let mut meta = loaded.vendor_meta(&vendor);
            if !loaded.registry.vendors.contains_key(&vendor) {
                meta.display_name = format!("Custom RPC ({vendor})");
            }
            let mut reg = Registration::new(meta);
            for chain in loaded.registry.chains.enabled() {
                // ponytail: `public` uses the first public URL only; round-robin across all when needed.
                let Some(url) = loaded.rpc_url(&vendor, &chain.id) else {
                    continue;
                };
                reg = match chain.family {
                    ChainFamily::Evm => {
                        let Some(chain_id) = chain.id.evm_chain_id() else {
                            continue;
                        };
                        let c = Arc::new(EvmRpcClient::new(http.clone(), chain_id, url));
                        reg.chain_port(chain.id.clone(), PortHandle::EvmRpc(c.clone()))
                            .chain_port(chain.id.clone(), PortHandle::Broadcast(c))
                    }
                    ChainFamily::Solana => {
                        let c = Arc::new(SolanaRpcClient::new(http.clone(), url));
                        reg.chain_port(chain.id.clone(), PortHandle::SolanaRpc(c.clone()))
                            .chain_port(chain.id.clone(), PortHandle::Broadcast(c))
                    }
                };
            }
            (!reg.ports.is_empty()).then_some(reg)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bdm_config::{ConfigDir, ConfigLoader, EnvSource};
    use bdm_ports::Capability;
    use std::collections::BTreeSet;

    fn load(env: &[(&str, &str)]) -> Loaded {
        ConfigLoader::new(
            ConfigDir::new("/nonexistent"),
            EnvSource::from_pairs(env.iter().copied()),
        )
        .unwrap()
        .load_texts("", "")
        .unwrap()
    }

    /// vendor → set of (chain, capability)
    fn index(regs: &[Registration]) -> Vec<(String, BTreeSet<(String, Capability)>)> {
        regs.iter()
            .map(|r| {
                let ports = r
                    .ports
                    .iter()
                    .map(|(c, h)| (c.as_ref().unwrap().to_string(), h.capability()))
                    .collect();
                (r.vendor.id.clone(), ports)
            })
            .collect()
    }

    fn chains_for<'a>(
        idx: &'a [(String, BTreeSet<(String, Capability)>)],
        vendor: &str,
        cap: Capability,
    ) -> Vec<&'a str> {
        idx.iter()
            .filter(|(v, _)| v == vendor)
            .flat_map(|(_, p)| {
                p.iter()
                    .filter(move |(_, c)| *c == cap)
                    .map(|(ch, _)| ch.as_str())
            })
            .collect()
    }

    const EVM: [&str; 8] = [
        "eip155:1",
        "eip155:10",
        "eip155:137",
        "eip155:42161",
        "eip155:43114",
        "eip155:4663",
        "eip155:56",
        "eip155:8453",
    ];
    const SOL: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";

    #[test]
    fn alchemy_and_public_everywhere_helius_needs_key() {
        let regs = base_registrations(&load(&[("ALCHEMY_API_KEY", "alc_key_123456")]));
        let idx = index(&regs);

        let mut alchemy_evm = chains_for(&idx, "alchemy", Capability::EvmRpc);
        alchemy_evm.sort();
        assert_eq!(alchemy_evm, EVM);
        assert_eq!(chains_for(&idx, "alchemy", Capability::SolanaRpc), [SOL]);
        assert_eq!(chains_for(&idx, "alchemy", Capability::Broadcast).len(), 9);

        let public_all: BTreeSet<&str> = chains_for(&idx, "public", Capability::EvmRpc)
            .into_iter()
            .chain(chains_for(&idx, "public", Capability::SolanaRpc))
            .collect();
        assert_eq!(public_all.len(), 9);

        assert!(!idx.iter().any(|(v, _)| v == "helius" || v == "quicknode"));
        let alchemy = regs.iter().find(|r| r.vendor.id == "alchemy").unwrap();
        assert!(alchemy.vendor.requires_key);
        assert_eq!(alchemy.vendor.rpc_features.get_logs_max_range, Some(10));
    }

    #[test]
    fn helius_with_key_and_legacy_rpc_url_ethereum_only() {
        let regs = base_registrations(&load(&[
            ("HELIUS_API_KEY", "hel_key_123456"),
            ("RPC_URL", "http://127.0.0.1:1"),
        ]));
        let idx = index(&regs);
        assert_eq!(chains_for(&idx, "helius", Capability::SolanaRpc), [SOL]);
        assert_eq!(
            chains_for(&idx, "rpc_url", Capability::EvmRpc),
            ["eip155:1"]
        );
        let custom = regs.iter().find(|r| r.vendor.id == "rpc_url").unwrap();
        assert!(!custom.vendor.requires_key);
        assert!(!idx.iter().any(|(v, _)| v == "alchemy"));
    }
}
