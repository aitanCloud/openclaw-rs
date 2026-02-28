//! Planner domain port and associated types.
//!
//! The `Planner` trait defines a pluggable plan-generation strategy:
//! could be backed by an LLM (Claude Code), a rule engine, or manual entry.
//! This is a **domain port** -- the domain defines the interface, the
//! infrastructure layer provides the implementation.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::types::BudgetCents;

// ═══════════════════════════════════════════════════════════════════════════════
// Error
// ═══════════════════════════════════════════════════════════════════════════════

/// Errors that can occur during plan generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// The requested cycle or project was not found.
    NotFound { entity: String, id: String },
    /// Plan generation failed (LLM error, parse failure, etc.).
    GenerationFailed(String),
    /// Insufficient context to generate a plan.
    ContextError(String),
    /// Plan generation timed out.
    Timeout { elapsed_secs: u64 },
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { entity, id } => write!(f, "{entity} not found: {id}"),
            Self::GenerationFailed(msg) => write!(f, "plan generation failed: {msg}"),
            Self::ContextError(msg) => write!(f, "context error: {msg}"),
            Self::Timeout { elapsed_secs } => {
                write!(f, "plan generation timed out after {elapsed_secs}s")
            }
        }
    }
}

impl std::error::Error for PlanError {}

// ═══════════════════════════════════════════════════════════════════════════════
// Artifact reference
// ═══════════════════════════════════════════════════════════════════════════════

/// A reference to a stored artifact (plan JSON, reasoning trace, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    /// Kind of artifact (e.g. "plan_proposal", "reasoning_trace", "error_log").
    pub kind: String,
    /// Content-addressable hash (SHA-256 hex).
    pub hash: String,
}

// ═══════════════════════════════════════════════════════════════════════════════
// Context types
// ═══════════════════════════════════════════════════════════════════════════════

/// A summary of a previous cycle's outcome, used as context for planning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CycleSummary {
    pub cycle_id: Uuid,
    pub outcome: String,
    pub summary: String,
}

/// A single file with a content preview, for providing repo context to the planner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileContext {
    pub path: String,
    pub content_preview: String,
}

/// A commit summary for repo context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitSummary {
    pub sha: String,
    pub message: String,
    pub author: String,
}

/// Repository context provided to the planner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoContext {
    /// Summary of the file tree (e.g. directory listing, tree output).
    pub file_tree_summary: String,
    /// Recent commits for change context.
    pub recent_commits: Vec<CommitSummary>,
    /// Relevant files the planner should be aware of.
    pub relevant_files: Vec<FileContext>,
}

/// Constraints on plan generation (budget, scope, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanConstraints {
    /// Maximum number of tasks allowed.
    pub max_tasks: u32,
    /// Maximum number of concurrent tasks.
    pub max_concurrent: u32,
    /// Remaining budget for the cycle, in cents.
    pub budget_remaining: BudgetCents,
    /// Paths the planner must not modify.
    pub forbidden_paths: Vec<String>,
}

/// Full context provided to the planner for plan generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanningContext {
    pub cycle_id: Uuid,
    pub project_id: Uuid,
    pub objective: String,
    pub repo_context: RepoContext,
    pub constraints: PlanConstraints,
    pub previous_cycle_summary: Option<CycleSummary>,
    /// SHA-256 hash of the context, for deduplication and caching.
    pub context_hash: String,
}

// ═══════════════════════════════════════════════════════════════════════════════
// Plan proposal types
// ═══════════════════════════════════════════════════════════════════════════════

/// Scope definition for a task: which files/dirs the task should touch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskScope {
    /// Paths this task is expected to modify.
    pub target_paths: Vec<String>,
    /// Paths this task may read but must not modify.
    pub read_only_paths: Vec<String>,
}

/// A single proposed task within a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskProposal {
    /// Unique key within the plan (e.g. "task-1", "setup-db").
    pub task_key: String,
    /// Human-readable title.
    pub title: String,
    /// Detailed description of what to implement.
    pub description: String,
    /// Concrete acceptance criteria.
    pub acceptance_criteria: Vec<String>,
    /// Task keys this task depends on (must complete first).
    pub dependencies: Vec<String>,
    /// Estimated token usage for this task.
    pub estimated_tokens: Option<u64>,
    /// Scope of files/directories this task will modify.
    pub scope: Option<TaskScope>,
}

/// Metadata about the plan generation process.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanMetadata {
    /// Model used for generation (e.g. "claude-opus-4-20250514").
    pub model_id: String,
    /// Prompt hash for auditability.
    pub prompt_hash: String,
    /// Hash of the context used for generation.
    pub context_hash: String,
    /// Temperature used for generation.
    pub temperature: f32,
    /// Timestamp when the plan was generated.
    pub generated_at: DateTime<Utc>,
}

/// The full plan proposal produced by the Planner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanProposal {
    /// Ordered list of proposed tasks.
    pub tasks: Vec<TaskProposal>,
    /// Human-readable plan summary.
    pub summary: String,
    /// Reference to the stored reasoning trace.
    pub reasoning_ref: Option<ArtifactRef>,
    /// Total estimated cost for the plan, in cents.
    pub estimated_cost: BudgetCents,
    /// Metadata about the generation process.
    pub metadata: PlanMetadata,
}

/// A request for additional context from the planner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextRequest {
    /// File paths to retrieve (max 50).
    pub requested_files: Vec<String>,
    /// Description of additional context needed.
    pub requested_context: String,
}

// ═══════════════════════════════════════════════════════════════════════════════
// Planner trait (domain port)
// ═══════════════════════════════════════════════════════════════════════════════

/// Domain port for plan generation.
///
/// Implementors provide the actual plan-generation logic:
/// - `ClaudeCodePlanner` (infra) -- calls Claude Code to generate plans
/// - `StaticPlanner` (test) -- returns a fixed plan for testing
/// - `ManualPlanner` (future) -- human-authored plans
#[async_trait::async_trait]
pub trait Planner: Send + Sync {
    /// Generate a plan proposal from the given planning context.
    async fn generate_plan(&self, context: &PlanningContext) -> Result<PlanProposal, PlanError>;

    /// Optionally request more context before generating a plan.
    /// Returns `None` if the planner has enough context.
    async fn request_more_context(
        &self,
        context: &PlanningContext,
    ) -> Result<Option<ContextRequest>, PlanError> {
        let _ = context;
        Ok(None)
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── PlanError display ──

    #[test]
    fn display_not_found() {
        let err = PlanError::NotFound {
            entity: "Cycle".to_string(),
            id: "abc-123".to_string(),
        };
        assert_eq!(err.to_string(), "Cycle not found: abc-123");
    }

    #[test]
    fn display_generation_failed() {
        let err = PlanError::GenerationFailed("LLM rate limited".to_string());
        assert_eq!(err.to_string(), "plan generation failed: LLM rate limited");
    }

    #[test]
    fn display_context_error() {
        let err = PlanError::ContextError("missing repo".to_string());
        assert_eq!(err.to_string(), "context error: missing repo");
    }

    #[test]
    fn display_timeout() {
        let err = PlanError::Timeout { elapsed_secs: 120 };
        assert_eq!(
            err.to_string(),
            "plan generation timed out after 120s"
        );
    }

    #[test]
    fn plan_error_clone_and_eq() {
        let err = PlanError::GenerationFailed("x".into());
        let cloned = err.clone();
        assert_eq!(err, cloned);
    }

    #[test]
    fn plan_error_implements_error_trait() {
        let err: Box<dyn std::error::Error> =
            Box::new(PlanError::GenerationFailed("test".into()));
        assert!(err.to_string().contains("plan generation failed"));
    }

    // ── ArtifactRef serde ──

    #[test]
    fn artifact_ref_serde_round_trip() {
        let ar = ArtifactRef {
            kind: "plan_proposal".to_string(),
            hash: "abc123def456".to_string(),
        };
        let json = serde_json::to_string(&ar).unwrap();
        let restored: ArtifactRef = serde_json::from_str(&json).unwrap();
        assert_eq!(ar, restored);
    }

    // ── CycleSummary serde ──

    #[test]
    fn cycle_summary_serde_round_trip() {
        let cs = CycleSummary {
            cycle_id: Uuid::new_v4(),
            outcome: "completed".to_string(),
            summary: "All tasks passed".to_string(),
        };
        let json = serde_json::to_string(&cs).unwrap();
        let restored: CycleSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(cs, restored);
    }

    // ── FileContext serde ──

    #[test]
    fn file_context_serde_round_trip() {
        let fc = FileContext {
            path: "src/main.rs".to_string(),
            content_preview: "fn main() {}".to_string(),
        };
        let json = serde_json::to_string(&fc).unwrap();
        let restored: FileContext = serde_json::from_str(&json).unwrap();
        assert_eq!(fc, restored);
    }

    // ── CommitSummary serde ──

    #[test]
    fn commit_summary_serde_round_trip() {
        let cs = CommitSummary {
            sha: "abc123".to_string(),
            message: "fix: bug".to_string(),
            author: "alice".to_string(),
        };
        let json = serde_json::to_string(&cs).unwrap();
        let restored: CommitSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(cs, restored);
    }

    // ── RepoContext ──

    #[test]
    fn repo_context_serde_round_trip() {
        let rc = RepoContext {
            file_tree_summary: "src/\n  lib.rs\n  main.rs".to_string(),
            recent_commits: vec![CommitSummary {
                sha: "def456".to_string(),
                message: "chore: cleanup".to_string(),
                author: "bob".to_string(),
            }],
            relevant_files: vec![FileContext {
                path: "src/lib.rs".to_string(),
                content_preview: "pub mod app;".to_string(),
            }],
        };
        let json = serde_json::to_string(&rc).unwrap();
        let restored: RepoContext = serde_json::from_str(&json).unwrap();
        assert_eq!(rc, restored);
    }

    #[test]
    fn repo_context_empty() {
        let rc = RepoContext {
            file_tree_summary: String::new(),
            recent_commits: vec![],
            relevant_files: vec![],
        };
        let json = serde_json::to_string(&rc).unwrap();
        let restored: RepoContext = serde_json::from_str(&json).unwrap();
        assert_eq!(rc, restored);
    }

    // ── PlanConstraints ──

    #[test]
    fn plan_constraints_serde_round_trip() {
        let pc = PlanConstraints {
            max_tasks: 10,
            max_concurrent: 3,
            budget_remaining: 50000,
            forbidden_paths: vec![".env".to_string(), "secrets/".to_string()],
        };
        let json = serde_json::to_string(&pc).unwrap();
        let restored: PlanConstraints = serde_json::from_str(&json).unwrap();
        assert_eq!(pc, restored);
    }

    #[test]
    fn plan_constraints_defaults() {
        let pc = PlanConstraints {
            max_tasks: 0,
            max_concurrent: 0,
            budget_remaining: 0,
            forbidden_paths: vec![],
        };
        let json = serde_json::to_string(&pc).unwrap();
        let restored: PlanConstraints = serde_json::from_str(&json).unwrap();
        assert_eq!(pc, restored);
    }

    // ── PlanningContext ──

    #[test]
    fn planning_context_serde_round_trip() {
        let ctx = make_planning_context();
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: PlanningContext = serde_json::from_str(&json).unwrap();
        assert_eq!(ctx, restored);
    }

    #[test]
    fn planning_context_without_previous_cycle() {
        let mut ctx = make_planning_context();
        ctx.previous_cycle_summary = None;
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: PlanningContext = serde_json::from_str(&json).unwrap();
        assert_eq!(ctx, restored);
    }

    // ── TaskScope ──

    #[test]
    fn task_scope_serde_round_trip() {
        let ts = TaskScope {
            target_paths: vec!["src/domain/planner.rs".to_string()],
            read_only_paths: vec!["src/domain/types.rs".to_string()],
        };
        let json = serde_json::to_string(&ts).unwrap();
        let restored: TaskScope = serde_json::from_str(&json).unwrap();
        assert_eq!(ts, restored);
    }

    // ── TaskProposal ──

    #[test]
    fn task_proposal_serde_round_trip() {
        let tp = make_task_proposal("task-1");
        let json = serde_json::to_string(&tp).unwrap();
        let restored: TaskProposal = serde_json::from_str(&json).unwrap();
        assert_eq!(tp, restored);
    }

    #[test]
    fn task_proposal_without_optionals() {
        let tp = TaskProposal {
            task_key: "task-1".to_string(),
            title: "Setup".to_string(),
            description: "Set up project".to_string(),
            acceptance_criteria: vec!["builds".to_string()],
            dependencies: vec![],
            estimated_tokens: None,
            scope: None,
        };
        let json = serde_json::to_string(&tp).unwrap();
        let restored: TaskProposal = serde_json::from_str(&json).unwrap();
        assert_eq!(tp, restored);
    }

    // ── PlanMetadata ──

    #[test]
    fn plan_metadata_serde_round_trip() {
        let pm = PlanMetadata {
            model_id: "claude-opus-4-20250514".to_string(),
            prompt_hash: "sha256-abc123".to_string(),
            context_hash: "sha256-ctx789".to_string(),
            temperature: 0.7,
            generated_at: Utc::now(),
        };
        let json = serde_json::to_string(&pm).unwrap();
        let restored: PlanMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(pm, restored);
    }

    #[test]
    fn plan_metadata_zero_temperature() {
        let pm = PlanMetadata {
            model_id: "test-model".to_string(),
            prompt_hash: "hash1".to_string(),
            context_hash: "hash2".to_string(),
            temperature: 0.0,
            generated_at: Utc::now(),
        };
        let json = serde_json::to_string(&pm).unwrap();
        let restored: PlanMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(pm, restored);
    }

    // ── PlanProposal ──

    #[test]
    fn plan_proposal_serde_round_trip() {
        let pp = make_plan_proposal();
        let json = serde_json::to_string(&pp).unwrap();
        let restored: PlanProposal = serde_json::from_str(&json).unwrap();
        assert_eq!(pp, restored);
    }

    #[test]
    fn plan_proposal_to_json_value() {
        let pp = make_plan_proposal();
        let value = serde_json::to_value(&pp).unwrap();
        assert!(value.get("tasks").unwrap().is_array());
        assert!(value.get("summary").unwrap().is_string());
        assert!(value.get("metadata").unwrap().is_object());
    }

    #[test]
    fn plan_proposal_empty_tasks() {
        let pp = PlanProposal {
            tasks: vec![],
            summary: "Empty plan".to_string(),
            reasoning_ref: None,
            estimated_cost: 0,
            metadata: PlanMetadata {
                model_id: "test-model".to_string(),
                prompt_hash: "hash1".to_string(),
                context_hash: "hash2".to_string(),
                temperature: 0.0,
                generated_at: Utc::now(),
            },
        };
        let json = serde_json::to_string(&pp).unwrap();
        let restored: PlanProposal = serde_json::from_str(&json).unwrap();
        assert_eq!(pp, restored);
        assert!(pp.tasks.is_empty());
    }

    // ── ContextRequest ──

    #[test]
    fn context_request_serde_round_trip() {
        let cr = ContextRequest {
            requested_files: vec!["migrations/".to_string(), "schema.sql".to_string()],
            requested_context: "Need database schema and ORM details".to_string(),
        };
        let json = serde_json::to_string(&cr).unwrap();
        let restored: ContextRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(cr, restored);
    }

    // ── Planner trait object safety ──

    #[test]
    fn planner_is_object_safe() {
        // Compile-time test: if Planner is not object-safe, this won't compile.
        fn _assert_object_safe(_: &dyn Planner) {}
    }

    // ── Mock Planner for trait tests ──

    struct StaticPlanner {
        proposal: PlanProposal,
    }

    #[async_trait::async_trait]
    impl Planner for StaticPlanner {
        async fn generate_plan(&self, _context: &PlanningContext) -> Result<PlanProposal, PlanError> {
            Ok(self.proposal.clone())
        }
    }

    #[tokio::test]
    async fn static_planner_returns_proposal() {
        let proposal = make_plan_proposal();
        let planner = StaticPlanner {
            proposal: proposal.clone(),
        };
        let ctx = make_planning_context();
        let result = planner.generate_plan(&ctx).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), proposal);
    }

    #[tokio::test]
    async fn default_request_more_context_returns_none() {
        let planner = StaticPlanner {
            proposal: make_plan_proposal(),
        };
        let ctx = make_planning_context();
        let result = planner.request_more_context(&ctx).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    struct FailingPlanner;

    #[async_trait::async_trait]
    impl Planner for FailingPlanner {
        async fn generate_plan(&self, _context: &PlanningContext) -> Result<PlanProposal, PlanError> {
            Err(PlanError::GenerationFailed("model unavailable".into()))
        }
    }

    #[tokio::test]
    async fn failing_planner_returns_error() {
        let planner = FailingPlanner;
        let ctx = make_planning_context();
        let result = planner.generate_plan(&ctx).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            PlanError::GenerationFailed(msg) => assert_eq!(msg, "model unavailable"),
            other => panic!("expected GenerationFailed, got: {other:?}"),
        }
    }

    // ── Test helpers ──

    fn make_planning_context() -> PlanningContext {
        PlanningContext {
            cycle_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            objective: "Implement user authentication".to_string(),
            repo_context: RepoContext {
                file_tree_summary: "src/\n  main.rs\n  lib.rs".to_string(),
                recent_commits: vec![CommitSummary {
                    sha: "abc123".to_string(),
                    message: "initial commit".to_string(),
                    author: "alice".to_string(),
                }],
                relevant_files: vec![FileContext {
                    path: "src/main.rs".to_string(),
                    content_preview: "fn main() { ... }".to_string(),
                }],
            },
            constraints: PlanConstraints {
                max_tasks: 5,
                max_concurrent: 2,
                budget_remaining: 100_00,
                forbidden_paths: vec![],
            },
            previous_cycle_summary: Some(CycleSummary {
                cycle_id: Uuid::new_v4(),
                outcome: "completed".to_string(),
                summary: "Set up project scaffolding".to_string(),
            }),
            context_hash: "sha256-deadbeef".to_string(),
        }
    }

    fn make_task_proposal(key: &str) -> TaskProposal {
        TaskProposal {
            task_key: key.to_string(),
            title: format!("Task {key}"),
            description: format!("Implement {key}"),
            acceptance_criteria: vec!["tests pass".to_string(), "no warnings".to_string()],
            dependencies: vec![],
            estimated_tokens: Some(25_000),
            scope: Some(TaskScope {
                target_paths: vec!["src/".to_string()],
                read_only_paths: vec![],
            }),
        }
    }

    fn make_plan_proposal() -> PlanProposal {
        PlanProposal {
            tasks: vec![
                make_task_proposal("task-1"),
                {
                    let mut t = make_task_proposal("task-2");
                    t.dependencies = vec!["task-1".to_string()];
                    t
                },
            ],
            summary: "Two-phase implementation plan".to_string(),
            reasoning_ref: Some(ArtifactRef {
                kind: "reasoning_trace".to_string(),
                hash: "sha256-trace123".to_string(),
            }),
            estimated_cost: 5000,
            metadata: PlanMetadata {
                model_id: "claude-opus-4-20250514".to_string(),
                prompt_hash: "sha256-prompt456".to_string(),
                context_hash: "sha256-ctx789".to_string(),
                temperature: 0.7,
                generated_at: Utc::now(),
            },
        }
    }
}
