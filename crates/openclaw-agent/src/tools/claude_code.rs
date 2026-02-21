use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::llm::streaming::StreamEvent;
use super::{Tool, ToolContext, ToolResult};

const MAX_OUTPUT_BYTES: usize = 128 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 600;
const DEFAULT_MAX_BUDGET_USD: f64 = 5.0;

/// Tool that invokes Claude Code CLI (`claude -p`) for code generation tasks.
/// Streams structured JSON events from Claude Code in real-time, forwarding
/// them as StreamEvents so the Telegram gateway can show live progress.
pub struct ClaudeCodeTool;

/// Emit a StreamEvent if the tool has a stream channel.
fn emit(ctx: &ToolContext, event: StreamEvent) {
    if let Some(ref tx) = ctx.stream_tx {
        let _ = tx.send(event);
    }
}

/// Parse a single stream-json line from Claude Code and emit appropriate events.
/// Returns any final result text extracted from the stream.
fn process_stream_line(line: &str, ctx: &ToolContext, final_text: &mut String) {
    let Ok(obj) = serde_json::from_str::<Value>(line) else {
        return;
    };

    let event_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match event_type {
        "system" => {
            // Init event — log model and tools
            let model = obj.get("model").and_then(|v| v.as_str()).unwrap_or("unknown");
            let tool_count = obj.get("tools").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
            debug!("claude_code: init model={} tools={}", model, tool_count);
            emit(ctx, StreamEvent::ContentDelta(
                format!("🔧 Claude Code started (model: {}, {} tools)\n", model, tool_count),
            ));
        }
        "assistant" => {
            if let Some(msg) = obj.get("message") {
                // Extract content blocks
                if let Some(content) = msg.get("content").and_then(|v| v.as_array()) {
                    for block in content {
                        let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        match block_type {
                            "text" => {
                                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                    final_text.push_str(text);
                                    // Stream text in chunks to avoid flooding
                                    if text.len() > 200 {
                                        let preview = &text[..200];
                                        emit(ctx, StreamEvent::ContentDelta(
                                            format!("💬 {}...\n", preview.trim()),
                                        ));
                                    } else if !text.trim().is_empty() {
                                        emit(ctx, StreamEvent::ContentDelta(
                                            format!("💬 {}\n", text.trim()),
                                        ));
                                    }
                                }
                            }
                            "tool_use" => {
                                let tool_name = block.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
                                let input = block.get("input").cloned().unwrap_or(Value::Null);
                                // Summarize the tool call
                                let summary = summarize_tool_input(tool_name, &input);
                                emit(ctx, StreamEvent::ToolExec {
                                    name: format!("cc:{}", tool_name),
                                    call_id: block.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                    args_summary: summary,
                                });
                            }
                            "tool_result" => {
                                let is_error = block.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
                                let output = block.get("content")
                                    .and_then(|v| v.as_str())
                                    .or_else(|| block.get("output").and_then(|v| v.as_str()))
                                    .unwrap_or("")
                                    .to_string();
                                let preview = if output.len() > 200 {
                                    format!("{}...", &output[..200])
                                } else {
                                    output
                                };
                                emit(ctx, StreamEvent::ToolResult {
                                    name: "cc:tool".to_string(),
                                    success: !is_error,
                                    output_preview: preview,
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        "result" => {
            // Final result event
            let cost = obj.get("total_cost_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let duration_ms = obj.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0);
            let num_turns = obj.get("num_turns").and_then(|v| v.as_u64()).unwrap_or(0);
            let is_error = obj.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);

            // Extract final result text if we haven't captured it from assistant messages
            if final_text.is_empty() {
                if let Some(result) = obj.get("result").and_then(|v| v.as_str()) {
                    final_text.push_str(result);
                }
            }

            let status = if is_error { "❌" } else { "✅" };
            emit(ctx, StreamEvent::ContentDelta(
                format!(
                    "\n{} Claude Code finished — {} turns, {:.1}s, ${:.4}\n",
                    status, num_turns, duration_ms as f64 / 1000.0, cost,
                ),
            ));
        }
        "user" => {
            // Claude Code echoing back the user prompt — skip silently
        }
        _ => {
            debug!("claude_code: unknown event type: {}", event_type);
        }
    }
}

/// Create a human-readable summary of a Claude Code tool invocation.
fn summarize_tool_input(tool_name: &str, input: &Value) -> String {
    match tool_name {
        "Bash" | "bash" => {
            let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("...");
            let truncated = if cmd.len() > 120 { &cmd[..120] } else { cmd };
            format!("$ {}", truncated)
        }
        "Edit" | "edit" | "MultiEdit" => {
            let file = input.get("file_path")
                .or_else(|| input.get("file"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("✏️  {}", file)
        }
        "Write" | "write" => {
            let file = input.get("file_path")
                .or_else(|| input.get("file"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("📝 {}", file)
        }
        "Read" | "read" => {
            let file = input.get("file_path")
                .or_else(|| input.get("file"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("📖 {}", file)
        }
        "Grep" | "grep" | "Glob" | "glob" => {
            let pattern = input.get("pattern")
                .or_else(|| input.get("query"))
                .and_then(|v| v.as_str())
                .unwrap_or("...");
            format!("🔍 {}", pattern)
        }
        "WebFetch" | "web_fetch" => {
            let url = input.get("url").and_then(|v| v.as_str()).unwrap_or("...");
            format!("🌐 {}", url)
        }
        _ => {
            // Generic: show first key-value pair
            if let Some(obj) = input.as_object() {
                if let Some((k, v)) = obj.iter().next() {
                    let val_str = match v {
                        Value::String(s) => {
                            if s.len() > 80 { format!("{}...", &s[..80]) } else { s.clone() }
                        }
                        _ => v.to_string(),
                    };
                    return format!("{}: {}", k, val_str);
                }
            }
            tool_name.to_string()
        }
    }
}

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
         The tool streams progress events in real-time and returns the final output."
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

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        // Be forgiving of small models that mangle parameter names
        let task = args.get("task").and_then(|v| v.as_str())
            .or_else(|| args.get("prompt").and_then(|v| v.as_str()))
            .or_else(|| args.get("message").and_then(|v| v.as_str()))
            .or_else(|| args.get("query").and_then(|v| v.as_str()))
            .or_else(|| args.get("instruction").and_then(|v| v.as_str()))
            .or_else(|| args.get("command").and_then(|v| v.as_str()))
            .or_else(|| {
                // Last resort: if there's only one string value, use it
                args.as_object().and_then(|obj| {
                    let strings: Vec<&str> = obj.values()
                        .filter_map(|v| v.as_str())
                        .collect();
                    if strings.len() == 1 { Some(strings[0]) } else { None }
                })
            })
            .ok_or_else(|| anyhow::anyhow!("claude_code: missing 'task' parameter. Send {{\"task\": \"...\", \"project_dir\": \"...\"}}"))?;

        let project_dir = args.get("project_dir").and_then(|v| v.as_str())
            .or_else(|| args.get("project").and_then(|v| v.as_str()))
            .or_else(|| args.get("dir").and_then(|v| v.as_str()))
            .or_else(|| args.get("directory").and_then(|v| v.as_str()))
            .or_else(|| args.get("path").and_then(|v| v.as_str()))
            .unwrap_or(&ctx.workspace_dir);

        let project_path = std::path::Path::new(project_dir);
        if !project_path.exists() {
            return Ok(ToolResult::error(format!(
                "Project directory does not exist: {}",
                project_dir
            )));
        }

        let model = args.get("model").and_then(|v| v.as_str()).unwrap_or("sonnet");
        let max_budget = args.get("max_budget_usd").and_then(|v| v.as_f64()).unwrap_or(DEFAULT_MAX_BUDGET_USD);
        let timeout_secs = args.get("timeout_secs").and_then(|v| v.as_u64()).unwrap_or(DEFAULT_TIMEOUT_SECS);
        let allowed_tools = args.get("allowed_tools").and_then(|v| v.as_str());
        let system_prompt = args.get("system_prompt").and_then(|v| v.as_str());

        info!(
            "claude_code: task={} project={} model={} budget=${} timeout={}s",
            &task[..task.len().min(100)],
            project_dir, model, max_budget, timeout_secs,
        );

        // Build the claude command — stream-json for structured real-time output
        let mut cmd = Command::new("claude");
        cmd.arg("-p")
            .arg("--output-format").arg("stream-json")
            .arg("--verbose")
            .arg("--model").arg(model)
            .arg("--max-budget-usd").arg(format!("{:.2}", max_budget))
            .arg("--dangerously-skip-permissions")
            .current_dir(project_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

        if let Some(tools) = allowed_tools {
            cmd.arg("--allowedTools").arg(tools);
        }

        if let Some(prompt) = system_prompt {
            cmd.arg("--append-system-prompt").arg(prompt);
        }

        cmd.arg(task);

        // Spawn the process — read stdout line-by-line as events arrive
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                warn!("claude_code: failed to spawn: {}", e);
                return Ok(ToolResult::error(format!(
                    "Failed to execute claude: {}. Is claude-code installed?", e
                )));
            }
        };

        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        let mut stdout_reader = BufReader::new(stdout).lines();
        let mut stderr_reader = BufReader::new(stderr);

        // Collect stderr in background
        let stderr_handle = tokio::spawn(async move {
            let mut buf = String::new();
            let _ = tokio::io::AsyncReadExt::read_to_string(&mut stderr_reader, &mut buf).await;
            buf
        });

        let mut final_text = String::new();
        let mut lines_processed = 0u64;

        // Stream stdout line-by-line with timeout
        let stream_result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            async {
                while let Ok(Some(line)) = stdout_reader.next_line().await {
                    if line.trim().is_empty() {
                        continue;
                    }
                    lines_processed += 1;
                    process_stream_line(&line, ctx, &mut final_text);
                }
            },
        )
        .await;

        // Handle timeout — kill the process
        if stream_result.is_err() {
            warn!("claude_code: timed out after {}s, killing process", timeout_secs);
            let _ = child.kill().await;
            return Ok(ToolResult::error(format!(
                "Claude Code timed out after {}s. The task may be too complex — try breaking it into smaller steps.\n\nPartial output ({} events processed):\n{}",
                timeout_secs, lines_processed,
                if final_text.len() > 2000 { &final_text[final_text.len()-2000..] } else { &final_text },
            )));
        }

        // Wait for process exit
        let status = child.wait().await;
        let exit_code = status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);

        // Collect stderr
        let stderr_text = stderr_handle.await.unwrap_or_default();
        if !stderr_text.is_empty() {
            let filtered: String = stderr_text
                .lines()
                .filter(|l| !l.contains("npm warn") && !l.contains("npm notice") && !l.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if !filtered.is_empty() {
                debug!("claude_code stderr: {}", &filtered[..filtered.len().min(500)]);
            }
        }

        // Truncate final output if too long
        if final_text.len() > MAX_OUTPUT_BYTES {
            let keep = MAX_OUTPUT_BYTES / 2;
            let head = &final_text[..keep];
            let tail = &final_text[final_text.len() - keep..];
            final_text = format!(
                "{}\n\n... ({} bytes truncated) ...\n\n{}",
                head, final_text.len() - MAX_OUTPUT_BYTES, tail
            );
        }

        if final_text.is_empty() {
            final_text = format!("(claude exited with code {}, {} stream events)", exit_code, lines_processed);
        }

        info!(
            "claude_code: completed exit={} events={} output={}B",
            exit_code, lines_processed, final_text.len(),
        );

        if exit_code == 0 {
            Ok(ToolResult::success(final_text))
        } else {
            warn!("claude_code: non-zero exit code {}", exit_code);
            Ok(ToolResult::error(final_text))
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

    #[test]
    fn test_summarize_tool_input() {
        let input = serde_json::json!({"command": "cargo test"});
        assert_eq!(summarize_tool_input("Bash", &input), "$ cargo test");

        let input = serde_json::json!({"file_path": "/src/main.rs"});
        assert_eq!(summarize_tool_input("Edit", &input), "✏️  /src/main.rs");
        assert_eq!(summarize_tool_input("Read", &input), "📖 /src/main.rs");
        assert_eq!(summarize_tool_input("Write", &input), "📝 /src/main.rs");

        let input = serde_json::json!({"pattern": "TODO"});
        assert_eq!(summarize_tool_input("Grep", &input), "🔍 TODO");
    }

    #[test]
    fn test_process_stream_line_system() {
        let ctx = ToolContext::default();
        let mut text = String::new();
        let line = r#"{"type":"system","subtype":"init","model":"claude-sonnet-4","tools":["Bash","Edit"]}"#;
        process_stream_line(line, &ctx, &mut text);
        // Should not crash, text stays empty (system events don't add to final text)
        assert!(text.is_empty());
    }

    #[test]
    fn test_process_stream_line_result() {
        let ctx = ToolContext::default();
        let mut text = String::new();
        let line = r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":5000,"num_turns":3,"result":"Done!","total_cost_usd":0.05}"#;
        process_stream_line(line, &ctx, &mut text);
        assert_eq!(text, "Done!");
    }

    #[test]
    fn test_process_stream_line_invalid_json() {
        let ctx = ToolContext::default();
        let mut text = String::new();
        process_stream_line("not json at all", &ctx, &mut text);
        assert!(text.is_empty());
    }
}
