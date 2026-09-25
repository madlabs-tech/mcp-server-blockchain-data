use bdm_ports::metering::{CallContext, RateLimitSnapshot, UsageSink};
use std::sync::Mutex;

/// Records metering events for assertions.
#[derive(Default)]
pub struct CountingSink {
    requests: Mutex<Vec<(String, String, Option<String>)>>,
}

impl CountingSink {
    /// `(vendor, method, tool)` per recorded request.
    pub fn requests(&self) -> Vec<(String, String, Option<String>)> {
        self.requests.lock().unwrap().clone()
    }

    /// Methods recorded for `vendor`, in order.
    pub fn methods(&self, vendor: &str) -> Vec<String> {
        self.requests()
            .into_iter()
            .filter(|(v, _, _)| v == vendor)
            .map(|(_, m, _)| m)
            .collect()
    }
}

impl UsageSink for CountingSink {
    fn record_request(&self, vendor: &str, method: &str, ctx: &CallContext) {
        self.requests.lock().unwrap().push((
            vendor.to_owned(),
            method.to_owned(),
            ctx.tool.clone(),
        ));
    }

    fn record_rate_limit(&self, _: &str, _: &RateLimitSnapshot) {}
}
