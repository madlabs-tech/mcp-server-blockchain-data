//! Usage counters behind a trait: in-memory here, sqlite in `bdm-store` (T1.D1).

use chrono::{DateTime, Datelike, Utc};
use serde::Serialize;
use std::{collections::HashMap, sync::Mutex};

/// Calendar window a counter belongs to. Monthly budgets reset on the 1st (UTC), daily at 00:00 UTC.
// ponytail: calendar windows only; vendors billing from the signup date drift by < 1 cycle.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum WindowKey {
    Day(String),
    Month(String),
}

impl WindowKey {
    pub fn day(now: DateTime<Utc>) -> Self {
        Self::Day(now.format("%Y-%m-%d").to_string())
    }

    pub fn month(now: DateTime<Utc>) -> Self {
        Self::Month(format!("{:04}-{:02}", now.year(), now.month()))
    }

    /// When the window containing `now` resets.
    pub fn resets_at(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        let date = now.date_naive();
        let next = match self {
            Self::Day(_) => date.succ_opt().expect("date in range"),
            Self::Month(_) => {
                let (y, m) = if date.month() == 12 {
                    (date.year() + 1, 1)
                } else {
                    (date.year(), date.month() + 1)
                };
                chrono::NaiveDate::from_ymd_opt(y, m, 1).expect("valid date")
            }
        };
        next.and_hms_opt(0, 0, 0).expect("midnight").and_utc()
    }
}

/// Attribution of one metered request (dashboard breakdowns).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize)]
pub struct Dims {
    pub method: String,
    pub tool: Option<String>,
    pub chain: Option<String>,
    pub client: Option<String>,
}

pub trait CounterStore: Send + Sync {
    fn add(&self, vendor: &str, window: &WindowKey, amount: u64, dims: &Dims);
    fn total(&self, vendor: &str, window: &WindowKey) -> u64;
    /// Breakdown rows for a vendor/window (dashboard).
    fn breakdown(&self, vendor: &str, window: &WindowKey) -> Vec<(Dims, u64)>;
}

#[derive(Default)]
pub struct InMemoryCounterStore {
    totals: Mutex<HashMap<(String, WindowKey), u64>>,
    rows: Mutex<HashMap<(String, WindowKey, Dims), u64>>,
}

impl CounterStore for InMemoryCounterStore {
    fn add(&self, vendor: &str, window: &WindowKey, amount: u64, dims: &Dims) {
        *self
            .totals
            .lock()
            .expect("lock")
            .entry((vendor.to_owned(), window.clone()))
            .or_default() += amount;
        *self
            .rows
            .lock()
            .expect("lock")
            .entry((vendor.to_owned(), window.clone(), dims.clone()))
            .or_default() += amount;
    }

    fn total(&self, vendor: &str, window: &WindowKey) -> u64 {
        self.totals
            .lock()
            .expect("lock")
            .get(&(vendor.to_owned(), window.clone()))
            .copied()
            .unwrap_or(0)
    }

    fn breakdown(&self, vendor: &str, window: &WindowKey) -> Vec<(Dims, u64)> {
        self.rows
            .lock()
            .expect("lock")
            .iter()
            .filter(|((v, w, _), _)| v == vendor && w == window)
            .map(|((_, _, d), n)| (d.clone(), *n))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn window_keys_and_resets() {
        let t = Utc.with_ymd_and_hms(2026, 12, 31, 23, 59, 0).unwrap();
        assert_eq!(WindowKey::day(t), WindowKey::Day("2026-12-31".into()));
        assert_eq!(WindowKey::month(t), WindowKey::Month("2026-12".into()));
        assert_eq!(
            WindowKey::day(t).resets_at(t),
            Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap()
        );
        assert_eq!(
            WindowKey::month(t).resets_at(t),
            Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap()
        );
    }
}
