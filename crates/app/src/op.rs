use crate::ctx::Ctx;
use async_trait::async_trait;
use bdm_domain::{DomainError, Provenance, SourceKind};
use bdm_routing::Routed;
use schemars::{generate::SchemaSettings, JsonSchema};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{Map, Value};
use std::time::Duration;

/// Tool domain (groups REST routes `/v1/<domain>/<op>` and the dashboard tool list).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    Chain,
    Wallet,
    Tx,
    Payments,
    Stablecoin,
    Compliance,
    Neobank,
    Market,
    Trade,
    Rwa,
    /// Pre-refactor tool names kept for one release.
    Legacy,
}

impl Domain {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Chain => "chain",
            Self::Wallet => "wallet",
            Self::Tx => "tx",
            Self::Payments => "payments",
            Self::Stablecoin => "stablecoin",
            Self::Compliance => "compliance",
            Self::Neobank => "neobank",
            Self::Market => "market",
            Self::Trade => "trade",
            Self::Rwa => "rwa",
            Self::Legacy => "legacy",
        }
    }
}

/// Tool profiles keep each agent's tool list small. `all` and `custom` are selections, not tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    Payments,
    Trading,
    Neobank,
    Defi,
}

impl Profile {
    pub const ALL: &'static [Profile] = &[Self::Payments, Self::Trading, Self::Neobank, Self::Defi];

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "payments" => Some(Self::Payments),
            "trading" => Some(Self::Trading),
            "neobank" => Some(Self::Neobank),
            "defi" => Some(Self::Defi),
            _ => None,
        }
    }
}

/// Output of an operation plus where it came from (`meta` in the envelope).
#[derive(Debug, Clone)]
pub struct OpOutput<T> {
    pub data: T,
    pub meta: Provenance,
}

impl<T> OpOutput<T> {
    pub fn new(data: T, meta: Provenance) -> Self {
        Self { data, meta }
    }

    /// For answers computed without any vendor call (validation, static registry data).
    pub fn local(data: T) -> Self {
        let mut meta = Provenance::new(SourceKind::Primary);
        meta.provider = Some("local".into());
        Self { data, meta }
    }

    pub fn from_routed<U>(r: Routed<U>, f: impl FnOnce(U) -> T) -> Self {
        Self {
            data: f(r.value),
            meta: r.provenance,
        }
    }
}

impl<T> From<Routed<T>> for OpOutput<T> {
    fn from(r: Routed<T>) -> Self {
        Self {
            data: r.value,
            meta: r.provenance,
        }
    }
}

/// One tool. Implemented by use cases in `ops/*`.
#[async_trait]
pub trait Operation: Send + Sync + 'static {
    type Input: DeserializeOwned + JsonSchema + Send;
    type Output: Serialize + JsonSchema + Send;

    /// Stable tool name, e.g. `payments_verify_transfer`.
    const NAME: &'static str;
    const DOMAIN: Domain;
    /// Written for an LLM: what it does, when to use it, key caveats.
    const DESCRIPTION: &'static str;
    /// Profiles that include this tool (every profile gets `chain_*` and legacy aliases).
    const PROFILES: &'static [Profile];
    /// False for tools with external side effects (e.g. broadcasting a signed transaction).
    const READ_ONLY: bool = true;
    /// Legacy aliases render bare data (no envelope) and surface errors as protocol errors.
    const LEGACY: bool = false;

    /// Default response cache TTL (config `operations.<op>.cache_ttl_secs` overrides).
    fn cache_ttl(&self) -> Option<Duration> {
        None
    }

    async fn execute(
        &self,
        ctx: &Ctx,
        input: Self::Input,
    ) -> Result<OpOutput<Self::Output>, DomainError>;
}

/// Type-erased operation (JSON in, rendered JSON out) used by the Catalog and transports.
#[async_trait]
pub trait DynOperation: Send + Sync {
    fn name(&self) -> &'static str;
    fn domain(&self) -> Domain;
    fn description(&self) -> &'static str;
    fn profiles(&self) -> &'static [Profile];
    fn read_only(&self) -> bool;
    fn legacy(&self) -> bool;
    fn cache_ttl(&self) -> Option<Duration>;
    fn input_schema(&self) -> Map<String, Value>;
    fn output_schema(&self) -> Map<String, Value>;
    /// Executes and renders `{"data": …, "meta": …}`. Legacy aliases get the envelope too, so the
    /// call log sees their chain and provider; [`crate::App::call`] strips it before returning.
    async fn call(&self, ctx: &Ctx, input: Value) -> Result<Value, DomainError>;
}

pub(crate) struct OpBox<O>(pub O);

fn schema_of<T: JsonSchema>() -> Map<String, Value> {
    let schema = SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<T>();
    match schema.to_value() {
        Value::Object(m) => m,
        other => Map::from_iter([("type".to_owned(), other)]),
    }
}

#[async_trait]
impl<O: Operation> DynOperation for OpBox<O> {
    fn name(&self) -> &'static str {
        O::NAME
    }
    fn domain(&self) -> Domain {
        O::DOMAIN
    }
    fn description(&self) -> &'static str {
        O::DESCRIPTION
    }
    fn profiles(&self) -> &'static [Profile] {
        O::PROFILES
    }
    fn read_only(&self) -> bool {
        O::READ_ONLY
    }
    fn legacy(&self) -> bool {
        O::LEGACY
    }
    fn cache_ttl(&self) -> Option<Duration> {
        self.0.cache_ttl()
    }
    fn input_schema(&self) -> Map<String, Value> {
        schema_of::<O::Input>()
    }
    fn output_schema(&self) -> Map<String, Value> {
        if O::LEGACY {
            return schema_of::<O::Output>();
        }
        let mut m = Map::new();
        m.insert("type".into(), "object".into());
        m.insert(
            "properties".into(),
            serde_json::json!({
                "data": Value::Object(schema_of::<O::Output>()),
                "meta": Value::Object(schema_of::<Provenance>()),
            }),
        );
        m.insert("required".into(), serde_json::json!(["data", "meta"]));
        m
    }

    async fn call(&self, ctx: &Ctx, input: Value) -> Result<Value, DomainError> {
        let input: O::Input = serde_json::from_value(input)
            .map_err(|e| DomainError::invalid(format!("invalid input for {}: {e}", O::NAME)))?;
        let out = self.0.execute(ctx, input).await?;
        let data =
            serde_json::to_value(out.data).map_err(|e| DomainError::internal(e.to_string()))?;
        let meta =
            serde_json::to_value(out.meta).map_err(|e| DomainError::internal(e.to_string()))?;
        Ok(serde_json::json!({ "data": data, "meta": meta }))
    }
}
