use std::path::PathBuf;

use crate::domain::git::*;

/// Git CLI implementation that shells out to the `git` binary.
pub struct GitCli {
    git_binary: String, // defaults to "git"
}

impl GitCli {
    pub fn new(git_binary: Option<String>) -> Self {
        Self {
            git_binary: git_binary.unwrap_or_else(|| "git".to_string()),
        }
    }

    /// Run a git command and return stdout.
    async fn run_git(&self, repo_path: &str, args: &[&str]) -> Result<String, GitError> {
        let output = tokio::process::Command::new(&self.git_binary)
            .args(args)
            .current_dir(repo_path)
            .output()
            .await
            .map_err(|e| GitError::IoError(format!("run git: {e}")))?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout)
                .trim()
                .to_string())
        } else {
            Err(GitError::CommandFailed {
                command: format!("git {}", args.join(" ")),
                stderr: String::from_utf8_lossy(&output.stderr)
                    .trim()
                    .to_string(),
                exit_code: output.status.code(),
            })
        }
    }

    /// Run a git command and return the raw Output (for commands where non-zero
    /// exit codes carry semantic meaning, e.g. merge conflicts).
    async fn run_git_raw(
        &self,
        repo_path: &str,
        args: &[&str],
    ) -> Result<std::process::Output, GitError> {
        tokio::process::Command::new(&self.git_binary)
            .args(args)
            .current_dir(repo_path)
            .output()
            .await
            .map_err(|e| GitError::IoError(format!("run git: {e}")))
    }

    /// Parse `git diff --shortstat` output into a DiffStat.
    ///
    /// Expected format: ` 3 files changed, 45 insertions(+), 12 deletions(-)`
    /// Any of the parts may be missing (e.g. no insertions, only deletions).
    fn parse_shortstat(output: &str) -> DiffStat {
        let trimmed = output.trim();
        if trimmed.is_empty() {
            return DiffStat {
                files_changed: 0,
                insertions: 0,
                deletions: 0,
            };
        }

        let mut files_changed = 0u32;
        let mut insertions = 0u32;
        let mut deletions = 0u32;

        // Split on commas and parse each segment
        for segment in trimmed.split(',') {
            let segment = segment.trim();
            if segment.contains("file") && segment.contains("changed") {
                // "3 files changed" or "1 file changed"
                if let Some(num_str) = segment.split_whitespace().next() {
                    files_changed = num_str.parse().unwrap_or(0);
                }
            } else if segment.contains("insertion") {
                // "45 insertions(+)" or "1 insertion(+)"
                if let Some(num_str) = segment.split_whitespace().next() {
                    insertions = num_str.parse().unwrap_or(0);
                }
            } else if segment.contains("deletion") {
                // "12 deletions(-)" or "1 deletion(-)"
                if let Some(num_str) = segment.split_whitespace().next() {
                    deletions = num_str.parse().unwrap_or(0);
                }
            }
        }

        DiffStat {
            files_changed,
            insertions,
            deletions,
        }
    }

    /// Parse `git status --porcelain` output into a list of FileStatus.
    ///
    /// Each line has the format: `XY path` where XY is a two-character status
    /// code and path is the relative file path. The first 2 characters are the
    /// status, character 3 is a space, and the rest is the path.
    fn parse_porcelain(output: &str) -> Vec<FileStatus> {
        output
            .lines()
            .filter(|line| line.len() >= 4) // minimum: "XY p"
            .map(|line| {
                let status = line[..2].trim().to_string();
                let path = line[3..].to_string();
                FileStatus { status, path }
            })
            .collect()
    }

    /// Check if a branch exists in the repository.
    async fn branch_exists(&self, repo_path: &str, branch: &str) -> Result<bool, GitError> {
        let result = self
            .run_git(repo_path, &["rev-parse", "--verify", branch])
            .await;
        match result {
            Ok(_) => Ok(true),
            Err(GitError::CommandFailed { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

#[async_trait::async_trait]
impl GitProvider for GitCli {
    async fn create_worktree(
        &self,
        repo_path: &str,
        worktree_path: &str,
        branch: &str,
    ) -> Result<PathBuf, GitError> {
        // Check if the worktree path already exists
        if tokio::fs::metadata(worktree_path).await.is_ok() {
            return Err(GitError::WorktreeExists(worktree_path.to_string()));
        }

        if self.branch_exists(repo_path, branch).await? {
            // Branch exists -- attach worktree to it
            self.run_git(repo_path, &["worktree", "add", worktree_path, branch])
                .await?;
        } else {
            // Branch does not exist -- create it from HEAD
            self.run_git(
                repo_path,
                &["worktree", "add", worktree_path, "-b", branch],
            )
            .await?;
        }

        Ok(PathBuf::from(worktree_path))
    }

    async fn remove_worktree(
        &self,
        repo_path: &str,
        worktree_path: &str,
    ) -> Result<(), GitError> {
        self.run_git(
            repo_path,
            &["worktree", "remove", worktree_path, "--force"],
        )
        .await?;
        Ok(())
    }

    async fn merge(
        &self,
        repo_path: &str,
        source_branch: &str,
        target_branch: &str,
    ) -> Result<MergeResult, GitError> {
        // Step 1: checkout the target branch
        self.run_git(repo_path, &["checkout", target_branch])
            .await?;

        // Step 2: attempt the merge
        let output = self
            .run_git_raw(repo_path, &["merge", source_branch, "--no-edit"])
            .await?;

        if output.status.success() {
            // Merge succeeded -- get the resulting SHA
            let sha = self.run_git(repo_path, &["rev-parse", "HEAD"]).await?;
            Ok(MergeResult::Success { merge_sha: sha })
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);

            // Check if it's a conflict
            if stderr.contains("CONFLICT")
                || stdout.contains("CONFLICT")
                || stderr.contains("Automatic merge failed")
                || stdout.contains("Automatic merge failed")
            {
                // Get the list of conflicting files
                let conflict_output = self
                    .run_git_raw(
                        repo_path,
                        &["diff", "--name-only", "--diff-filter=U"],
                    )
                    .await?;

                let conflicting_files: Vec<String> =
                    String::from_utf8_lossy(&conflict_output.stdout)
                        .lines()
                        .filter(|l| !l.is_empty())
                        .map(|l| l.to_string())
                        .collect();

                // Abort the merge to clean up
                let _ = self.run_git(repo_path, &["merge", "--abort"]).await;

                Ok(MergeResult::Conflict { conflicting_files })
            } else {
                // Non-conflict failure
                let _ = self.run_git(repo_path, &["merge", "--abort"]).await;
                Ok(MergeResult::Failed(
                    stderr.trim().to_string(),
                ))
            }
        }
    }

    async fn diff_stat(
        &self,
        repo_path: &str,
        base: &str,
        head: &str,
    ) -> Result<DiffStat, GitError> {
        let range = format!("{base}..{head}");
        let output = self
            .run_git(repo_path, &["diff", "--shortstat", &range])
            .await?;
        Ok(Self::parse_shortstat(&output))
    }

    async fn current_sha(&self, repo_path: &str) -> Result<String, GitError> {
        self.run_git(repo_path, &["rev-parse", "HEAD"]).await
    }

    async fn status_porcelain(&self, repo_path: &str) -> Result<Vec<FileStatus>, GitError> {
        // status --porcelain returns exit 0 even with changes.
        // We use run_git_raw instead of run_git because porcelain output has
        // semantically significant leading spaces (e.g. " M file.rs") that
        // run_git's trim() would strip from the first line.
        let output = self
            .run_git_raw(repo_path, &["status", "--porcelain"])
            .await?;

        if !output.status.success() {
            return Err(GitError::CommandFailed {
                command: "git status --porcelain".to_string(),
                stderr: String::from_utf8_lossy(&output.stderr)
                    .trim()
                    .to_string(),
                exit_code: output.status.code(),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(Self::parse_porcelain(&stdout))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Parse helpers ──────────────────────────────────────────────────

    #[test]
    fn parse_shortstat_full_line() {
        let stat = GitCli::parse_shortstat(" 3 files changed, 45 insertions(+), 12 deletions(-)");
        assert_eq!(stat.files_changed, 3);
        assert_eq!(stat.insertions, 45);
        assert_eq!(stat.deletions, 12);
    }

    #[test]
    fn parse_shortstat_single_file() {
        let stat = GitCli::parse_shortstat(" 1 file changed, 1 insertion(+)");
        assert_eq!(stat.files_changed, 1);
        assert_eq!(stat.insertions, 1);
        assert_eq!(stat.deletions, 0);
    }

    #[test]
    fn parse_shortstat_deletions_only() {
        let stat = GitCli::parse_shortstat(" 2 files changed, 5 deletions(-)");
        assert_eq!(stat.files_changed, 2);
        assert_eq!(stat.insertions, 0);
        assert_eq!(stat.deletions, 5);
    }

    #[test]
    fn parse_shortstat_empty() {
        let stat = GitCli::parse_shortstat("");
        assert_eq!(stat.files_changed, 0);
        assert_eq!(stat.insertions, 0);
        assert_eq!(stat.deletions, 0);
    }

    #[test]
    fn parse_shortstat_whitespace_only() {
        let stat = GitCli::parse_shortstat("   \n  ");
        assert_eq!(stat.files_changed, 0);
        assert_eq!(stat.insertions, 0);
        assert_eq!(stat.deletions, 0);
    }

    #[test]
    fn parse_porcelain_modified_file() {
        let files = GitCli::parse_porcelain(" M src/main.rs\n");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, "M");
        assert_eq!(files[0].path, "src/main.rs");
    }

    #[test]
    fn parse_porcelain_multiple_files() {
        let input = " M src/main.rs\nA  new_file.rs\n?? untracked.txt\n";
        let files = GitCli::parse_porcelain(input);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].status, "M");
        assert_eq!(files[0].path, "src/main.rs");
        assert_eq!(files[1].status, "A");
        assert_eq!(files[1].path, "new_file.rs");
        assert_eq!(files[2].status, "??");
        assert_eq!(files[2].path, "untracked.txt");
    }

    #[test]
    fn parse_porcelain_empty() {
        let files = GitCli::parse_porcelain("");
        assert!(files.is_empty());
    }

    #[test]
    fn parse_porcelain_deleted_file() {
        let files = GitCli::parse_porcelain(" D old_file.rs\n");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, "D");
        assert_eq!(files[0].path, "old_file.rs");
    }

    // ── GitCli construction ────────────────────────────────────────────

    #[test]
    fn git_cli_default_binary() {
        let cli = GitCli::new(None);
        assert_eq!(cli.git_binary, "git");
    }

    #[test]
    fn git_cli_custom_binary() {
        let cli = GitCli::new(Some("/usr/local/bin/git".to_string()));
        assert_eq!(cli.git_binary, "/usr/local/bin/git");
    }

    // ── Integration tests (require real git) ───────────────────────────

    /// Create a temporary git repo for testing.
    /// Returns (repo_path, cleanup_fn).
    async fn setup_test_repo() -> (PathBuf, impl FnOnce()) {
        let dir = std::env::temp_dir().join(format!(
            "openclaw-git-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        // git init
        tokio::process::Command::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(&dir)
            .output()
            .await
            .unwrap();

        // git config (for commits)
        tokio::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&dir)
            .output()
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&dir)
            .output()
            .await
            .unwrap();

        // Initial commit
        tokio::fs::write(dir.join("README.md"), "# Test\n")
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&dir)
            .output()
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&dir)
            .output()
            .await
            .unwrap();

        let cleanup_dir = dir.clone();
        (dir, move || {
            let _ = std::fs::remove_dir_all(cleanup_dir);
        })
    }

    #[tokio::test]
    async fn current_sha_returns_valid_hex_string() {
        let (repo, cleanup) = setup_test_repo().await;
        let cli = GitCli::new(None);

        let sha = cli
            .current_sha(repo.to_str().unwrap())
            .await
            .expect("current_sha should succeed");

        assert_eq!(sha.len(), 40, "SHA should be 40 hex characters");
        assert!(
            sha.chars().all(|c| c.is_ascii_hexdigit()),
            "SHA should only contain hex digits, got: {sha}"
        );

        cleanup();
    }

    #[tokio::test]
    async fn status_porcelain_clean_repo() {
        let (repo, cleanup) = setup_test_repo().await;
        let cli = GitCli::new(None);

        let status = cli
            .status_porcelain(repo.to_str().unwrap())
            .await
            .expect("status_porcelain should succeed");

        assert!(status.is_empty(), "clean repo should have no status entries");

        cleanup();
    }

    #[tokio::test]
    async fn status_porcelain_modified_files() {
        let (repo, cleanup) = setup_test_repo().await;
        let cli = GitCli::new(None);

        // Modify an existing file and create a new one
        tokio::fs::write(repo.join("README.md"), "# Modified\n")
            .await
            .unwrap();
        tokio::fs::write(repo.join("new_file.txt"), "hello\n")
            .await
            .unwrap();

        let status = cli
            .status_porcelain(repo.to_str().unwrap())
            .await
            .expect("status_porcelain should succeed");

        assert!(
            status.len() >= 2,
            "should have at least 2 status entries, got: {status:?}"
        );

        let paths: Vec<&str> = status.iter().map(|s| s.path.as_str()).collect();
        assert!(paths.contains(&"README.md"), "should contain README.md");
        assert!(
            paths.contains(&"new_file.txt"),
            "should contain new_file.txt"
        );

        cleanup();
    }

    #[tokio::test]
    async fn create_worktree_creates_directory() {
        let (repo, cleanup) = setup_test_repo().await;
        let cli = GitCli::new(None);

        let wt_path = repo.parent().unwrap().join(format!(
            "openclaw-wt-test-{}",
            uuid::Uuid::new_v4()
        ));

        let result = cli
            .create_worktree(
                repo.to_str().unwrap(),
                wt_path.to_str().unwrap(),
                "feature-branch",
            )
            .await
            .expect("create_worktree should succeed");

        assert_eq!(result, wt_path);
        assert!(
            tokio::fs::metadata(&wt_path).await.is_ok(),
            "worktree directory should exist"
        );

        // Verify the worktree has the README.md from main
        assert!(
            tokio::fs::metadata(wt_path.join("README.md")).await.is_ok(),
            "worktree should contain README.md"
        );

        // Clean up worktree
        let _ = cli
            .remove_worktree(repo.to_str().unwrap(), wt_path.to_str().unwrap())
            .await;
        let _ = std::fs::remove_dir_all(&wt_path);

        cleanup();
    }

    #[tokio::test]
    async fn create_worktree_errors_on_existing_path() {
        let (repo, cleanup) = setup_test_repo().await;
        let cli = GitCli::new(None);

        let wt_path = repo.parent().unwrap().join(format!(
            "openclaw-wt-exist-{}",
            uuid::Uuid::new_v4()
        ));

        // Create the directory first
        std::fs::create_dir_all(&wt_path).unwrap();

        let result = cli
            .create_worktree(
                repo.to_str().unwrap(),
                wt_path.to_str().unwrap(),
                "some-branch",
            )
            .await;

        assert!(result.is_err(), "should fail when path already exists");
        match result.unwrap_err() {
            GitError::WorktreeExists(path) => {
                assert_eq!(path, wt_path.to_str().unwrap());
            }
            other => panic!("expected WorktreeExists, got: {other}"),
        }

        let _ = std::fs::remove_dir_all(&wt_path);
        cleanup();
    }

    #[tokio::test]
    async fn remove_worktree_removes_previously_created() {
        let (repo, cleanup) = setup_test_repo().await;
        let cli = GitCli::new(None);

        let wt_path = repo.parent().unwrap().join(format!(
            "openclaw-wt-rm-{}",
            uuid::Uuid::new_v4()
        ));

        // Create worktree
        cli.create_worktree(
            repo.to_str().unwrap(),
            wt_path.to_str().unwrap(),
            "remove-test-branch",
        )
        .await
        .expect("create_worktree should succeed");

        assert!(tokio::fs::metadata(&wt_path).await.is_ok());

        // Remove worktree
        cli.remove_worktree(repo.to_str().unwrap(), wt_path.to_str().unwrap())
            .await
            .expect("remove_worktree should succeed");

        assert!(
            tokio::fs::metadata(&wt_path).await.is_err(),
            "worktree directory should be removed"
        );

        cleanup();
    }

    #[tokio::test]
    async fn diff_stat_identical_refs_returns_zeros() {
        let (repo, cleanup) = setup_test_repo().await;
        let cli = GitCli::new(None);

        let sha = cli
            .current_sha(repo.to_str().unwrap())
            .await
            .unwrap();

        // Diffing a ref against itself should yield zero changes
        let stat = cli
            .diff_stat(repo.to_str().unwrap(), &sha, &sha)
            .await
            .expect("diff_stat should succeed");

        assert_eq!(stat.files_changed, 0);
        assert_eq!(stat.insertions, 0);
        assert_eq!(stat.deletions, 0);

        cleanup();
    }

    #[tokio::test]
    async fn diff_stat_diverged_refs() {
        let (repo, cleanup) = setup_test_repo().await;
        let cli = GitCli::new(None);

        let base_sha = cli
            .current_sha(repo.to_str().unwrap())
            .await
            .unwrap();

        // Make a new commit with changes
        tokio::fs::write(repo.join("new_file.rs"), "fn main() {}\n")
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .output()
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["commit", "-m", "add new_file.rs"])
            .current_dir(&repo)
            .output()
            .await
            .unwrap();

        let head_sha = cli
            .current_sha(repo.to_str().unwrap())
            .await
            .unwrap();

        let stat = cli
            .diff_stat(repo.to_str().unwrap(), &base_sha, &head_sha)
            .await
            .expect("diff_stat should succeed");

        assert!(
            stat.files_changed > 0,
            "should have at least 1 file changed, got: {stat:?}"
        );
        assert!(
            stat.insertions > 0,
            "should have at least 1 insertion, got: {stat:?}"
        );

        cleanup();
    }

    #[tokio::test]
    async fn merge_clean_merge() {
        let (repo, cleanup) = setup_test_repo().await;
        let cli = GitCli::new(None);
        let repo_str = repo.to_str().unwrap();

        // Create a feature branch with a new file
        tokio::process::Command::new("git")
            .args(["checkout", "-b", "feature"])
            .current_dir(&repo)
            .output()
            .await
            .unwrap();

        tokio::fs::write(repo.join("feature.rs"), "fn feature() {}\n")
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .output()
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["commit", "-m", "add feature"])
            .current_dir(&repo)
            .output()
            .await
            .unwrap();

        // Go back to main
        tokio::process::Command::new("git")
            .args(["checkout", "main"])
            .current_dir(&repo)
            .output()
            .await
            .unwrap();

        // Merge feature into main
        let result = cli.merge(repo_str, "feature", "main").await.unwrap();

        match result {
            MergeResult::Success { merge_sha } => {
                assert_eq!(merge_sha.len(), 40);
                assert!(merge_sha.chars().all(|c| c.is_ascii_hexdigit()));
            }
            other => panic!("expected Success, got: {other:?}"),
        }

        // Verify the merged file exists
        assert!(tokio::fs::metadata(repo.join("feature.rs")).await.is_ok());

        cleanup();
    }

    #[tokio::test]
    async fn merge_with_conflict() {
        let (repo, cleanup) = setup_test_repo().await;
        let cli = GitCli::new(None);
        let repo_str = repo.to_str().unwrap();

        // Create a feature branch that modifies README.md
        tokio::process::Command::new("git")
            .args(["checkout", "-b", "conflict-feature"])
            .current_dir(&repo)
            .output()
            .await
            .unwrap();

        tokio::fs::write(repo.join("README.md"), "# Feature version\n")
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .output()
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["commit", "-m", "feature change"])
            .current_dir(&repo)
            .output()
            .await
            .unwrap();

        // Go back to main and make a conflicting change
        tokio::process::Command::new("git")
            .args(["checkout", "main"])
            .current_dir(&repo)
            .output()
            .await
            .unwrap();

        tokio::fs::write(repo.join("README.md"), "# Main version\n")
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .output()
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["commit", "-m", "main change"])
            .current_dir(&repo)
            .output()
            .await
            .unwrap();

        // Merge should produce a conflict
        let result = cli
            .merge(repo_str, "conflict-feature", "main")
            .await
            .unwrap();

        match result {
            MergeResult::Conflict { conflicting_files } => {
                assert!(
                    !conflicting_files.is_empty(),
                    "should have at least one conflicting file"
                );
                assert!(
                    conflicting_files.contains(&"README.md".to_string()),
                    "README.md should be in conflict list, got: {conflicting_files:?}"
                );
            }
            other => panic!("expected Conflict, got: {other:?}"),
        }

        // Verify repo is clean (merge was aborted)
        let status = cli
            .status_porcelain(repo_str)
            .await
            .expect("status should succeed after merge abort");
        assert!(
            status.is_empty(),
            "repo should be clean after merge abort, got: {status:?}"
        );

        cleanup();
    }

    #[tokio::test]
    async fn create_worktree_with_existing_branch() {
        let (repo, cleanup) = setup_test_repo().await;
        let cli = GitCli::new(None);
        let repo_str = repo.to_str().unwrap();

        // Create a branch first
        tokio::process::Command::new("git")
            .args(["branch", "existing-branch"])
            .current_dir(&repo)
            .output()
            .await
            .unwrap();

        let wt_path = repo.parent().unwrap().join(format!(
            "openclaw-wt-existing-{}",
            uuid::Uuid::new_v4()
        ));

        let result = cli
            .create_worktree(repo_str, wt_path.to_str().unwrap(), "existing-branch")
            .await
            .expect("should succeed with existing branch");

        assert_eq!(result, wt_path);
        assert!(tokio::fs::metadata(&wt_path).await.is_ok());

        // Clean up
        let _ = cli
            .remove_worktree(repo_str, wt_path.to_str().unwrap())
            .await;
        let _ = std::fs::remove_dir_all(&wt_path);

        cleanup();
    }

    #[tokio::test]
    async fn branch_exists_returns_true_for_main() {
        let (repo, cleanup) = setup_test_repo().await;
        let cli = GitCli::new(None);

        let exists = cli
            .branch_exists(repo.to_str().unwrap(), "main")
            .await
            .unwrap();
        assert!(exists, "main branch should exist");

        cleanup();
    }

    #[tokio::test]
    async fn branch_exists_returns_false_for_nonexistent() {
        let (repo, cleanup) = setup_test_repo().await;
        let cli = GitCli::new(None);

        let exists = cli
            .branch_exists(repo.to_str().unwrap(), "nonexistent-branch")
            .await
            .unwrap();
        assert!(!exists, "nonexistent branch should not exist");

        cleanup();
    }

    #[tokio::test]
    async fn run_git_fails_for_invalid_repo() {
        let cli = GitCli::new(None);
        let result = cli
            .run_git("/tmp/definitely-not-a-git-repo-12345", &["status"])
            .await;
        assert!(result.is_err());
    }
}
