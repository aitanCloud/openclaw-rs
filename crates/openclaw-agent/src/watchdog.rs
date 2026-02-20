//! Activity-based watchdog timeout.
//!
//! Instead of hard wall-clock timeouts that kill active work, the watchdog
//! monitors for *inactivity*. As long as the agent is making progress (LLM
//! streaming, tool calls, round transitions), the watchdog stays quiet.
//! Only when there's been no activity for `idle_timeout` does it trigger
//! graceful cancellation via the cancel token.
//!
//! A max wall-clock safety net prevents truly runaway tasks, but is set
//! generously (10+ minutes) so legitimate long-running work isn't killed.
//!
//! This mirrors how professional CI/CD systems (GitHub Actions, Travis CI)
//! handle timeouts: "no output timeout" + "max job timeout".

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// Reason the watchdog triggered cancellation.
#[derive(Debug, Clone, PartialEq)]
pub enum TimeoutReason {
    /// No activity (stream events, tool calls) for longer than the idle threshold.
    Idle(Duration),
    /// Absolute wall-clock limit exceeded (safety net).
    WallClock(Duration),
}

impl std::fmt::Display for TimeoutReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TimeoutReason::Idle(d) => write!(f, "no activity for {}s", d.as_secs()),
            TimeoutReason::WallClock(d) => write!(f, "max wall-clock {}s exceeded", d.as_secs()),
        }
    }
}

/// Activity-based watchdog that cancels via a `CancellationToken` when
/// the monitored task stops making progress.
pub struct ActivityWatchdog {
    last_activity_ms: Arc<AtomicU64>,
    cancel_token: CancellationToken,
    idle_timeout: Duration,
    max_wall_clock: Duration,
}

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl ActivityWatchdog {
    /// Create a new watchdog.
    ///
    /// - `idle_timeout`: cancel if no `touch()` call for this long (e.g., 60s)
    /// - `max_wall_clock`: absolute max duration (safety net, e.g., 600s)
    /// - `cancel_token`: the token to cancel when timeout triggers
    pub fn new(
        idle_timeout: Duration,
        max_wall_clock: Duration,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            last_activity_ms: Arc::new(AtomicU64::new(epoch_ms())),
            cancel_token,
            idle_timeout,
            max_wall_clock,
        }
    }

    /// Signal that the task is still making progress.
    /// Call this on every stream event, tool call start/end, round transition, etc.
    #[inline]
    pub fn touch(&self) {
        self.last_activity_ms.store(epoch_ms(), Ordering::Relaxed);
    }

    /// Spawn the watchdog background task. Returns a handle that should be
    /// aborted when the monitored task completes (to clean up the watchdog).
    ///
    /// The watchdog checks every 5 seconds. When it triggers, it cancels
    /// the token gracefully — the agent runtime will see the cancellation
    /// at the next round boundary and return a clean result.
    pub fn spawn(&self, label: &str) -> JoinHandle<Option<TimeoutReason>> {
        let last_activity = Arc::clone(&self.last_activity_ms);
        let cancel = self.cancel_token.clone();
        let idle_timeout_ms = self.idle_timeout.as_millis() as u64;
        let max_wall_clock = self.max_wall_clock;
        let idle_timeout = self.idle_timeout;
        let label = label.to_string();
        let start = Instant::now();

        tokio::spawn(async move {
            let check_interval = Duration::from_secs(5);

            loop {
                tokio::time::sleep(check_interval).await;

                // Already cancelled externally (e.g., user /cancel)
                if cancel.is_cancelled() {
                    return None;
                }

                let now_ms = epoch_ms();
                let last = last_activity.load(Ordering::Relaxed);
                let idle_ms = now_ms.saturating_sub(last);
                let elapsed = start.elapsed();

                // Check idle timeout
                if idle_ms > idle_timeout_ms {
                    warn!(
                        "Watchdog [{}]: idle for {:.0}s (threshold: {}s) — cancelling",
                        label,
                        idle_ms as f64 / 1000.0,
                        idle_timeout.as_secs()
                    );
                    cancel.cancel();
                    return Some(TimeoutReason::Idle(idle_timeout));
                }

                // Check absolute wall-clock safety net
                if elapsed > max_wall_clock {
                    warn!(
                        "Watchdog [{}]: wall-clock {:.0}s exceeded max {}s — cancelling",
                        label,
                        elapsed.as_secs_f64(),
                        max_wall_clock.as_secs()
                    );
                    cancel.cancel();
                    return Some(TimeoutReason::WallClock(max_wall_clock));
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_watchdog_no_timeout_when_active() {
        let token = CancellationToken::new();
        let wd = ActivityWatchdog::new(
            Duration::from_millis(200),
            Duration::from_secs(10),
            token.clone(),
        );

        let handle = wd.spawn("test-active");

        // Touch repeatedly — should not timeout
        for _ in 0..5 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            wd.touch();
        }

        // Still alive
        assert!(!token.is_cancelled());
        handle.abort();
    }

    #[tokio::test]
    async fn test_watchdog_idle_timeout() {
        let token = CancellationToken::new();
        let wd = ActivityWatchdog::new(
            Duration::from_millis(100), // very short idle for test
            Duration::from_secs(60),
            token.clone(),
        );

        let handle = wd.spawn("test-idle");

        // Don't touch — should timeout after idle period + check interval
        tokio::time::sleep(Duration::from_secs(6)).await;

        assert!(token.is_cancelled());
        let reason = handle.await.unwrap();
        assert!(matches!(reason, Some(TimeoutReason::Idle(_))));
    }

    #[tokio::test]
    async fn test_watchdog_wall_clock_timeout() {
        let token = CancellationToken::new();
        let wd = ActivityWatchdog::new(
            Duration::from_secs(60), // long idle (won't trigger)
            Duration::from_millis(100), // very short wall clock for test
            token.clone(),
        );

        let handle = wd.spawn("test-wall");

        // Keep touching but wall clock is tiny
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_millis(50));
            wd.touch();
        }

        tokio::time::sleep(Duration::from_secs(6)).await;
        assert!(token.is_cancelled());
        let reason = handle.await.unwrap();
        assert!(matches!(reason, Some(TimeoutReason::WallClock(_))));
    }

    #[tokio::test]
    async fn test_watchdog_external_cancel() {
        let token = CancellationToken::new();
        let wd = ActivityWatchdog::new(
            Duration::from_secs(60),
            Duration::from_secs(60),
            token.clone(),
        );

        let handle = wd.spawn("test-external");

        // Cancel externally
        token.cancel();
        tokio::time::sleep(Duration::from_secs(6)).await;

        let reason = handle.await.unwrap();
        assert_eq!(reason, None); // None = external cancel, not watchdog
    }
}
