use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;

use super::{Tool, ToolContext, ToolResult};

pub struct WriteTool;

fn resolve_safe_write_path(workspace: &str, file_path: &str) -> Result<PathBuf> {
    let workspace = PathBuf::from(workspace).canonicalize()?;
    let target = if file_path.starts_with('/') {
        PathBuf::from(file_path)
    } else {
        workspace.join(file_path)
    };

    // For writes, the parent must exist and be within allowed dirs
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Invalid path: {}", file_path))?;

    // Create parent dirs if needed (within workspace)
    let parent_canonical = if parent.exists() {
        parent.canonicalize()?
    } else {
        // Check if the parent's parent is in workspace
        let mut check = parent.to_path_buf();
        while !check.exists() {
            check = check
                .parent()
                .ok_or_else(|| anyhow::anyhow!("Cannot resolve path: {}", file_path))?
                .to_path_buf();
        }
        check.canonicalize()?
    };

    let home = dirs::home_dir().unwrap_or_default();
    let allowed_roots = [workspace.clone(), home, PathBuf::from("/tmp")];

    if allowed_roots
        .iter()
        .any(|root| parent_canonical.starts_with(root))
    {
        Ok(target)
    } else {
        anyhow::bail!(
            "Write denied: {} is outside allowed directories",
            file_path
        )
    }
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Creates parent directories as needed."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to write to (relative to workspace or absolute)"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                },
                "append": {
                    "type": "boolean",
                    "description": "If true, append to file instead of overwriting"
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let file_path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("write: missing 'path' argument"))?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("write: missing 'content' argument"))?;

        let append = args
            .get("append")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let safe_path = match resolve_safe_write_path(&ctx.workspace_dir, file_path) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(format!("{}", e))),
        };

        // Create parent directories
        if let Some(parent) = safe_path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return Ok(ToolResult::error(format!(
                    "Failed to create directories: {}",
                    e
                )));
            }
        }

        let result = if append {
            use tokio::io::AsyncWriteExt;
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&safe_path)
                .await;
            match file {
                Ok(ref mut f) => f.write_all(content.as_bytes()).await,
                Err(e) => Err(e),
            }
        } else {
            tokio::fs::write(&safe_path, content).await
        };

        match result {
            Ok(()) => {
                let bytes = content.len();
                let action = if append { "appended to" } else { "wrote" };
                Ok(ToolResult::success(format!(
                    "{} {} ({} bytes)",
                    action, file_path, bytes
                )))
            }
            Err(e) => Ok(ToolResult::error(format!(
                "Failed to write {}: {}",
                file_path, e
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_write_file() {
        let tool = WriteTool;
        let ctx = ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test-session".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        };

        let args = serde_json::json!({
            "path": "/tmp/openclaw-test-write.txt",
            "content": "hello world"
        });
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("wrote"));

        let content = tokio::fs::read_to_string("/tmp/openclaw-test-write.txt")
            .await
            .unwrap();
        assert_eq!(content, "hello world");

        tokio::fs::remove_file("/tmp/openclaw-test-write.txt")
            .await
            .ok();
    }

    #[tokio::test]
    async fn test_write_append() {
        let tool = WriteTool;
        let ctx = ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test-session".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        };

        tokio::fs::write("/tmp/openclaw-test-append.txt", "first\n")
            .await
            .unwrap();

        let args = serde_json::json!({
            "path": "/tmp/openclaw-test-append.txt",
            "content": "second\n",
            "append": true
        });
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(!result.is_error);

        let content = tokio::fs::read_to_string("/tmp/openclaw-test-append.txt")
            .await
            .unwrap();
        assert_eq!(content, "first\nsecond\n");

        tokio::fs::remove_file("/tmp/openclaw-test-append.txt")
            .await
            .ok();
    }
}
