use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;
use uuid::Uuid;

use crate::domain::ports::WorkerManager;
use crate::domain::worker::{WorkerError, WorkerHandle, WorkerSpawnConfig};

/// Notification sent when a worker process exits.
#[derive(Debug, Clone)]
pub struct WorkerExitNotification {
    pub session_id: Uuid,
    pub run_id: Uuid,
    pub instance_id: Uuid,
    pub exit_code: Option<i32>,
    pub log_stdout: String,
    pub log_stderr: String,
}

/// Entry in the local worker state file. Maps a session to a running process.
///
/// This is adapter-level state -- NOT domain events. It tracks PID-to-session
/// mappings for process management only.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct WorkerStateEntry {
    pid: u32,
    run_id: Uuid,
    session_id: Uuid,
    instance_id: Uuid,
    started_at: chrono::DateTime<chrono::Utc>,
    log_stdout: String,
    log_stderr: String,
}

/// Local worker state -- tracks all active PID-to-session mappings.
/// Persisted to disk as `{data_dir}/worker_state.json` for crash recovery.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
struct WorkerState {
    /// Keyed by session_id.
    entries: HashMap<Uuid, WorkerStateEntry>,
}

/// Subprocess-based worker manager that spawns `claude` CLI processes.
///
/// Responsibilities:
/// - Spawn `claude --print --output-format stream-json` with prompt as arg
/// - Redirect stdout/stderr to log files
/// - Track PIDs in a local state file (NOT events)
/// - Check process liveness via `/proc/{pid}/status` or `kill -0`
/// - Send SIGTERM/SIGKILL for cancellation/kill
pub struct ProcessWorkerManager {
    data_dir: PathBuf,
    state: Arc<Mutex<WorkerState>>,
    claude_binary: String,
    /// Optional channel for notifying when workers exit.
    completion_tx: Option<tokio::sync::mpsc::UnboundedSender<WorkerExitNotification>>,
}

impl ProcessWorkerManager {
    /// Create a new ProcessWorkerManager.
    ///
    /// # Arguments
    /// * `data_dir` - Base directory for logs and state files
    /// * `claude_binary` - Path/name of the claude binary (defaults to "claude")
    pub fn new(data_dir: PathBuf, claude_binary: Option<String>) -> Self {
        Self {
            data_dir,
            state: Arc::new(Mutex::new(WorkerState::default())),
            claude_binary: claude_binary.unwrap_or_else(|| "claude".to_string()),
            completion_tx: None,
        }
    }

    /// Set the completion notification channel.
    pub fn with_completion_sender(
        mut self,
        tx: tokio::sync::mpsc::UnboundedSender<WorkerExitNotification>,
    ) -> Self {
        self.completion_tx = Some(tx);
        self
    }

    /// Build the log directory path for a given instance and run.
    fn log_dir(&self, instance_id: Uuid, run_id: Uuid) -> PathBuf {
        self.data_dir
            .join(instance_id.to_string())
            .join(run_id.to_string())
    }

    /// Build the stdout log path for a given instance and run.
    fn stdout_path(&self, instance_id: Uuid, run_id: Uuid) -> PathBuf {
        self.log_dir(instance_id, run_id).join("stdout.log")
    }

    /// Build the stderr log path for a given instance and run.
    fn stderr_path(&self, instance_id: Uuid, run_id: Uuid) -> PathBuf {
        self.log_dir(instance_id, run_id).join("stderr.log")
    }

    /// Path to the state file.
    fn state_file_path(&self) -> PathBuf {
        self.data_dir.join("worker_state.json")
    }

    /// Persist the worker state to disk atomically (write temp + rename).
    async fn persist_state(&self, state: &WorkerState) -> Result<(), WorkerError> {
        let json = serde_json::to_string_pretty(state)
            .map_err(|e| WorkerError::IoError(format!("serialize state: {e}")))?;

        let state_path = self.state_file_path();

        // Ensure parent directory exists
        if let Some(parent) = state_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Atomic write: write to temp file, then rename
        let temp_path = state_path.with_extension("json.tmp");
        tokio::fs::write(&temp_path, json.as_bytes()).await?;
        if let Err(e) = tokio::fs::rename(&temp_path, &state_path).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(WorkerError::from(e));
        }

        Ok(())
    }

    /// Load the worker state from disk, returning default if file doesn't exist.
    async fn load_state(&self) -> Result<WorkerState, WorkerError> {
        let state_path = self.state_file_path();
        match tokio::fs::read_to_string(&state_path).await {
            Ok(contents) => serde_json::from_str(&contents)
                .map_err(|e| WorkerError::IoError(format!("deserialize state: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(WorkerState::default()),
            Err(e) => Err(WorkerError::from(e)),
        }
    }

    /// Load state from disk into memory (used on startup/reattach).
    pub async fn load_persisted_state(&self) -> Result<(), WorkerError> {
        let loaded = self.load_state().await?;
        let mut state = self.state.lock().await;
        *state = loaded;
        Ok(())
    }

    /// Check if a PID is alive by checking `/proc/{pid}/status`.
    ///
    /// Returns true if the process exists (the procfs entry is readable).
    fn is_pid_alive(pid: u32) -> bool {
        std::fs::metadata(format!("/proc/{pid}/status")).is_ok()
    }

    /// Verify that the process with the given PID belongs to the expected session
    /// by checking `/proc/{pid}/environ` for the OPENCLAW_SESSION_ID.
    ///
    /// Uses null-byte splitting for exact key=value matching to prevent
    /// false positives from substring matches (e.g. PARENT_OPENCLAW_SESSION_ID).
    async fn verify_session(pid: u32, session_id: Uuid) -> bool {
        let environ_path = format!("/proc/{pid}/environ");
        match tokio::fs::read(&environ_path).await {
            Ok(data) => {
                // /proc/pid/environ uses null bytes as separators between KEY=VALUE pairs
                let needle = format!("OPENCLAW_SESSION_ID={session_id}");
                data.split(|&b| b == 0)
                    .any(|var| var == needle.as_bytes())
            }
            Err(_) => false,
        }
    }

    /// Send a signal to a PID by shelling out to `kill`.
    ///
    /// Uses `std::process::Command` to avoid requiring the `libc` crate.
    async fn send_signal(pid: u32, signal: &str) -> Result<(), WorkerError> {
        let output = tokio::process::Command::new("kill")
            .arg(format!("-{signal}"))
            .arg(pid.to_string())
            .output()
            .await
            .map_err(|e| WorkerError::IoError(format!("failed to run kill command: {e}")))?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // "No such process" is not an error for cancel/kill -- process already exited
            if stderr.contains("No such process") || stderr.contains("ESRCH") {
                Ok(())
            } else if stderr.contains("Operation not permitted") {
                Err(WorkerError::IoError(format!(
                    "permission denied sending signal {signal} to pid {pid}"
                )))
            } else {
                tracing::warn!(
                    pid = pid,
                    signal = signal,
                    stderr = %stderr,
                    "unexpected error sending signal, treating as non-fatal"
                );
                Ok(())
            }
        }
    }
}

#[async_trait::async_trait]
impl WorkerManager for ProcessWorkerManager {
    async fn spawn(&self, config: WorkerSpawnConfig) -> Result<WorkerHandle, WorkerError> {
        // Check if session is already running
        {
            let state = self.state.lock().await;
            if state.entries.contains_key(&config.session_id) {
                return Err(WorkerError::AlreadyRunning(config.session_id));
            }
        }

        // Create log directory
        let log_dir = self.log_dir(config.instance_id, config.run_id);
        tokio::fs::create_dir_all(&log_dir).await?;

        // Open log files
        let stdout_path = self.stdout_path(config.instance_id, config.run_id);
        let stderr_path = self.stderr_path(config.instance_id, config.run_id);

        let stdout_file = std::fs::File::create(&stdout_path)
            .map_err(|e| WorkerError::SpawnFailed(format!("create stdout log: {e}")))?;
        let stderr_file = std::fs::File::create(&stderr_path)
            .map_err(|e| WorkerError::SpawnFailed(format!("create stderr log: {e}")))?;

        // Build the command
        let mut cmd = tokio::process::Command::new(&self.claude_binary);
        cmd.arg("--print")
            .arg("--verbose")
            .arg("--output-format")
            .arg("stream-json")
            .arg(&config.prompt)
            .current_dir(&config.worktree_path)
            .stdout(std::process::Stdio::from(stdout_file))
            .stderr(std::process::Stdio::from(stderr_file))
            .stdin(std::process::Stdio::null());

        // Set environment variables from config
        for (key, value) in &config.environment {
            cmd.env(key, value);
        }

        // Set OpenClaw-specific env vars
        cmd.env("OPENCLAW_SESSION_ID", config.session_id.to_string());
        cmd.env("OPENCLAW_RUN_ID", config.run_id.to_string());
        cmd.env("OPENCLAW_INSTANCE_ID", config.instance_id.to_string());

        // Spawn the process
        let mut child = cmd
            .spawn()
            .map_err(|e| WorkerError::SpawnFailed(format!("spawn {}: {e}", self.claude_binary)))?;

        let pid = child
            .id()
            .ok_or_else(|| WorkerError::SpawnFailed("process exited immediately".to_string()))?;

        // Spawn a background task to reap the child process (prevents zombies).
        // The child handle must be awaited to prevent accumulating zombie processes.
        let reap_session_id = config.session_id;
        let reap_run_id = config.run_id;
        let reap_instance_id = config.instance_id;
        let reap_stdout = stdout_path.to_string_lossy().to_string();
        let reap_stderr = stderr_path.to_string_lossy().to_string();
        let reap_tx = self.completion_tx.clone();
        let reap_state = self.state.clone();
        tokio::spawn(async move {
            let exit_code = match child.wait().await {
                Ok(status) => {
                    tracing::info!(
                        session_id = %reap_session_id,
                        run_id = %reap_run_id,
                        pid = pid,
                        exit_code = status.code(),
                        "worker process exited"
                    );
                    status.code()
                }
                Err(e) => {
                    tracing::warn!(
                        session_id = %reap_session_id,
                        run_id = %reap_run_id,
                        pid = pid,
                        error = %e,
                        "failed to wait on worker process"
                    );
                    None
                }
            };

            // Remove from in-memory state
            {
                let mut state = reap_state.lock().await;
                state.entries.remove(&reap_session_id);
                // Best-effort persist — don't block on failure
            }

            // Notify completion handler
            if let Some(tx) = reap_tx {
                let _ = tx.send(WorkerExitNotification {
                    session_id: reap_session_id,
                    run_id: reap_run_id,
                    instance_id: reap_instance_id,
                    exit_code,
                    log_stdout: reap_stdout,
                    log_stderr: reap_stderr,
                });
            }
        });

        // Store state entry
        let entry = WorkerStateEntry {
            pid,
            run_id: config.run_id,
            session_id: config.session_id,
            instance_id: config.instance_id,
            started_at: chrono::Utc::now(),
            log_stdout: stdout_path.to_string_lossy().to_string(),
            log_stderr: stderr_path.to_string_lossy().to_string(),
        };

        let handle = WorkerHandle {
            session_id: config.session_id,
            run_id: config.run_id,
            log_stdout: entry.log_stdout.clone(),
            log_stderr: entry.log_stderr.clone(),
        };

        // Update in-memory state and persist
        {
            let mut state = self.state.lock().await;
            state.entries.insert(config.session_id, entry);
            self.persist_state(&state).await?;
        }

        tracing::info!(
            session_id = %config.session_id,
            run_id = %config.run_id,
            pid = pid,
            "worker spawned"
        );

        Ok(handle)
    }

    async fn is_alive(&self, session_id: Uuid) -> Result<bool, WorkerError> {
        let state = self.state.lock().await;
        let entry = state
            .entries
            .get(&session_id)
            .ok_or(WorkerError::NotFound(session_id))?;

        let pid = entry.pid;

        // Check PID is alive
        if !Self::is_pid_alive(pid) {
            return Ok(false);
        }

        // Verify the process belongs to this session
        Ok(Self::verify_session(pid, session_id).await)
    }

    async fn cancel(&self, session_id: Uuid) -> Result<(), WorkerError> {
        let state = self.state.lock().await;
        let entry = state
            .entries
            .get(&session_id)
            .ok_or(WorkerError::NotFound(session_id))?;

        let pid = entry.pid;
        drop(state); // Release lock before signaling

        tracing::info!(session_id = %session_id, pid = pid, "sending SIGTERM to worker");
        Self::send_signal(pid, "TERM").await
    }

    async fn kill(&self, session_id: Uuid) -> Result<(), WorkerError> {
        let pid;
        {
            let mut state = self.state.lock().await;
            let entry = state
                .entries
                .get(&session_id)
                .ok_or(WorkerError::NotFound(session_id))?;

            pid = entry.pid;

            // Remove from state since we're force-killing
            state.entries.remove(&session_id);
            self.persist_state(&state).await?;
        }

        tracing::info!(session_id = %session_id, pid = pid, "sending SIGKILL to worker");
        Self::send_signal(pid, "KILL").await
    }

    async fn reattach(
        &self,
        session_id: Uuid,
        run_id: Uuid,
    ) -> Result<Option<WorkerHandle>, WorkerError> {
        // Load persisted state from disk first
        let disk_state = self.load_state().await?;

        let entry = match disk_state.entries.get(&session_id) {
            Some(e) => e.clone(),
            None => return Ok(None),
        };

        // Verify run_id matches
        if entry.run_id != run_id {
            return Ok(None);
        }

        // Check if the process is still alive
        if !Self::is_pid_alive(entry.pid) {
            // Clean up stale entry
            let mut state = self.state.lock().await;
            state.entries.remove(&session_id);
            self.persist_state(&state).await?;
            return Ok(None);
        }

        // Verify session ownership
        if !Self::verify_session(entry.pid, session_id).await {
            // PID reuse -- different process
            let mut state = self.state.lock().await;
            state.entries.remove(&session_id);
            self.persist_state(&state).await?;
            return Ok(None);
        }

        // Re-populate in-memory state
        let handle = WorkerHandle {
            session_id: entry.session_id,
            run_id: entry.run_id,
            log_stdout: entry.log_stdout.clone(),
            log_stderr: entry.log_stderr.clone(),
        };

        {
            let mut state = self.state.lock().await;
            state.entries.insert(session_id, entry);
            // No need to persist -- it's already on disk
        }

        tracing::info!(
            session_id = %session_id,
            run_id = %run_id,
            "reattached to worker"
        );

        Ok(Some(handle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::Duration;

    #[test]
    fn process_worker_manager_new_defaults() {
        let mgr = ProcessWorkerManager::new(PathBuf::from("/tmp/test-data"), None);
        assert_eq!(mgr.data_dir, PathBuf::from("/tmp/test-data"));
        assert_eq!(mgr.claude_binary, "claude");
    }

    #[test]
    fn process_worker_manager_new_custom_binary() {
        let mgr = ProcessWorkerManager::new(
            PathBuf::from("/data"),
            Some("/usr/local/bin/claude-code".to_string()),
        );
        assert_eq!(mgr.claude_binary, "/usr/local/bin/claude-code");
    }

    #[test]
    fn log_dir_construction() {
        let mgr = ProcessWorkerManager::new(PathBuf::from("/data/openclaw"), None);
        let instance_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let run_id = Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001").unwrap();

        let log_dir = mgr.log_dir(instance_id, run_id);
        assert_eq!(
            log_dir,
            PathBuf::from("/data/openclaw/550e8400-e29b-41d4-a716-446655440000/660e8400-e29b-41d4-a716-446655440001")
        );
    }

    #[test]
    fn stdout_path_construction() {
        let mgr = ProcessWorkerManager::new(PathBuf::from("/data"), None);
        let instance_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let run_id = Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001").unwrap();

        let path = mgr.stdout_path(instance_id, run_id);
        assert!(path.to_string_lossy().ends_with("stdout.log"));
        assert!(path
            .to_string_lossy()
            .contains("550e8400-e29b-41d4-a716-446655440000"));
        assert!(path
            .to_string_lossy()
            .contains("660e8400-e29b-41d4-a716-446655440001"));
    }

    #[test]
    fn stderr_path_construction() {
        let mgr = ProcessWorkerManager::new(PathBuf::from("/data"), None);
        let instance_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let run_id = Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001").unwrap();

        let path = mgr.stderr_path(instance_id, run_id);
        assert!(path.to_string_lossy().ends_with("stderr.log"));
    }

    #[test]
    fn state_file_path_construction() {
        let mgr = ProcessWorkerManager::new(PathBuf::from("/data/openclaw"), None);
        assert_eq!(
            mgr.state_file_path(),
            PathBuf::from("/data/openclaw/worker_state.json")
        );
    }

    #[test]
    fn worker_state_default_is_empty() {
        let state = WorkerState::default();
        assert!(state.entries.is_empty());
    }

    #[test]
    fn worker_state_entry_serde_round_trip() {
        let entry = WorkerStateEntry {
            pid: 12345,
            run_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            started_at: chrono::Utc::now(),
            log_stdout: "/data/inst/run/stdout.log".to_string(),
            log_stderr: "/data/inst/run/stderr.log".to_string(),
        };

        let json = serde_json::to_string(&entry).expect("serialize entry");
        let restored: WorkerStateEntry = serde_json::from_str(&json).expect("deserialize entry");

        assert_eq!(entry.pid, restored.pid);
        assert_eq!(entry.run_id, restored.run_id);
        assert_eq!(entry.session_id, restored.session_id);
        assert_eq!(entry.instance_id, restored.instance_id);
        assert_eq!(entry.log_stdout, restored.log_stdout);
        assert_eq!(entry.log_stderr, restored.log_stderr);
    }

    #[test]
    fn worker_state_serde_round_trip() {
        let session_id = Uuid::new_v4();
        let mut state = WorkerState::default();
        state.entries.insert(
            session_id,
            WorkerStateEntry {
                pid: 42,
                run_id: Uuid::new_v4(),
                session_id,
                instance_id: Uuid::new_v4(),
                started_at: chrono::Utc::now(),
                log_stdout: "/tmp/stdout.log".to_string(),
                log_stderr: "/tmp/stderr.log".to_string(),
            },
        );

        let json = serde_json::to_string_pretty(&state).expect("serialize state");
        let restored: WorkerState = serde_json::from_str(&json).expect("deserialize state");

        assert_eq!(restored.entries.len(), 1);
        assert!(restored.entries.contains_key(&session_id));
        assert_eq!(restored.entries[&session_id].pid, 42);
    }

    #[test]
    fn worker_state_multiple_entries() {
        let mut state = WorkerState::default();
        for i in 0..5 {
            let sid = Uuid::new_v4();
            state.entries.insert(
                sid,
                WorkerStateEntry {
                    pid: 1000 + i,
                    run_id: Uuid::new_v4(),
                    session_id: sid,
                    instance_id: Uuid::new_v4(),
                    started_at: chrono::Utc::now(),
                    log_stdout: format!("/tmp/{i}/stdout.log"),
                    log_stderr: format!("/tmp/{i}/stderr.log"),
                },
            );
        }
        assert_eq!(state.entries.len(), 5);

        let json = serde_json::to_string(&state).expect("serialize");
        let restored: WorkerState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.entries.len(), 5);
    }

    #[test]
    fn is_pid_alive_for_init_process() {
        // PID 1 (init/systemd) should always be alive
        assert!(ProcessWorkerManager::is_pid_alive(1));
    }

    #[test]
    fn is_pid_alive_for_nonexistent_pid() {
        // PID max on Linux is typically 4194304; use something unlikely
        assert!(!ProcessWorkerManager::is_pid_alive(4_194_303));
    }

    #[tokio::test]
    async fn persist_and_load_state() {
        let temp_dir = std::env::temp_dir().join(format!("openclaw-test-{}", Uuid::new_v4()));
        let mgr = ProcessWorkerManager::new(temp_dir.clone(), None);

        let session_id = Uuid::new_v4();
        let mut state = WorkerState::default();
        state.entries.insert(
            session_id,
            WorkerStateEntry {
                pid: 9999,
                run_id: Uuid::new_v4(),
                session_id,
                instance_id: Uuid::new_v4(),
                started_at: chrono::Utc::now(),
                log_stdout: "/tmp/stdout.log".to_string(),
                log_stderr: "/tmp/stderr.log".to_string(),
            },
        );

        mgr.persist_state(&state).await.expect("persist");

        let loaded = mgr.load_state().await.expect("load");
        assert_eq!(loaded.entries.len(), 1);
        assert!(loaded.entries.contains_key(&session_id));
        assert_eq!(loaded.entries[&session_id].pid, 9999);

        // Cleanup
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    }

    #[tokio::test]
    async fn load_state_returns_default_when_no_file() {
        let temp_dir = std::env::temp_dir().join(format!("openclaw-test-missing-{}", Uuid::new_v4()));
        let mgr = ProcessWorkerManager::new(temp_dir, None);

        let state = mgr.load_state().await.expect("load missing");
        assert!(state.entries.is_empty());
    }

    #[tokio::test]
    async fn spawn_fails_with_nonexistent_binary() {
        let temp_dir = std::env::temp_dir().join(format!("openclaw-test-spawn-{}", Uuid::new_v4()));
        let mgr = ProcessWorkerManager::new(
            temp_dir.clone(),
            Some("/nonexistent/binary/claude-fake".to_string()),
        );

        let config = WorkerSpawnConfig {
            run_id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            prompt: "test prompt".to_string(),
            timeout: Duration::from_secs(60),
            heartbeat_interval: Duration::from_secs(5),
            worktree_path: std::env::temp_dir().to_string_lossy().to_string(),
            environment: HashMap::new(),
        };

        let result = mgr.spawn(config).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            WorkerError::SpawnFailed(msg) => {
                assert!(msg.contains("claude-fake"), "error should mention binary: {msg}");
            }
            other => panic!("expected SpawnFailed, got: {other}"),
        }

        // Cleanup
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    }

    #[tokio::test]
    async fn spawn_rejects_duplicate_session() {
        let temp_dir = std::env::temp_dir().join(format!("openclaw-test-dup-{}", Uuid::new_v4()));
        let mgr = ProcessWorkerManager::new(temp_dir.clone(), Some("sleep".to_string()));

        let session_id = Uuid::new_v4();
        let instance_id = Uuid::new_v4();

        // Spawn first worker (using 'sleep' as a stand-in)
        let config1 = WorkerSpawnConfig {
            run_id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            instance_id,
            session_id,
            prompt: "3600".to_string(), // sleep argument
            timeout: Duration::from_secs(60),
            heartbeat_interval: Duration::from_secs(5),
            worktree_path: std::env::temp_dir().to_string_lossy().to_string(),
            environment: HashMap::new(),
        };

        let handle = mgr.spawn(config1).await.expect("first spawn should succeed");
        assert_eq!(handle.session_id, session_id);

        // Try to spawn with same session_id
        let config2 = WorkerSpawnConfig {
            run_id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            instance_id,
            session_id, // same session_id!
            prompt: "3600".to_string(),
            timeout: Duration::from_secs(60),
            heartbeat_interval: Duration::from_secs(5),
            worktree_path: std::env::temp_dir().to_string_lossy().to_string(),
            environment: HashMap::new(),
        };

        let result = mgr.spawn(config2).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), WorkerError::AlreadyRunning(_)));

        // Kill the spawned process
        let _ = mgr.kill(session_id).await;

        // Cleanup
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    }

    #[tokio::test]
    async fn is_alive_returns_not_found_for_unknown_session() {
        let mgr = ProcessWorkerManager::new(PathBuf::from("/tmp"), None);
        let result = mgr.is_alive(Uuid::new_v4()).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), WorkerError::NotFound(_)));
    }

    #[tokio::test]
    async fn cancel_returns_not_found_for_unknown_session() {
        let mgr = ProcessWorkerManager::new(PathBuf::from("/tmp"), None);
        let result = mgr.cancel(Uuid::new_v4()).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), WorkerError::NotFound(_)));
    }

    #[tokio::test]
    async fn kill_returns_not_found_for_unknown_session() {
        let mgr = ProcessWorkerManager::new(PathBuf::from("/tmp"), None);
        let result = mgr.kill(Uuid::new_v4()).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), WorkerError::NotFound(_)));
    }

    #[tokio::test]
    async fn reattach_returns_none_for_unknown_session() {
        let temp_dir = std::env::temp_dir().join(format!("openclaw-test-reattach-{}", Uuid::new_v4()));
        let mgr = ProcessWorkerManager::new(temp_dir.clone(), None);

        let result = mgr
            .reattach(Uuid::new_v4(), Uuid::new_v4())
            .await
            .expect("reattach should not error");
        assert!(result.is_none());

        // Cleanup
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    }

    #[tokio::test]
    async fn spawn_creates_log_files() {
        let temp_dir = std::env::temp_dir().join(format!("openclaw-test-logs-{}", Uuid::new_v4()));
        let mgr = ProcessWorkerManager::new(temp_dir.clone(), Some("sleep".to_string()));

        let instance_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        let config = WorkerSpawnConfig {
            run_id,
            task_id: Uuid::new_v4(),
            instance_id,
            session_id,
            prompt: "3600".to_string(),
            timeout: Duration::from_secs(60),
            heartbeat_interval: Duration::from_secs(5),
            worktree_path: std::env::temp_dir().to_string_lossy().to_string(),
            environment: HashMap::new(),
        };

        let handle = mgr.spawn(config).await.expect("spawn should succeed");

        // Verify log files were created
        assert!(Path::new(&handle.log_stdout).exists());
        assert!(Path::new(&handle.log_stderr).exists());
        assert!(handle.log_stdout.ends_with("stdout.log"));
        assert!(handle.log_stderr.ends_with("stderr.log"));

        // Verify state file was written
        assert!(mgr.state_file_path().exists());

        // Kill the spawned process
        let _ = mgr.kill(session_id).await;

        // Cleanup
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    }

    #[tokio::test]
    async fn load_persisted_state_populates_memory() {
        let temp_dir = std::env::temp_dir().join(format!("openclaw-test-load-{}", Uuid::new_v4()));
        let mgr = ProcessWorkerManager::new(temp_dir.clone(), None);

        let session_id = Uuid::new_v4();
        let mut state = WorkerState::default();
        state.entries.insert(
            session_id,
            WorkerStateEntry {
                pid: 1, // init process, always alive
                run_id: Uuid::new_v4(),
                session_id,
                instance_id: Uuid::new_v4(),
                started_at: chrono::Utc::now(),
                log_stdout: "/tmp/stdout.log".to_string(),
                log_stderr: "/tmp/stderr.log".to_string(),
            },
        );

        mgr.persist_state(&state).await.expect("persist");

        // Create a fresh manager and load
        let mgr2 = ProcessWorkerManager::new(temp_dir.clone(), None);
        mgr2.load_persisted_state().await.expect("load");

        // Verify in-memory state is populated
        let in_memory = mgr2.state.lock().await;
        assert!(in_memory.entries.contains_key(&session_id));

        // Cleanup
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    }

    #[tokio::test]
    async fn send_signal_to_nonexistent_pid() {
        // Should succeed (no error) since "No such process" is treated as OK
        let result = ProcessWorkerManager::send_signal(4_194_303, "TERM").await;
        assert!(result.is_ok());
    }

    #[test]
    fn worker_state_entry_preserves_all_fields_in_json() {
        let entry = WorkerStateEntry {
            pid: 55555,
            run_id: Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap(),
            session_id: Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap(),
            instance_id: Uuid::parse_str("66666666-7777-8888-9999-aaaaaaaaaaaa").unwrap(),
            started_at: chrono::Utc::now(),
            log_stdout: "/data/instance/run/stdout.log".to_string(),
            log_stderr: "/data/instance/run/stderr.log".to_string(),
        };

        let json = serde_json::to_value(&entry).expect("to_value");
        assert_eq!(json["pid"], 55555);
        assert_eq!(
            json["run_id"],
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
        );
        assert_eq!(
            json["session_id"],
            "11111111-2222-3333-4444-555555555555"
        );
        assert_eq!(
            json["instance_id"],
            "66666666-7777-8888-9999-aaaaaaaaaaaa"
        );
        assert_eq!(json["log_stdout"], "/data/instance/run/stdout.log");
        assert_eq!(json["log_stderr"], "/data/instance/run/stderr.log");
    }
}
