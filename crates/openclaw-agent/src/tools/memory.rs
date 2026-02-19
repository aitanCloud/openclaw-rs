use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::info;

use super::{Tool, ToolContext, ToolResult};

/// Persistent memory tool — stores and retrieves key-value notes per agent.
/// Memory is persisted as a JSON file in the agent's workspace directory.
pub struct MemoryTool;

fn memory_path(workspace_dir: &str, agent_name: &str) -> PathBuf {
    PathBuf::from(workspace_dir).join(format!(".memory-{}.json", agent_name))
}

fn load_memory(path: &PathBuf) -> HashMap<String, String> {
    if let Ok(content) = std::fs::read_to_string(path) {
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        HashMap::new()
    }
}

fn save_memory(path: &PathBuf, memory: &HashMap<String, String>) -> Result<()> {
    let content = serde_json::to_string_pretty(memory)?;
    std::fs::write(path, content)?;
    Ok(())
}

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &str {
        "memory"
    }

    fn description(&self) -> &str {
        "Persistent memory: store, retrieve, list, or delete key-value notes that persist across sessions. \
         Use 'set' to remember something, 'get' to recall, 'list' to see all keys, 'delete' to forget."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["set", "get", "list", "delete"],
                    "description": "The memory operation to perform"
                },
                "key": {
                    "type": "string",
                    "description": "The memory key (required for set/get/delete)"
                },
                "value": {
                    "type": "string",
                    "description": "The value to store (required for set)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' parameter"))?;

        let path = memory_path(&ctx.workspace_dir, &ctx.agent_name);

        match action {
            "set" => {
                let key = args.get("key").and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'key' for set"))?;
                let value = args.get("value").and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'value' for set"))?;

                let mut mem = load_memory(&path);
                mem.insert(key.to_string(), value.to_string());
                save_memory(&path, &mem)?;

                info!("Memory set: key={}, agent={}", key, ctx.agent_name);
                Ok(ToolResult::success(format!("Stored '{}' = '{}'", key, &value[..value.len().min(100)])))
            }
            "get" => {
                let key = args.get("key").and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'key' for get"))?;

                let mem = load_memory(&path);
                match mem.get(key) {
                    Some(value) => Ok(ToolResult::success(value.clone())),
                    None => Ok(ToolResult::success(format!("No memory found for key '{}'", key))),
                }
            }
            "list" => {
                let mem = load_memory(&path);
                if mem.is_empty() {
                    Ok(ToolResult::success("No memories stored."))
                } else {
                    let keys: Vec<_> = mem.keys().map(|k| k.as_str()).collect();
                    Ok(ToolResult::success(format!("{} memories: {}", keys.len(), keys.join(", "))))
                }
            }
            "delete" => {
                let key = args.get("key").and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'key' for delete"))?;

                let mut mem = load_memory(&path);
                if mem.remove(key).is_some() {
                    save_memory(&path, &mem)?;
                    info!("Memory deleted: key={}, agent={}", key, ctx.agent_name);
                    Ok(ToolResult::success(format!("Deleted memory '{}'", key)))
                } else {
                    Ok(ToolResult::success(format!("No memory found for key '{}'", key)))
                }
            }
            _ => Ok(ToolResult::error(format!("Unknown action '{}'. Use set/get/list/delete.", action))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::SandboxPolicy;

    #[test]
    fn test_memory_tool_definition() {
        let tool = MemoryTool;
        assert_eq!(tool.name(), "memory");
        assert!(tool.description().contains("Persistent"));
        let params = tool.parameters();
        assert!(params["properties"]["action"].is_object());
        assert!(params["properties"]["key"].is_object());
        assert!(params["properties"]["value"].is_object());
    }

    #[tokio::test]
    async fn test_memory_set_get_list_delete() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext {
            workspace_dir: dir.path().to_str().unwrap().to_string(),
            agent_name: "test-mem".to_string(),
            session_key: "s1".to_string(),
            sandbox: SandboxPolicy::default(),
        };
        let tool = MemoryTool;

        // Set
        let result = tool.execute(serde_json::json!({"action": "set", "key": "name", "value": "Alice"}), &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("Alice"));

        // Get
        let result = tool.execute(serde_json::json!({"action": "get", "key": "name"}), &ctx).await.unwrap();
        assert_eq!(result.output, "Alice");

        // List
        let result = tool.execute(serde_json::json!({"action": "list"}), &ctx).await.unwrap();
        assert!(result.output.contains("name"));

        // Delete
        let result = tool.execute(serde_json::json!({"action": "delete", "key": "name"}), &ctx).await.unwrap();
        assert!(result.output.contains("Deleted"));

        // Get after delete
        let result = tool.execute(serde_json::json!({"action": "get", "key": "name"}), &ctx).await.unwrap();
        assert!(result.output.contains("No memory found"));
    }

    #[test]
    fn test_load_memory_missing_file() {
        let path = PathBuf::from("/tmp/nonexistent-memory-test-12345.json");
        let mem = load_memory(&path);
        assert!(mem.is_empty());
    }
}
