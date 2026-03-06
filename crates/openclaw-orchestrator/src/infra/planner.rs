//! ClaudeCodePlanner — generates plans by shelling out to the `claude` CLI.
//!
//! Sends a structured planning prompt and parses the JSON response
//! into a `PlanProposal`.

use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use sha2::{Digest, Sha256};

use crate::domain::planner::{
    ArtifactRef, PlanError, PlanMetadata, PlanProposal, Planner, PlanningContext, TaskProposal,
};

/// Default timeout for plan generation (5 minutes).
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Plan generator that shells out to `claude --print --output-format json`.
pub struct ClaudeCodePlanner {
    claude_binary: String,
    working_dir: PathBuf,
    timeout: Duration,
}

impl ClaudeCodePlanner {
    pub fn new(working_dir: PathBuf, claude_binary: Option<String>) -> Self {
        Self {
            claude_binary: claude_binary.unwrap_or_else(|| "claude".to_string()),
            working_dir,
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        }
    }

    /// Build the planning prompt from the context.
    fn build_prompt(context: &PlanningContext) -> String {
        let mut prompt = String::with_capacity(4096);

        prompt.push_str("You are a software architect planning work for a coding agent.\n\n");
        prompt.push_str(&format!("## Objective\n{}\n\n", context.objective));

        if !context.repo_context.file_tree_summary.is_empty() {
            prompt.push_str(&format!(
                "## Project Structure\n```\n{}\n```\n\n",
                context.repo_context.file_tree_summary
            ));
        }

        if !context.repo_context.recent_commits.is_empty() {
            prompt.push_str("## Recent Commits\n");
            for commit in &context.repo_context.recent_commits {
                prompt.push_str(&format!(
                    "- {} ({}) — {}\n",
                    commit.sha, commit.author, commit.message
                ));
            }
            prompt.push('\n');
        }

        if !context.repo_context.relevant_files.is_empty() {
            prompt.push_str("## Relevant Files\n");
            for file in &context.repo_context.relevant_files {
                prompt.push_str(&format!("### {}\n```\n{}\n```\n\n", file.path, file.content_preview));
            }
        }

        prompt.push_str(&format!(
            "## Constraints\n- Maximum tasks: {}\n- Maximum concurrent: {}\n- Budget remaining: {} cents\n",
            context.constraints.max_tasks,
            context.constraints.max_concurrent,
            context.constraints.budget_remaining,
        ));

        if !context.constraints.forbidden_paths.is_empty() {
            prompt.push_str(&format!(
                "- Forbidden paths: {}\n",
                context.constraints.forbidden_paths.join(", ")
            ));
        }

        if let Some(ref prev) = context.previous_cycle_summary {
            prompt.push_str(&format!(
                "\n## Previous Cycle\nOutcome: {}\nSummary: {}\n",
                prev.outcome, prev.summary
            ));
        }

        prompt.push_str(r#"
## Instructions
Break the objective into concrete tasks for a coding agent. Each task should be independently executable.

Respond with ONLY valid JSON matching this exact schema (no markdown, no explanation):
{
  "tasks": [
    {
      "task_key": "unique-key",
      "title": "Short title",
      "description": "Detailed description of what to implement",
      "acceptance_criteria": ["criterion 1", "criterion 2"],
      "dependencies": ["task-key-of-dependency"]
    }
  ],
  "summary": "One-paragraph plan summary",
  "estimated_cost": 5000
}

Rules:
- task_key must be unique within the plan (e.g. "setup-auth", "add-tests")
- dependencies reference other task_keys that must complete first
- estimated_cost is total plan cost in cents
- Keep tasks focused: each task = one logical unit of work
"#);

        prompt
    }

    /// Parse the raw JSON output from Claude into a PlanProposal.
    fn parse_response(raw: &str, context: &PlanningContext) -> Result<PlanProposal, PlanError> {
        tracing::debug!(raw_len = raw.len(), raw_preview = %raw.chars().take(200).collect::<String>(), "parsing planner response");

        // Claude might wrap JSON in markdown code blocks
        let json_str = extract_json(raw);

        tracing::debug!(json_len = json_str.len(), json_preview = %json_str.chars().take(200).collect::<String>(), "extracted JSON from response");

        let parsed: serde_json::Value = serde_json::from_str(json_str)
            .map_err(|e| {
                tracing::error!(
                    json_tail = %json_str.chars().rev().take(200).collect::<String>().chars().rev().collect::<String>(),
                    "JSON parse failed — showing last 200 chars of extracted JSON"
                );
                PlanError::GenerationFailed(format!("invalid JSON from planner: {e}"))
            })?;

        let tasks_val = parsed
            .get("tasks")
            .and_then(|v| v.as_array())
            .ok_or_else(|| PlanError::GenerationFailed("missing 'tasks' array".into()))?;

        let mut tasks = Vec::with_capacity(tasks_val.len());
        for (i, task_val) in tasks_val.iter().enumerate() {
            let task_key = task_val
                .get("task_key")
                .and_then(|v| v.as_str())
                .unwrap_or(&format!("task-{}", i + 1))
                .to_string();

            let title = task_val
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("Untitled task")
                .to_string();

            let description = task_val
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let acceptance_criteria = task_val
                .get("acceptance_criteria")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            let dependencies = task_val
                .get("dependencies")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            tasks.push(TaskProposal {
                task_key,
                title,
                description,
                acceptance_criteria,
                dependencies,
                estimated_tokens: None,
                scope: None,
            });
        }

        if tasks.is_empty() {
            return Err(PlanError::GenerationFailed("plan has zero tasks".into()));
        }

        let summary = parsed
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("Plan generated by Claude")
            .to_string();

        let estimated_cost = parsed
            .get("estimated_cost")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        // Compute prompt hash for metadata
        let prompt_hash = {
            let mut hasher = Sha256::new();
            hasher.update(context.objective.as_bytes());
            format!("sha256-{}", hex::encode(hasher.finalize()))
        };

        Ok(PlanProposal {
            tasks,
            summary,
            reasoning_ref: Some(ArtifactRef {
                kind: "claude_output".to_string(),
                hash: format!(
                    "sha256-{}",
                    hex::encode(Sha256::digest(raw.as_bytes()))
                ),
            }),
            estimated_cost,
            metadata: PlanMetadata {
                model_id: "claude-code-cli".to_string(),
                prompt_hash,
                context_hash: context.context_hash.clone(),
                temperature: 0.0,
                generated_at: Utc::now(),
            },
        })
    }
}

/// Extract the "result" text from Claude CLI JSON output.
///
/// Handles both single-object mode and JSONL (--verbose) mode.
/// Looks for the `{"type":"result",...}` object and returns its `"result"` field.
fn extract_result_text(stdout: &str) -> Option<String> {
    // Try each line as a JSON object (handles JSONL / --verbose mode)
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(line) {
            if obj.get("type").and_then(|v| v.as_str()) == Some("result") {
                return obj.get("result").and_then(|v| v.as_str()).map(String::from);
            }
        }
    }

    // Fallback: try parsing the whole stdout as a single JSON object
    if let Ok(obj) = serde_json::from_str::<serde_json::Value>(stdout.trim()) {
        if obj.get("type").and_then(|v| v.as_str()) == Some("result") {
            return obj.get("result").and_then(|v| v.as_str()).map(String::from);
        }
        // If it has a "tasks" field, it might be the plan JSON directly (no envelope)
        if obj.get("tasks").is_some() {
            return Some(stdout.trim().to_string());
        }
    }

    None
}

/// Extract JSON from a response that might contain markdown code blocks.
fn extract_json(raw: &str) -> &str {
    let trimmed = raw.trim();

    // Try to find JSON inside ```json ... ``` or ``` ... ```
    if let Some(start) = trimmed.find("```json") {
        let after_marker = &trimmed[start + 7..];
        if let Some(end) = after_marker.find("```") {
            return after_marker[..end].trim();
        }
    }

    if let Some(start) = trimmed.find("```") {
        let after_marker = &trimmed[start + 3..];
        if let Some(end) = after_marker.find("```") {
            let inner = after_marker[..end].trim();
            // Skip language identifier if present on the first line
            if let Some(newline_pos) = inner.find('\n') {
                let first_line = &inner[..newline_pos];
                if !first_line.starts_with('{') && !first_line.starts_with('[') {
                    return inner[newline_pos + 1..].trim();
                }
            }
            return inner;
        }
    }

    // Try to find raw JSON object
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            return &trimmed[start..=end];
        }
    }

    trimmed
}

#[async_trait::async_trait]
impl Planner for ClaudeCodePlanner {
    async fn generate_plan(&self, context: &PlanningContext) -> Result<PlanProposal, PlanError> {
        let prompt = Self::build_prompt(context);

        tracing::info!(
            cycle_id = %context.cycle_id,
            prompt_len = prompt.len(),
            "invoking claude CLI for plan generation"
        );

        let output = tokio::time::timeout(self.timeout, async {
            tokio::process::Command::new(&self.claude_binary)
                .arg("--print")
                .arg("--output-format")
                .arg("json")
                .arg("--tools")
                .arg("")
                .arg("--model")
                .arg("sonnet")
                .arg(&prompt)
                .current_dir(&self.working_dir)
                .stdin(std::process::Stdio::null())
                .output()
                .await
        })
        .await
        .map_err(|_| PlanError::Timeout {
            elapsed_secs: self.timeout.as_secs(),
        })?
        .map_err(|e| PlanError::GenerationFailed(format!("failed to run claude CLI: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(PlanError::GenerationFailed(format!(
                "claude CLI exited with {}: {}",
                output.status,
                stderr.chars().take(500).collect::<String>()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().is_empty() {
            return Err(PlanError::GenerationFailed(
                "claude CLI returned empty output".into(),
            ));
        }

        // Claude --output-format json --verbose outputs JSONL (one JSON object per line).
        // Without --verbose it outputs a single JSON object.
        // In both cases, we need to find the {"type":"result",...} object and extract its "result" field.
        let result_text = extract_result_text(&stdout)
            .ok_or_else(|| PlanError::GenerationFailed(
                format!("no result object in claude CLI output (len={})", stdout.len()),
            ))?;

        Self::parse_response(&result_text, context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::planner::{
        CommitSummary, CycleSummary, FileContext, PlanConstraints, PlanningContext, RepoContext,
    };
    use uuid::Uuid;

    fn make_context() -> PlanningContext {
        PlanningContext {
            cycle_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            objective: "Add user authentication".to_string(),
            repo_context: RepoContext {
                file_tree_summary: "src/\n  main.rs".to_string(),
                recent_commits: vec![CommitSummary {
                    sha: "abc123".into(),
                    message: "init".into(),
                    author: "alice".into(),
                }],
                relevant_files: vec![FileContext {
                    path: "src/main.rs".into(),
                    content_preview: "fn main() {}".into(),
                }],
            },
            constraints: PlanConstraints {
                max_tasks: 5,
                max_concurrent: 2,
                budget_remaining: 10000,
                forbidden_paths: vec![],
            },
            previous_cycle_summary: None,
            context_hash: "sha256-test".into(),
        }
    }

    #[test]
    fn build_prompt_contains_objective() {
        let ctx = make_context();
        let prompt = ClaudeCodePlanner::build_prompt(&ctx);
        assert!(prompt.contains("Add user authentication"));
    }

    #[test]
    fn build_prompt_contains_constraints() {
        let ctx = make_context();
        let prompt = ClaudeCodePlanner::build_prompt(&ctx);
        assert!(prompt.contains("Maximum tasks: 5"));
        assert!(prompt.contains("Budget remaining: 10000 cents"));
    }

    #[test]
    fn build_prompt_with_previous_cycle() {
        let mut ctx = make_context();
        ctx.previous_cycle_summary = Some(CycleSummary {
            cycle_id: Uuid::new_v4(),
            outcome: "completed".into(),
            summary: "Set up scaffolding".into(),
        });
        let prompt = ClaudeCodePlanner::build_prompt(&ctx);
        assert!(prompt.contains("Previous Cycle"));
        assert!(prompt.contains("Set up scaffolding"));
    }

    #[test]
    fn parse_valid_json() {
        let raw = r#"{
            "tasks": [
                {
                    "task_key": "setup",
                    "title": "Setup auth",
                    "description": "Add auth middleware",
                    "acceptance_criteria": ["tests pass"],
                    "dependencies": []
                }
            ],
            "summary": "Add authentication",
            "estimated_cost": 3000
        }"#;
        let ctx = make_context();
        let proposal = ClaudeCodePlanner::parse_response(raw, &ctx).unwrap();
        assert_eq!(proposal.tasks.len(), 1);
        assert_eq!(proposal.tasks[0].task_key, "setup");
        assert_eq!(proposal.summary, "Add authentication");
        assert_eq!(proposal.estimated_cost, 3000);
    }

    #[test]
    fn parse_json_in_code_block() {
        let raw = r#"```json
{
    "tasks": [{"task_key": "t1", "title": "T1", "description": "D1"}],
    "summary": "Plan",
    "estimated_cost": 1000
}
```"#;
        let ctx = make_context();
        let proposal = ClaudeCodePlanner::parse_response(raw, &ctx).unwrap();
        assert_eq!(proposal.tasks.len(), 1);
    }

    #[test]
    fn parse_empty_tasks_fails() {
        let raw = r#"{"tasks": [], "summary": "Empty"}"#;
        let ctx = make_context();
        let result = ClaudeCodePlanner::parse_response(raw, &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn parse_invalid_json_fails() {
        let raw = "not json at all";
        let ctx = make_context();
        let result = ClaudeCodePlanner::parse_response(raw, &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn extract_result_text_single_json() {
        let stdout = r#"{"type":"result","subtype":"success","result":"hello world","total_cost_usd":0.01}"#;
        assert_eq!(extract_result_text(stdout), Some("hello world".into()));
    }

    #[test]
    fn extract_result_text_jsonl_verbose() {
        let stdout = concat!(
            r#"{"type":"system","subtype":"init","session_id":"abc"}"#, "\n",
            r#"{"type":"assistant","message":{"content":"thinking..."}}"#, "\n",
            r#"{"type":"result","subtype":"success","result":"the plan json","total_cost_usd":0.05}"#, "\n",
        );
        assert_eq!(extract_result_text(stdout), Some("the plan json".into()));
    }

    #[test]
    fn extract_result_text_no_result() {
        let stdout = r#"{"type":"system","subtype":"init"}"#;
        assert_eq!(extract_result_text(stdout), None);
    }

    #[test]
    fn extract_json_plain() {
        assert_eq!(extract_json(r#"{"a": 1}"#), r#"{"a": 1}"#);
    }

    #[test]
    fn extract_json_from_code_block() {
        let input = "```json\n{\"a\": 1}\n```";
        assert_eq!(extract_json(input), "{\"a\": 1}");
    }

    #[test]
    fn extract_json_with_surrounding_text() {
        let input = "Here is the plan: {\"tasks\": []} and more text";
        assert_eq!(extract_json(input), "{\"tasks\": []}");
    }
}
