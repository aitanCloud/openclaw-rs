use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

use super::{Tool, ToolContext, ToolResult};

const MAX_RESULTS: usize = 200;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search for a pattern across files in the workspace. Uses ripgrep (rg) if available, falls back to grep. Returns matching lines with file paths and line numbers. Supports regex patterns."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Search pattern (regex by default)"
                },
                "path": {
                    "type": "string",
                    "description": "Directory or file to search in (default: workspace root)"
                },
                "include": {
                    "type": "string",
                    "description": "File glob pattern to include, e.g. '*.rs' or '*.py'"
                },
                "fixed_strings": {
                    "type": "boolean",
                    "description": "Treat pattern as a literal string, not regex (default: false)"
                },
                "case_sensitive": {
                    "type": "boolean",
                    "description": "Case-sensitive search (default: false, smart case)"
                },
                "context_lines": {
                    "type": "integer",
                    "description": "Number of context lines around each match (default: 0)"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let pattern = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("grep: missing 'pattern' argument"))?;

        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".");

        let include = args.get("include").and_then(|v| v.as_str());
        let fixed_strings = args.get("fixed_strings").and_then(|v| v.as_bool()).unwrap_or(false);
        let case_sensitive = args.get("case_sensitive").and_then(|v| v.as_bool()).unwrap_or(false);
        let context_lines = args.get("context_lines").and_then(|v| v.as_u64()).unwrap_or(0);

        let search_path = if path == "." || path.is_empty() {
            PathBuf::from(&ctx.workspace_dir)
        } else if path.starts_with('/') {
            PathBuf::from(path)
        } else {
            PathBuf::from(&ctx.workspace_dir).join(path)
        };

        // Try rg first, fall back to grep
        let (cmd, cmd_args) = if which_exists("rg") {
            build_rg_args(pattern, &search_path, include, fixed_strings, case_sensitive, context_lines)
        } else {
            build_grep_args(pattern, &search_path, include, fixed_strings, case_sensitive, context_lines)
        };

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            Command::new(&cmd)
                .args(&cmd_args)
                .current_dir(&ctx.workspace_dir)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let exit_code = output.status.code().unwrap_or(-1);

                // exit code 1 = no matches (not an error)
                if exit_code == 1 && stdout.is_empty() {
                    return Ok(ToolResult::success("No matches found."));
                }

                if stdout.len() > MAX_OUTPUT_BYTES {
                    stdout.truncate(MAX_OUTPUT_BYTES);
                    stdout.push_str("\n... (output truncated)");
                }

                // Count and cap result lines
                let lines: Vec<&str> = stdout.lines().collect();
                if lines.len() > MAX_RESULTS {
                    let truncated: String = lines[..MAX_RESULTS].join("\n");
                    return Ok(ToolResult::success(format!(
                        "{}\n... ({} total matches, showing first {})",
                        truncated,
                        lines.len(),
                        MAX_RESULTS
                    )));
                }

                if stdout.is_empty() {
                    Ok(ToolResult::success("No matches found."))
                } else {
                    Ok(ToolResult::success(format!("{} match(es):\n{}", lines.len(), stdout)))
                }
            }
            Ok(Err(e)) => Ok(ToolResult::error(format!("grep failed: {}", e))),
            Err(_) => Ok(ToolResult::error("grep timed out after 15s")),
        }
    }
}

fn which_exists(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn build_rg_args(
    pattern: &str,
    path: &PathBuf,
    include: Option<&str>,
    fixed_strings: bool,
    case_sensitive: bool,
    context_lines: u64,
) -> (String, Vec<String>) {
    let mut args = vec![
        "--no-heading".to_string(),
        "--line-number".to_string(),
        "--color=never".to_string(),
        format!("--max-count={}", MAX_RESULTS),
    ];

    if fixed_strings {
        args.push("--fixed-strings".to_string());
    }
    if case_sensitive {
        args.push("--case-sensitive".to_string());
    } else {
        args.push("--smart-case".to_string());
    }
    if context_lines > 0 {
        args.push(format!("--context={}", context_lines.min(5)));
    }
    if let Some(glob) = include {
        args.push(format!("--glob={}", glob));
    }

    args.push(pattern.to_string());
    args.push(path.to_string_lossy().to_string());

    ("rg".to_string(), args)
}

fn build_grep_args(
    pattern: &str,
    path: &PathBuf,
    include: Option<&str>,
    fixed_strings: bool,
    case_sensitive: bool,
    context_lines: u64,
) -> (String, Vec<String>) {
    let mut args = vec![
        "-rn".to_string(),
        "--color=never".to_string(),
    ];

    if fixed_strings {
        args.push("-F".to_string());
    }
    if !case_sensitive {
        args.push("-i".to_string());
    }
    if context_lines > 0 {
        args.push(format!("-C{}", context_lines.min(5)));
    }
    if let Some(glob) = include {
        args.push(format!("--include={}", glob));
    }

    args.push(pattern.to_string());
    args.push(path.to_string_lossy().to_string());

    ("grep".to_string(), args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_grep_echo() {
        let tool = GrepTool;
        let ctx = ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        ..ToolContext::default()
        };

        // Create a test file
        tokio::fs::write("/tmp/openclaw-test-grep.txt", "hello world\nfoo bar\nhello again\n")
            .await
            .unwrap();

        let args = serde_json::json!({"pattern": "hello", "path": "/tmp/openclaw-test-grep.txt"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("hello"));

        tokio::fs::remove_file("/tmp/openclaw-test-grep.txt").await.ok();
    }

    #[tokio::test]
    async fn test_grep_no_match() {
        let tool = GrepTool;
        let ctx = ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        ..ToolContext::default()
        };

        tokio::fs::write("/tmp/openclaw-test-grep2.txt", "hello world\n")
            .await
            .unwrap();

        let args = serde_json::json!({"pattern": "zzzznotfound", "path": "/tmp/openclaw-test-grep2.txt"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("No matches"));

        tokio::fs::remove_file("/tmp/openclaw-test-grep2.txt").await.ok();
    }

    #[test]
    fn test_rg_args() {
        let (cmd, args) = build_rg_args("pattern", &PathBuf::from("/tmp"), Some("*.rs"), false, false, 2);
        assert_eq!(cmd, "rg");
        assert!(args.contains(&"--smart-case".to_string()));
        assert!(args.contains(&"--glob=*.rs".to_string()));
        assert!(args.contains(&"--context=2".to_string()));
    }
}
