use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

use super::{Tool, ToolContext, ToolResult};

const MAX_RESULTS: usize = 500;

pub struct FindTool;

#[async_trait]
impl Tool for FindTool {
    fn name(&self) -> &str {
        "find"
    }

    fn description(&self) -> &str {
        "Find files and directories by name pattern (glob). Uses fd if available, falls back to find. Returns paths relative to the search directory."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match file/directory names, e.g. '*.rs', 'Cargo.toml', 'test_*'"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (default: workspace root)"
                },
                "type": {
                    "type": "string",
                    "description": "Filter by type: 'file', 'dir', or 'any' (default: 'any')",
                    "enum": ["file", "dir", "any"]
                },
                "max_depth": {
                    "type": "integer",
                    "description": "Maximum directory depth to search (default: unlimited)"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let pattern = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("find: missing 'pattern' argument"))?;

        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".");

        let type_filter = args
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("any");

        let max_depth = args.get("max_depth").and_then(|v| v.as_u64());

        let search_path = if path == "." || path.is_empty() {
            PathBuf::from(&ctx.workspace_dir)
        } else if path.starts_with('/') {
            PathBuf::from(path)
        } else {
            PathBuf::from(&ctx.workspace_dir).join(path)
        };

        let (cmd, cmd_args) = if which_exists("fd") {
            build_fd_args(pattern, &search_path, type_filter, max_depth)
        } else {
            build_find_args(pattern, &search_path, type_filter, max_depth)
        };

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
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
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();

                if stdout.trim().is_empty() {
                    return Ok(ToolResult::success("No files found matching pattern."));
                }

                let lines: Vec<&str> = stdout.lines().collect();
                if lines.len() > MAX_RESULTS {
                    let truncated: String = lines[..MAX_RESULTS].join("\n");
                    Ok(ToolResult::success(format!(
                        "{}\n... ({} total, showing first {})",
                        truncated,
                        lines.len(),
                        MAX_RESULTS
                    )))
                } else {
                    Ok(ToolResult::success(format!("{} file(s) found:\n{}", lines.len(), stdout.trim())))
                }
            }
            Ok(Err(e)) => Ok(ToolResult::error(format!("find failed: {}", e))),
            Err(_) => Ok(ToolResult::error("find timed out after 10s")),
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

fn build_fd_args(
    pattern: &str,
    path: &PathBuf,
    type_filter: &str,
    max_depth: Option<u64>,
) -> (String, Vec<String>) {
    let mut args = vec![
        "--color=never".to_string(),
    ];

    match type_filter {
        "file" => args.push("--type=f".to_string()),
        "dir" => args.push("--type=d".to_string()),
        _ => {}
    }

    if let Some(depth) = max_depth {
        args.push(format!("--max-depth={}", depth));
    }

    args.push(pattern.to_string());
    args.push(path.to_string_lossy().to_string());

    ("fd".to_string(), args)
}

fn build_find_args(
    pattern: &str,
    path: &PathBuf,
    type_filter: &str,
    max_depth: Option<u64>,
) -> (String, Vec<String>) {
    let mut args = vec![
        path.to_string_lossy().to_string(),
    ];

    if let Some(depth) = max_depth {
        args.push("-maxdepth".to_string());
        args.push(depth.to_string());
    }

    match type_filter {
        "file" => {
            args.push("-type".to_string());
            args.push("f".to_string());
        }
        "dir" => {
            args.push("-type".to_string());
            args.push("d".to_string());
        }
        _ => {}
    }

    args.push("-name".to_string());
    args.push(pattern.to_string());

    ("find".to_string(), args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_find_in_tmp() {
        let tool = FindTool;
        let ctx = ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        };

        // Create test files
        tokio::fs::write("/tmp/openclaw-find-test.txt", "test").await.unwrap();

        let args = serde_json::json!({"pattern": "openclaw-find-test*", "path": "/tmp", "type": "file"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("openclaw-find-test"));

        tokio::fs::remove_file("/tmp/openclaw-find-test.txt").await.ok();
    }

    #[tokio::test]
    async fn test_find_no_match() {
        let tool = FindTool;
        let ctx = ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        };

        let args = serde_json::json!({"pattern": "zzz_nonexistent_file_*"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("No files found"));
    }

    #[test]
    fn test_fd_args() {
        let (cmd, args) = build_fd_args("*.rs", &PathBuf::from("/tmp"), "file", Some(3));
        assert_eq!(cmd, "fd");
        assert!(args.contains(&"--type=f".to_string()));
        assert!(args.contains(&"--max-depth=3".to_string()));
    }
}
