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
    pub agent_timeouts: AtomicU64,
    pub agent_turns: AtomicU64,
    pub tool_calls: AtomicU64,
    pub webhook_requests: AtomicU64,
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
            agent_timeouts: AtomicU64::new(0),
            agent_turns: AtomicU64::new(0),
            tool_calls: AtomicU64::new(0),
            webhook_requests: AtomicU64::new(0),
        }
    }

    pub fn record_telegram_request(&self) {
        self.telegram_requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_discord_request(&self) {
        self.discord_requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_webhook_request(&self) {
        self.webhook_requests.fetch_add(1, Ordering::Relaxed);
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

    pub fn record_agent_timeout(&self) {
        self.agent_timeouts.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_agent_turn(&self, tool_calls: u64) {
        self.agent_turns.fetch_add(1, Ordering::Relaxed);
        self.tool_calls.fetch_add(tool_calls, Ordering::Relaxed);
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

    pub fn total_requests(&self) -> u64 {
        self.telegram_requests.load(Ordering::Relaxed)
            + self.discord_requests.load(Ordering::Relaxed)
    }

    pub fn total_errors(&self) -> u64 {
        self.telegram_errors.load(Ordering::Relaxed)
            + self.discord_errors.load(Ordering::Relaxed)
    }

    pub fn webhook_requests(&self) -> u64 {
        self.webhook_requests.load(Ordering::Relaxed)
    }

    pub fn agent_turns(&self) -> u64 {
        self.agent_turns.load(Ordering::Relaxed)
    }

    pub fn tool_calls(&self) -> u64 {
        self.tool_calls.load(Ordering::Relaxed)
    }

    pub fn completed_requests(&self) -> u64 {
        self.completed_requests.load(Ordering::Relaxed)
    }

    pub fn rate_limited(&self) -> u64 {
        self.rate_limited.load(Ordering::Relaxed)
    }

    pub fn concurrency_rejected(&self) -> u64 {
        self.concurrency_rejected.load(Ordering::Relaxed)
    }

    pub fn agent_timeouts(&self) -> u64 {
        self.agent_timeouts.load(Ordering::Relaxed)
    }

    pub fn tasks_cancelled(&self) -> u64 {
        self.tasks_cancelled.load(Ordering::Relaxed)
    }

    pub fn gateway_connects(&self) -> u64 {
        self.gateway_connects.load(Ordering::Relaxed)
    }

    pub fn gateway_disconnects(&self) -> u64 {
        self.gateway_disconnects.load(Ordering::Relaxed)
    }

    pub fn gateway_resumes(&self) -> u64 {
        self.gateway_resumes.load(Ordering::Relaxed)
    }

    pub fn error_rate_pct(&self) -> f64 {
        let total = self.total_requests();
        if total == 0 {
            return 0.0;
        }
        (self.total_errors() as f64 / total as f64) * 100.0
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

        out.push_str("# HELP openclaw_gateway_agent_timeouts_total Agent turns that hit the 120s timeout\n");
        out.push_str("# TYPE openclaw_gateway_agent_timeouts_total counter\n");
        out.push_str(&format!("openclaw_gateway_agent_timeouts_total {}\n",
            self.agent_timeouts.load(Ordering::Relaxed)));

        out.push_str("# HELP openclaw_gateway_error_rate_pct Current error rate percentage\n");
        out.push_str("# TYPE openclaw_gateway_error_rate_pct gauge\n");
        out.push_str(&format!("openclaw_gateway_error_rate_pct {:.2}\n", self.error_rate_pct()));

        out.push_str("# HELP openclaw_gateway_agent_turns_total Total agent turns completed\n");
        out.push_str("# TYPE openclaw_gateway_agent_turns_total counter\n");
        out.push_str(&format!("openclaw_gateway_agent_turns_total {}\n",
            self.agent_turns.load(Ordering::Relaxed)));

        out.push_str("# HELP openclaw_gateway_tool_calls_total Total tool calls made by agents\n");
        out.push_str("# TYPE openclaw_gateway_tool_calls_total counter\n");
        out.push_str(&format!("openclaw_gateway_tool_calls_total {}\n",
            self.tool_calls.load(Ordering::Relaxed)));

        out.push_str("# HELP openclaw_gateway_webhook_requests_total Total webhook requests received\n");
        out.push_str("# TYPE openclaw_gateway_webhook_requests_total counter\n");
        out.push_str(&format!("openclaw_gateway_webhook_requests_total {}\n",
            self.webhook_requests.load(Ordering::Relaxed)));

        out.push_str("# HELP openclaw_gateway_process_rss_bytes Resident set size of the gateway process in bytes\n");
        out.push_str("# TYPE openclaw_gateway_process_rss_bytes gauge\n");
        out.push_str(&format!("openclaw_gateway_process_rss_bytes {}\n",
            crate::process_rss_bytes()));

        out.push_str("# HELP openclaw_gateway_uptime_seconds Gateway process uptime in seconds\n");
        out.push_str("# TYPE openclaw_gateway_uptime_seconds gauge\n");
        out.push_str(&format!("openclaw_gateway_uptime_seconds {}\n",
            crate::handler::BOOT_TIME.elapsed().as_secs()));

        out.push_str("# HELP openclaw_gateway_sessions_total Total sessions in the database\n");
        out.push_str("# TYPE openclaw_gateway_sessions_total gauge\n");
        let agent = crate::handler::agent_name();
        let session_count = openclaw_agent::sessions::SessionStore::open(agent)
            .ok()
            .and_then(|s| s.db_stats(agent).ok())
            .map(|stats| stats.session_count)
            .unwrap_or(0);
        out.push_str(&format!("openclaw_gateway_sessions_total {}\n", session_count));

        out.push_str("# HELP openclaw_gateway_doctor_checks_total Total number of doctor health checks\n");
        out.push_str("# TYPE openclaw_gateway_doctor_checks_total gauge\n");
        out.push_str("openclaw_gateway_doctor_checks_total 16\n");

        let llm_stats = openclaw_agent::llm_log::stats();
        out.push_str("# HELP openclaw_gateway_llm_log_total Total LLM API calls recorded\n");
        out.push_str("# TYPE openclaw_gateway_llm_log_total counter\n");
        out.push_str(&format!("openclaw_gateway_llm_log_total {}\n", llm_stats.total_recorded));

        out.push_str("# HELP openclaw_gateway_llm_log_errors_total Total LLM API call errors\n");
        out.push_str("# TYPE openclaw_gateway_llm_log_errors_total counter\n");
        out.push_str(&format!("openclaw_gateway_llm_log_errors_total {}\n", llm_stats.errors));

        out.push_str("# HELP openclaw_gateway_llm_log_avg_latency_ms Average LLM API call latency\n");
        out.push_str("# TYPE openclaw_gateway_llm_log_avg_latency_ms gauge\n");
        out.push_str(&format!("openclaw_gateway_llm_log_avg_latency_ms {}\n", llm_stats.avg_latency_ms));

        out.push_str("# HELP openclaw_gateway_pid Process ID of the gateway\n");
        out.push_str("# TYPE openclaw_gateway_pid gauge\n");
        out.push_str(&format!("openclaw_gateway_pid {}\n", std::process::id()));

        out.push_str("# HELP openclaw_gateway_hostname_info Host identification\n");
        out.push_str("# TYPE openclaw_gateway_hostname_info gauge\n");
        let hn = std::fs::read_to_string("/etc/hostname").unwrap_or_default().trim().to_string();
        out.push_str(&format!("openclaw_gateway_hostname_info{{hostname=\"{}\"}} 1\n", hn));

        out.push_str("# HELP openclaw_gateway_info Gateway build information\n");
        out.push_str("# TYPE openclaw_gateway_info gauge\n");
        out.push_str(&format!("openclaw_gateway_info{{version=\"{}\"}} 1\n",
            env!("CARGO_PKG_VERSION")));

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
            "latency_ms_total": self.total_latency_ms.load(Ordering::Relaxed),
            "avg_latency_ms": self.avg_latency_ms(),
            "gateway_connects": self.gateway_connects.load(Ordering::Relaxed),
            "gateway_disconnects": self.gateway_disconnects.load(Ordering::Relaxed),
            "gateway_resumes": self.gateway_resumes.load(Ordering::Relaxed),
            "tasks_cancelled": self.tasks_cancelled.load(Ordering::Relaxed),
            "agent_timeouts": self.agent_timeouts.load(Ordering::Relaxed),
            "agent_turns": self.agent_turns.load(Ordering::Relaxed),
            "tool_calls": self.tool_calls.load(Ordering::Relaxed),
            "webhook_requests": self.webhook_requests.load(Ordering::Relaxed),
            "error_rate_pct": (self.error_rate_pct() * 100.0).round() / 100.0,
            "process_rss_bytes": crate::process_rss_bytes(),
            "uptime_seconds": crate::handler::BOOT_TIME.elapsed().as_secs(),
            "sessions_total": openclaw_agent::sessions::SessionStore::open(crate::handler::agent_name())
                .ok()
                .and_then(|s| s.db_stats(crate::handler::agent_name()).ok())
                .map(|stats| stats.session_count)
                .unwrap_or(0),
            "doctor_checks_total": 16,
            "llm_log_total": openclaw_agent::llm_log::total_count(),
            "llm_log_errors": openclaw_agent::llm_log::stats().errors,
            "llm_log_avg_latency_ms": openclaw_agent::llm_log::stats().avg_latency_ms,
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

    #[test]
    fn test_tasks_cancelled_metric() {
        let m = GatewayMetrics::new();
        assert_eq!(m.tasks_cancelled.load(Ordering::Relaxed), 0);

        m.record_task_cancelled();
        m.record_task_cancelled();
        m.record_task_cancelled();

        assert_eq!(m.tasks_cancelled.load(Ordering::Relaxed), 3);

        let prom = m.to_prometheus();
        assert!(prom.contains("openclaw_gateway_tasks_cancelled_total 3"));

        let json = m.to_json();
        assert_eq!(json["tasks_cancelled"], 3);
    }

    #[test]
    fn test_agent_timeouts_metric() {
        let m = GatewayMetrics::new();
        assert_eq!(m.agent_timeouts.load(Ordering::Relaxed), 0);

        m.record_agent_timeout();
        m.record_agent_timeout();

        assert_eq!(m.agent_timeouts.load(Ordering::Relaxed), 2);

        let prom = m.to_prometheus();
        assert!(prom.contains("openclaw_gateway_agent_timeouts_total 2"));

        let json = m.to_json();
        assert_eq!(json["agent_timeouts"], 2);
    }

    #[test]
    fn test_error_rate_pct() {
        let m = GatewayMetrics::new();
        assert_eq!(m.error_rate_pct(), 0.0);

        // 10 requests, 2 errors = 20%
        for _ in 0..8 { m.record_telegram_request(); }
        for _ in 0..2 { m.record_discord_request(); }
        m.record_telegram_error();
        m.record_discord_error();

        let rate = m.error_rate_pct();
        assert!((rate - 20.0).abs() < 0.1, "Expected ~20%, got {}", rate);
    }

    #[test]
    fn test_prometheus_format_headers() {
        let m = GatewayMetrics::new();
        m.record_telegram_request();
        m.record_agent_timeout();
        m.record_task_cancelled();

        let prom = m.to_prometheus();
        // Verify all HELP and TYPE headers are present
        assert!(prom.contains("# HELP openclaw_gateway_requests_total"));
        assert!(prom.contains("# TYPE openclaw_gateway_requests_total counter"));
        assert!(prom.contains("# HELP openclaw_gateway_errors_total"));
        assert!(prom.contains("# HELP openclaw_gateway_latency_ms"));
        assert!(prom.contains("# HELP openclaw_gateway_ws_events_total"));
        assert!(prom.contains("# HELP openclaw_gateway_tasks_cancelled_total"));
        assert!(prom.contains("# HELP openclaw_gateway_agent_timeouts_total"));
        assert!(prom.contains("# HELP openclaw_gateway_error_rate_pct"));
        assert!(prom.contains("# TYPE openclaw_gateway_error_rate_pct gauge"));
        assert!(prom.contains("# HELP openclaw_gateway_agent_turns_total"));
        assert!(prom.contains("# HELP openclaw_gateway_tool_calls_total"));
    }

    #[test]
    fn test_agent_turns_and_tool_calls_metric() {
        let m = GatewayMetrics::new();
        assert_eq!(m.agent_turns.load(Ordering::Relaxed), 0);
        assert_eq!(m.tool_calls.load(Ordering::Relaxed), 0);

        m.record_agent_turn(3); // 1 turn with 3 tool calls
        m.record_agent_turn(0); // 1 turn with 0 tool calls

        assert_eq!(m.agent_turns.load(Ordering::Relaxed), 2);
        assert_eq!(m.tool_calls.load(Ordering::Relaxed), 3);

        let prom = m.to_prometheus();
        assert!(prom.contains("openclaw_gateway_agent_turns_total 2"));
        assert!(prom.contains("openclaw_gateway_tool_calls_total 3"));

        let json = m.to_json();
        assert_eq!(json["agent_turns"], 2);
        assert_eq!(json["tool_calls"], 3);
    }

    #[test]
    fn test_error_rate_in_json_and_prometheus() {
        let m = GatewayMetrics::new();
        for _ in 0..4 { m.record_telegram_request(); }
        m.record_telegram_error();

        let json = m.to_json();
        assert_eq!(json["error_rate_pct"], 25.0);

        let prom = m.to_prometheus();
        assert!(prom.contains("openclaw_gateway_error_rate_pct 25.00"));
    }

    #[test]
    fn test_webhook_requests_metric() {
        let m = GatewayMetrics::new();
        m.record_webhook_request();
        m.record_webhook_request();
        m.record_webhook_request();

        let prom = m.to_prometheus();
        assert!(prom.contains("openclaw_gateway_webhook_requests_total 3"));

        let json = m.to_json();
        assert_eq!(json["webhook_requests"], 3);
    }

    #[test]
    fn test_process_rss_in_prometheus() {
        let m = GatewayMetrics::new();
        let prom = m.to_prometheus();
        assert!(prom.contains("openclaw_gateway_process_rss_bytes"),
            "Prometheus output should contain process_rss_bytes gauge");
    }

    #[test]
    fn test_process_rss_in_json() {
        let m = GatewayMetrics::new();
        let json = m.to_json();
        assert!(json.get("process_rss_bytes").is_some(),
            "JSON metrics should contain process_rss_bytes");
    }

    #[test]
    fn test_prometheus_has_all_expected_metrics() {
        let m = GatewayMetrics::new();
        let prom = m.to_prometheus();
        let expected = [
            "openclaw_gateway_requests_total{channel=\"telegram\"}",
            "openclaw_gateway_requests_total{channel=\"discord\"}",
            "openclaw_gateway_errors_total{channel=\"telegram\"}",
            "openclaw_gateway_errors_total{channel=\"discord\"}",
            "openclaw_gateway_rate_limited_total",
            "openclaw_gateway_concurrency_rejected_total",
            "openclaw_gateway_completed_requests_total",
            "openclaw_gateway_avg_latency_ms",
            "openclaw_gateway_ws_events_total{event=\"connect\"}",
            "openclaw_gateway_ws_events_total{event=\"disconnect\"}",
            "openclaw_gateway_ws_events_total{event=\"resume\"}",
            "openclaw_gateway_tasks_cancelled_total",
            "openclaw_gateway_agent_timeouts_total",
            "openclaw_gateway_error_rate_pct",
            "openclaw_gateway_agent_turns_total",
            "openclaw_gateway_tool_calls_total",
            "openclaw_gateway_webhook_requests_total",
            "openclaw_gateway_process_rss_bytes",
            "openclaw_gateway_uptime_seconds",
            "openclaw_gateway_sessions_total",
            "openclaw_gateway_doctor_checks_total",
            "openclaw_gateway_pid ",
            "openclaw_gateway_hostname_info{hostname=",
            "openclaw_gateway_info{version=",
            "openclaw_gateway_llm_log_total",
            "openclaw_gateway_llm_log_errors_total",
            "openclaw_gateway_llm_log_avg_latency_ms",
        ];
        for metric in &expected {
            assert!(prom.contains(metric),
                "Prometheus output missing metric: {}", metric);
        }
        assert_eq!(expected.len(), 27, "Expected 27 unique Prometheus metric strings");
    }

    #[test]
    fn test_prometheus_help_type_lines() {
        let m = GatewayMetrics::new();
        let prom = m.to_prometheus();
        let help_count = prom.lines().filter(|l| l.starts_with("# HELP")).count();
        let type_count = prom.lines().filter(|l| l.starts_with("# TYPE")).count();
        assert_eq!(help_count, type_count, "Each HELP should have a matching TYPE");
        assert!(help_count >= 18, "Should have at least 18 HELP lines, got {}", help_count);
    }

    #[test]
    fn test_metrics_new_all_zeros() {
        let m = GatewayMetrics::new();
        assert_eq!(m.total_requests(), 0);
        assert_eq!(m.total_errors(), 0);
        assert_eq!(m.avg_latency_ms(), 0);
        assert_eq!(m.webhook_requests(), 0);
        assert_eq!(m.agent_turns(), 0);
        assert_eq!(m.tool_calls(), 0);
        assert_eq!(m.completed_requests(), 0);
        assert_eq!(m.rate_limited(), 0);
        assert_eq!(m.concurrency_rejected(), 0);
        assert_eq!(m.agent_timeouts(), 0);
        assert_eq!(m.tasks_cancelled(), 0);
        assert_eq!(m.gateway_connects(), 0);
        assert_eq!(m.gateway_disconnects(), 0);
        assert_eq!(m.gateway_resumes(), 0);
        assert_eq!(m.error_rate_pct(), 0.0);
    }

    #[test]
    fn test_json_has_all_expected_fields() {
        let m = GatewayMetrics::new();
        let json = m.to_json();
        let expected_fields = [
            "telegram_requests", "discord_requests",
            "telegram_errors", "discord_errors",
            "rate_limited", "concurrency_rejected",
            "completed_requests", "latency_ms_total", "avg_latency_ms",
            "gateway_connects", "gateway_disconnects", "gateway_resumes",
            "tasks_cancelled", "agent_timeouts",
            "agent_turns", "tool_calls", "webhook_requests",
            "error_rate_pct", "process_rss_bytes",
            "uptime_seconds", "sessions_total", "doctor_checks_total",
            "llm_log_total", "llm_log_errors", "llm_log_avg_latency_ms",
        ];
        for field in &expected_fields {
            assert!(json.get(*field).is_some(),
                "JSON metrics missing field: {}", field);
        }
        assert_eq!(expected_fields.len(), 25, "Expected 25 JSON metrics fields");
        // Verify the actual JSON object has exactly 25 keys
        let obj = json.as_object().expect("JSON metrics should be an object");
        assert_eq!(obj.len(), 25, "JSON metrics object should have 25 keys, got {}", obj.len());
    }

    #[test]
    fn test_error_rate_pct_calculation() {
        let m = GatewayMetrics::new();
        // No requests → 0% error rate
        assert_eq!(m.error_rate_pct(), 0.0);
        assert_eq!(m.total_requests(), 0);
        assert_eq!(m.total_errors(), 0);

        // 10 telegram requests, 0 errors → 0%
        for _ in 0..10 {
            m.record_telegram_request();
        }
        assert_eq!(m.total_requests(), 10);
        assert_eq!(m.error_rate_pct(), 0.0);

        // 2 telegram errors out of 10 → 20%
        m.record_telegram_error();
        m.record_telegram_error();
        assert_eq!(m.total_errors(), 2);
        assert!((m.error_rate_pct() - 20.0).abs() < 0.01,
            "Expected ~20% error rate, got {}", m.error_rate_pct());
    }

    #[test]
    fn test_record_agent_turn() {
        let m = GatewayMetrics::new();
        assert_eq!(m.agent_turns(), 0);
        assert_eq!(m.tool_calls(), 0);

        m.record_agent_turn(3);
        assert_eq!(m.agent_turns(), 1);
        assert_eq!(m.tool_calls(), 3);

        m.record_agent_turn(5);
        assert_eq!(m.agent_turns(), 2);
        assert_eq!(m.tool_calls(), 8);
    }

    #[test]
    fn test_avg_latency_ms_calculation() {
        let m = GatewayMetrics::new();
        // No completions → 0 avg
        assert_eq!(m.avg_latency_ms(), 0);

        // 3 completions: 100ms, 200ms, 300ms → avg 200ms
        m.record_completion(100);
        m.record_completion(200);
        m.record_completion(300);
        assert_eq!(m.avg_latency_ms(), 200);
    }

    #[test]
    fn test_record_completion() {
        let m = GatewayMetrics::new();
        assert_eq!(m.completed_requests(), 0);

        m.record_completion(150);
        assert_eq!(m.completed_requests(), 1);

        m.record_completion(250);
        assert_eq!(m.completed_requests(), 2);
        assert_eq!(m.avg_latency_ms(), 200); // (150+250)/2
    }
}
