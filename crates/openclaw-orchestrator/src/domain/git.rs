use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Result of a git merge operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeResult {
    /// Merge succeeded cleanly.
    Success { merge_sha: String },
    /// Merge had conflicts.
    Conflict { conflicting_files: Vec<String> },
    /// Merge failed for a non-conflict reason.
    Failed(String),
}

/// Single file status from `git status --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStatus {
    pub status: String, // e.g. "M", "A", "D", "??"
    pub path: String,   // relative file path
}

/// Diff statistics between two refs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffStat {
    pub files_changed: u32,
    pub insertions: u32,
    pub deletions: u32,
}

/// Errors from git operations.
#[derive(Debug)]
pub enum GitError {
    CommandFailed {
        command: String,
        stderr: String,
        exit_code: Option<i32>,
    },
    IoError(String),
    WorktreeExists(String),
    NotARepository(String),
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CommandFailed {
                command,
                stderr,
                exit_code,
            } => {
                write!(
                    f,
                    "git command failed: {command} (exit={exit_code:?}): {stderr}"
                )
            }
            Self::IoError(msg) => write!(f, "git IO error: {msg}"),
            Self::WorktreeExists(path) => write!(f, "worktree already exists: {path}"),
            Self::NotARepository(path) => write!(f, "not a git repository: {path}"),
        }
    }
}

impl std::error::Error for GitError {}

impl From<std::io::Error> for GitError {
    fn from(err: std::io::Error) -> Self {
        Self::IoError(err.to_string())
    }
}

/// Domain interface for git operations. Knows nothing about the `git` binary.
#[async_trait::async_trait]
pub trait GitProvider: Send + Sync {
    /// Create a new worktree for a branch. Returns the worktree path.
    async fn create_worktree(
        &self,
        repo_path: &str,
        worktree_path: &str,
        branch: &str,
    ) -> Result<PathBuf, GitError>;

    /// Remove a worktree.
    async fn remove_worktree(
        &self,
        repo_path: &str,
        worktree_path: &str,
    ) -> Result<(), GitError>;

    /// Merge source branch into target branch. Returns the merge result.
    async fn merge(
        &self,
        repo_path: &str,
        source_branch: &str,
        target_branch: &str,
    ) -> Result<MergeResult, GitError>;

    /// Get diff statistics between two refs.
    async fn diff_stat(
        &self,
        repo_path: &str,
        base: &str,
        head: &str,
    ) -> Result<DiffStat, GitError>;

    /// Get the current HEAD sha.
    async fn current_sha(&self, repo_path: &str) -> Result<String, GitError>;

    /// Get the status of the working tree (porcelain format).
    async fn status_porcelain(&self, repo_path: &str) -> Result<Vec<FileStatus>, GitError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_result_success_variant() {
        let result = MergeResult::Success {
            merge_sha: "abc123".to_string(),
        };
        assert_eq!(
            result,
            MergeResult::Success {
                merge_sha: "abc123".to_string()
            }
        );
    }

    #[test]
    fn merge_result_conflict_variant() {
        let result = MergeResult::Conflict {
            conflicting_files: vec!["file.rs".to_string(), "other.rs".to_string()],
        };
        match &result {
            MergeResult::Conflict { conflicting_files } => {
                assert_eq!(conflicting_files.len(), 2);
                assert_eq!(conflicting_files[0], "file.rs");
                assert_eq!(conflicting_files[1], "other.rs");
            }
            _ => panic!("expected Conflict variant"),
        }
    }

    #[test]
    fn merge_result_failed_variant() {
        let result = MergeResult::Failed("something went wrong".to_string());
        assert_eq!(
            result,
            MergeResult::Failed("something went wrong".to_string())
        );
    }

    #[test]
    fn merge_result_clone() {
        let result = MergeResult::Success {
            merge_sha: "def456".to_string(),
        };
        let cloned = result.clone();
        assert_eq!(result, cloned);
    }

    #[test]
    fn file_status_serde_round_trip() {
        let status = FileStatus {
            status: "M".to_string(),
            path: "src/main.rs".to_string(),
        };

        let json = serde_json::to_string(&status).expect("serialize FileStatus");
        let restored: FileStatus = serde_json::from_str(&json).expect("deserialize FileStatus");

        assert_eq!(status, restored);
    }

    #[test]
    fn file_status_serde_untracked() {
        let status = FileStatus {
            status: "??".to_string(),
            path: "new_file.txt".to_string(),
        };

        let json = serde_json::to_string(&status).expect("serialize");
        let restored: FileStatus = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(restored.status, "??");
        assert_eq!(restored.path, "new_file.txt");
    }

    #[test]
    fn diff_stat_serde_round_trip() {
        let stat = DiffStat {
            files_changed: 3,
            insertions: 45,
            deletions: 12,
        };

        let json = serde_json::to_string(&stat).expect("serialize DiffStat");
        let restored: DiffStat = serde_json::from_str(&json).expect("deserialize DiffStat");

        assert_eq!(stat, restored);
    }

    #[test]
    fn diff_stat_zero_values() {
        let stat = DiffStat {
            files_changed: 0,
            insertions: 0,
            deletions: 0,
        };

        let json = serde_json::to_string(&stat).expect("serialize");
        let restored: DiffStat = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(restored.files_changed, 0);
        assert_eq!(restored.insertions, 0);
        assert_eq!(restored.deletions, 0);
    }

    #[test]
    fn git_error_display_command_failed() {
        let err = GitError::CommandFailed {
            command: "git merge feature".to_string(),
            stderr: "merge conflict".to_string(),
            exit_code: Some(1),
        };
        let msg = err.to_string();
        assert!(msg.contains("git command failed"));
        assert!(msg.contains("git merge feature"));
        assert!(msg.contains("merge conflict"));
        assert!(msg.contains("Some(1)"));
    }

    #[test]
    fn git_error_display_command_failed_no_exit_code() {
        let err = GitError::CommandFailed {
            command: "git status".to_string(),
            stderr: "killed".to_string(),
            exit_code: None,
        };
        let msg = err.to_string();
        assert!(msg.contains("None"));
    }

    #[test]
    fn git_error_display_io_error() {
        let err = GitError::IoError("file not found".to_string());
        assert_eq!(err.to_string(), "git IO error: file not found");
    }

    #[test]
    fn git_error_display_worktree_exists() {
        let err = GitError::WorktreeExists("/tmp/worktree".to_string());
        assert_eq!(err.to_string(), "worktree already exists: /tmp/worktree");
    }

    #[test]
    fn git_error_display_not_a_repository() {
        let err = GitError::NotARepository("/tmp/not-a-repo".to_string());
        assert_eq!(
            err.to_string(),
            "not a git repository: /tmp/not-a-repo"
        );
    }

    #[test]
    fn git_error_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "missing file");
        let git_err = GitError::from(io_err);
        assert!(matches!(git_err, GitError::IoError(_)));
        assert!(git_err.to_string().contains("missing file"));
    }

    #[test]
    fn git_error_from_io_error_permission_denied() {
        let io_err =
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let git_err = GitError::from(io_err);
        assert!(matches!(git_err, GitError::IoError(_)));
        assert!(git_err.to_string().contains("access denied"));
    }

    #[test]
    fn git_error_implements_error_trait() {
        let err: Box<dyn std::error::Error> =
            Box::new(GitError::IoError("test".to_string()));
        assert!(err.to_string().contains("git IO error"));
    }

    #[test]
    fn git_error_debug_format() {
        let err = GitError::WorktreeExists("/tmp/wt".to_string());
        let debug = format!("{err:?}");
        assert!(debug.contains("WorktreeExists"));
        assert!(debug.contains("/tmp/wt"));
    }

    #[test]
    fn file_status_debug_format() {
        let status = FileStatus {
            status: "A".to_string(),
            path: "new.rs".to_string(),
        };
        let debug = format!("{status:?}");
        assert!(debug.contains("FileStatus"));
        assert!(debug.contains("new.rs"));
    }

    #[test]
    fn diff_stat_debug_format() {
        let stat = DiffStat {
            files_changed: 1,
            insertions: 10,
            deletions: 5,
        };
        let debug = format!("{stat:?}");
        assert!(debug.contains("DiffStat"));
        assert!(debug.contains("10"));
    }
}
