use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::process::Stdio;
use tokio::process::Command;

use super::{Tool, ToolContext, ToolResult};

const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 30;

pub struct ExecTool;

#[async_trait]
impl Tool for ExecTool {
    fn name(&self) -> &str {
        "exec"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return its output. Commands run in the workspace directory with a timeout."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 30)"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("exec: missing 'command' argument"))?;

        // Sandbox: check command blocklist
        if let Some(blocked) = ctx.sandbox.is_command_blocked(command) {
            return Ok(ToolResult::error(format!(
                "Command blocked by sandbox policy: contains '{}'",
                blocked
            )));
        }

        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        // Sandbox: clamp timeout
        let timeout_secs = ctx.sandbox.clamp_timeout(timeout_secs);

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            Command::new("sh")
                .arg("-c")
                .arg(command)
                .current_dir(&ctx.workspace_dir)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let exit_code = output.status.code().unwrap_or(-1);

                // Truncate if too long
                if stdout.len() > MAX_OUTPUT_BYTES {
                    stdout.truncate(MAX_OUTPUT_BYTES);
                    stdout.push_str("\n... (output truncated)");
                }

                let mut result_text = String::new();
                if !stdout.is_empty() {
                    result_text.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !result_text.is_empty() {
                        result_text.push('\n');
                    }
                    result_text.push_str("[stderr] ");
                    result_text.push_str(&stderr);
                }
                if result_text.is_empty() {
                    result_text = format!("(exit code {})", exit_code);
                } else if exit_code != 0 {
                    result_text.push_str(&format!("\n(exit code {})", exit_code));
                }

                if exit_code == 0 {
                    Ok(ToolResult::success(result_text))
                } else {
                    Ok(ToolResult::error(result_text))
                }
            }
            Ok(Err(e)) => Ok(ToolResult::error(format!("exec failed: {}", e))),
            Err(_) => Ok(ToolResult::error(format!(
                "exec timed out after {}s",
                timeout_secs
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_exec_echo() {
        let tool = ExecTool;
        let ctx = ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test-session".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        ..ToolContext::default()
        };
        let args = serde_json::json!({"command": "echo hello"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("hello"));
    }

    #[tokio::test]
    async fn test_exec_failure() {
        let tool = ExecTool;
        let ctx = ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test-session".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        ..ToolContext::default()
        };
        let args = serde_json::json!({"command": "false"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn test_exec_blocked_command() {
        let tool = ExecTool;
        let ctx = ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test-session".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        ..ToolContext::default()
        };
        let args = serde_json::json!({"command": "rm -rf /"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("blocked"));
    }

    #[tokio::test]
    async fn test_exec_blocked_shadow() {
        let tool = ExecTool;
        let ctx = ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test-session".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        ..ToolContext::default()
        };
        let args = serde_json::json!({"command": "cat /etc/shadow"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("blocked"));
    }

    #[tokio::test]
    async fn test_exec_timeout() {
        let tool = ExecTool;
        let ctx = ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test-session".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        ..ToolContext::default()
        };
        let args = serde_json::json!({"command": "sleep 10", "timeout_secs": 1});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("timed out"));
    }
}
