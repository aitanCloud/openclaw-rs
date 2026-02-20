use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use tokio::process::Command;

use super::{Tool, ToolContext, ToolResult};

const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// A running or finished background process
struct ProcessEntry {
    label: String,
    command: String,
    child: Option<tokio::process::Child>,
    stdout_buf: Vec<u8>,
    stderr_buf: Vec<u8>,
    exit_code: Option<i32>,
    started_at: std::time::Instant,
}

fn processes() -> &'static Mutex<HashMap<String, ProcessEntry>> {
    static PROCESSES: OnceLock<Mutex<HashMap<String, ProcessEntry>>> = OnceLock::new();
    PROCESSES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    format!("proc_{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

pub struct ProcessTool;

#[async_trait]
impl Tool for ProcessTool {
    fn name(&self) -> &str {
        "process"
    }

    fn description(&self) -> &str {
        "Manage background processes. Actions: start (launch a command in background), poll (check output/status), kill (terminate), list (show all)."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["start", "poll", "kill", "list"],
                    "description": "Action to perform"
                },
                "command": {
                    "type": "string",
                    "description": "Shell command to run (for 'start' action)"
                },
                "label": {
                    "type": "string",
                    "description": "Optional human-readable label for the process (for 'start')"
                },
                "id": {
                    "type": "string",
                    "description": "Process ID returned by 'start' (for 'poll' and 'kill')"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("process: missing 'action' argument"))?;

        match action {
            "start" => self.start(args, ctx).await,
            "poll" => self.poll(args).await,
            "kill" => self.kill(args).await,
            "list" => self.list().await,
            _ => Ok(ToolResult::error(format!(
                "Unknown action '{}'. Use: start, poll, kill, list",
                action
            ))),
        }
    }
}

impl ProcessTool {
    async fn start(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let command = match args.get("command").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return Ok(ToolResult::error("process start: missing 'command'")),
        };

        // Sandbox: check command blocklist
        if let Some(blocked) = ctx.sandbox.is_command_blocked(command) {
            return Ok(ToolResult::error(format!(
                "Command blocked by sandbox policy: contains '{}'",
                blocked
            )));
        }

        let label = args
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or(command)
            .to_string();

        let child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&ctx.workspace_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

        match child {
            Ok(child) => {
                let id = next_id();
                let entry = ProcessEntry {
                    label: label.clone(),
                    command: command.to_string(),
                    child: Some(child),
                    stdout_buf: Vec::new(),
                    stderr_buf: Vec::new(),
                    exit_code: None,
                    started_at: std::time::Instant::now(),
                };

                {
                    let mut procs = processes().lock().unwrap();
                    procs.insert(id.clone(), entry);
                }

                Ok(ToolResult::success(format!(
                    "Started background process: id={}, label={}",
                    id, label
                )))
            }
            Err(e) => Ok(ToolResult::error(format!("Failed to start process: {}", e))),
        }
    }

    async fn poll(&self, args: Value) -> Result<ToolResult> {
        let id = match args.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return Ok(ToolResult::error("process poll: missing 'id'")),
        };

        // Check state under the lock (no .await here)
        enum PollState {
            AlreadyDone { stdout: String, stderr: String, label: String, exit_code: Option<i32>, elapsed: std::time::Duration },
            JustFinished,
            StillRunning,
            NoChild,
        }

        let state = {
            let mut procs = processes().lock().unwrap();
            let entry = match procs.get_mut(&id) {
                Some(e) => e,
                None => return Ok(ToolResult::error(format!("No process with id '{}'", id))),
            };

            if entry.exit_code.is_some() && entry.child.is_none() {
                PollState::AlreadyDone {
                    stdout: String::from_utf8_lossy(&entry.stdout_buf).to_string(),
                    stderr: String::from_utf8_lossy(&entry.stderr_buf).to_string(),
                    label: entry.label.clone(),
                    exit_code: entry.exit_code,
                    elapsed: entry.started_at.elapsed(),
                }
            } else if let Some(ref mut child) = entry.child {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        entry.exit_code = Some(status.code().unwrap_or(-1));
                        PollState::JustFinished
                    }
                    Ok(None) => PollState::StillRunning,
                    Err(e) => {
                        return Ok(ToolResult::error(format!("poll error: {}", e)));
                    }
                }
            } else {
                PollState::NoChild
            }
        };

        match state {
            PollState::AlreadyDone { stdout, stderr, label, exit_code, elapsed } => {
                return Ok(ToolResult::success(format_poll_output(
                    &id, &label, &stdout, &stderr, exit_code, elapsed,
                )));
            }
            PollState::StillRunning | PollState::NoChild => {
                let procs = processes().lock().unwrap();
                if let Some(entry) = procs.get(&id) {
                    return Ok(ToolResult::success(format!(
                        "Process {} ({}) is still running ({}s elapsed)",
                        id,
                        entry.label,
                        entry.started_at.elapsed().as_secs()
                    )));
                }
                return Ok(ToolResult::error(format!("No process with id '{}'", id)));
            }
            PollState::JustFinished => {}
        }

        // Process just finished — take child out of lock, read output, store back
        let mut child_opt = {
            let mut procs = processes().lock().unwrap();
            procs.get_mut(&id).and_then(|e| e.child.take())
        };

        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();

        if let Some(ref mut child) = child_opt {
            if let Some(mut reader) = child.stdout.take() {
                use tokio::io::AsyncReadExt;
                let _ = reader.read_to_end(&mut stdout_buf).await;
            }
            if let Some(mut reader) = child.stderr.take() {
                use tokio::io::AsyncReadExt;
                let _ = reader.read_to_end(&mut stderr_buf).await;
            }
        }

        let mut procs = processes().lock().unwrap();
        if let Some(entry) = procs.get_mut(&id) {
            entry.stdout_buf = stdout_buf;
            entry.stderr_buf = stderr_buf;
            let stdout = String::from_utf8_lossy(&entry.stdout_buf).to_string();
            let stderr = String::from_utf8_lossy(&entry.stderr_buf).to_string();
            Ok(ToolResult::success(format_poll_output(
                &id,
                &entry.label,
                &stdout,
                &stderr,
                entry.exit_code,
                entry.started_at.elapsed(),
            )))
        } else {
            Ok(ToolResult::error(format!("No process with id '{}'", id)))
        }
    }

    async fn kill(&self, args: Value) -> Result<ToolResult> {
        let id = match args.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return Ok(ToolResult::error("process kill: missing 'id'")),
        };

        // Take the child out of the mutex so we don't hold the guard across .await
        let (mut child_opt, label) = {
            let mut procs = processes().lock().unwrap();
            let entry = match procs.get_mut(&id) {
                Some(e) => e,
                None => return Ok(ToolResult::error(format!("No process with id '{}'", id))),
            };
            (entry.child.take(), entry.label.clone())
        };

        if let Some(ref mut child) = child_opt {
            match child.kill().await {
                Ok(_) => {
                    let mut procs = processes().lock().unwrap();
                    if let Some(entry) = procs.get_mut(&id) {
                        entry.exit_code = Some(-9);
                    }
                    Ok(ToolResult::success(format!(
                        "Killed process {} ({})",
                        id, label
                    )))
                }
                Err(e) => Ok(ToolResult::error(format!("Failed to kill {}: {}", id, e))),
            }
        } else {
            let exit_code = {
                let procs = processes().lock().unwrap();
                procs.get(&id).and_then(|e| e.exit_code)
            };
            Ok(ToolResult::success(format!(
                "Process {} already finished (exit code {:?})",
                id, exit_code
            )))
        }
    }

    async fn list(&self) -> Result<ToolResult> {
        let procs = processes().lock().unwrap();
        if procs.is_empty() {
            return Ok(ToolResult::success("No background processes."));
        }

        let mut lines = Vec::new();
        for (id, entry) in procs.iter() {
            let status = if let Some(code) = entry.exit_code {
                format!("exited ({})", code)
            } else {
                format!("running ({}s)", entry.started_at.elapsed().as_secs())
            };
            lines.push(format!(
                "{}: {} [{}] — {}",
                id, entry.label, status, entry.command
            ));
        }

        Ok(ToolResult::success(lines.join("\n")))
    }
}

fn format_poll_output(
    id: &str,
    label: &str,
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
    elapsed: std::time::Duration,
) -> String {
    let mut out = String::new();

    let status = match exit_code {
        Some(code) => format!("exited (code {})", code),
        None => "running".to_string(),
    };

    out.push_str(&format!(
        "Process {} ({}) — {} — {}s\n",
        id,
        label,
        status,
        elapsed.as_secs()
    ));

    if !stdout.is_empty() {
        let s = if stdout.len() > MAX_OUTPUT_BYTES {
            format!("{}... (truncated)", &stdout[..MAX_OUTPUT_BYTES])
        } else {
            stdout.to_string()
        };
        out.push_str(&s);
    }

    if !stderr.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("[stderr] ");
        let s = if stderr.len() > MAX_OUTPUT_BYTES {
            format!("{}... (truncated)", &stderr[..MAX_OUTPUT_BYTES])
        } else {
            stderr.to_string()
        };
        out.push_str(&s);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> ToolContext {
        ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test-session".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        ..ToolContext::default()
        }
    }

    #[tokio::test]
    async fn test_process_start_and_poll() {
        let tool = ProcessTool;
        let ctx = test_ctx();

        // Start a quick process
        let args = serde_json::json!({"action": "start", "command": "echo hello_bg", "label": "test-echo"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("proc_"));

        // Extract ID
        let id = result.output.split("id=").nth(1).unwrap().split(',').next().unwrap();

        // Give it a moment to finish
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Poll
        let args = serde_json::json!({"action": "poll", "id": id});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("hello_bg"));
    }

    #[tokio::test]
    async fn test_process_list() {
        let tool = ProcessTool;
        let ctx = test_ctx();

        let args = serde_json::json!({"action": "list"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn test_process_kill() {
        let tool = ProcessTool;
        let ctx = test_ctx();

        // Start a long-running process
        let args = serde_json::json!({"action": "start", "command": "sleep 60", "label": "sleeper"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(!result.is_error);
        let id = result.output.split("id=").nth(1).unwrap().split(',').next().unwrap();

        // Kill it
        let args = serde_json::json!({"action": "kill", "id": id});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("Killed"));
    }

    #[tokio::test]
    async fn test_process_blocked_command() {
        let tool = ProcessTool;
        let ctx = test_ctx();

        let args = serde_json::json!({"action": "start", "command": "rm -rf /"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("blocked"));
    }

    #[tokio::test]
    async fn test_process_poll_unknown_id() {
        let tool = ProcessTool;
        let ctx = test_ctx();

        let args = serde_json::json!({"action": "poll", "id": "nonexistent"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("No process"));
    }
}
