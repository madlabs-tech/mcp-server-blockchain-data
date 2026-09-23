use crate::op::{DynOperation, OpBox, Operation, Profile};
use bdm_config::Loaded;
use std::{collections::BTreeMap, sync::Arc};

/// Every registered operation, keyed by tool name.
#[derive(Default, Clone)]
pub struct Catalog {
    ops: BTreeMap<&'static str, Arc<dyn DynOperation>>,
}

impl Catalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an operation. Panics on duplicate names (a programming error caught by tests).
    pub fn register<O: Operation>(&mut self, op: O) {
        let prev = self.ops.insert(O::NAME, Arc::new(OpBox(op)));
        assert!(prev.is_none(), "duplicate operation name {}", O::NAME);
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn DynOperation>> {
        self.ops.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn DynOperation>> {
        self.ops.values()
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

/// Which tools a caller sees: a profile (`payments`, …), `all`, or an explicit `custom` list,
/// minus `disabled_tools` and operations with `operations.<op>.enabled = false`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileSelection {
    All,
    Profile(Profile),
    Custom(Vec<String>),
}

impl ProfileSelection {
    /// Unknown profile names fall back to `All` (config validation reports them).
    pub fn parse(name: &str, custom: &[String]) -> Self {
        match name {
            "all" => Self::All,
            "custom" => Self::Custom(custom.to_vec()),
            other => Profile::parse(other)
                .map(Self::Profile)
                .unwrap_or(Self::All),
        }
    }

    pub fn from_config(cfg: &Loaded) -> Self {
        Self::parse(
            &cfg.settings.server.tool_profile,
            &cfg.settings.server.enabled_tools,
        )
    }

    pub fn includes(&self, op: &dyn DynOperation, cfg: &Loaded) -> bool {
        let selected = match self {
            Self::All => true,
            Self::Profile(p) => op.profiles().contains(p),
            Self::Custom(list) => list.iter().any(|n| n == op.name()),
        };
        selected
            && !cfg
                .settings
                .server
                .disabled_tools
                .iter()
                .any(|n| n == op.name())
            && cfg.operation(op.name()).enabled != Some(false)
    }
}
