use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

/// Maximum entries in the ring buffer
const MAX_LOG_ENTRIES: usize = 100;

/// Global LLM activity log
static GLOBAL_LOG: OnceLock<Mutex<LlmActivityLog>> = OnceLock::new();

/// Initialize the global log (call once at startup)
pub fn init_global() {
    let _ = GLOBAL_LOG.get_or_init(|| Mutex::new(LlmActivityLog::new(MAX_LOG_ENTRIES)));
}

/// Record an LLM interaction (in-memory + Postgres if available)
pub fn record(entry: LlmLogEntry) {
    // Dual-write to Postgres (fire-and-forget)
    if let Some(pool) = openclaw_db::pool() {
        let id = uuid::Uuid::parse_str(&entry.id).unwrap_or_else(|_| uuid::Uuid::new_v4());
        let session_key = entry.session_key.clone();
        let model = entry.model.clone();
        let provider_attempt = entry.provider_attempt as i16;
        let messages_count = entry.messages_count as i32;
        let request_tokens_est = entry.request_tokens_est as i32;
        let streaming = entry.streaming;
        let response_content = entry.response_content.clone();
        let response_reasoning = entry.response_reasoning.clone();
        let response_tool_calls = entry.response_tool_calls as i32;
        let tool_call_names = entry.tool_call_names.clone();
        let usage_prompt = entry.usage_prompt_tokens as i32;
        let usage_completion = entry.usage_completion_tokens as i32;
        let usage_total = entry.usage_total_tokens as i32;
        let latency_ms = entry.latency_ms as i32;
        let error = entry.error.clone();
        let pool = pool.clone();
        tokio::spawn(async move {
            let _ = openclaw_db::llm_log::record_llm_call(
                &pool, &id, session_key.as_deref(), &model, provider_attempt,
                messages_count, request_tokens_est, streaming,
                response_content.as_deref(), response_reasoning.as_deref(),
                response_tool_calls, &tool_call_names, usage_prompt,
                usage_completion, usage_total, latency_ms, error.as_deref(),
            ).await;
        });
    }

    // In-memory ring buffer (always)
    if let Some(log) = GLOBAL_LOG.get() {
        if let Ok(mut log) = log.lock() {
            log.push(entry);
        }
    }
}

/// Get recent log entries (newest first)
pub fn recent(limit: usize) -> Vec<LlmLogEntry> {
    GLOBAL_LOG
        .get()
        .and_then(|log| log.lock().ok())
        .map(|log| log.recent(limit))
        .unwrap_or_default()
}

/// Get all log entries (newest first)
pub fn all_entries() -> Vec<LlmLogEntry> {
    GLOBAL_LOG
        .get()
        .and_then(|log| log.lock().ok())
        .map(|log| log.all())
        .unwrap_or_default()
}

/// Get a single entry by ID
pub fn get_by_id(id: &str) -> Option<LlmLogEntry> {
    GLOBAL_LOG
        .get()
        .and_then(|log| log.lock().ok())
        .and_then(|log| log.find_by_id(id))
}

/// Get total number of entries ever recorded
pub fn total_count() -> u64 {
    GLOBAL_LOG
        .get()
        .and_then(|log| log.lock().ok())
        .map(|log| log.total_count)
        .unwrap_or(0)
}

/// Per-task session context — keyed by tokio task ID so concurrent agent turns
/// (main + subagent) don't clobber each other's session keys.
static CURRENT_SESSION: std::sync::Mutex<Option<HashMap<String, String>>> = std::sync::Mutex::new(None);

/// Current provider attempt number (1-based, for fallback chains)
static CURRENT_PROVIDER_ATTEMPT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// Set the current provider attempt number (call before each fallback attempt)
pub fn set_provider_attempt(attempt: u32) {
    CURRENT_PROVIDER_ATTEMPT.store(attempt, std::sync::atomic::Ordering::Relaxed);
}

/// Get the current provider attempt number
pub fn current_provider_attempt() -> u32 {
    CURRENT_PROVIDER_ATTEMPT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Set the current session key for this tokio task (call before running an agent turn)
pub fn set_session_context(session_key: &str) {
    let task_id = current_task_id();
    if let Ok(mut ctx) = CURRENT_SESSION.lock() {
        let map = ctx.get_or_insert_with(HashMap::new);
        map.insert(task_id, session_key.to_string());
    }
}

/// Clear the current session key for this tokio task (call after agent turn completes)
pub fn clear_session_context() {
    let task_id = current_task_id();
    if let Ok(mut ctx) = CURRENT_SESSION.lock() {
        if let Some(map) = ctx.as_mut() {
            map.remove(&task_id);
        }
    }
}

/// Get the current session key for this tokio task (used internally by log entry creation)
pub fn current_session_key() -> Option<String> {
    let task_id = current_task_id();
    CURRENT_SESSION
        .lock()
        .ok()
        .and_then(|ctx| ctx.as_ref().and_then(|map| map.get(&task_id).cloned()))
}

/// Get a stable identifier for the current tokio task (or "0" if not in a task).
fn current_task_id() -> String {
    tokio::task::try_id()
        .map(|id| format!("{:?}", id))
        .unwrap_or_else(|| "0".to_string())
}

/// Get summary stats for the log
pub fn stats() -> LogStats {
    GLOBAL_LOG
        .get()
        .and_then(|log| log.lock().ok())
        .map(|log| LogStats {
            total_recorded: log.total_count,
            buffered: log.entries.len() as u64,
            errors: log.entries.iter().filter(|e| e.error.is_some()).count() as u64,
            avg_latency_ms: if log.entries.is_empty() {
                0
            } else {
                log.entries.iter().map(|e| e.latency_ms).sum::<u64>() / log.entries.len() as u64
            },
        })
        .unwrap_or_default()
}

/// Summary statistics for the LLM activity log
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct LogStats {
    pub total_recorded: u64,
    pub buffered: u64,
    pub errors: u64,
    pub avg_latency_ms: u64,
}

/// A single LLM API interaction log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmLogEntry {
    /// Unique ID for this entry
    pub id: String,
    /// ISO 8601 timestamp
    pub timestamp: String,
    /// Model name (e.g. "moonshot/kimi-k2.5")
    pub model: String,
    /// Number of messages in the request
    pub messages_count: usize,
    /// Estimated request tokens
    pub request_tokens_est: u32,
    /// Whether this was a streaming request
    pub streaming: bool,
    /// Response content (truncated for display)
    pub response_content: Option<String>,
    /// Response reasoning content (if any)
    pub response_reasoning: Option<String>,
    /// Number of tool calls in response
    pub response_tool_calls: usize,
    /// Tool call names (if any)
    pub tool_call_names: Vec<String>,
    /// Token usage from API
    pub usage_prompt_tokens: u32,
    pub usage_completion_tokens: u32,
    pub usage_total_tokens: u32,
    /// Latency in milliseconds
    pub latency_ms: u64,
    /// Error message if the request failed
    pub error: Option<String>,
    /// Which provider attempt this was (1-based, for fallback chains)
    pub provider_attempt: u32,
    /// Session key (if available from context)
    pub session_key: Option<String>,
}

impl LlmLogEntry {
    /// Create a new entry with auto-generated ID, timestamp, and current session context
    pub fn new(model: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            model: model.to_string(),
            messages_count: 0,
            request_tokens_est: 0,
            streaming: false,
            response_content: None,
            response_reasoning: None,
            response_tool_calls: 0,
            tool_call_names: Vec::new(),
            usage_prompt_tokens: 0,
            usage_completion_tokens: 0,
            usage_total_tokens: 0,
            latency_ms: 0,
            error: None,
            provider_attempt: current_provider_attempt(),
            session_key: current_session_key(),
        }
    }

    /// Truncated summary for display (one-liner)
    pub fn summary(&self) -> String {
        let status = if self.error.is_some() { "❌" } else { "✅" };
        let content_preview = self
            .response_content
            .as_deref()
            .map(|c| {
                if c.len() > 80 {
                    format!("{}...", &c[..77])
                } else {
                    c.to_string()
                }
            })
            .unwrap_or_else(|| {
                if self.response_tool_calls > 0 {
                    format!("[{} tool call(s): {}]", self.response_tool_calls, self.tool_call_names.join(", "))
                } else {
                    "(no content)".to_string()
                }
            });
        let attempt = if self.provider_attempt > 1 {
            format!(" [attempt {}]", self.provider_attempt)
        } else {
            String::new()
        };
        let session = self.session_key.as_deref()
            .map(|s| {
                let short = if s.len() > 20 { &s[..20] } else { s };
                format!(" ({})", short)
            })
            .unwrap_or_default();
        format!(
            "{} {}{} | {}ms | {}tok{} | {}",
            status, self.model, attempt, self.latency_ms, self.usage_total_tokens, session, content_preview
        )
    }
}

/// Thread-safe ring buffer for LLM activity log entries
struct LlmActivityLog {
    entries: VecDeque<LlmLogEntry>,
    capacity: usize,
    total_count: u64,
}

impl LlmActivityLog {
    fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity),
            capacity,
            total_count: 0,
        }
    }

    fn push(&mut self, entry: LlmLogEntry) {
        if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
        self.total_count += 1;
    }

    fn recent(&self, limit: usize) -> Vec<LlmLogEntry> {
        self.entries
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect()
    }

    fn all(&self) -> Vec<LlmLogEntry> {
        self.entries.iter().rev().cloned().collect()
    }

    fn find_by_id(&self, id: &str) -> Option<LlmLogEntry> {
        self.entries.iter().find(|e| e.id == id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_llm_log_entry_new() {
        let entry = LlmLogEntry::new("moonshot/kimi-k2.5");
        assert_eq!(entry.model, "moonshot/kimi-k2.5");
        assert!(!entry.id.is_empty());
        assert!(!entry.timestamp.is_empty());
        assert_eq!(entry.messages_count, 0);
        assert!(entry.error.is_none());
        assert!(entry.response_content.is_none());
    }

    #[test]
    fn test_llm_log_entry_summary_success() {
        let mut entry = LlmLogEntry::new("test/model");
        entry.response_content = Some("Hello, world!".to_string());
        entry.latency_ms = 150;
        entry.usage_total_tokens = 42;
        let summary = entry.summary();
        assert!(summary.contains("✅"));
        assert!(summary.contains("test/model"));
        assert!(summary.contains("150ms"));
        assert!(summary.contains("42tok"));
        assert!(summary.contains("Hello, world!"));
    }

    #[test]
    fn test_llm_log_entry_summary_error() {
        let mut entry = LlmLogEntry::new("test/model");
        entry.error = Some("connection refused".to_string());
        entry.latency_ms = 50;
        let summary = entry.summary();
        assert!(summary.contains("❌"));
    }

    #[test]
    fn test_llm_log_entry_summary_tool_calls() {
        let mut entry = LlmLogEntry::new("test/model");
        entry.response_tool_calls = 2;
        entry.tool_call_names = vec!["exec".to_string(), "read_file".to_string()];
        entry.latency_ms = 200;
        entry.usage_total_tokens = 100;
        let summary = entry.summary();
        assert!(summary.contains("2 tool call(s)"));
        assert!(summary.contains("exec"));
        assert!(summary.contains("read_file"));
    }

    #[test]
    fn test_llm_log_entry_summary_long_content_truncated() {
        let mut entry = LlmLogEntry::new("test/model");
        entry.response_content = Some("x".repeat(200));
        entry.latency_ms = 100;
        entry.usage_total_tokens = 50;
        let summary = entry.summary();
        assert!(summary.contains("..."));
        assert!(summary.len() < 300);
    }

    #[test]
    fn test_ring_buffer_push_and_recent() {
        let mut log = LlmActivityLog::new(3);
        for i in 0..5 {
            let mut entry = LlmLogEntry::new("model");
            entry.latency_ms = i as u64;
            log.push(entry);
        }
        // Should only keep last 3
        assert_eq!(log.entries.len(), 3);
        assert_eq!(log.total_count, 5);

        let recent = log.recent(2);
        assert_eq!(recent.len(), 2);
        // Newest first
        assert_eq!(recent[0].latency_ms, 4);
        assert_eq!(recent[1].latency_ms, 3);
    }

    #[test]
    fn test_ring_buffer_find_by_id() {
        let mut log = LlmActivityLog::new(10);
        let mut entry = LlmLogEntry::new("model");
        let id = entry.id.clone();
        entry.latency_ms = 999;
        log.push(entry);

        let found = log.find_by_id(&id);
        assert!(found.is_some());
        assert_eq!(found.unwrap().latency_ms, 999);

        assert!(log.find_by_id("nonexistent").is_none());
    }

    #[test]
    fn test_ring_buffer_all() {
        let mut log = LlmActivityLog::new(5);
        for i in 0..3 {
            let mut entry = LlmLogEntry::new("model");
            entry.latency_ms = i as u64;
            log.push(entry);
        }
        let all = log.all();
        assert_eq!(all.len(), 3);
        // Newest first
        assert_eq!(all[0].latency_ms, 2);
        assert_eq!(all[2].latency_ms, 0);
    }

    #[test]
    fn test_ring_buffer_capacity_eviction() {
        let mut log = LlmActivityLog::new(2);
        let mut e1 = LlmLogEntry::new("model");
        e1.latency_ms = 1;
        let mut e2 = LlmLogEntry::new("model");
        e2.latency_ms = 2;
        let mut e3 = LlmLogEntry::new("model");
        e3.latency_ms = 3;

        log.push(e1);
        log.push(e2);
        log.push(e3);

        assert_eq!(log.entries.len(), 2);
        // e1 should be evicted
        let all = log.all();
        assert_eq!(all[0].latency_ms, 3);
        assert_eq!(all[1].latency_ms, 2);
    }

    #[test]
    fn test_llm_log_entry_serialization() {
        let entry = LlmLogEntry::new("test/model");
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("test/model"));
        let deserialized: LlmLogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.model, "test/model");
        assert_eq!(deserialized.id, entry.id);
    }

    #[test]
    fn test_session_context_set_and_clear() {
        set_session_context("tg:main:123:456");
        assert_eq!(current_session_key(), Some("tg:main:123:456".to_string()));

        let entry = LlmLogEntry::new("test/model");
        assert_eq!(entry.session_key, Some("tg:main:123:456".to_string()));

        clear_session_context();
        assert_eq!(current_session_key(), None);

        let entry2 = LlmLogEntry::new("test/model");
        assert!(entry2.session_key.is_none());
    }

    #[test]
    fn test_log_stats_default() {
        let stats = LogStats::default();
        assert_eq!(stats.total_recorded, 0);
        assert_eq!(stats.buffered, 0);
        assert_eq!(stats.errors, 0);
        assert_eq!(stats.avg_latency_ms, 0);
    }

    #[test]
    fn test_log_stats_serialization() {
        let stats = LogStats {
            total_recorded: 10,
            buffered: 5,
            errors: 2,
            avg_latency_ms: 150,
        };
        let json = serde_json::to_string(&stats).unwrap();
        assert!(json.contains("\"total_recorded\":10"));
        assert!(json.contains("\"errors\":2"));
        let deserialized: LogStats = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.avg_latency_ms, 150);
    }
}
