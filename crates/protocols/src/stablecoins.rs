//! Canonical stablecoin registry (`registry/stablecoins.toml`). Owner: `payments-stablecoin` (T1.P1).
//! Other teams use only [`StablecoinRegistry`]'s lookup API; the owner extends the entry fields.

use ems_domain::{AssetId, ChainId};

#[derive(Debug, Clone, PartialEq)]
pub struct StablecoinEntry {
    pub asset: AssetId,
    pub symbol: String,
    pub issuer_entity: String,
    pub decimals: u8,
}

#[derive(Debug, Clone, Default)]
pub struct StablecoinRegistry {
    entries: Vec<StablecoinEntry>,
}

impl StablecoinRegistry {
    /// Built-in registry (empty until T1.P1 adds `registry/stablecoins.toml`).
    pub fn builtin() -> Result<Self, String> {
        Ok(Self::default())
    }

    pub fn by_asset(&self, asset: &AssetId) -> Option<&StablecoinEntry> {
        self.entries.iter().find(|e| &e.asset == asset)
    }

    pub fn by_symbol(&self, chain: &ChainId, symbol: &str) -> Option<&StablecoinEntry> {
        self.entries
            .iter()
            .find(|e| &e.asset.chain == chain && e.symbol.eq_ignore_ascii_case(symbol))
    }

    pub fn for_chain<'a>(
        &'a self,
        chain: &'a ChainId,
    ) -> impl Iterator<Item = &'a StablecoinEntry> {
        self.entries.iter().filter(move |e| &e.asset.chain == chain)
    }
}
