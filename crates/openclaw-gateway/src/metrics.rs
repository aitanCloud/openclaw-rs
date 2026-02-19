use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// Global metrics instance accessible from handlers
static GLOBAL_METRICS: OnceLock<&'static GatewayMetrics> = OnceLock::new();

/// Initialize the global metrics reference (call once from main)
pub fn init_global(metrics: &'static GatewayMetrics) {
    let _ = GLOBAL_METRICS.set(metrics);
}

/// Get the global metrics instance (returns None if not initialized)
pub fn global() -> Option<&'static GatewayMetrics> {
    GLOBAL_METRICS.get().copied()
}

/// Lightweight gateway metrics using atomics (no external deps)
pub struct GatewayMetrics {
    pub telegram_requests: AtomicU64,
    pub discord_requests: AtomicU64,
    pub telegram_errors: AtomicU64,
    pub discord_errors: AtomicU64,
    pub rate_limited: AtomicU64,
    pub concurrency_rejected: AtomicU64,
    pub total_latency_ms: AtomicU64,
    pub completed_requests: AtomicU64,
    pub gateway_connects: AtomicU64,
    pub gateway_disconnects: AtomicU64,
    pub gateway_resumes: AtomicU64,
    pub tasks_cancelled: AtomicU64,
}

impl GatewayMetrics {
    pub fn new() -> Self {
        Self {
            telegram_requests: AtomicU64::new(0),
            discord_requests: AtomicU64::new(0),
            telegram_errors: AtomicU64::new(0),
            discord_errors: AtomicU64::new(0),
            rate_limited: AtomicU64::new(0),
            concurrency_rejected: AtomicU64::new(0),
            total_latency_ms: AtomicU64::new(0),
            completed_requests: AtomicU64::new(0),
            gateway_connects: AtomicU64::new(0),
            gateway_disconnects: AtomicU64::new(0),
            gateway_resumes: AtomicU64::new(0),
            tasks_cancelled: AtomicU64::new(0),
        }
    }

    pub fn record_telegram_request(&self) {
        self.telegram_requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_discord_request(&self) {
        self.discord_requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_telegram_error(&self) {
        self.telegram_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_discord_error(&self) {
        self.discord_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_rate_limited(&self) {
        self.rate_limited.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_concurrency_rejected(&self) {
        self.concurrency_rejected.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_gateway_connect(&self) {
        self.gateway_connects.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_gateway_disconnect(&self) {
        self.gateway_disconnects.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_gateway_resume(&self) {
        self.gateway_resumes.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_task_cancelled(&self) {
        self.tasks_cancelled.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_completion(&self, latency_ms: u64) {
        self.completed_requests.fetch_add(1, Ordering::Relaxed);
        self.total_latency_ms.fetch_add(latency_ms, Ordering::Relaxed);
    }

    pub fn avg_latency_ms(&self) -> u64 {
        let completed = self.completed_requests.load(Ordering::Relaxed);
        if completed == 0 {
            return 0;
        }
        self.total_latency_ms.load(Ordering::Relaxed) / completed
    }

    /// Prometheus text exposition format
    pub fn to_prometheus(&self) -> String {
        let mut out = String::new();
        out.push_str("# HELP openclaw_gateway_requests_total Total requests by channel\n");
        out.push_str("# TYPE openclaw_gateway_requests_total counter\n");
        out.push_str(&format!("openclaw_gateway_requests_total{{channel=\"telegram\"}} {}\n",
            self.telegram_requests.load(Ordering::Relaxed)));
        out.push_str(&format!("openclaw_gateway_requests_total{{channel=\"discord\"}} {}\n",
            self.discord_requests.load(Ordering::Relaxed)));

        out.push_str("# HELP openclaw_gateway_errors_total Total errors by channel\n");
        out.push_str("# TYPE openclaw_gateway_errors_total counter\n");
        out.push_str(&format!("openclaw_gateway_errors_total{{channel=\"telegram\"}} {}\n",
            self.telegram_errors.load(Ordering::Relaxed)));
        out.push_str(&format!("openclaw_gateway_errors_total{{channel=\"discord\"}} {}\n",
            self.discord_errors.load(Ordering::Relaxed)));

        out.push_str("# HELP openclaw_gateway_rate_limited_total Total rate limited requests\n");
        out.push_str("# TYPE openclaw_gateway_rate_limited_total counter\n");
        out.push_str(&format!("openclaw_gateway_rate_limited_total {}\n",
            self.rate_limited.load(Ordering::Relaxed)));

        out.push_str("# HELP openclaw_gateway_concurrency_rejected_total Total concurrency rejected requests\n");
        out.push_str("# TYPE openclaw_gateway_concurrency_rejected_total counter\n");
        out.push_str(&format!("openclaw_gateway_concurrency_rejected_total {}\n",
            self.concurrency_rejected.load(Ordering::Relaxed)));

        out.push_str("# HELP openclaw_gateway_completed_requests_total Total completed requests\n");
        out.push_str("# TYPE openclaw_gateway_completed_requests_total counter\n");
        out.push_str(&format!("openclaw_gateway_completed_requests_total {}\n",
            self.completed_requests.load(Ordering::Relaxed)));

        out.push_str("# HELP openclaw_gateway_latency_ms_total Total latency in milliseconds\n");
        out.push_str("# TYPE openclaw_gateway_latency_ms_total counter\n");
        out.push_str(&format!("openclaw_gateway_latency_ms_total {}\n",
            self.total_latency_ms.load(Ordering::Relaxed)));

        out.push_str("# HELP openclaw_gateway_avg_latency_ms Average request latency in milliseconds\n");
        out.push_str("# TYPE openclaw_gateway_avg_latency_ms gauge\n");
        out.push_str(&format!("openclaw_gateway_avg_latency_ms {}\n",
            self.avg_latency_ms()));

        out.push_str("# HELP openclaw_gateway_ws_events_total Discord Gateway WebSocket events\n");
        out.push_str("# TYPE openclaw_gateway_ws_events_total counter\n");
        out.push_str(&format!("openclaw_gateway_ws_events_total{{event=\"connect\"}} {}\n",
            self.gateway_connects.load(Ordering::Relaxed)));
        out.push_str(&format!("openclaw_gateway_ws_events_total{{event=\"disconnect\"}} {}\n",
            self.gateway_disconnects.load(Ordering::Relaxed)));
        out.push_str(&format!("openclaw_gateway_ws_events_total{{event=\"resume\"}} {}\n",
            self.gateway_resumes.load(Ordering::Relaxed)));

        out.push_str("# HELP openclaw_gateway_tasks_cancelled_total Tasks cancelled by user\n");
        out.push_str("# TYPE openclaw_gateway_tasks_cancelled_total counter\n");
        out.push_str(&format!("openclaw_gateway_tasks_cancelled_total {}\n",
            self.tasks_cancelled.load(Ordering::Relaxed)));

        out
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "telegram_requests": self.telegram_requests.load(Ordering::Relaxed),
            "discord_requests": self.discord_requests.load(Ordering::Relaxed),
            "telegram_errors": self.telegram_errors.load(Ordering::Relaxed),
            "discord_errors": self.discord_errors.load(Ordering::Relaxed),
            "rate_limited": self.rate_limited.load(Ordering::Relaxed),
            "concurrency_rejected": self.concurrency_rejected.load(Ordering::Relaxed),
            "completed_requests": self.completed_requests.load(Ordering::Relaxed),
            "total_latency_ms": self.total_latency_ms.load(Ordering::Relaxed),
            "avg_latency_ms": self.avg_latency_ms(),
            "gateway_connects": self.gateway_connects.load(Ordering::Relaxed),
            "gateway_disconnects": self.gateway_disconnects.load(Ordering::Relaxed),
            "gateway_resumes": self.gateway_resumes.load(Ordering::Relaxed),
            "tasks_cancelled": self.tasks_cancelled.load(Ordering::Relaxed),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_basic() {
        let m = GatewayMetrics::new();
        assert_eq!(m.telegram_requests.load(Ordering::Relaxed), 0);

        m.record_telegram_request();
        m.record_telegram_request();
        m.record_discord_request();
        assert_eq!(m.telegram_requests.load(Ordering::Relaxed), 2);
        assert_eq!(m.discord_requests.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_metrics_latency() {
        let m = GatewayMetrics::new();
        m.record_completion(100);
        m.record_completion(200);
        m.record_completion(300);
        assert_eq!(m.avg_latency_ms(), 200);
        assert_eq!(m.completed_requests.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn test_metrics_prometheus() {
        let m = GatewayMetrics::new();
        m.record_telegram_request();
        m.record_telegram_request();
        m.record_discord_request();
        m.record_telegram_error();
        m.record_rate_limited();
        m.record_completion(500);

        let prom = m.to_prometheus();
        assert!(prom.contains("openclaw_gateway_requests_total{channel=\"telegram\"} 2"));
        assert!(prom.contains("openclaw_gateway_requests_total{channel=\"discord\"} 1"));
        assert!(prom.contains("openclaw_gateway_errors_total{channel=\"telegram\"} 1"));
        assert!(prom.contains("openclaw_gateway_rate_limited_total 1"));
        assert!(prom.contains("openclaw_gateway_avg_latency_ms 500"));
        assert!(prom.contains("# TYPE openclaw_gateway_requests_total counter"));
    }

    #[test]
    fn test_metrics_json() {
        let m = GatewayMetrics::new();
        m.record_telegram_request();
        m.record_rate_limited();
        m.record_completion(500);

        let json = m.to_json();
        assert_eq!(json["telegram_requests"], 1);
        assert_eq!(json["rate_limited"], 1);
        assert_eq!(json["avg_latency_ms"], 500);
    }

    #[test]
    fn test_gateway_ws_metrics() {
        let m = GatewayMetrics::new();
        assert_eq!(m.gateway_connects.load(Ordering::Relaxed), 0);

        m.record_gateway_connect();
        m.record_gateway_connect();
        m.record_gateway_disconnect();
        m.record_gateway_resume();

        assert_eq!(m.gateway_connects.load(Ordering::Relaxed), 2);
        assert_eq!(m.gateway_disconnects.load(Ordering::Relaxed), 1);
        assert_eq!(m.gateway_resumes.load(Ordering::Relaxed), 1);

        let prom = m.to_prometheus();
        assert!(prom.contains("openclaw_gateway_ws_events_total{event=\"connect\"} 2"));
        assert!(prom.contains("openclaw_gateway_ws_events_total{event=\"disconnect\"} 1"));
        assert!(prom.contains("openclaw_gateway_ws_events_total{event=\"resume\"} 1"));

        let json = m.to_json();
        assert_eq!(json["gateway_connects"], 2);
        assert_eq!(json["gateway_disconnects"], 1);
        assert_eq!(json["gateway_resumes"], 1);
    }
}
