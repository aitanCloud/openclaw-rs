use std::sync::atomic::{AtomicU64, Ordering};

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
}
