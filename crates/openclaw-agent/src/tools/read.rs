use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;

use super::{Tool, ToolContext, ToolResult};

const MAX_FILE_BYTES: usize = 128 * 1024;

pub struct ReadTool;

fn resolve_safe_path(workspace: &str, file_path: &str) -> Result<PathBuf> {
    let workspace = PathBuf::from(workspace).canonicalize()?;
    let target = if file_path.starts_with('/') {
        PathBuf::from(file_path)
    } else {
        workspace.join(file_path)
    };

    let canonical = target
        .canonicalize()
        .map_err(|_| anyhow::anyhow!("File not found: {}", file_path))?;

    // Allow reading from workspace, home dir, and /tmp
    let home = dirs::home_dir().unwrap_or_default();
    let allowed_roots = [
        workspace.clone(),
        home.clone(),
        PathBuf::from("/tmp"),
    ];

    if allowed_roots.iter().any(|root| canonical.starts_with(root)) {
        Ok(canonical)
    } else {
        anyhow::bail!(
            "Path traversal denied: {} is outside allowed directories",
            file_path
        )
    }
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read the contents of a file. Returns the file content as text. Paths are relative to the workspace directory unless absolute."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read (relative to workspace or absolute)"
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start reading from (1-indexed)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let file_path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("read: missing 'path' argument"))?;

        let safe_path = match resolve_safe_path(&ctx.workspace_dir, file_path) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(format!("{}", e))),
        };

        let content = match tokio::fs::read_to_string(&safe_path).await {
            Ok(c) => c,
            Err(e) => return Ok(ToolResult::error(format!("Failed to read {}: {}", file_path, e))),
        };

        if content.len() > MAX_FILE_BYTES {
            return Ok(ToolResult::error(format!(
                "File too large ({} bytes, max {}). Use offset/limit to read a portion.",
                content.len(),
                MAX_FILE_BYTES
            )));
        }

        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
        let limit = args.get("limit").and_then(|v| v.as_u64());

        let lines: Vec<&str> = content.lines().collect();
        let start = if offset > 0 { offset - 1 } else { 0 };
        let end = match limit {
            Some(l) => std::cmp::min(start + l as usize, lines.len()),
            None => lines.len(),
        };

        if start >= lines.len() {
            return Ok(ToolResult::success(format!(
                "(file has {} lines, offset {} is past end)",
                lines.len(),
                offset
            )));
        }

        let selected: Vec<String> = lines[start..end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>4}\t{}", start + i + 1, line))
            .collect();

        Ok(ToolResult::success(selected.join("\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_read_file() {
        let tool = ReadTool;
        let ctx = ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test-session".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        };

        // Create a test file
        tokio::fs::write("/tmp/openclaw-test-read.txt", "line1\nline2\nline3\n")
            .await
            .unwrap();

        let args = serde_json::json!({"path": "/tmp/openclaw-test-read.txt"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("line1"));
        assert!(result.output.contains("line3"));

        tokio::fs::remove_file("/tmp/openclaw-test-read.txt")
            .await
            .ok();
    }

    #[tokio::test]
    async fn test_read_with_offset_limit() {
        let tool = ReadTool;
        let ctx = ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test-session".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        };

        tokio::fs::write("/tmp/openclaw-test-read2.txt", "a\nb\nc\nd\ne\n")
            .await
            .unwrap();

        let args = serde_json::json!({"path": "/tmp/openclaw-test-read2.txt", "offset": 2, "limit": 2});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("b"));
        assert!(result.output.contains("c"));
        assert!(!result.output.contains("a"));
        assert!(!result.output.contains("d"));

        tokio::fs::remove_file("/tmp/openclaw-test-read2.txt")
            .await
            .ok();
    }
}
