use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;

use super::{Tool, ToolContext, ToolResult};

const MAX_ENTRIES: usize = 500;

pub struct ListDirTool;

#[async_trait]
impl Tool for ListDirTool {
    fn name(&self) -> &str {
        "list_dir"
    }

    fn description(&self) -> &str {
        "List files and directories in a given path. Returns name, type (file/dir), and size for each entry. Defaults to the workspace directory."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory path to list (default: workspace root)"
                },
                "recursive": {
                    "type": "boolean",
                    "description": "If true, list recursively up to 3 levels deep (default: false)"
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".");

        let recursive = args
            .get("recursive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let target = if path_str == "." || path_str.is_empty() {
            PathBuf::from(&ctx.workspace_dir)
        } else if path_str.starts_with('/') {
            PathBuf::from(path_str)
        } else {
            PathBuf::from(&ctx.workspace_dir).join(path_str)
        };

        if !target.exists() {
            return Ok(ToolResult::error(format!("Directory not found: {}", path_str)));
        }

        if !target.is_dir() {
            return Ok(ToolResult::error(format!("Not a directory: {}", path_str)));
        }

        let max_depth = if recursive { 3 } else { 1 };
        let mut entries = Vec::new();
        list_entries(&target, &target, 0, max_depth, &mut entries);

        if entries.is_empty() {
            return Ok(ToolResult::success("(empty directory)"));
        }

        let truncated = entries.len() > MAX_ENTRIES;
        if truncated {
            entries.truncate(MAX_ENTRIES);
        }

        let mut output = entries.join("\n");
        if truncated {
            output.push_str(&format!("\n... (truncated at {} entries)", MAX_ENTRIES));
        }

        Ok(ToolResult::success(output))
    }
}

fn list_entries(
    base: &std::path::Path,
    dir: &std::path::Path,
    depth: usize,
    max_depth: usize,
    entries: &mut Vec<String>,
) {
    if depth >= max_depth || entries.len() >= MAX_ENTRIES {
        return;
    }

    let mut items: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(_) => return,
    };

    items.sort_by_key(|e| e.file_name());

    for entry in items {
        if entries.len() >= MAX_ENTRIES {
            break;
        }

        let path = entry.path();
        let rel = path.strip_prefix(base).unwrap_or(&path);
        let name = rel.display().to_string();
        let indent = "  ".repeat(depth);

        if path.is_dir() {
            entries.push(format!("{}[DIR]  {}/", indent, name));
            list_entries(base, &path, depth + 1, max_depth, entries);
        } else {
            let size = path.metadata().map(|m| m.len()).unwrap_or(0);
            let size_str = format_size(size);
            entries.push(format!("{}[FILE] {} ({})", indent, name, size_str));
        }
    }
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_list_dir_tmp() {
        let tool = ListDirTool;
        let ctx = ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        };
        let args = serde_json::json!({});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn test_list_dir_not_found() {
        let tool = ListDirTool;
        let ctx = ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        };
        let args = serde_json::json!({"path": "/nonexistent_dir_xyz"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
    }
}
