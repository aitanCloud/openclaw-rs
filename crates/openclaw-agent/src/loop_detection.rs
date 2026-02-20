//! Tool loop detection — detects and blocks repetitive tool call patterns.
//!
//! Inspired by OpenClaw's Node.js `tool-loop-detection` module.
//! Tracks a sliding window of recent tool calls, hashing (tool_name + args).
//! Detects three patterns:
//!   1. Generic repeat: same tool+args called N times
//!   2. No-progress repeat: same tool+args producing identical results
//!   3. Ping-pong: alternating between two tool call patterns
//!
//! Returns a `LoopVerdict` that the runtime uses to either allow, warn, or block.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use tracing::warn;

/// Configuration for loop detection thresholds.
#[derive(Debug, Clone)]
pub struct LoopDetectionConfig {
    /// Max tool calls to track in the sliding window.
    pub history_size: usize,
    /// Number of identical calls before emitting a warning (injected into LLM context).
    pub warning_threshold: usize,
    /// Number of identical calls before blocking the tool call entirely.
    pub critical_threshold: usize,
    /// Global circuit breaker: total no-progress calls before hard stop.
    pub circuit_breaker_threshold: usize,
}

impl Default for LoopDetectionConfig {
    fn default() -> Self {
        Self {
            history_size: 30,
            warning_threshold: 8,
            critical_threshold: 15,
            circuit_breaker_threshold: 25,
        }
    }
}

/// Result of checking a tool call against the loop detector.
#[derive(Debug, Clone)]
pub enum LoopVerdict {
    /// Tool call is fine, proceed normally.
    Allow,
    /// Tool call looks repetitive — inject a warning message to the LLM.
    Warn {
        message: String,
        detector: &'static str,
        count: usize,
    },
    /// Tool call is blocked — return error to the LLM instead of executing.
    Block {
        message: String,
        detector: &'static str,
        count: usize,
    },
}

/// A recorded tool call in the history.
#[derive(Debug, Clone)]
struct ToolCallRecord {
    tool_name: String,
    args_hash: u64,
    result_hash: Option<u64>,
}

/// Per-session loop detection state. Create one per agent turn.
#[derive(Debug)]
pub struct LoopDetector {
    config: LoopDetectionConfig,
    history: Vec<ToolCallRecord>,
    /// Total tool calls blocked in this session.
    pub blocked_count: usize,
}

impl LoopDetector {
    pub fn new() -> Self {
        Self::with_config(LoopDetectionConfig::default())
    }

    pub fn with_config(config: LoopDetectionConfig) -> Self {
        Self {
            history: Vec::with_capacity(config.history_size),
            config,
            blocked_count: 0,
        }
    }

    /// Check whether a tool call should be allowed, warned, or blocked.
    /// Call this BEFORE executing the tool.
    pub fn check(&self, tool_name: &str, args: &serde_json::Value) -> LoopVerdict {
        let args_hash = hash_value(args);
        let current_hash = hash_call(tool_name, args_hash);

        // Count identical calls (same tool + same args)
        let identical_count = self.history.iter()
            .filter(|r| r.tool_name == tool_name && r.args_hash == args_hash)
            .count();

        // Count no-progress calls (same tool + same args + same result)
        let no_progress_streak = self.get_no_progress_streak(tool_name, args_hash);

        // Check global circuit breaker
        if no_progress_streak >= self.config.circuit_breaker_threshold {
            return LoopVerdict::Block {
                message: format!(
                    "BLOCKED: You have called `{}` {} times with identical arguments and identical results. \
                     This is a stuck loop with no progress. Stop retrying this approach and try something different, \
                     or report the task as complete/failed.",
                    tool_name, no_progress_streak
                ),
                detector: "circuit_breaker",
                count: no_progress_streak,
            };
        }

        // Check critical threshold (identical calls regardless of result)
        if identical_count >= self.config.critical_threshold {
            return LoopVerdict::Block {
                message: format!(
                    "BLOCKED: You have called `{}` {} times with identical arguments. \
                     This appears to be a stuck loop. Stop retrying and either try a different approach \
                     or report the task as complete/failed.",
                    tool_name, identical_count
                ),
                detector: "generic_repeat_critical",
                count: identical_count,
            };
        }

        // Check no-progress at critical level
        if no_progress_streak >= self.config.critical_threshold {
            return LoopVerdict::Block {
                message: format!(
                    "BLOCKED: You have called `{}` {} times with identical arguments and no progress \
                     (same result each time). This is a stuck loop. Try a different approach.",
                    tool_name, no_progress_streak
                ),
                detector: "no_progress_critical",
                count: no_progress_streak,
            };
        }

        // Check ping-pong pattern
        let ping_pong = self.detect_ping_pong(current_hash);
        if ping_pong >= self.config.critical_threshold {
            return LoopVerdict::Block {
                message: format!(
                    "BLOCKED: You are alternating between repeated tool-call patterns ({} consecutive calls) \
                     with no progress. This is a stuck ping-pong loop. Try a different approach.",
                    ping_pong
                ),
                detector: "ping_pong_critical",
                count: ping_pong,
            };
        }

        // Warning thresholds
        if no_progress_streak >= self.config.warning_threshold {
            return LoopVerdict::Warn {
                message: format!(
                    "WARNING: You have called `{}` {} times with identical arguments and no progress. \
                     If this is not making progress, stop retrying and try a different approach.",
                    tool_name, no_progress_streak
                ),
                detector: "no_progress_warning",
                count: no_progress_streak,
            };
        }

        if identical_count >= self.config.warning_threshold {
            return LoopVerdict::Warn {
                message: format!(
                    "WARNING: You have called `{}` {} times with identical arguments. \
                     If this is not making progress, stop retrying and try a different approach.",
                    tool_name, identical_count
                ),
                detector: "generic_repeat_warning",
                count: identical_count,
            };
        }

        if ping_pong >= self.config.warning_threshold {
            return LoopVerdict::Warn {
                message: format!(
                    "WARNING: You appear to be alternating between the same tool-call patterns ({} times). \
                     If you're not making progress, stop and try a different approach.",
                    ping_pong
                ),
                detector: "ping_pong_warning",
                count: ping_pong,
            };
        }

        LoopVerdict::Allow
    }

    /// Record a tool call (before execution). Call `record_outcome` after execution.
    pub fn record_call(&mut self, tool_name: &str, args: &serde_json::Value) {
        let args_hash = hash_value(args);
        self.history.push(ToolCallRecord {
            tool_name: tool_name.to_string(),
            args_hash,
            result_hash: None,
        });
        // Maintain sliding window
        if self.history.len() > self.config.history_size {
            self.history.remove(0);
        }
    }

    /// Record the outcome of the most recent call for the given tool.
    /// This enables no-progress detection.
    pub fn record_outcome(&mut self, tool_name: &str, args: &serde_json::Value, output: &str) {
        let args_hash = hash_value(args);
        let result_hash = hash_string(output);

        // Find the most recent matching call without a result
        for record in self.history.iter_mut().rev() {
            if record.tool_name == tool_name
                && record.args_hash == args_hash
                && record.result_hash.is_none()
            {
                record.result_hash = Some(result_hash);
                break;
            }
        }
    }

    /// Record that a tool call was blocked.
    pub fn record_block(&mut self) {
        self.blocked_count += 1;
    }

    /// Count consecutive no-progress calls: same tool+args with same result hash.
    fn get_no_progress_streak(&self, tool_name: &str, args_hash: u64) -> usize {
        let mut streak = 0;
        let mut latest_result: Option<u64> = None;

        for record in self.history.iter().rev() {
            if record.tool_name != tool_name || record.args_hash != args_hash {
                continue;
            }
            match (record.result_hash, latest_result) {
                (Some(rh), None) => {
                    latest_result = Some(rh);
                    streak = 1;
                }
                (Some(rh), Some(latest)) if rh == latest => {
                    streak += 1;
                }
                (Some(_), Some(_)) => break, // Different result — progress made
                (None, _) => continue, // No result recorded yet
            }
        }
        streak
    }

    /// Detect ping-pong: alternating between two different call signatures.
    fn detect_ping_pong(&self, current_hash: u64) -> usize {
        if self.history.len() < 4 {
            return 0;
        }

        // Get the last call's hash
        let last = match self.history.last() {
            Some(r) => hash_call(&r.tool_name, r.args_hash),
            None => return 0,
        };

        // Find the "other" pattern
        let mut other_hash = None;
        for record in self.history.iter().rev().skip(1) {
            let h = hash_call(&record.tool_name, record.args_hash);
            if h != last {
                other_hash = Some(h);
                break;
            }
        }

        let other = match other_hash {
            Some(h) => h,
            None => return 0,
        };

        // Count alternating tail
        let mut alternating = 0;
        for record in self.history.iter().rev() {
            let h = hash_call(&record.tool_name, record.args_hash);
            let expected = if alternating % 2 == 0 { last } else { other };
            if h != expected {
                break;
            }
            alternating += 1;
        }

        if alternating >= 4 && current_hash == other {
            alternating
        } else {
            0
        }
    }
}

/// Hash a serde_json::Value deterministically.
fn hash_value(value: &serde_json::Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    // Use canonical JSON string for hashing
    let s = serde_json::to_string(value).unwrap_or_default();
    s.hash(&mut hasher);
    hasher.finish()
}

/// Hash a string.
fn hash_string(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

/// Combined hash of tool name + args hash.
fn hash_call(tool_name: &str, args_hash: u64) -> u64 {
    let mut hasher = DefaultHasher::new();
    tool_name.hash(&mut hasher);
    args_hash.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_allow_normal_calls() {
        let detector = LoopDetector::new();
        let verdict = detector.check("exec", &json!({"command": "ls"}));
        assert!(matches!(verdict, LoopVerdict::Allow));
    }

    #[test]
    fn test_warn_on_repeated_calls() {
        let mut detector = LoopDetector::new();
        let args = json!({"command": "cat /tmp/test.txt"});

        for _ in 0..8 {
            detector.record_call("exec", &args);
        }

        let verdict = detector.check("exec", &args);
        assert!(matches!(verdict, LoopVerdict::Warn { .. }));
    }

    #[test]
    fn test_block_on_critical_repeated_calls() {
        let mut detector = LoopDetector::new();
        let args = json!({"command": "cat /tmp/test.txt"});

        for _ in 0..15 {
            detector.record_call("exec", &args);
        }

        let verdict = detector.check("exec", &args);
        assert!(matches!(verdict, LoopVerdict::Block { .. }));
    }

    #[test]
    fn test_no_progress_detection() {
        let mut detector = LoopDetector::new();
        let args = json!({"command": "git status"});

        for _ in 0..8 {
            detector.record_call("exec", &args);
            detector.record_outcome("exec", &args, "nothing to commit, working tree clean");
        }

        let verdict = detector.check("exec", &args);
        assert!(matches!(verdict, LoopVerdict::Warn { detector: "no_progress_warning", .. }));
    }

    #[test]
    fn test_progress_resets_streak() {
        let mut detector = LoopDetector::new();
        let args = json!({"command": "git status"});

        for i in 0..10 {
            detector.record_call("exec", &args);
            // Each call has a different result — progress is being made
            detector.record_outcome("exec", &args, &format!("result {}", i));
        }

        let verdict = detector.check("exec", &args);
        // Should warn on generic repeat (10 identical args) but not no-progress
        assert!(matches!(verdict, LoopVerdict::Warn { detector: "generic_repeat_warning", .. }));
    }

    #[test]
    fn test_different_args_not_counted() {
        let mut detector = LoopDetector::new();

        for i in 0..20 {
            let args = json!({"command": format!("echo {}", i)});
            detector.record_call("exec", &args);
        }

        // Each call has different args, so no loop detected
        let verdict = detector.check("exec", &json!({"command": "echo 20"}));
        assert!(matches!(verdict, LoopVerdict::Allow));
    }

    #[test]
    fn test_circuit_breaker() {
        let mut detector = LoopDetector::new();
        let args = json!({"command": "curl http://localhost:3000/health"});

        for _ in 0..25 {
            detector.record_call("exec", &args);
            detector.record_outcome("exec", &args, "connection refused");
        }

        let verdict = detector.check("exec", &args);
        assert!(matches!(verdict, LoopVerdict::Block { detector: "circuit_breaker", .. }));
    }

    #[test]
    fn test_sliding_window_eviction() {
        let mut detector = LoopDetector::new();
        let args = json!({"command": "ls"});

        // Fill history with "ls" calls
        for _ in 0..8 {
            detector.record_call("exec", &args);
        }

        // Now fill with different calls to push "ls" out of the window
        for i in 0..30 {
            let different_args = json!({"command": format!("echo {}", i)});
            detector.record_call("exec", &different_args);
        }

        // "ls" should be evicted from the window
        let verdict = detector.check("exec", &args);
        assert!(matches!(verdict, LoopVerdict::Allow));
    }
}
