use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::process::Stdio;
use tokio::process::Command;
use tracing::{info, warn};

use super::{Tool, ToolContext, ToolResult};

const MAX_OUTPUT_BYTES: usize = 128 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 600; // 10 minutes — code gen tasks are slow
const DEFAULT_MAX_BUDGET_USD: f64 = 5.0;

/// Tool that invokes Claude Code CLI (`claude -p`) for code generation tasks.
/// This bridges OpenClaw-RS to Anthropic's Claude Code, enabling the agent to
/// delegate complex coding tasks (write code, fix bugs, refactor, run tests)
/// to Claude Code's agentic coding capabilities.
pub struct ClaudeCodeTool;

#[async_trait]
impl Tool for ClaudeCodeTool {
    fn name(&self) -> &str {
        "claude_code"
    }

    fn description(&self) -> &str {
        "Invoke Claude Code CLI to perform coding tasks in a project directory. \
         Claude Code can read/write files, run shell commands, search code, and \
         iteratively build or fix software. Use this for tasks that require actual \
         code generation, bug fixing, refactoring, test writing, or any multi-step \
         coding work. Provide a clear task description and the project directory. \
         The tool runs non-interactively and returns Claude Code's final output."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "The coding task to perform. Be specific: what to build, fix, or change. Include file names, function names, and expected behavior."
                },
                "project_dir": {
                    "type": "string",
                    "description": "Absolute path to the project directory where Claude Code should work (e.g. /home/shawaz/CascadeProjects/my-app)"
                },
                "model": {
                    "type": "string",
                    "description": "Model to use (default: sonnet). Options: sonnet, opus, haiku"
                },
                "max_budget_usd": {
                    "type": "number",
                    "description": "Maximum dollar budget for this task (default: 5.0)"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 600 = 10 minutes)"
                },
                "allowed_tools": {
                    "type": "string",
                    "description": "Comma-separated list of Claude Code tools to allow (default: all). Example: 'Bash,Edit,Read,Write'"
                },
                "system_prompt": {
                    "type": "string",
                    "description": "Optional system prompt to append to Claude Code's default prompt"
                }
            },
            "required": ["task", "project_dir"]
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolResult> {
        let task = args
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("claude_code: missing 'task' parameter"))?;

        let project_dir = args
            .get("project_dir")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("claude_code: missing 'project_dir' parameter"))?;

        // Validate project directory exists
        let project_path = std::path::Path::new(project_dir);
        if !project_path.exists() {
            return Ok(ToolResult::error(format!(
                "Project directory does not exist: {}",
                project_dir
            )));
        }

        let model = args
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("sonnet");

        let max_budget = args
            .get("max_budget_usd")
            .and_then(|v| v.as_f64())
            .unwrap_or(DEFAULT_MAX_BUDGET_USD);

        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        let allowed_tools = args
            .get("allowed_tools")
            .and_then(|v| v.as_str());

        let system_prompt = args
            .get("system_prompt")
            .and_then(|v| v.as_str());

        info!(
            "claude_code: task={} project={} model={} budget=${} timeout={}s",
            &task[..task.len().min(100)],
            project_dir,
            model,
            max_budget,
            timeout_secs,
        );

        // Build the claude command
        let mut cmd = Command::new("claude");
        cmd.arg("-p") // print mode (non-interactive)
            .arg("--output-format").arg("text")
            .arg("--model").arg(model)
            .arg("--max-budget-usd").arg(format!("{:.2}", max_budget))
            .arg("--dangerously-skip-permissions") // unattended execution
            .current_dir(project_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::piped());

        if let Some(tools) = allowed_tools {
            cmd.arg("--allowedTools").arg(tools);
        }

        if let Some(prompt) = system_prompt {
            cmd.arg("--append-system-prompt").arg(prompt);
        }

        // Pass the task as the positional prompt argument
        cmd.arg(task);

        // Execute with timeout
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            cmd.output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let exit_code = output.status.code().unwrap_or(-1);

                // Truncate if too long
                if stdout.len() > MAX_OUTPUT_BYTES {
                    let keep = MAX_OUTPUT_BYTES / 2;
                    let head = &stdout[..keep];
                    let tail = &stdout[stdout.len() - keep..];
                    stdout = format!(
                        "{}\n\n... ({} bytes truncated) ...\n\n{}",
                        head,
                        stdout.len() - MAX_OUTPUT_BYTES,
                        tail
                    );
                }

                let mut result_text = String::new();

                if !stdout.is_empty() {
                    result_text.push_str(&stdout);
                }

                if !stderr.is_empty() {
                    // Filter out common noise from stderr
                    let filtered_stderr: String = stderr
                        .lines()
                        .filter(|l| {
                            !l.contains("npm warn")
                                && !l.contains("npm notice")
                                && !l.trim().is_empty()
                        })
                        .collect::<Vec<_>>()
                        .join("\n");

                    if !filtered_stderr.is_empty() {
                        if !result_text.is_empty() {
                            result_text.push('\n');
                        }
                        result_text.push_str("[stderr] ");
                        result_text.push_str(&filtered_stderr);
                    }
                }

                if result_text.is_empty() {
                    result_text = format!("(claude exited with code {})", exit_code);
                }

                info!(
                    "claude_code: completed exit={} output={}B stderr={}B",
                    exit_code,
                    stdout.len(),
                    stderr.len(),
                );

                if exit_code == 0 {
                    Ok(ToolResult::success(result_text))
                } else {
                    warn!("claude_code: non-zero exit code {}", exit_code);
                    Ok(ToolResult::error(result_text))
                }
            }
            Ok(Err(e)) => {
                warn!("claude_code: failed to execute: {}", e);
                Ok(ToolResult::error(format!(
                    "Failed to execute claude: {}. Is claude-code installed?",
                    e
                )))
            }
            Err(_) => {
                warn!("claude_code: timed out after {}s", timeout_secs);
                Ok(ToolResult::error(format!(
                    "Claude Code timed out after {}s. The task may be too complex — try breaking it into smaller steps.",
                    timeout_secs
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_code_tool_definition() {
        let tool = ClaudeCodeTool;
        assert_eq!(tool.name(), "claude_code");
        assert!(tool.description().contains("coding tasks"));
        let params = tool.parameters();
        assert!(params["properties"]["task"].is_object());
        assert!(params["properties"]["project_dir"].is_object());
        let required = params["required"].as_array().unwrap();
        assert_eq!(required.len(), 2);
        assert_eq!(required[0], "task");
        assert_eq!(required[1], "project_dir");
    }

    #[test]
    fn test_claude_code_optional_params() {
        let tool = ClaudeCodeTool;
        let params = tool.parameters();
        assert!(params["properties"]["model"].is_object());
        assert!(params["properties"]["max_budget_usd"].is_object());
        assert!(params["properties"]["timeout_secs"].is_object());
        assert!(params["properties"]["allowed_tools"].is_object());
        assert!(params["properties"]["system_prompt"].is_object());
    }
}
