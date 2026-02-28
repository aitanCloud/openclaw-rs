use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::app::errors::AppError;
use crate::domain::events::EventEnvelope;
use crate::domain::ports::EventStore;
use crate::infra::errors::InfraError;

// ── Constants ──

/// Maximum number of destructive operations per janitor tick (prevents I/O storms).
pub const MAX_DELETIONS_PER_TICK: u32 = 100;

/// Default disk space threshold in bytes (1 GB).
pub const DEFAULT_DISK_THRESHOLD_BYTES: u64 = 1_073_741_824;

/// Hysteresis multiplier for disk guard recovery (1.5x threshold).
pub const DISK_RECOVERY_MULTIPLIER: f64 = 1.5;

// ── JanitorConfig ──

/// Configurable retention settings for the janitor.
///
/// All defaults match the architecture specification (Section 24).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JanitorConfig {
    /// Days to retain artifact content before tombstoning (default: 30).
    pub artifact_retain_days: u32,
    /// Days to retain run logs before purging (default: 14).
    pub log_retain_days: u32,
    /// Hours to retain worktrees for succeeded runs (default: 4).
    pub worktree_retain_hours_success: u32,
    /// Hours to retain worktrees for failed runs (default: 24).
    pub worktree_retain_hours_failed: u32,
    /// Minimum free disk bytes before blocking (default: 1 GB).
    pub disk_threshold_bytes: u64,
    /// Maximum deletions per cleanup tick (default: 100).
    pub max_deletions_per_tick: u32,
}

impl Default for JanitorConfig {
    fn default() -> Self {
        Self {
            artifact_retain_days: 30,
            log_retain_days: 14,
            worktree_retain_hours_success: 4,
            worktree_retain_hours_failed: 24,
            disk_threshold_bytes: DEFAULT_DISK_THRESHOLD_BYTES,
            max_deletions_per_tick: MAX_DELETIONS_PER_TICK,
        }
    }
}

// ── Result tracking ──

/// Summary of a single janitor cleanup run.
#[derive(Debug, Clone, Default)]
pub struct CleanupResult {
    pub artifacts_purged: u32,
    pub artifact_bytes_freed: u64,
    pub logs_purged: u32,
    pub log_bytes_freed: u64,
    pub worktrees_cleaned: u32,
    pub total_operations: u32,
}

impl CleanupResult {
    /// Whether the rate limit has been reached.
    pub fn rate_limited(&self, max: u32) -> bool {
        self.total_operations >= max
    }
}

// ── DB row types ──

/// Row type for expired artifact query.
#[derive(sqlx::FromRow)]
struct ExpiredArtifactRow {
    id: Uuid,
    #[allow(dead_code)]
    instance_id: Uuid,
    content_size: Option<i64>,
}

/// Row type for expired log query.
#[derive(sqlx::FromRow)]
struct ExpiredLogRow {
    id: Uuid,
    #[allow(dead_code)]
    instance_id: Uuid,
    log_stdout_size: Option<i64>,
    log_stderr_size: Option<i64>,
}

/// Row type for expired worktree query.
#[derive(sqlx::FromRow)]
struct ExpiredWorktreeRow {
    id: Uuid,
    instance_id: Uuid,
    worktree_path: String,
    state: String,
}

// ── Janitor ──

/// The Janitor handles periodic cleanup of expired artifacts, logs, and worktrees.
///
/// Follows the Reconciler pattern: holds a PgPool and EventStore, operates
/// per-instance, emits domain events for all destructive operations.
///
/// Actor: System (all events use `{ "kind": "System", "id": "janitor" }`)
///
/// Rate limit: max `config.max_deletions_per_tick` destructive operations per
/// `run_cleanup()` invocation to prevent I/O storms.
pub struct Janitor {
    pool: PgPool,
    event_store: Arc<dyn EventStore>,
    config: JanitorConfig,
}

impl Janitor {
    pub fn new(pool: PgPool, event_store: Arc<dyn EventStore>, config: JanitorConfig) -> Self {
        Self {
            pool,
            event_store,
            config,
        }
    }

    /// Run all cleanup phases for a given instance.
    ///
    /// Phase 1 (sequential, per-instance):
    ///   a. Purge expired artifact content (tombstone)
    ///   b. Purge expired logs (nullify)
    ///   c. Clean expired worktrees (git worktree remove + clear DB)
    ///   d. Emit cleanup events
    ///
    /// Precondition: caller should check maintenance_mode before calling.
    /// Rate limit: stops at `config.max_deletions_per_tick` total operations.
    pub async fn run_cleanup(&self, instance_id: Uuid) -> Result<CleanupResult, AppError> {
        let mut result = CleanupResult::default();
        let max = self.config.max_deletions_per_tick;

        tracing::info!(
            instance_id = %instance_id,
            "janitor: starting cleanup"
        );

        // Phase 1a: Purge expired artifacts
        if !result.rate_limited(max) {
            self.purge_expired_artifacts(instance_id, &mut result)
                .await?;
        }

        // Phase 1b: Purge expired logs
        if !result.rate_limited(max) {
            self.purge_expired_logs(instance_id, &mut result).await?;
        }

        // Phase 1c: Clean expired worktrees
        if !result.rate_limited(max) {
            self.clean_expired_worktrees(instance_id, &mut result)
                .await?;
        }

        tracing::info!(
            instance_id = %instance_id,
            artifacts_purged = result.artifacts_purged,
            logs_purged = result.logs_purged,
            worktrees_cleaned = result.worktrees_cleaned,
            total_operations = result.total_operations,
            "janitor: cleanup complete"
        );

        Ok(result)
    }

    /// Purge expired artifact content (tombstone: set content='{}', deleted_at=now()).
    ///
    /// Artifacts are expired when:
    /// - The artifact's run is in a terminal state
    /// - The artifact was created more than `artifact_retain_days` ago
    /// - The artifact has not already been tombstoned (deleted_at IS NULL)
    ///
    /// Uses SELECT ... FOR UPDATE to lock rows before modification.
    pub async fn purge_expired_artifacts(
        &self,
        instance_id: Uuid,
        result: &mut CleanupResult,
    ) -> Result<(), AppError> {
        let retain_days = self.config.artifact_retain_days as i64;
        let remaining = self.config.max_deletions_per_tick.saturating_sub(result.total_operations);

        if remaining == 0 {
            return Ok(());
        }

        // Find expired artifacts with per-row locking
        let expired = sqlx::query_as::<_, ExpiredArtifactRow>(
            r#"
            SELECT a.id, a.instance_id,
                   octet_length(a.content::text)::bigint AS content_size
            FROM orch_artifacts a
            JOIN orch_runs r ON r.id = a.run_id
            WHERE a.instance_id = $1
              AND a.deleted_at IS NULL
              AND r.state IN ('completed', 'failed', 'timed_out', 'cancelled', 'abandoned')
              AND r.finished_at IS NOT NULL
              AND r.finished_at < NOW() - make_interval(days => $2)
            ORDER BY a.created_at ASC
            LIMIT $3
            FOR UPDATE OF a SKIP LOCKED
            "#,
        )
        .bind(instance_id)
        .bind(retain_days)
        .bind(remaining as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

        if expired.is_empty() {
            return Ok(());
        }

        let mut total_bytes: u64 = 0;
        let mut purged_count: u32 = 0;

        for artifact in &expired {
            // Tombstone: set content to empty JSON, mark deleted_at
            sqlx::query(
                r#"
                UPDATE orch_artifacts
                SET content = '{}'::jsonb, deleted_at = NOW()
                WHERE id = $1 AND deleted_at IS NULL
                "#,
            )
            .bind(artifact.id)
            .execute(&self.pool)
            .await
            .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

            total_bytes += artifact.content_size.unwrap_or(0) as u64;
            purged_count += 1;
        }

        if purged_count > 0 {
            // Emit ArtifactsPurged event
            let event = EventEnvelope {
                event_id: Uuid::new_v4(),
                instance_id,
                seq: 0,
                event_type: "ArtifactsPurged".to_string(),
                event_version: 1,
                payload: serde_json::json!({
                    "count": purged_count,
                    "bytes_freed": total_bytes,
                    "actor": { "kind": "System", "id": "janitor" }
                }),
                idempotency_key: None,
                correlation_id: None,
                causation_id: None,
                occurred_at: Utc::now(),
                recorded_at: Utc::now(),
            };

            self.event_store.emit(event).await.map_err(|e| {
                AppError::Infra(InfraError::Database(format!(
                    "emit ArtifactsPurged event: {e}"
                )))
            })?;

            tracing::info!(
                instance_id = %instance_id,
                count = purged_count,
                bytes_freed = total_bytes,
                "janitor: artifacts purged"
            );
        }

        result.artifacts_purged += purged_count;
        result.artifact_bytes_freed += total_bytes;
        result.total_operations += purged_count;

        Ok(())
    }

    /// Purge expired logs (nullify log_stdout, log_stderr on terminal runs).
    ///
    /// Logs are expired when:
    /// - The run is in a terminal state
    /// - The run finished more than `log_retain_days` ago
    /// - At least one of log_stdout or log_stderr is not null
    ///
    /// Uses SELECT ... FOR UPDATE to lock rows before modification.
    pub async fn purge_expired_logs(
        &self,
        instance_id: Uuid,
        result: &mut CleanupResult,
    ) -> Result<(), AppError> {
        let retain_days = self.config.log_retain_days as i64;
        let remaining = self.config.max_deletions_per_tick.saturating_sub(result.total_operations);

        if remaining == 0 {
            return Ok(());
        }

        // Find expired logs with per-row locking
        let expired = sqlx::query_as::<_, ExpiredLogRow>(
            r#"
            SELECT id, instance_id,
                   octet_length(log_stdout)::bigint AS log_stdout_size,
                   octet_length(log_stderr)::bigint AS log_stderr_size
            FROM orch_runs
            WHERE instance_id = $1
              AND state IN ('completed', 'failed', 'timed_out', 'cancelled', 'abandoned')
              AND finished_at IS NOT NULL
              AND finished_at < NOW() - make_interval(days => $2)
              AND (log_stdout IS NOT NULL OR log_stderr IS NOT NULL)
            ORDER BY finished_at ASC
            LIMIT $3
            FOR UPDATE SKIP LOCKED
            "#,
        )
        .bind(instance_id)
        .bind(retain_days)
        .bind(remaining as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

        if expired.is_empty() {
            return Ok(());
        }

        let mut total_bytes: u64 = 0;
        let mut purged_count: u32 = 0;

        for run in &expired {
            sqlx::query(
                r#"
                UPDATE orch_runs
                SET log_stdout = NULL, log_stderr = NULL
                WHERE id = $1
                "#,
            )
            .bind(run.id)
            .execute(&self.pool)
            .await
            .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

            let stdout_size = run.log_stdout_size.unwrap_or(0) as u64;
            let stderr_size = run.log_stderr_size.unwrap_or(0) as u64;
            total_bytes += stdout_size + stderr_size;
            purged_count += 1;
        }

        if purged_count > 0 {
            // Emit LogsPurged event
            let event = EventEnvelope {
                event_id: Uuid::new_v4(),
                instance_id,
                seq: 0,
                event_type: "LogsPurged".to_string(),
                event_version: 1,
                payload: serde_json::json!({
                    "count": purged_count,
                    "bytes_freed": total_bytes,
                    "actor": { "kind": "System", "id": "janitor" }
                }),
                idempotency_key: None,
                correlation_id: None,
                causation_id: None,
                occurred_at: Utc::now(),
                recorded_at: Utc::now(),
            };

            self.event_store.emit(event).await.map_err(|e| {
                AppError::Infra(InfraError::Database(format!("emit LogsPurged event: {e}")))
            })?;

            tracing::info!(
                instance_id = %instance_id,
                count = purged_count,
                bytes_freed = total_bytes,
                "janitor: logs purged"
            );
        }

        result.logs_purged += purged_count;
        result.log_bytes_freed += total_bytes;
        result.total_operations += purged_count;

        Ok(())
    }

    /// Clean expired worktrees (git worktree remove + clear worktree_path in DB).
    ///
    /// Worktrees are expired based on state-dependent retention:
    /// - Succeeded runs (`state = 'completed'`): `worktree_retain_hours_success`
    /// - Failed runs (`state IN ('failed', 'timed_out', 'cancelled', 'abandoned')`):
    ///   `worktree_retain_hours_failed`
    ///
    /// Uses SELECT ... FOR UPDATE to lock rows before modification.
    pub async fn clean_expired_worktrees(
        &self,
        instance_id: Uuid,
        result: &mut CleanupResult,
    ) -> Result<(), AppError> {
        let success_hours = self.config.worktree_retain_hours_success as i64;
        let failed_hours = self.config.worktree_retain_hours_failed as i64;
        let remaining = self.config.max_deletions_per_tick.saturating_sub(result.total_operations);

        if remaining == 0 {
            return Ok(());
        }

        // Find expired worktrees: state-dependent retention, with per-row locking
        let expired = sqlx::query_as::<_, ExpiredWorktreeRow>(
            r#"
            SELECT id, instance_id, worktree_path, state
            FROM orch_runs
            WHERE instance_id = $1
              AND worktree_path IS NOT NULL
              AND finished_at IS NOT NULL
              AND state IN ('completed', 'failed', 'timed_out', 'cancelled', 'abandoned')
              AND (
                (state = 'completed' AND finished_at < NOW() - make_interval(hours => $2))
                OR
                (state IN ('failed', 'timed_out', 'cancelled', 'abandoned')
                 AND finished_at < NOW() - make_interval(hours => $3))
              )
            ORDER BY finished_at ASC
            LIMIT $4
            FOR UPDATE SKIP LOCKED
            "#,
        )
        .bind(instance_id)
        .bind(success_hours)
        .bind(failed_hours)
        .bind(remaining as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

        if expired.is_empty() {
            return Ok(());
        }

        let mut cleaned_count: u32 = 0;

        for run in &expired {
            // Attempt to remove the worktree from disk
            let removal_result = remove_worktree(&run.worktree_path).await;

            match &removal_result {
                Ok(()) => {
                    tracing::info!(
                        run_id = %run.id,
                        path = %run.worktree_path,
                        "janitor: worktree removed from disk"
                    );
                }
                Err(e) => {
                    // If the path does not exist, treat as success (already cleaned).
                    // Otherwise log a warning but still clear the DB field.
                    if Path::new(&run.worktree_path).exists() {
                        tracing::warn!(
                            run_id = %run.id,
                            path = %run.worktree_path,
                            error = %e,
                            "janitor: worktree removal failed, clearing DB reference anyway"
                        );
                    } else {
                        tracing::info!(
                            run_id = %run.id,
                            path = %run.worktree_path,
                            "janitor: worktree path does not exist, clearing DB reference"
                        );
                    }
                }
            }

            // Clear worktree_path in DB regardless (the directory is either gone or we gave up)
            sqlx::query(
                r#"
                UPDATE orch_runs
                SET worktree_path = NULL
                WHERE id = $1
                "#,
            )
            .bind(run.id)
            .execute(&self.pool)
            .await
            .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

            // Emit WorktreeEvicted event
            let event = EventEnvelope {
                event_id: Uuid::new_v4(),
                instance_id: run.instance_id,
                seq: 0,
                event_type: "WorktreeEvicted".to_string(),
                event_version: 1,
                payload: serde_json::json!({
                    "run_id": run.id,
                    "reason": "retention_expired",
                    "state": run.state,
                    "path": run.worktree_path,
                    "actor": { "kind": "System", "id": "janitor" }
                }),
                idempotency_key: Some(format!("janitor-worktree-{}", run.id)),
                correlation_id: None,
                causation_id: None,
                occurred_at: Utc::now(),
                recorded_at: Utc::now(),
            };

            self.event_store.emit(event).await.map_err(|e| {
                AppError::Infra(InfraError::Database(format!(
                    "emit WorktreeEvicted event for run {}: {e}",
                    run.id
                )))
            })?;

            cleaned_count += 1;
        }

        if cleaned_count > 0 {
            tracing::info!(
                instance_id = %instance_id,
                count = cleaned_count,
                "janitor: worktrees cleaned"
            );
        }

        result.worktrees_cleaned += cleaned_count;
        result.total_operations += cleaned_count;

        Ok(())
    }

    /// Check available disk space on a given path.
    ///
    /// Returns `Ok(true)` if free space is above the threshold (healthy),
    /// `Ok(false)` if free space is below threshold (should block).
    ///
    /// The disk guard uses hysteresis: recovery requires `threshold * 1.5` free.
    pub async fn disk_guard_check(
        &self,
        path: &str,
        currently_blocked: bool,
    ) -> Result<DiskGuardStatus, AppError> {
        let free_bytes = get_free_disk_bytes(path).await?;
        let threshold = self.config.disk_threshold_bytes;
        let recovery_threshold = (threshold as f64 * DISK_RECOVERY_MULTIPLIER) as u64;

        let status = if currently_blocked {
            // Hysteresis: must exceed recovery threshold to unblock
            if free_bytes >= recovery_threshold {
                DiskGuardStatus::Healthy { free_bytes }
            } else {
                DiskGuardStatus::Critical { free_bytes }
            }
        } else {
            // Normal: block if below threshold
            if free_bytes >= threshold {
                DiskGuardStatus::Healthy { free_bytes }
            } else {
                DiskGuardStatus::Critical { free_bytes }
            }
        };

        tracing::info!(
            path = %path,
            free_bytes = free_bytes,
            threshold = threshold,
            recovery_threshold = recovery_threshold,
            currently_blocked = currently_blocked,
            status = ?status,
            "janitor: disk guard check"
        );

        Ok(status)
    }

    /// Get a reference to the janitor configuration.
    pub fn config(&self) -> &JanitorConfig {
        &self.config
    }
}

// ── Disk guard status ──

/// Result of a disk guard check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskGuardStatus {
    /// Free space is above threshold (or recovery threshold if was blocked).
    Healthy { free_bytes: u64 },
    /// Free space is below threshold (or recovery threshold if was blocked).
    Critical { free_bytes: u64 },
}

impl DiskGuardStatus {
    pub fn is_healthy(&self) -> bool {
        matches!(self, DiskGuardStatus::Healthy { .. })
    }

    pub fn free_bytes(&self) -> u64 {
        match self {
            DiskGuardStatus::Healthy { free_bytes } => *free_bytes,
            DiskGuardStatus::Critical { free_bytes } => *free_bytes,
        }
    }
}

// ── Worktree removal ──

/// Remove a git worktree by shelling out to `git worktree remove --force`.
///
/// This intentionally does NOT use the GitProvider trait -- the janitor
/// cleans up the filesystem directly.
async fn remove_worktree(path: &str) -> Result<(), AppError> {
    let output = tokio::process::Command::new("git")
        .args(["worktree", "remove", "--force", path])
        .output()
        .await
        .map_err(|e| AppError::Infra(InfraError::Io(format!("git worktree remove: {e}"))))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(AppError::Infra(InfraError::Io(format!(
            "git worktree remove failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            stderr.trim()
        ))))
    }
}

/// Get available disk bytes for the filesystem containing `path`.
///
/// Uses `statvfs` via `std::process::Command` calling `df` for portability.
async fn get_free_disk_bytes(path: &str) -> Result<u64, AppError> {
    // Use `df --output=avail -B1 <path>` for a single-value bytes output.
    let output = tokio::process::Command::new("df")
        .args(["--output=avail", "-B1", path])
        .output()
        .await
        .map_err(|e| AppError::Infra(InfraError::Io(format!("df command: {e}"))))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::Infra(InfraError::Io(format!(
            "df command failed: {}",
            stderr.trim()
        ))));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Output format: header line, then value line
    let value_line = stdout
        .lines()
        .nth(1)
        .ok_or_else(|| AppError::Infra(InfraError::Io("df output missing value line".into())))?
        .trim();

    value_line
        .parse::<u64>()
        .map_err(|e| AppError::Infra(InfraError::Io(format!("df parse error: {e}"))))
}

// ── Event builder helpers (test-only, extracted for testability) ──

/// Build an ArtifactsPurged event envelope.
#[cfg(test)]
pub(crate) fn build_artifacts_purged_event(
    instance_id: Uuid,
    count: u32,
    bytes_freed: u64,
) -> EventEnvelope {
    let now = Utc::now();
    EventEnvelope {
        event_id: Uuid::new_v4(),
        instance_id,
        seq: 0,
        event_type: "ArtifactsPurged".to_string(),
        event_version: 1,
        payload: serde_json::json!({
            "count": count,
            "bytes_freed": bytes_freed,
            "actor": { "kind": "System", "id": "janitor" }
        }),
        idempotency_key: None,
        correlation_id: None,
        causation_id: None,
        occurred_at: now,
        recorded_at: now,
    }
}

/// Build a LogsPurged event envelope.
#[cfg(test)]
pub(crate) fn build_logs_purged_event(
    instance_id: Uuid,
    count: u32,
    bytes_freed: u64,
) -> EventEnvelope {
    let now = Utc::now();
    EventEnvelope {
        event_id: Uuid::new_v4(),
        instance_id,
        seq: 0,
        event_type: "LogsPurged".to_string(),
        event_version: 1,
        payload: serde_json::json!({
            "count": count,
            "bytes_freed": bytes_freed,
            "actor": { "kind": "System", "id": "janitor" }
        }),
        idempotency_key: None,
        correlation_id: None,
        causation_id: None,
        occurred_at: now,
        recorded_at: now,
    }
}

/// Build a WorktreeEvicted event envelope.
#[cfg(test)]
pub(crate) fn build_worktree_evicted_event(
    instance_id: Uuid,
    run_id: Uuid,
    reason: &str,
    state: &str,
    path: &str,
) -> EventEnvelope {
    let now = Utc::now();
    EventEnvelope {
        event_id: Uuid::new_v4(),
        instance_id,
        seq: 0,
        event_type: "WorktreeEvicted".to_string(),
        event_version: 1,
        payload: serde_json::json!({
            "run_id": run_id,
            "reason": reason,
            "state": state,
            "path": path,
            "actor": { "kind": "System", "id": "janitor" }
        }),
        idempotency_key: Some(format!("janitor-worktree-{}", run_id)),
        correlation_id: None,
        causation_id: None,
        occurred_at: now,
        recorded_at: now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::errors::DomainError;

    // ── Mock EventStore ──

    struct RecordingEventStore {
        events: tokio::sync::Mutex<Vec<EventEnvelope>>,
    }

    impl RecordingEventStore {
        fn new() -> Self {
            Self {
                events: tokio::sync::Mutex::new(Vec::new()),
            }
        }

        #[allow(dead_code)]
        async fn emitted_events(&self) -> Vec<EventEnvelope> {
            self.events.lock().await.clone()
        }
    }

    #[async_trait::async_trait]
    impl EventStore for RecordingEventStore {
        async fn emit(&self, event: EventEnvelope) -> Result<i64, DomainError> {
            let mut events = self.events.lock().await;
            let seq = events.len() as i64 + 1;
            events.push(event);
            Ok(seq)
        }

        async fn replay(
            &self,
            _instance_id: Uuid,
            _since_seq: i64,
        ) -> Result<Vec<EventEnvelope>, DomainError> {
            Ok(Vec::new())
        }

        async fn head_seq(&self, _instance_id: Uuid) -> Result<i64, DomainError> {
            Ok(0)
        }
    }

    // ── Helpers ──

    fn lazy_pool() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://fake:fake@localhost/fake")
            .expect("lazy pool creation should not fail")
    }

    fn make_janitor(event_store: Arc<dyn EventStore>) -> Janitor {
        Janitor::new(lazy_pool(), event_store, JanitorConfig::default())
    }

    fn make_janitor_with_config(
        event_store: Arc<dyn EventStore>,
        config: JanitorConfig,
    ) -> Janitor {
        Janitor::new(lazy_pool(), event_store, config)
    }

    // ── JanitorConfig tests ──

    #[test]
    fn config_defaults_match_spec() {
        let config = JanitorConfig::default();
        assert_eq!(config.artifact_retain_days, 30);
        assert_eq!(config.log_retain_days, 14);
        assert_eq!(config.worktree_retain_hours_success, 4);
        assert_eq!(config.worktree_retain_hours_failed, 24);
        assert_eq!(config.disk_threshold_bytes, 1_073_741_824);
        assert_eq!(config.max_deletions_per_tick, 100);
    }

    #[test]
    fn config_is_cloneable() {
        let config = JanitorConfig::default();
        let cloned = config.clone();
        assert_eq!(config, cloned);
    }

    #[test]
    fn config_is_debug() {
        let config = JanitorConfig::default();
        let debug = format!("{config:?}");
        assert!(debug.contains("JanitorConfig"));
        assert!(debug.contains("artifact_retain_days"));
    }

    #[test]
    fn config_custom_values() {
        let config = JanitorConfig {
            artifact_retain_days: 7,
            log_retain_days: 3,
            worktree_retain_hours_success: 1,
            worktree_retain_hours_failed: 12,
            disk_threshold_bytes: 500_000_000,
            max_deletions_per_tick: 50,
        };
        assert_eq!(config.artifact_retain_days, 7);
        assert_eq!(config.log_retain_days, 3);
        assert_eq!(config.worktree_retain_hours_success, 1);
        assert_eq!(config.worktree_retain_hours_failed, 12);
        assert_eq!(config.disk_threshold_bytes, 500_000_000);
        assert_eq!(config.max_deletions_per_tick, 50);
    }

    // ── CleanupResult tests ──

    #[test]
    fn cleanup_result_default_is_zero() {
        let result = CleanupResult::default();
        assert_eq!(result.artifacts_purged, 0);
        assert_eq!(result.artifact_bytes_freed, 0);
        assert_eq!(result.logs_purged, 0);
        assert_eq!(result.log_bytes_freed, 0);
        assert_eq!(result.worktrees_cleaned, 0);
        assert_eq!(result.total_operations, 0);
    }

    #[test]
    fn cleanup_result_rate_limited_at_zero() {
        let result = CleanupResult::default();
        assert!(!result.rate_limited(100));
    }

    #[test]
    fn cleanup_result_rate_limited_at_max() {
        let mut result = CleanupResult::default();
        result.total_operations = 100;
        assert!(result.rate_limited(100));
    }

    #[test]
    fn cleanup_result_rate_limited_over_max() {
        let mut result = CleanupResult::default();
        result.total_operations = 150;
        assert!(result.rate_limited(100));
    }

    #[test]
    fn cleanup_result_not_rate_limited_under_max() {
        let mut result = CleanupResult::default();
        result.total_operations = 99;
        assert!(!result.rate_limited(100));
    }

    // ── Janitor construction tests ──

    #[tokio::test]
    async fn janitor_is_constructible() {
        let es: Arc<dyn EventStore> = Arc::new(RecordingEventStore::new());
        let _janitor = make_janitor(es);
    }

    #[tokio::test]
    async fn janitor_with_custom_config() {
        let es: Arc<dyn EventStore> = Arc::new(RecordingEventStore::new());
        let config = JanitorConfig {
            artifact_retain_days: 7,
            ..Default::default()
        };
        let janitor = make_janitor_with_config(es, config);
        assert_eq!(janitor.config().artifact_retain_days, 7);
    }

    #[tokio::test]
    async fn janitor_config_accessor() {
        let es: Arc<dyn EventStore> = Arc::new(RecordingEventStore::new());
        let janitor = make_janitor(es);
        let config = janitor.config();
        assert_eq!(config.artifact_retain_days, 30);
        assert_eq!(config.log_retain_days, 14);
    }

    // ── DiskGuardStatus tests ──

    #[test]
    fn disk_guard_healthy_is_healthy() {
        let status = DiskGuardStatus::Healthy {
            free_bytes: 2_000_000_000,
        };
        assert!(status.is_healthy());
        assert_eq!(status.free_bytes(), 2_000_000_000);
    }

    #[test]
    fn disk_guard_critical_is_not_healthy() {
        let status = DiskGuardStatus::Critical {
            free_bytes: 500_000_000,
        };
        assert!(!status.is_healthy());
        assert_eq!(status.free_bytes(), 500_000_000);
    }

    // ── Disk guard logic tests (unit-testable without I/O) ──

    /// Simulates disk guard logic for different scenarios.
    fn evaluate_disk_guard(
        free_bytes: u64,
        threshold: u64,
        currently_blocked: bool,
    ) -> DiskGuardStatus {
        let recovery_threshold = (threshold as f64 * DISK_RECOVERY_MULTIPLIER) as u64;

        if currently_blocked {
            if free_bytes >= recovery_threshold {
                DiskGuardStatus::Healthy { free_bytes }
            } else {
                DiskGuardStatus::Critical { free_bytes }
            }
        } else if free_bytes >= threshold {
            DiskGuardStatus::Healthy { free_bytes }
        } else {
            DiskGuardStatus::Critical { free_bytes }
        }
    }

    #[test]
    fn disk_guard_above_threshold_not_blocked() {
        let status = evaluate_disk_guard(2_000_000_000, DEFAULT_DISK_THRESHOLD_BYTES, false);
        assert!(status.is_healthy());
    }

    #[test]
    fn disk_guard_below_threshold_not_blocked() {
        let status = evaluate_disk_guard(500_000_000, DEFAULT_DISK_THRESHOLD_BYTES, false);
        assert!(!status.is_healthy());
    }

    #[test]
    fn disk_guard_at_threshold_not_blocked() {
        let status =
            evaluate_disk_guard(DEFAULT_DISK_THRESHOLD_BYTES, DEFAULT_DISK_THRESHOLD_BYTES, false);
        assert!(status.is_healthy());
    }

    #[test]
    fn disk_guard_hysteresis_below_recovery_still_blocked() {
        // Currently blocked, free is between threshold and recovery_threshold
        let threshold = DEFAULT_DISK_THRESHOLD_BYTES;
        let free = threshold + 100; // above threshold but below recovery
        let status = evaluate_disk_guard(free, threshold, true);
        assert!(!status.is_healthy(), "hysteresis: should remain blocked");
    }

    #[test]
    fn disk_guard_hysteresis_above_recovery_unblocks() {
        let threshold = DEFAULT_DISK_THRESHOLD_BYTES;
        let recovery = (threshold as f64 * DISK_RECOVERY_MULTIPLIER) as u64;
        let free = recovery + 1;
        let status = evaluate_disk_guard(free, threshold, true);
        assert!(status.is_healthy(), "above recovery threshold should unblock");
    }

    #[test]
    fn disk_guard_hysteresis_at_recovery_unblocks() {
        let threshold = DEFAULT_DISK_THRESHOLD_BYTES;
        let recovery = (threshold as f64 * DISK_RECOVERY_MULTIPLIER) as u64;
        let status = evaluate_disk_guard(recovery, threshold, true);
        assert!(
            status.is_healthy(),
            "at exactly recovery threshold should unblock"
        );
    }

    #[test]
    fn disk_guard_very_low_space() {
        let status = evaluate_disk_guard(0, DEFAULT_DISK_THRESHOLD_BYTES, false);
        assert!(!status.is_healthy());
    }

    #[test]
    fn disk_guard_zero_threshold() {
        let status = evaluate_disk_guard(0, 0, false);
        assert!(status.is_healthy(), "0 >= 0 should be healthy");
    }

    // ── Event builder tests ──

    #[test]
    fn artifacts_purged_event_has_correct_type() {
        let event = build_artifacts_purged_event(Uuid::new_v4(), 5, 1024);
        assert_eq!(event.event_type, "ArtifactsPurged");
        assert_eq!(event.event_version, 1);
        assert_eq!(event.seq, 0);
    }

    #[test]
    fn artifacts_purged_event_has_correct_payload() {
        let instance_id = Uuid::new_v4();
        let event = build_artifacts_purged_event(instance_id, 5, 1024);
        assert_eq!(event.instance_id, instance_id);
        assert_eq!(event.payload["count"], 5);
        assert_eq!(event.payload["bytes_freed"], 1024);
        assert_eq!(event.payload["actor"]["kind"], "System");
        assert_eq!(event.payload["actor"]["id"], "janitor");
    }

    #[test]
    fn artifacts_purged_event_has_no_idempotency_key() {
        let event = build_artifacts_purged_event(Uuid::new_v4(), 1, 100);
        assert!(event.idempotency_key.is_none());
    }

    #[test]
    fn logs_purged_event_has_correct_type() {
        let event = build_logs_purged_event(Uuid::new_v4(), 10, 2048);
        assert_eq!(event.event_type, "LogsPurged");
        assert_eq!(event.event_version, 1);
        assert_eq!(event.seq, 0);
    }

    #[test]
    fn logs_purged_event_has_correct_payload() {
        let instance_id = Uuid::new_v4();
        let event = build_logs_purged_event(instance_id, 10, 2048);
        assert_eq!(event.instance_id, instance_id);
        assert_eq!(event.payload["count"], 10);
        assert_eq!(event.payload["bytes_freed"], 2048);
        assert_eq!(event.payload["actor"]["kind"], "System");
        assert_eq!(event.payload["actor"]["id"], "janitor");
    }

    #[test]
    fn logs_purged_event_has_no_idempotency_key() {
        let event = build_logs_purged_event(Uuid::new_v4(), 1, 100);
        assert!(event.idempotency_key.is_none());
    }

    #[test]
    fn worktree_evicted_event_has_correct_type() {
        let event = build_worktree_evicted_event(
            Uuid::new_v4(),
            Uuid::new_v4(),
            "retention_expired",
            "completed",
            "/tmp/wt",
        );
        assert_eq!(event.event_type, "WorktreeEvicted");
        assert_eq!(event.event_version, 1);
        assert_eq!(event.seq, 0);
    }

    #[test]
    fn worktree_evicted_event_has_correct_payload() {
        let instance_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let event = build_worktree_evicted_event(
            instance_id,
            run_id,
            "disk_cap",
            "failed",
            "/tmp/wt/abc",
        );
        assert_eq!(event.instance_id, instance_id);
        assert_eq!(event.payload["run_id"], run_id.to_string());
        assert_eq!(event.payload["reason"], "disk_cap");
        assert_eq!(event.payload["state"], "failed");
        assert_eq!(event.payload["path"], "/tmp/wt/abc");
        assert_eq!(event.payload["actor"]["kind"], "System");
        assert_eq!(event.payload["actor"]["id"], "janitor");
    }

    #[test]
    fn worktree_evicted_event_has_idempotency_key() {
        let run_id = Uuid::new_v4();
        let event = build_worktree_evicted_event(
            Uuid::new_v4(),
            run_id,
            "retention_expired",
            "completed",
            "/tmp/wt",
        );
        let key = event.idempotency_key.as_ref().expect("should have key");
        assert_eq!(*key, format!("janitor-worktree-{}", run_id));
    }

    #[test]
    fn worktree_evicted_events_have_unique_event_ids() {
        let e1 = build_worktree_evicted_event(
            Uuid::new_v4(),
            Uuid::new_v4(),
            "retention_expired",
            "completed",
            "/a",
        );
        let e2 = build_worktree_evicted_event(
            Uuid::new_v4(),
            Uuid::new_v4(),
            "retention_expired",
            "completed",
            "/b",
        );
        assert_ne!(e1.event_id, e2.event_id);
    }

    // ── Constants tests ──

    #[test]
    fn max_deletions_per_tick_is_100() {
        assert_eq!(MAX_DELETIONS_PER_TICK, 100);
    }

    #[test]
    fn default_disk_threshold_is_1gb() {
        assert_eq!(DEFAULT_DISK_THRESHOLD_BYTES, 1_073_741_824);
    }

    #[test]
    fn disk_recovery_multiplier_is_1_5() {
        assert!((DISK_RECOVERY_MULTIPLIER - 1.5).abs() < f64::EPSILON);
    }

    // ── Disk guard check (live test, uses actual `df`) ──

    #[tokio::test]
    async fn disk_guard_check_on_root() {
        let es: Arc<dyn EventStore> = Arc::new(RecordingEventStore::new());
        let janitor = make_janitor(es);

        // This should work on any Linux system
        let result = janitor.disk_guard_check("/", false).await;
        assert!(result.is_ok(), "disk guard should succeed on /");

        let status = result.unwrap();
        // Root partition should have SOME free space
        assert!(status.free_bytes() > 0, "root should have free bytes");
    }

    #[tokio::test]
    async fn disk_guard_check_invalid_path() {
        let es: Arc<dyn EventStore> = Arc::new(RecordingEventStore::new());
        let janitor = make_janitor(es);

        let result = janitor
            .disk_guard_check("/nonexistent/path/that/doesnt/exist", false)
            .await;
        // `df` on a nonexistent path should fail
        assert!(result.is_err(), "disk guard should fail on invalid path");
    }

    #[tokio::test]
    async fn disk_guard_check_hysteresis_in_practice() {
        let es: Arc<dyn EventStore> = Arc::new(RecordingEventStore::new());
        // Use a huge threshold to force critical status
        let config = JanitorConfig {
            disk_threshold_bytes: u64::MAX,
            ..Default::default()
        };
        let janitor = make_janitor_with_config(es, config);

        let result = janitor.disk_guard_check("/", false).await;
        assert!(result.is_ok());
        let status = result.unwrap();
        assert!(
            !status.is_healthy(),
            "with max threshold, should be critical"
        );
    }

    // ── get_free_disk_bytes tests ──

    #[tokio::test]
    async fn get_free_disk_bytes_on_root() {
        let result = get_free_disk_bytes("/").await;
        assert!(result.is_ok());
        assert!(result.unwrap() > 0);
    }

    #[tokio::test]
    async fn get_free_disk_bytes_on_invalid_path() {
        let result = get_free_disk_bytes("/this/path/does/not/exist/at/all").await;
        assert!(result.is_err());
    }
}
