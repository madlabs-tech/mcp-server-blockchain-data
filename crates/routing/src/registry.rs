use ems_domain::ChainId;
use ems_ports::{Capability, PortHandle, QuotaReporter, Registration, VendorMeta};
use std::{collections::HashMap, sync::Arc};

type Key = (Capability, Option<ChainId>);

/// Every registered port, indexed by `(capability, chain)` then vendor id.
#[derive(Clone, Default)]
pub struct ProviderRegistry {
    ports: HashMap<Key, HashMap<String, PortHandle>>,
    vendors: HashMap<String, VendorMeta>,
    quota_reporters: HashMap<String, Arc<dyn QuotaReporter>>,
}

impl ProviderRegistry {
    pub fn new(registrations: impl IntoIterator<Item = Registration>) -> Self {
        let mut r = Self::default();
        for reg in registrations {
            r.add(reg);
        }
        r
    }

    /// Later registrations for the same `(capability, chain, vendor)` replace earlier ones.
    pub fn add(&mut self, reg: Registration) {
        let vendor = reg.vendor.id.clone();
        for (chain, handle) in reg.ports {
            self.ports
                .entry((handle.capability(), chain))
                .or_default()
                .insert(vendor.clone(), handle);
        }
        if let Some(q) = reg.quota_reporter {
            self.quota_reporters.insert(vendor.clone(), q);
        }
        self.vendors.insert(vendor, reg.vendor);
    }

    /// Port for a vendor: chain-bound entry first, then a chain-agnostic one.
    pub fn get(
        &self,
        cap: Capability,
        chain: Option<&ChainId>,
        vendor: &str,
    ) -> Option<&PortHandle> {
        chain
            .and_then(|c| self.ports.get(&(cap, Some(c.clone()))))
            .and_then(|m| m.get(vendor))
            .or_else(|| self.ports.get(&(cap, None)).and_then(|m| m.get(vendor)))
    }

    pub fn vendor(&self, id: &str) -> Option<&VendorMeta> {
        self.vendors.get(id)
    }

    pub fn vendors(&self) -> impl Iterator<Item = &VendorMeta> {
        self.vendors.values()
    }

    pub fn quota_reporters(&self) -> impl Iterator<Item = (&String, &Arc<dyn QuotaReporter>)> {
        self.quota_reporters.iter()
    }

    /// Vendors registered for a capability on a chain (for the dashboard's effective-order view).
    pub fn registered_for(&self, cap: Capability, chain: Option<&ChainId>) -> Vec<String> {
        let mut v: Vec<String> = chain
            .and_then(|c| self.ports.get(&(cap, Some(c.clone()))))
            .into_iter()
            .chain(self.ports.get(&(cap, None)))
            .flat_map(|m| m.keys().cloned())
            .collect();
        v.sort();
        v.dedup();
        v
    }
}
