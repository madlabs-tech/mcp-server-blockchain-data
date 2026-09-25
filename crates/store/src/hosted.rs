//! Hosted mode: client-key authentication and per-client limits.

use crate::db::{ClientRecord, Exceeded, Store};
use async_trait::async_trait;
use bdm_app::{CallGuard, Caller, ClientAuth, ProfileSelection};
use bdm_config::{ClientLimits, Loaded};
use bdm_domain::{DomainError, ErrorCode};
use bdm_routing::{Router, TokenBucket, WindowKey};
use chrono::Utc;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};

/// Client limits: store override > `clients.overrides.<id>` > `clients.default`, field by field.
pub fn effective_client_limits(cfg: &Loaded, rec: &ClientRecord) -> ClientLimits {
    let base = cfg.client_limits(&rec.id);
    match &rec.limits {
        None => base,
        Some(o) => ClientLimits {
            requests_per_minute: o.requests_per_minute.or(base.requests_per_minute),
            daily_requests: o.daily_requests.or(base.daily_requests),
            monthly_credits: o.monthly_credits.or(base.monthly_credits),
            tool_profile: o.tool_profile.clone().or(base.tool_profile),
        },
    }
}

fn unauthorized() -> DomainError {
    DomainError::new(ErrorCode::Unauthorized, "missing or invalid client key")
        .with_hint("send `Authorization: Bearer <client key>`; keys are issued by the operator")
}

/// `Authorization: Bearer <key>` → SHA-256 → active client → `Caller` with its tool profile.
pub struct ClientKeyAuth {
    store: Store,
    router: Arc<Router>,
}

impl ClientKeyAuth {
    pub fn new(store: Store, router: Arc<Router>) -> Self {
        Self { store, router }
    }
}

#[async_trait]
impl ClientAuth for ClientKeyAuth {
    async fn authenticate(&self, bearer: Option<&str>) -> Result<Caller, DomainError> {
        let key = bearer
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .ok_or_else(unauthorized)?;
        let rec = self
            .store
            .client_by_key(key)
            .filter(ClientRecord::active)
            .ok_or_else(unauthorized)?;
        let cfg = self.router.table().config.clone();
        let limits = effective_client_limits(&cfg, &rec);
        let profile = limits
            .tool_profile
            .as_deref()
            .map(|p| ProfileSelection::parse(p, &cfg.settings.server.enabled_tools));
        Ok(Caller {
            client: Some(rec.id),
            profile,
        })
    }
}

/// Per-client admission: requests/minute (token bucket), daily requests, monthly credits.
/// Local callers (`client = None`) are never limited here.
pub struct ClientGuard {
    store: Store,
    router: Arc<Router>,
    buckets: Mutex<HashMap<String, TokenBucket>>,
}

impl ClientGuard {
    pub fn new(store: Store, router: Arc<Router>) -> Self {
        Self {
            store,
            router,
            buckets: Mutex::default(),
        }
    }

    /// Take one token; `Err(secs)` = wait this long for the next one.
    fn take(&self, client: &str, rpm: u32) -> Result<(), u64> {
        if rpm == 0 {
            return Err(60);
        }
        let cap = f64::from(rpm);
        let per_sec = cap / 60.0;
        let now = Instant::now();
        let mut map = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        map.entry(client.to_owned())
            .or_insert(TokenBucket::full(cap, now))
            .take(cap, per_sec, now)
            .map_err(|wait| wait.as_secs_f64().ceil().max(1.0) as u64)
    }
}

fn exceeded(message: String, retry_after: u64) -> DomainError {
    DomainError::new(ErrorCode::QuotaExceeded, message)
        .with_hint("limits are per client key; ask the operator to raise them")
        .with_retry_after(retry_after.max(1))
}

#[async_trait]
impl CallGuard for ClientGuard {
    async fn admit(&self, caller: &Caller, _op: &str) -> Result<(), DomainError> {
        let Some(id) = caller.client.as_deref() else {
            return Ok(());
        };
        let rec = self
            .store
            .client(id)
            .filter(ClientRecord::active)
            .ok_or_else(unauthorized)?;
        let limits = effective_client_limits(&self.router.table().config, &rec);
        let now = Utc::now();
        if let Some(rpm) = limits.requests_per_minute {
            if let Err(wait) = self.take(id, rpm) {
                self.store.record_throttled(id, now);
                return Err(exceeded(
                    format!("client rate limit reached ({rpm} requests/minute)"),
                    wait,
                ));
            }
        }
        match self
            .store
            .admit_client(id, now, limits.daily_requests, limits.monthly_credits)
        {
            Ok(()) => Ok(()),
            Err(which) => {
                self.store.record_throttled(id, now);
                let (what, key) = match which {
                    Exceeded::DailyRequests => (
                        format!(
                            "client daily request quota reached ({})",
                            limits.daily_requests.unwrap_or_default()
                        ),
                        WindowKey::day(now),
                    ),
                    Exceeded::MonthlyCredits => (
                        format!(
                            "client monthly credit quota reached ({})",
                            limits.monthly_credits.unwrap_or_default()
                        ),
                        WindowKey::month(now),
                    ),
                };
                let reset = key.resets_at(now);
                Err(exceeded(
                    format!("{what}; resets at {}", reset.to_rfc3339()),
                    (reset - now).num_seconds().max(1) as u64,
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bdm_config::{ConfigDir, ConfigLoader, EnvSource};
    use bdm_routing::{ProviderRegistry, RouterOptions, RoutingTable};

    fn setup(cfg: &str) -> (Store, Arc<Router>) {
        let loader =
            ConfigLoader::new(ConfigDir::new("/nonexistent"), EnvSource::default()).unwrap();
        let store = Store::open_in_memory().unwrap();
        let router = Router::new(
            RoutingTable {
                config: Arc::new(loader.load_texts(cfg, "").unwrap()),
                registry: ProviderRegistry::new(vec![]),
            },
            Arc::new(store.clone()),
            RouterOptions::default(),
        );
        (store, router)
    }

    #[tokio::test]
    async fn auth_rejects_missing_bad_and_revoked_keys() {
        let (store, router) = setup("");
        let auth = ClientKeyAuth::new(store.clone(), router);
        let (rec, key) = store.create_client("acme", None).await.unwrap();
        for bad in [None, Some(""), Some("odm_nope")] {
            assert_eq!(
                auth.authenticate(bad).await.unwrap_err().code,
                ErrorCode::Unauthorized
            );
        }
        let caller = auth.authenticate(Some(&key)).await.unwrap();
        assert_eq!(caller.client.as_deref(), Some(rec.id.as_str()));
        // default client profile is "payments"
        assert_eq!(
            caller.profile,
            Some(ProfileSelection::Profile(bdm_app::Profile::Payments))
        );
        store.revoke_client(&rec.id).await.unwrap();
        assert!(auth.authenticate(Some(&key)).await.is_err());
    }

    #[tokio::test]
    async fn per_client_limits_do_not_affect_other_clients() {
        let (store, router) =
            setup("[clients.default]\nrequests_per_minute = 100\ndaily_requests = 2\n");
        let guard = ClientGuard::new(store.clone(), router);
        let (a, _) = store.create_client("a", None).await.unwrap();
        let (b, _) = store
            .create_client(
                "b",
                Some(ClientLimits {
                    requests_per_minute: Some(1),
                    daily_requests: Some(100),
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
        let ca = Caller {
            client: Some(a.id.clone()),
            profile: None,
        };
        let cb = Caller {
            client: Some(b.id.clone()),
            profile: None,
        };
        guard.admit(&ca, "t").await.unwrap();
        guard.admit(&ca, "t").await.unwrap();
        let e = guard.admit(&ca, "t").await.unwrap_err();
        assert_eq!(e.code, ErrorCode::QuotaExceeded);
        assert!(e.retry_after_secs.unwrap() >= 1);
        // b is unaffected by a's daily quota, but has its own rpm = 1
        guard.admit(&cb, "t").await.unwrap();
        let e = guard.admit(&cb, "t").await.unwrap_err();
        assert!(e.message.contains("requests/minute"), "{e:?}");
        assert!((1..=60).contains(&e.retry_after_secs.unwrap()));
        // local callers are never limited
        guard.admit(&Caller::local(), "t").await.unwrap();
        let month = WindowKey::month(Utc::now());
        assert_eq!(store.client_counters(&a.id, &month).throttled, 1);
    }
}
