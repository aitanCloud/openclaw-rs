use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::app::errors::AppError;
use crate::domain::events::EventEnvelope;
use crate::domain::git::{DiffStat, GitProvider, MergeResult};
use crate::domain::ports::EventStore;
use crate::infra::errors::InfraError;

/// Orchestrates merging verified task results back to the target branch.
///
/// Responsibilities:
/// - Emit merge lifecycle events (Attempted, Succeeded, Conflicted, Failed, Skipped)
/// - In-flight merge guard (prevents concurrent merges for the same cycle)
/// - Retry transient failures with exponential backoff
/// - Cycle completion co-emission when all tasks pass and all merges succeed
pub struct MergeService {
    git_provider: Arc<dyn GitProvider>,
    event_store: Arc<dyn EventStore>,
    pool: PgPool,
}

/// Parameters for a merge attempt, avoiding a long parameter list.
#[derive(Debug, Clone)]
pub struct MergeParams {
    pub task_id: Uuid,
    pub run_id: Uuid,
    pub instance_id: Uuid,
    pub cycle_id: Uuid,
    pub repo_path: String,
    pub source_branch: String,
    pub target_branch: String,
}

/// Outcome of an `attempt_merge` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    Succeeded {
        merge_sha: String,
        diff: DiffStat,
    },
    Conflicted {
        conflicting_files: Vec<String>,
        conflict_count: u32,
    },
    Failed {
        reason: String,
        is_transient: bool,
    },
}

impl MergeService {
    pub fn new(
        git_provider: Arc<dyn GitProvider>,
        event_store: Arc<dyn EventStore>,
        pool: PgPool,
    ) -> Self {
        Self {
            git_provider,
            event_store,
            pool,
        }
    }

    /// Maintenance mode guard: reject if instance is not in 'active' state.
    ///
    /// This prevents merge operations while the instance is in maintenance mode,
    /// suspended, or any non-active state (architecture E7).
    async fn check_instance_active(&self, instance_id: Uuid) -> Result<(), AppError> {
        let state = sqlx::query_scalar::<_, String>(
            "SELECT state FROM orch_instances WHERE id = $1",
        )
        .bind(instance_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            AppError::Infra(InfraError::Database(format!(
                "maintenance guard query: {e}"
            )))
        })?;

        match state {
            Some(s) if s == "active" => Ok(()),
            Some(s) => Err(AppError::Domain(
                crate::domain::errors::DomainError::Precondition(format!(
                    "instance {instance_id} is in '{s}' state, operations blocked"
                )),
            )),
            None => Err(AppError::Infra(InfraError::Database(format!(
                "instance {instance_id} not found"
            )))),
        }
    }

    /// Attempt a single merge of source_branch into target_branch.
    ///
    /// Emits MergeAttempted before the merge, then one of:
    /// - MergeSucceeded (with merge_sha + diff stats)
    /// - MergeConflicted (with conflicting_files + conflict_count)
    /// - MergeFailed (with failure category: Transient or Permanent)
    ///
    /// Guarded: rejects if instance is not in 'active' state (maintenance mode check).
    pub async fn attempt_merge(
        &self,
        params: &MergeParams,
        attempt: u32,
    ) -> Result<MergeOutcome, AppError> {
        self.check_instance_active(params.instance_id).await?;

        self.check_in_flight_merge(params.cycle_id, params.task_id)
            .await?;

        self.attempt_merge_core(params, attempt).await
    }

    /// Core merge logic: event emission + git merge + result handling.
    ///
    /// Separated from `attempt_merge` so callers that already hold/checked
    /// the in-flight guard (e.g. retry loops, tests) can invoke it directly.
    pub(crate) async fn attempt_merge_core(
        &self,
        params: &MergeParams,
        attempt: u32,
    ) -> Result<MergeOutcome, AppError> {
        // Emit MergeAttempted
        let now = Utc::now();
        let attempted_envelope = EventEnvelope {
            event_id: Uuid::new_v4(),
            instance_id: params.instance_id,
            seq: 0,
            event_type: "MergeAttempted".to_string(),
            event_version: 1,
            payload: serde_json::json!({
                "task_id": params.task_id,
                "run_id": params.run_id,
                "cycle_id": params.cycle_id,
                "source_branch": params.source_branch,
                "target_branch": params.target_branch,
                "attempt": attempt,
                "actor": { "kind": "System" },
            }),
            idempotency_key: Some(format!(
                "merge-attempted-{}-{}",
                params.task_id, attempt
            )),
            correlation_id: None,
            causation_id: None,
            occurred_at: now,
            recorded_at: now,
        };

        self.event_store.emit(attempted_envelope).await.map_err(|e| {
            AppError::Infra(InfraError::Database(format!("emit MergeAttempted: {e}")))
        })?;

        tracing::info!(
            task_id = %params.task_id,
            run_id = %params.run_id,
            attempt = attempt,
            source = %params.source_branch,
            target = %params.target_branch,
            "merge attempted"
        );

        // Call git merge
        let merge_result = self
            .git_provider
            .merge(&params.repo_path, &params.source_branch, &params.target_branch)
            .await
            .map_err(|e| {
                AppError::Infra(InfraError::Io(format!("git merge error: {e}")))
            })?;

        match merge_result {
            MergeResult::Success { merge_sha } => {
                // Get diff stats: compare what source adds relative to target
                let diff = self
                    .git_provider
                    .diff_stat(
                        &params.repo_path,
                        &params.target_branch,
                        &params.source_branch,
                    )
                    .await
                    .unwrap_or(DiffStat {
                        files_changed: 0,
                        insertions: 0,
                        deletions: 0,
                    });

                // Emit MergeSucceeded
                let now = Utc::now();
                let envelope = EventEnvelope {
                    event_id: Uuid::new_v4(),
                    instance_id: params.instance_id,
                    seq: 0,
                    event_type: "MergeSucceeded".to_string(),
                    event_version: 1,
                    payload: serde_json::json!({
                        "task_id": params.task_id,
                        "run_id": params.run_id,
                        "cycle_id": params.cycle_id,
                        "merge_sha": merge_sha,
                        "files_changed": diff.files_changed,
                        "insertions": diff.insertions,
                        "deletions": diff.deletions,
                        "actor": { "kind": "System" },
                    }),
                    idempotency_key: Some(format!(
                        "merge-succeeded-{}-{}",
                        params.task_id, attempt
                    )),
                    correlation_id: None,
                    causation_id: None,
                    occurred_at: now,
                    recorded_at: now,
                };

                self.event_store.emit(envelope).await.map_err(|e| {
                    AppError::Infra(InfraError::Database(format!(
                        "emit MergeSucceeded: {e}"
                    )))
                })?;

                tracing::info!(
                    task_id = %params.task_id,
                    merge_sha = %merge_sha,
                    files_changed = diff.files_changed,
                    "merge succeeded"
                );

                Ok(MergeOutcome::Succeeded {
                    merge_sha,
                    diff,
                })
            }

            MergeResult::Conflict { conflicting_files } => {
                let conflict_count = conflicting_files.len() as u32;

                // Emit MergeConflicted
                let now = Utc::now();
                let envelope = EventEnvelope {
                    event_id: Uuid::new_v4(),
                    instance_id: params.instance_id,
                    seq: 0,
                    event_type: "MergeConflicted".to_string(),
                    event_version: 1,
                    payload: serde_json::json!({
                        "task_id": params.task_id,
                        "run_id": params.run_id,
                        "cycle_id": params.cycle_id,
                        "conflicting_files": conflicting_files,
                        "conflict_count": conflict_count,
                        "actor": { "kind": "System" },
                    }),
                    idempotency_key: Some(format!(
                        "merge-conflicted-{}-{}",
                        params.task_id, attempt
                    )),
                    correlation_id: None,
                    causation_id: None,
                    occurred_at: now,
                    recorded_at: now,
                };

                self.event_store.emit(envelope).await.map_err(|e| {
                    AppError::Infra(InfraError::Database(format!(
                        "emit MergeConflicted: {e}"
                    )))
                })?;

                tracing::info!(
                    task_id = %params.task_id,
                    conflict_count = conflict_count,
                    "merge conflicted"
                );

                Ok(MergeOutcome::Conflicted {
                    conflicting_files,
                    conflict_count,
                })
            }

            MergeResult::Failed(reason) => {
                let is_transient = Self::classify_failure_transient(&reason);
                let failure_category = if is_transient {
                    "Transient"
                } else {
                    "Permanent"
                };

                // Emit MergeFailed
                let now = Utc::now();
                let envelope = EventEnvelope {
                    event_id: Uuid::new_v4(),
                    instance_id: params.instance_id,
                    seq: 0,
                    event_type: "MergeFailed".to_string(),
                    event_version: 1,
                    payload: serde_json::json!({
                        "task_id": params.task_id,
                        "run_id": params.run_id,
                        "cycle_id": params.cycle_id,
                        "failure_category": failure_category,
                        "reason": reason,
                        "actor": { "kind": "System" },
                    }),
                    idempotency_key: Some(format!(
                        "merge-failed-{}-{}",
                        params.task_id, attempt
                    )),
                    correlation_id: None,
                    causation_id: None,
                    occurred_at: now,
                    recorded_at: now,
                };

                self.event_store.emit(envelope).await.map_err(|e| {
                    AppError::Infra(InfraError::Database(format!(
                        "emit MergeFailed: {e}"
                    )))
                })?;

                tracing::error!(
                    task_id = %params.task_id,
                    failure_category = failure_category,
                    reason = %reason,
                    "merge failed"
                );

                Ok(MergeOutcome::Failed {
                    reason,
                    is_transient,
                })
            }
        }
    }

    /// Skip the merge for a task that has no changes.
    ///
    /// Emits MergeSkipped with the provided reason.
    pub async fn skip_merge(
        &self,
        task_id: Uuid,
        run_id: Uuid,
        instance_id: Uuid,
        cycle_id: Uuid,
        reason: &str,
    ) -> Result<(), AppError> {
        let now = Utc::now();
        let envelope = EventEnvelope {
            event_id: Uuid::new_v4(),
            instance_id,
            seq: 0,
            event_type: "MergeSkipped".to_string(),
            event_version: 1,
            payload: serde_json::json!({
                "task_id": task_id,
                "run_id": run_id,
                "cycle_id": cycle_id,
                "reason": reason,
                "actor": { "kind": "System" },
            }),
            idempotency_key: Some(format!("merge-skipped-{}", task_id)),
            correlation_id: None,
            causation_id: None,
            occurred_at: now,
            recorded_at: now,
        };

        self.event_store.emit(envelope).await.map_err(|e| {
            AppError::Infra(InfraError::Database(format!("emit MergeSkipped: {e}")))
        })?;

        tracing::info!(
            task_id = %task_id,
            reason = %reason,
            "merge skipped"
        );

        Ok(())
    }

    /// Attempt merge with retry logic for transient failures.
    ///
    /// Retries up to `max_retries` times with exponential backoff starting
    /// at `base_delay_ms` milliseconds. Only retries on transient failures.
    /// The maintenance guard and in-flight guard are checked once at the start.
    ///
    /// Guarded: rejects if instance is not in 'active' state (maintenance mode check).
    pub async fn attempt_merge_with_retry(
        &self,
        params: &MergeParams,
        max_retries: u32,
        base_delay_ms: u64,
    ) -> Result<MergeOutcome, AppError> {
        self.check_instance_active(params.instance_id).await?;

        self.check_in_flight_merge(params.cycle_id, params.task_id)
            .await?;

        let mut attempt = 1u32;

        loop {
            let outcome = self.attempt_merge_core(params, attempt).await?;

            match &outcome {
                MergeOutcome::Failed {
                    is_transient: true, ..
                } if attempt <= max_retries => {
                    let delay_ms = base_delay_ms * 2u64.saturating_pow(attempt - 1);
                    tracing::info!(
                        task_id = %params.task_id,
                        attempt = attempt,
                        max_retries = max_retries,
                        delay_ms = delay_ms,
                        "retrying merge after transient failure"
                    );
                    tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                    attempt += 1;
                }
                _ => return Ok(outcome),
            }
        }
    }

    /// Check if all tasks in a cycle are in terminal passed states and all
    /// merges have succeeded. If so, emit CycleCompleted.
    pub async fn check_cycle_completion(
        &self,
        cycle_id: Uuid,
        instance_id: Uuid,
    ) -> Result<bool, AppError> {
        // Query orch_tasks: all tasks for this cycle must be in 'passed' state
        let incomplete_count = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT COUNT(*)
            FROM orch_tasks
            WHERE cycle_id = $1
              AND instance_id = $2
              AND state != 'passed'
              AND state != 'skipped'
            "#,
        )
        .bind(cycle_id)
        .bind(instance_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            AppError::Infra(InfraError::Database(format!(
                "check cycle completion (tasks): {e}"
            )))
        })?;

        if incomplete_count > 0 {
            tracing::info!(
                cycle_id = %cycle_id,
                incomplete_count = incomplete_count,
                "cycle not complete: tasks still pending"
            );
            return Ok(false);
        }

        // Query: all merge events for tasks in this cycle succeeded or skipped
        // We check there are no tasks missing a MergeSucceeded or MergeSkipped event
        let unmerged_count = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT COUNT(*)
            FROM orch_tasks t
            WHERE t.cycle_id = $1
              AND t.instance_id = $2
              AND NOT EXISTS (
                  SELECT 1 FROM orch_events e
                  WHERE e.instance_id = t.instance_id
                    AND e.event_type IN ('MergeSucceeded', 'MergeSkipped')
                    AND e.payload->>'task_id' = t.id::text
              )
            "#,
        )
        .bind(cycle_id)
        .bind(instance_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            AppError::Infra(InfraError::Database(format!(
                "check cycle completion (merges): {e}"
            )))
        })?;

        if unmerged_count > 0 {
            tracing::info!(
                cycle_id = %cycle_id,
                unmerged_count = unmerged_count,
                "cycle not complete: merges still pending"
            );
            return Ok(false);
        }

        // All tasks passed and all merges succeeded -- emit CycleCompleted
        let now = Utc::now();
        let envelope = EventEnvelope {
            event_id: Uuid::new_v4(),
            instance_id,
            seq: 0,
            event_type: "CycleCompleted".to_string(),
            event_version: 1,
            payload: serde_json::json!({
                "cycle_id": cycle_id,
                "actor": { "kind": "System" },
            }),
            idempotency_key: Some(format!("cycle-completed-{}", cycle_id)),
            correlation_id: None,
            causation_id: None,
            occurred_at: now,
            recorded_at: now,
        };

        self.event_store.emit(envelope).await.map_err(|e| {
            AppError::Infra(InfraError::Database(format!(
                "emit CycleCompleted: {e}"
            )))
        })?;

        tracing::info!(cycle_id = %cycle_id, "cycle completed: all tasks passed and merged");

        Ok(true)
    }

    /// In-flight merge guard: check there is no other merge in-flight for this cycle.
    ///
    /// Queries orch_events for a MergeAttempted event for a *different* task in
    /// the same cycle that has no corresponding terminal merge event
    /// (MergeSucceeded, MergeConflicted, MergeFailed, MergeSkipped).
    async fn check_in_flight_merge(
        &self,
        cycle_id: Uuid,
        task_id: Uuid,
    ) -> Result<(), AppError> {
        let in_flight = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT COUNT(*)
            FROM orch_events attempted
            WHERE attempted.event_type = 'MergeAttempted'
              AND attempted.payload->>'cycle_id' = $1::text
              AND attempted.payload->>'task_id' != $2::text
              AND NOT EXISTS (
                  SELECT 1 FROM orch_events terminal
                  WHERE terminal.event_type IN ('MergeSucceeded', 'MergeConflicted', 'MergeFailed', 'MergeSkipped')
                    AND terminal.payload->>'task_id' = attempted.payload->>'task_id'
              )
            "#,
        )
        .bind(cycle_id)
        .bind(task_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            AppError::Infra(InfraError::Database(format!(
                "check in-flight merge: {e}"
            )))
        })?;

        if in_flight > 0 {
            return Err(AppError::ConcurrencyConflict(format!(
                "another merge is in-flight for cycle {cycle_id}"
            )));
        }

        Ok(())
    }

    /// Classify whether a merge failure is transient (retryable) or permanent.
    ///
    /// Lock contention, IO errors, and timeouts are transient.
    /// Everything else is permanent.
    fn classify_failure_transient(reason: &str) -> bool {
        let lower = reason.to_lowercase();
        lower.contains("lock")
            || lower.contains("timeout")
            || lower.contains("timed out")
            || lower.contains("temporary")
            || lower.contains("io error")
            || lower.contains("connection")
            || lower.contains("transient")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::errors::DomainError;
    use crate::domain::git::{DiffStat, FileStatus, GitError, GitProvider, MergeResult};
    use std::path::PathBuf;

    // ── Mock GitProvider implementations ──

    /// Mock GitProvider that returns a successful merge.
    struct SuccessGitProvider {
        merge_sha: String,
        diff: DiffStat,
    }

    impl SuccessGitProvider {
        fn new() -> Self {
            Self {
                merge_sha: "abc123def456789012345678901234567890abcd".to_string(),
                diff: DiffStat {
                    files_changed: 3,
                    insertions: 45,
                    deletions: 12,
                },
            }
        }
    }

    #[async_trait::async_trait]
    impl GitProvider for SuccessGitProvider {
        async fn create_worktree(
            &self,
            _repo_path: &str,
            _worktree_path: &str,
            _branch: &str,
        ) -> Result<PathBuf, GitError> {
            Ok(PathBuf::from("/tmp/worktree"))
        }

        async fn remove_worktree(
            &self,
            _repo_path: &str,
            _worktree_path: &str,
        ) -> Result<(), GitError> {
            Ok(())
        }

        async fn merge(
            &self,
            _repo_path: &str,
            _source_branch: &str,
            _target_branch: &str,
        ) -> Result<MergeResult, GitError> {
            Ok(MergeResult::Success {
                merge_sha: self.merge_sha.clone(),
            })
        }

        async fn diff_stat(
            &self,
            _repo_path: &str,
            _base: &str,
            _head: &str,
        ) -> Result<DiffStat, GitError> {
            Ok(self.diff.clone())
        }

        async fn current_sha(&self, _repo_path: &str) -> Result<String, GitError> {
            Ok(self.merge_sha.clone())
        }

        async fn status_porcelain(
            &self,
            _repo_path: &str,
        ) -> Result<Vec<FileStatus>, GitError> {
            Ok(vec![])
        }
    }

    /// Mock GitProvider that returns a conflict.
    struct ConflictGitProvider {
        conflicting_files: Vec<String>,
    }

    impl ConflictGitProvider {
        fn new(files: Vec<&str>) -> Self {
            Self {
                conflicting_files: files.into_iter().map(|s| s.to_string()).collect(),
            }
        }
    }

    #[async_trait::async_trait]
    impl GitProvider for ConflictGitProvider {
        async fn create_worktree(
            &self,
            _repo_path: &str,
            _worktree_path: &str,
            _branch: &str,
        ) -> Result<PathBuf, GitError> {
            Ok(PathBuf::from("/tmp/worktree"))
        }

        async fn remove_worktree(
            &self,
            _repo_path: &str,
            _worktree_path: &str,
        ) -> Result<(), GitError> {
            Ok(())
        }

        async fn merge(
            &self,
            _repo_path: &str,
            _source_branch: &str,
            _target_branch: &str,
        ) -> Result<MergeResult, GitError> {
            Ok(MergeResult::Conflict {
                conflicting_files: self.conflicting_files.clone(),
            })
        }

        async fn diff_stat(
            &self,
            _repo_path: &str,
            _base: &str,
            _head: &str,
        ) -> Result<DiffStat, GitError> {
            Ok(DiffStat {
                files_changed: 0,
                insertions: 0,
                deletions: 0,
            })
        }

        async fn current_sha(&self, _repo_path: &str) -> Result<String, GitError> {
            Ok("0000000000000000000000000000000000000000".to_string())
        }

        async fn status_porcelain(
            &self,
            _repo_path: &str,
        ) -> Result<Vec<FileStatus>, GitError> {
            Ok(vec![])
        }
    }

    /// Mock GitProvider that returns a failed merge.
    struct FailedGitProvider {
        reason: String,
    }

    impl FailedGitProvider {
        fn new(reason: &str) -> Self {
            Self {
                reason: reason.to_string(),
            }
        }
    }

    #[async_trait::async_trait]
    impl GitProvider for FailedGitProvider {
        async fn create_worktree(
            &self,
            _repo_path: &str,
            _worktree_path: &str,
            _branch: &str,
        ) -> Result<PathBuf, GitError> {
            Ok(PathBuf::from("/tmp/worktree"))
        }

        async fn remove_worktree(
            &self,
            _repo_path: &str,
            _worktree_path: &str,
        ) -> Result<(), GitError> {
            Ok(())
        }

        async fn merge(
            &self,
            _repo_path: &str,
            _source_branch: &str,
            _target_branch: &str,
        ) -> Result<MergeResult, GitError> {
            Ok(MergeResult::Failed(self.reason.clone()))
        }

        async fn diff_stat(
            &self,
            _repo_path: &str,
            _base: &str,
            _head: &str,
        ) -> Result<DiffStat, GitError> {
            Ok(DiffStat {
                files_changed: 0,
                insertions: 0,
                deletions: 0,
            })
        }

        async fn current_sha(&self, _repo_path: &str) -> Result<String, GitError> {
            Ok("0000000000000000000000000000000000000000".to_string())
        }

        async fn status_porcelain(
            &self,
            _repo_path: &str,
        ) -> Result<Vec<FileStatus>, GitError> {
            Ok(vec![])
        }
    }

    /// Mock GitProvider that alternates between failure and success.
    /// Fails `fail_count` times then succeeds.
    struct TransientThenSuccessGitProvider {
        call_count: tokio::sync::Mutex<u32>,
        fail_count: u32,
        merge_sha: String,
    }

    impl TransientThenSuccessGitProvider {
        fn new(fail_count: u32) -> Self {
            Self {
                call_count: tokio::sync::Mutex::new(0),
                fail_count,
                merge_sha: "abc123def456789012345678901234567890abcd".to_string(),
            }
        }
    }

    #[async_trait::async_trait]
    impl GitProvider for TransientThenSuccessGitProvider {
        async fn create_worktree(
            &self,
            _repo_path: &str,
            _worktree_path: &str,
            _branch: &str,
        ) -> Result<PathBuf, GitError> {
            Ok(PathBuf::from("/tmp/worktree"))
        }

        async fn remove_worktree(
            &self,
            _repo_path: &str,
            _worktree_path: &str,
        ) -> Result<(), GitError> {
            Ok(())
        }

        async fn merge(
            &self,
            _repo_path: &str,
            _source_branch: &str,
            _target_branch: &str,
        ) -> Result<MergeResult, GitError> {
            let mut count = self.call_count.lock().await;
            *count += 1;
            if *count <= self.fail_count {
                Ok(MergeResult::Failed("lock contention transient error".to_string()))
            } else {
                Ok(MergeResult::Success {
                    merge_sha: self.merge_sha.clone(),
                })
            }
        }

        async fn diff_stat(
            &self,
            _repo_path: &str,
            _base: &str,
            _head: &str,
        ) -> Result<DiffStat, GitError> {
            Ok(DiffStat {
                files_changed: 1,
                insertions: 10,
                deletions: 2,
            })
        }

        async fn current_sha(&self, _repo_path: &str) -> Result<String, GitError> {
            Ok(self.merge_sha.clone())
        }

        async fn status_porcelain(
            &self,
            _repo_path: &str,
        ) -> Result<Vec<FileStatus>, GitError> {
            Ok(vec![])
        }
    }

    // ── Mock EventStore (RecordingEventStore) ──

    struct RecordingEventStore {
        events: tokio::sync::Mutex<Vec<EventEnvelope>>,
    }

    impl RecordingEventStore {
        fn new() -> Self {
            Self {
                events: tokio::sync::Mutex::new(Vec::new()),
            }
        }

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

    // ── Helper to build a MergeService without a real PgPool ──
    //
    // For tests that do NOT exercise SQL queries (in-flight guard, cycle completion),
    // we use a dummy PgPool via sqlx::postgres::PgPoolOptions with connect_lazy.
    // These tests only exercise event emission and git provider interactions.

    fn dummy_pool() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://dummy:dummy@localhost:5432/dummy")
            .expect("connect_lazy should not fail")
    }

    fn make_params() -> MergeParams {
        MergeParams {
            task_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            cycle_id: Uuid::new_v4(),
            repo_path: "/tmp/repo".to_string(),
            source_branch: "feature/task-1".to_string(),
            target_branch: "main".to_string(),
        }
    }

    // ═══════════════════════════════════════════════════════════════════
    // Tests
    // ═══════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn merge_service_is_constructible() {
        let gp: Arc<dyn GitProvider> = Arc::new(SuccessGitProvider::new());
        let es: Arc<dyn EventStore> = Arc::new(RecordingEventStore::new());
        let _svc = MergeService::new(gp, es, dummy_pool());
    }

    #[test]
    fn merge_params_fields_accessible() {
        let p = make_params();
        let _: Uuid = p.task_id;
        let _: Uuid = p.run_id;
        let _: Uuid = p.instance_id;
        let _: Uuid = p.cycle_id;
        let _: &str = &p.repo_path;
        let _: &str = &p.source_branch;
        let _: &str = &p.target_branch;
    }

    #[test]
    fn merge_outcome_succeeded_fields() {
        let outcome = MergeOutcome::Succeeded {
            merge_sha: "abc".to_string(),
            diff: DiffStat {
                files_changed: 1,
                insertions: 2,
                deletions: 3,
            },
        };
        match outcome {
            MergeOutcome::Succeeded { merge_sha, diff } => {
                assert_eq!(merge_sha, "abc");
                assert_eq!(diff.files_changed, 1);
            }
            _ => panic!("expected Succeeded"),
        }
    }

    #[test]
    fn merge_outcome_conflicted_fields() {
        let outcome = MergeOutcome::Conflicted {
            conflicting_files: vec!["a.rs".into(), "b.rs".into()],
            conflict_count: 2,
        };
        match outcome {
            MergeOutcome::Conflicted {
                conflicting_files,
                conflict_count,
            } => {
                assert_eq!(conflict_count, 2);
                assert_eq!(conflicting_files.len(), 2);
            }
            _ => panic!("expected Conflicted"),
        }
    }

    #[test]
    fn merge_outcome_failed_fields() {
        let outcome = MergeOutcome::Failed {
            reason: "bad".into(),
            is_transient: false,
        };
        match outcome {
            MergeOutcome::Failed {
                reason,
                is_transient,
            } => {
                assert_eq!(reason, "bad");
                assert!(!is_transient);
            }
            _ => panic!("expected Failed"),
        }
    }

    #[test]
    fn classify_failure_transient_lock() {
        assert!(MergeService::classify_failure_transient(
            "lock contention on ref"
        ));
    }

    #[test]
    fn classify_failure_transient_timeout() {
        assert!(MergeService::classify_failure_transient(
            "operation timed out"
        ));
    }

    #[test]
    fn classify_failure_transient_connection() {
        assert!(MergeService::classify_failure_transient(
            "connection reset by peer"
        ));
    }

    #[test]
    fn classify_failure_transient_io() {
        assert!(MergeService::classify_failure_transient(
            "IO error: disk full"
        ));
    }

    #[test]
    fn classify_failure_permanent() {
        assert!(!MergeService::classify_failure_transient(
            "fatal: not a git repository"
        ));
    }

    #[test]
    fn classify_failure_permanent_generic() {
        assert!(!MergeService::classify_failure_transient(
            "some unknown error happened"
        ));
    }

    // ── Helper: retry loop using attempt_merge_core (no SQL guard) ──

    async fn attempt_merge_core_with_retry(
        svc: &MergeService,
        params: &MergeParams,
        max_retries: u32,
        base_delay_ms: u64,
    ) -> Result<MergeOutcome, AppError> {
        let mut attempt = 1u32;
        loop {
            let outcome = svc.attempt_merge_core(params, attempt).await?;
            match &outcome {
                MergeOutcome::Failed {
                    is_transient: true, ..
                } if attempt <= max_retries => {
                    let delay_ms = base_delay_ms * 2u64.saturating_pow(attempt - 1);
                    tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                    attempt += 1;
                }
                _ => return Ok(outcome),
            }
        }
    }

    // ── Async tests ──

    #[tokio::test]
    async fn successful_merge_emits_attempted_and_succeeded() {
        let event_store = Arc::new(RecordingEventStore::new());
        let gp: Arc<dyn GitProvider> = Arc::new(SuccessGitProvider::new());
        let svc = MergeService::new(gp, event_store.clone(), dummy_pool());

        let params = make_params();
        let result = svc.attempt_merge_core(&params, 1).await;
        assert!(result.is_ok(), "merge should succeed: {result:?}");

        let outcome = result.unwrap();
        match &outcome {
            MergeOutcome::Succeeded { merge_sha, diff } => {
                assert_eq!(
                    merge_sha,
                    "abc123def456789012345678901234567890abcd"
                );
                assert_eq!(diff.files_changed, 3);
                assert_eq!(diff.insertions, 45);
                assert_eq!(diff.deletions, 12);
            }
            _ => panic!("expected Succeeded, got {outcome:?}"),
        }

        let events = event_store.emitted_events().await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "MergeAttempted");
        assert_eq!(events[1].event_type, "MergeSucceeded");

        // Both events have same instance_id
        assert_eq!(events[0].instance_id, params.instance_id);
        assert_eq!(events[1].instance_id, params.instance_id);
    }

    #[tokio::test]
    async fn conflict_emits_attempted_and_conflicted() {
        let event_store = Arc::new(RecordingEventStore::new());
        let gp: Arc<dyn GitProvider> =
            Arc::new(ConflictGitProvider::new(vec!["src/main.rs", "README.md"]));
        let svc = MergeService::new(gp, event_store.clone(), dummy_pool());

        let params = make_params();
        let result = svc.attempt_merge_core(&params, 1).await;
        assert!(result.is_ok());

        let outcome = result.unwrap();
        match &outcome {
            MergeOutcome::Conflicted {
                conflicting_files,
                conflict_count,
            } => {
                assert_eq!(*conflict_count, 2);
                assert!(conflicting_files.contains(&"src/main.rs".to_string()));
                assert!(conflicting_files.contains(&"README.md".to_string()));
            }
            _ => panic!("expected Conflicted, got {outcome:?}"),
        }

        let events = event_store.emitted_events().await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "MergeAttempted");
        assert_eq!(events[1].event_type, "MergeConflicted");

        // Verify conflict_count in payload
        let payload = &events[1].payload;
        assert_eq!(payload["conflict_count"], 2);
        let files = payload["conflicting_files"].as_array().expect("array");
        assert_eq!(files.len(), 2);
    }

    #[tokio::test]
    async fn failed_merge_emits_attempted_and_failed() {
        let event_store = Arc::new(RecordingEventStore::new());
        let gp: Arc<dyn GitProvider> =
            Arc::new(FailedGitProvider::new("fatal: not a git repository"));
        let svc = MergeService::new(gp, event_store.clone(), dummy_pool());

        let params = make_params();
        let result = svc.attempt_merge_core(&params, 1).await;
        assert!(result.is_ok());

        let outcome = result.unwrap();
        match &outcome {
            MergeOutcome::Failed {
                reason,
                is_transient,
            } => {
                assert_eq!(reason, "fatal: not a git repository");
                assert!(!is_transient);
            }
            _ => panic!("expected Failed, got {outcome:?}"),
        }

        let events = event_store.emitted_events().await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "MergeAttempted");
        assert_eq!(events[1].event_type, "MergeFailed");

        // Verify failure category in payload
        let payload = &events[1].payload;
        assert_eq!(payload["failure_category"], "Permanent");
        assert_eq!(payload["reason"], "fatal: not a git repository");
    }

    #[tokio::test]
    async fn transient_failure_classified_correctly() {
        let event_store = Arc::new(RecordingEventStore::new());
        let gp: Arc<dyn GitProvider> =
            Arc::new(FailedGitProvider::new("lock contention on ref"));
        let svc = MergeService::new(gp, event_store.clone(), dummy_pool());

        let params = make_params();
        let result = svc.attempt_merge_core(&params, 1).await;
        assert!(result.is_ok());

        let outcome = result.unwrap();
        match &outcome {
            MergeOutcome::Failed {
                is_transient, ..
            } => {
                assert!(*is_transient, "lock contention should be transient");
            }
            _ => panic!("expected Failed, got {outcome:?}"),
        }

        let events = event_store.emitted_events().await;
        let payload = &events[1].payload;
        assert_eq!(payload["failure_category"], "Transient");
    }

    #[tokio::test]
    async fn skip_merge_emits_merge_skipped() {
        let event_store = Arc::new(RecordingEventStore::new());
        let gp: Arc<dyn GitProvider> = Arc::new(SuccessGitProvider::new());
        let svc = MergeService::new(gp, event_store.clone(), dummy_pool());

        let task_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let instance_id = Uuid::new_v4();
        let cycle_id = Uuid::new_v4();

        let result = svc
            .skip_merge(task_id, run_id, instance_id, cycle_id, "no changes detected")
            .await;
        assert!(result.is_ok());

        let events = event_store.emitted_events().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "MergeSkipped");
        assert_eq!(events[0].instance_id, instance_id);

        let payload = &events[0].payload;
        assert_eq!(payload["task_id"], task_id.to_string());
        assert_eq!(payload["run_id"], run_id.to_string());
        assert_eq!(payload["cycle_id"], cycle_id.to_string());
        assert_eq!(payload["reason"], "no changes detected");
    }

    #[tokio::test]
    async fn idempotency_key_merge_attempted() {
        let event_store = Arc::new(RecordingEventStore::new());
        let gp: Arc<dyn GitProvider> = Arc::new(SuccessGitProvider::new());
        let svc = MergeService::new(gp, event_store.clone(), dummy_pool());

        let params = make_params();
        svc.attempt_merge_core(&params, 1)
            .await
            .expect("should succeed");

        let events = event_store.emitted_events().await;
        let key = events[0]
            .idempotency_key
            .as_ref()
            .expect("should have key");
        assert_eq!(
            *key,
            format!("merge-attempted-{}-1", params.task_id)
        );
    }

    #[tokio::test]
    async fn idempotency_key_merge_succeeded() {
        let event_store = Arc::new(RecordingEventStore::new());
        let gp: Arc<dyn GitProvider> = Arc::new(SuccessGitProvider::new());
        let svc = MergeService::new(gp, event_store.clone(), dummy_pool());

        let params = make_params();
        svc.attempt_merge_core(&params, 1)
            .await
            .expect("should succeed");

        let events = event_store.emitted_events().await;
        let key = events[1]
            .idempotency_key
            .as_ref()
            .expect("should have key");
        assert_eq!(
            *key,
            format!("merge-succeeded-{}-1", params.task_id)
        );
    }

    #[tokio::test]
    async fn idempotency_key_merge_conflicted() {
        let event_store = Arc::new(RecordingEventStore::new());
        let gp: Arc<dyn GitProvider> =
            Arc::new(ConflictGitProvider::new(vec!["file.rs"]));
        let svc = MergeService::new(gp, event_store.clone(), dummy_pool());

        let params = make_params();
        svc.attempt_merge_core(&params, 1)
            .await
            .expect("should succeed");

        let events = event_store.emitted_events().await;
        let key = events[1]
            .idempotency_key
            .as_ref()
            .expect("should have key");
        assert_eq!(
            *key,
            format!("merge-conflicted-{}-1", params.task_id)
        );
    }

    #[tokio::test]
    async fn idempotency_key_merge_failed() {
        let event_store = Arc::new(RecordingEventStore::new());
        let gp: Arc<dyn GitProvider> =
            Arc::new(FailedGitProvider::new("something broke"));
        let svc = MergeService::new(gp, event_store.clone(), dummy_pool());

        let params = make_params();
        svc.attempt_merge_core(&params, 1)
            .await
            .expect("should succeed");

        let events = event_store.emitted_events().await;
        let key = events[1]
            .idempotency_key
            .as_ref()
            .expect("should have key");
        assert_eq!(
            *key,
            format!("merge-failed-{}-1", params.task_id)
        );
    }

    #[tokio::test]
    async fn idempotency_key_merge_skipped() {
        let event_store = Arc::new(RecordingEventStore::new());
        let gp: Arc<dyn GitProvider> = Arc::new(SuccessGitProvider::new());
        let svc = MergeService::new(gp, event_store.clone(), dummy_pool());

        let task_id = Uuid::new_v4();
        svc.skip_merge(
            task_id,
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            "no changes",
        )
        .await
        .expect("should succeed");

        let events = event_store.emitted_events().await;
        let key = events[0]
            .idempotency_key
            .as_ref()
            .expect("should have key");
        assert_eq!(*key, format!("merge-skipped-{}", task_id));
    }

    #[tokio::test]
    async fn event_version_is_always_one() {
        let event_store = Arc::new(RecordingEventStore::new());
        let gp: Arc<dyn GitProvider> = Arc::new(SuccessGitProvider::new());
        let svc = MergeService::new(gp, event_store.clone(), dummy_pool());

        let params = make_params();
        svc.attempt_merge_core(&params, 1)
            .await
            .expect("should succeed");

        let events = event_store.emitted_events().await;
        for event in &events {
            assert_eq!(
                event.event_version, 1,
                "event_version should be 1 for {}",
                event.event_type
            );
        }
    }

    #[tokio::test]
    async fn event_seq_is_zero() {
        let event_store = Arc::new(RecordingEventStore::new());
        let gp: Arc<dyn GitProvider> = Arc::new(SuccessGitProvider::new());
        let svc = MergeService::new(gp, event_store.clone(), dummy_pool());

        let params = make_params();
        svc.attempt_merge_core(&params, 1)
            .await
            .expect("should succeed");

        let events = event_store.emitted_events().await;
        for event in &events {
            assert_eq!(
                event.seq, 0,
                "seq should be 0 (assigned by store) for {}",
                event.event_type
            );
        }
    }

    #[tokio::test]
    async fn succeeded_payload_contains_expected_fields() {
        let event_store = Arc::new(RecordingEventStore::new());
        let gp: Arc<dyn GitProvider> = Arc::new(SuccessGitProvider::new());
        let svc = MergeService::new(gp, event_store.clone(), dummy_pool());

        let params = make_params();
        svc.attempt_merge_core(&params, 1)
            .await
            .expect("should succeed");

        let events = event_store.emitted_events().await;
        let payload = &events[1].payload;

        assert_eq!(payload["task_id"], params.task_id.to_string());
        assert_eq!(payload["run_id"], params.run_id.to_string());
        assert_eq!(payload["cycle_id"], params.cycle_id.to_string());
        assert!(payload["merge_sha"].is_string());
        assert_eq!(payload["files_changed"], 3);
        assert_eq!(payload["insertions"], 45);
        assert_eq!(payload["deletions"], 12);
        assert_eq!(payload["actor"]["kind"], "System");
    }

    #[tokio::test]
    async fn attempted_payload_contains_expected_fields() {
        let event_store = Arc::new(RecordingEventStore::new());
        let gp: Arc<dyn GitProvider> = Arc::new(SuccessGitProvider::new());
        let svc = MergeService::new(gp, event_store.clone(), dummy_pool());

        let params = make_params();
        svc.attempt_merge_core(&params, 3)
            .await
            .expect("should succeed");

        let events = event_store.emitted_events().await;
        let payload = &events[0].payload;

        assert_eq!(payload["task_id"], params.task_id.to_string());
        assert_eq!(payload["run_id"], params.run_id.to_string());
        assert_eq!(payload["cycle_id"], params.cycle_id.to_string());
        assert_eq!(payload["source_branch"], "feature/task-1");
        assert_eq!(payload["target_branch"], "main");
        assert_eq!(payload["attempt"], 3);
        assert_eq!(payload["actor"]["kind"], "System");
    }

    #[tokio::test]
    async fn conflicted_payload_contains_expected_fields() {
        let event_store = Arc::new(RecordingEventStore::new());
        let gp: Arc<dyn GitProvider> = Arc::new(ConflictGitProvider::new(vec![
            "src/lib.rs",
            "Cargo.toml",
            "README.md",
        ]));
        let svc = MergeService::new(gp, event_store.clone(), dummy_pool());

        let params = make_params();
        svc.attempt_merge_core(&params, 1)
            .await
            .expect("should succeed");

        let events = event_store.emitted_events().await;
        let payload = &events[1].payload;

        assert_eq!(payload["task_id"], params.task_id.to_string());
        assert_eq!(payload["conflict_count"], 3);
        let files = payload["conflicting_files"]
            .as_array()
            .expect("should be array");
        assert_eq!(files.len(), 3);
        assert!(files.contains(&serde_json::json!("src/lib.rs")));
        assert!(files.contains(&serde_json::json!("Cargo.toml")));
        assert!(files.contains(&serde_json::json!("README.md")));
    }

    #[tokio::test]
    async fn failed_payload_contains_expected_fields() {
        let event_store = Arc::new(RecordingEventStore::new());
        let gp: Arc<dyn GitProvider> =
            Arc::new(FailedGitProvider::new("connection timed out"));
        let svc = MergeService::new(gp, event_store.clone(), dummy_pool());

        let params = make_params();
        svc.attempt_merge_core(&params, 1)
            .await
            .expect("should succeed");

        let events = event_store.emitted_events().await;
        let payload = &events[1].payload;

        assert_eq!(payload["task_id"], params.task_id.to_string());
        assert_eq!(payload["run_id"], params.run_id.to_string());
        assert_eq!(payload["cycle_id"], params.cycle_id.to_string());
        assert_eq!(payload["failure_category"], "Transient");
        assert_eq!(payload["reason"], "connection timed out");
        assert_eq!(payload["actor"]["kind"], "System");
    }

    #[tokio::test]
    async fn skipped_payload_contains_expected_fields() {
        let event_store = Arc::new(RecordingEventStore::new());
        let gp: Arc<dyn GitProvider> = Arc::new(SuccessGitProvider::new());
        let svc = MergeService::new(gp, event_store.clone(), dummy_pool());

        let task_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let instance_id = Uuid::new_v4();
        let cycle_id = Uuid::new_v4();

        svc.skip_merge(
            task_id,
            run_id,
            instance_id,
            cycle_id,
            "empty diff",
        )
        .await
        .expect("should succeed");

        let events = event_store.emitted_events().await;
        let payload = &events[0].payload;

        assert_eq!(payload["task_id"], task_id.to_string());
        assert_eq!(payload["run_id"], run_id.to_string());
        assert_eq!(payload["cycle_id"], cycle_id.to_string());
        assert_eq!(payload["reason"], "empty diff");
        assert_eq!(payload["actor"]["kind"], "System");
    }

    #[tokio::test]
    async fn retry_succeeds_after_transient_failures() {
        tokio::time::pause();

        let event_store = Arc::new(RecordingEventStore::new());
        let gp: Arc<dyn GitProvider> =
            Arc::new(TransientThenSuccessGitProvider::new(2));
        let svc = MergeService::new(gp, event_store.clone(), dummy_pool());

        let params = make_params();
        let result = attempt_merge_core_with_retry(&svc, &params, 3, 10).await;
        assert!(result.is_ok(), "retry should eventually succeed: {result:?}");

        let outcome = result.unwrap();
        assert!(
            matches!(outcome, MergeOutcome::Succeeded { .. }),
            "should be Succeeded after retries, got {outcome:?}"
        );

        let events = event_store.emitted_events().await;
        assert_eq!(events.len(), 6, "should have 6 events for 3 attempts");

        assert_eq!(events[0].event_type, "MergeAttempted");
        assert_eq!(events[1].event_type, "MergeFailed");
        assert_eq!(events[2].event_type, "MergeAttempted");
        assert_eq!(events[3].event_type, "MergeFailed");
        assert_eq!(events[4].event_type, "MergeAttempted");
        assert_eq!(events[5].event_type, "MergeSucceeded");
    }

    #[tokio::test]
    async fn retry_exhausted_returns_failed() {
        tokio::time::pause();

        let event_store = Arc::new(RecordingEventStore::new());
        let gp: Arc<dyn GitProvider> =
            Arc::new(TransientThenSuccessGitProvider::new(10));
        let svc = MergeService::new(gp, event_store.clone(), dummy_pool());

        let params = make_params();
        let result = attempt_merge_core_with_retry(&svc, &params, 2, 10).await;
        assert!(result.is_ok());

        let outcome = result.unwrap();
        assert!(
            matches!(
                outcome,
                MergeOutcome::Failed {
                    is_transient: true,
                    ..
                }
            ),
            "should be Failed transient after exhausting retries, got {outcome:?}"
        );

        let events = event_store.emitted_events().await;
        assert_eq!(events.len(), 6);
    }

    #[tokio::test]
    async fn retry_not_attempted_for_permanent_failure() {
        tokio::time::pause();

        let event_store = Arc::new(RecordingEventStore::new());
        let gp: Arc<dyn GitProvider> =
            Arc::new(FailedGitProvider::new("fatal: not a git repository"));
        let svc = MergeService::new(gp, event_store.clone(), dummy_pool());

        let params = make_params();
        let result = attempt_merge_core_with_retry(&svc, &params, 3, 10).await;
        assert!(result.is_ok());

        let outcome = result.unwrap();
        assert!(
            matches!(
                outcome,
                MergeOutcome::Failed {
                    is_transient: false,
                    ..
                }
            ),
            "should be permanent failure"
        );

        let events = event_store.emitted_events().await;
        assert_eq!(
            events.len(),
            2,
            "permanent failure should not retry"
        );
    }

    #[tokio::test]
    async fn retry_not_attempted_for_conflict() {
        tokio::time::pause();

        let event_store = Arc::new(RecordingEventStore::new());
        let gp: Arc<dyn GitProvider> =
            Arc::new(ConflictGitProvider::new(vec!["file.rs"]));
        let svc = MergeService::new(gp, event_store.clone(), dummy_pool());

        let params = make_params();
        let result = attempt_merge_core_with_retry(&svc, &params, 3, 10).await;
        assert!(result.is_ok());

        let outcome = result.unwrap();
        assert!(
            matches!(outcome, MergeOutcome::Conflicted { .. }),
            "should be Conflicted"
        );

        let events = event_store.emitted_events().await;
        assert_eq!(
            events.len(),
            2,
            "conflict should not retry"
        );
    }

    #[tokio::test]
    async fn retry_idempotency_keys_include_attempt_number() {
        tokio::time::pause();

        let event_store = Arc::new(RecordingEventStore::new());
        let gp: Arc<dyn GitProvider> =
            Arc::new(TransientThenSuccessGitProvider::new(1));
        let svc = MergeService::new(gp, event_store.clone(), dummy_pool());

        let params = make_params();
        attempt_merge_core_with_retry(&svc, &params, 2, 10)
            .await
            .expect("should succeed");

        let events = event_store.emitted_events().await;

        // First attempt
        let key1 = events[0].idempotency_key.as_ref().expect("key");
        assert!(
            key1.ends_with("-1"),
            "first attempt key should end with -1, got {key1}"
        );

        // Second attempt
        let key2 = events[2].idempotency_key.as_ref().expect("key");
        assert!(
            key2.ends_with("-2"),
            "second attempt key should end with -2, got {key2}"
        );
    }

    #[test]
    fn merge_params_clone() {
        let params = make_params();
        let cloned = params.clone();
        assert_eq!(params.task_id, cloned.task_id);
        assert_eq!(params.run_id, cloned.run_id);
        assert_eq!(params.repo_path, cloned.repo_path);
    }

    #[test]
    fn merge_params_debug() {
        let params = make_params();
        let debug = format!("{params:?}");
        assert!(debug.contains("MergeParams"));
        assert!(debug.contains("task_id"));
    }

    #[test]
    fn merge_outcome_debug() {
        let outcome = MergeOutcome::Succeeded {
            merge_sha: "abc".to_string(),
            diff: DiffStat {
                files_changed: 1,
                insertions: 2,
                deletions: 3,
            },
        };
        let debug = format!("{outcome:?}");
        assert!(debug.contains("Succeeded"));
    }

    #[test]
    fn merge_outcome_eq() {
        let a = MergeOutcome::Failed {
            reason: "x".into(),
            is_transient: true,
        };
        let b = MergeOutcome::Failed {
            reason: "x".into(),
            is_transient: true,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn merge_outcome_ne() {
        let a = MergeOutcome::Failed {
            reason: "x".into(),
            is_transient: true,
        };
        let b = MergeOutcome::Failed {
            reason: "y".into(),
            is_transient: true,
        };
        assert_ne!(a, b);
    }
}
