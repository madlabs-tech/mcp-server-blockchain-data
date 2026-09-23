use serde::Serialize;
use std::{collections::BTreeMap, sync::Mutex, time::Duration};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct OpStats {
    pub calls: u64,
    pub errors: u64,
    pub cache_hits: u64,
    pub latency_ms_total: u64,
}

/// Per-operation counters rendered at `/metrics` (Prometheus text) and in the dashboard.
#[derive(Default)]
pub struct OpMetrics {
    stats: Mutex<BTreeMap<String, OpStats>>,
}

impl OpMetrics {
    pub(crate) fn record(&self, op: &str, ok: bool, cached: bool, latency: Duration) {
        let mut m = self.stats.lock().expect("metrics lock");
        let s = m.entry(op.to_owned()).or_default();
        s.calls += 1;
        s.errors += u64::from(!ok);
        s.cache_hits += u64::from(cached);
        s.latency_ms_total += latency.as_millis() as u64;
    }

    pub fn snapshot(&self) -> BTreeMap<String, OpStats> {
        self.stats.lock().expect("metrics lock").clone()
    }

    /// Prometheus exposition format.
    pub fn render_prometheus(&self) -> String {
        let mut out = String::from(
            "# TYPE bdm_op_calls_total counter\n# TYPE bdm_op_errors_total counter\n\
             # TYPE bdm_op_cache_hits_total counter\n# TYPE bdm_op_latency_ms_total counter\n",
        );
        for (op, s) in self.snapshot() {
            out += &format!(
                "bdm_op_calls_total{{op=\"{op}\"}} {}\nbdm_op_errors_total{{op=\"{op}\"}} {}\n\
                 bdm_op_cache_hits_total{{op=\"{op}\"}} {}\nbdm_op_latency_ms_total{{op=\"{op}\"}} {}\n",
                s.calls, s.errors, s.cache_hits, s.latency_ms_total
            );
        }
        out
    }
}
