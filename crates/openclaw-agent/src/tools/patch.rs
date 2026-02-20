use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;

use super::{Tool, ToolContext, ToolResult};

pub struct PatchTool;

fn resolve_safe_write_path(workspace: &str, file_path: &str) -> Result<PathBuf> {
    let workspace = PathBuf::from(workspace).canonicalize()?;
    let target = if file_path.starts_with('/') {
        PathBuf::from(file_path)
    } else {
        workspace.join(file_path)
    };

    // For patch, file must already exist
    let canonical = target
        .canonicalize()
        .map_err(|_| anyhow::anyhow!("File not found: {}", file_path))?;

    let home = dirs::home_dir().unwrap_or_default();
    let allowed_roots = [workspace, home, PathBuf::from("/tmp")];

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
impl Tool for PatchTool {
    fn name(&self) -> &str {
        "patch"
    }

    fn description(&self) -> &str {
        "Apply a surgical edit to a file by replacing an exact string match with new content. More precise than rewriting the entire file. The old_string must match exactly (including whitespace)."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit"
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact string to find and replace (must be unique in the file)"
                },
                "new_string": {
                    "type": "string",
                    "description": "The replacement string"
                }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let file_path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("patch: missing 'path' argument"))?;

        let old_string = args
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("patch: missing 'old_string' argument"))?;

        let new_string = args
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("patch: missing 'new_string' argument"))?;

        if old_string == new_string {
            return Ok(ToolResult::error("old_string and new_string are identical — no change needed"));
        }

        let safe_path = match resolve_safe_write_path(&ctx.workspace_dir, file_path) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(format!("{}", e))),
        };

        let content = match tokio::fs::read_to_string(&safe_path).await {
            Ok(c) => c,
            Err(e) => return Ok(ToolResult::error(format!("Failed to read {}: {}", file_path, e))),
        };

        let match_count = content.matches(old_string).count();

        if match_count == 0 {
            return Ok(ToolResult::error(format!(
                "old_string not found in {}. Make sure it matches exactly (including whitespace and newlines).",
                file_path
            )));
        }

        if match_count > 1 {
            return Ok(ToolResult::error(format!(
                "old_string found {} times in {}. It must be unique — provide more surrounding context to disambiguate.",
                match_count, file_path
            )));
        }

        let new_content = content.replacen(old_string, new_string, 1);

        match tokio::fs::write(&safe_path, &new_content).await {
            Ok(()) => {
                let old_lines = old_string.lines().count();
                let new_lines = new_string.lines().count();
                Ok(ToolResult::success(format!(
                    "Patched {} — replaced {} line(s) with {} line(s)",
                    file_path, old_lines, new_lines
                )))
            }
            Err(e) => Ok(ToolResult::error(format!("Failed to write {}: {}", file_path, e))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_patch_file() {
        let tool = PatchTool;
        let ctx = ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        ..ToolContext::default()
        };

        tokio::fs::write("/tmp/openclaw-test-patch.txt", "hello world\nfoo bar\n")
            .await
            .unwrap();

        let args = serde_json::json!({
            "path": "/tmp/openclaw-test-patch.txt",
            "old_string": "foo bar",
            "new_string": "baz qux"
        });
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("Patched"));

        let content = tokio::fs::read_to_string("/tmp/openclaw-test-patch.txt")
            .await
            .unwrap();
        assert!(content.contains("baz qux"));
        assert!(!content.contains("foo bar"));

        tokio::fs::remove_file("/tmp/openclaw-test-patch.txt").await.ok();
    }

    #[tokio::test]
    async fn test_patch_not_found() {
        let tool = PatchTool;
        let ctx = ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        ..ToolContext::default()
        };

        tokio::fs::write("/tmp/openclaw-test-patch2.txt", "hello world\n")
            .await
            .unwrap();

        let args = serde_json::json!({
            "path": "/tmp/openclaw-test-patch2.txt",
            "old_string": "nonexistent",
            "new_string": "replacement"
        });
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("not found"));

        tokio::fs::remove_file("/tmp/openclaw-test-patch2.txt").await.ok();
    }

    #[tokio::test]
    async fn test_patch_ambiguous() {
        let tool = PatchTool;
        let ctx = ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        ..ToolContext::default()
        };

        tokio::fs::write("/tmp/openclaw-test-patch3.txt", "aaa\naaa\naaa\n")
            .await
            .unwrap();

        let args = serde_json::json!({
            "path": "/tmp/openclaw-test-patch3.txt",
            "old_string": "aaa",
            "new_string": "bbb"
        });
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("3 times"));

        tokio::fs::remove_file("/tmp/openclaw-test-patch3.txt").await.ok();
    }
}
