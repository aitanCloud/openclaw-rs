use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::info;

use super::{Tool, ToolContext, ToolResult};

/// Unified persistent memory tool — searches BOTH the built-in key-value store
/// AND the MCP knowledge graph when recalling information.
///
/// - Built-in store: `.memory-{agent}.json` in workspace (key-value pairs)
/// - Knowledge graph: `~/.openclaw/memory/knowledge-graph.json` (entities, relations, observations)
///
/// On `get` or `list`, this tool automatically searches both sources and merges results.
/// On `set`, this tool writes to the built-in store (use mcp_memory_* for knowledge graph writes).
pub struct MemoryTool;

fn memory_path(workspace_dir: &str, agent_name: &str) -> PathBuf {
    PathBuf::from(workspace_dir).join(format!(".memory-{}.json", agent_name))
}

fn knowledge_graph_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".openclaw")
        .join("memory")
        .join("knowledge-graph.json")
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

/// Search the knowledge graph file for a query string.
/// Returns matching entities with their observations.
fn search_knowledge_graph(query: &str) -> String {
    let kg_path = knowledge_graph_path();
    let content = match std::fs::read_to_string(&kg_path) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };

    let query_lower = query.to_lowercase();
    let mut results = Vec::new();

    // Knowledge graph is JSONL (one JSON object per line)
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(entity) = serde_json::from_str::<Value>(line) {
            let name = entity.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let entity_type = entity.get("entityType").and_then(|v| v.as_str()).unwrap_or("");
            let observations: Vec<&str> = entity
                .get("observations")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();

            // Match against name, type, or any observation
            let matches = name.to_lowercase().contains(&query_lower)
                || entity_type.to_lowercase().contains(&query_lower)
                || observations
                    .iter()
                    .any(|o| o.to_lowercase().contains(&query_lower));

            if matches {
                let obs_str = observations
                    .iter()
                    .map(|o| format!("  - {}", o))
                    .collect::<Vec<_>>()
                    .join("\n");
                results.push(format!("[{}] {}\n{}", entity_type, name, obs_str));
            }
        }
    }

    results.join("\n\n")
}

/// List all entities in the knowledge graph (summary).
fn list_knowledge_graph() -> String {
    let kg_path = knowledge_graph_path();
    let content = match std::fs::read_to_string(&kg_path) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };

    let mut entities = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(entity) = serde_json::from_str::<Value>(line) {
            let name = entity.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let entity_type = entity.get("entityType").and_then(|v| v.as_str()).unwrap_or("?");
            let obs_count = entity
                .get("observations")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            entities.push(format!("[{}] {} ({} observations)", entity_type, name, obs_count));
        }
    }

    if entities.is_empty() {
        String::new()
    } else {
        format!(
            "Knowledge graph ({} entities):\n{}",
            entities.len(),
            entities.join("\n")
        )
    }
}

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &str {
        "memory"
    }

    fn description(&self) -> &str {
        "Unified persistent memory: searches BOTH the key-value store AND the knowledge graph. \
         Use 'set' to store a key-value pair, 'get' to search ALL memory sources for a key/query, \
         'list' to see everything stored, 'delete' to remove a key-value entry. \
         When recalling information, this tool automatically searches the knowledge graph too — \
         you do NOT need to call mcp_memory_search_nodes separately."
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
                    "description": "The memory key or search query (required for set/get/delete)"
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

                let mut parts = Vec::new();

                // 1. Search built-in key-value store
                let mem = load_memory(&path);
                if let Some(value) = mem.get(key) {
                    parts.push(format!("[Key-value store] {} = {}", key, value));
                } else {
                    // Also try fuzzy match on keys
                    let key_lower = key.to_lowercase();
                    let matches: Vec<_> = mem.iter()
                        .filter(|(k, v)| k.to_lowercase().contains(&key_lower) || v.to_lowercase().contains(&key_lower))
                        .collect();
                    if !matches.is_empty() {
                        for (k, v) in &matches {
                            parts.push(format!("[Key-value store] {} = {}", k, v));
                        }
                    }
                }

                // 2. Search knowledge graph
                let kg_results = search_knowledge_graph(key);
                if !kg_results.is_empty() {
                    parts.push(format!("[Knowledge graph]\n{}", kg_results));
                }

                if parts.is_empty() {
                    Ok(ToolResult::success(format!("No memory found for '{}' in any source.", key)))
                } else {
                    Ok(ToolResult::success(parts.join("\n\n")))
                }
            }
            "list" => {
                let mut parts = Vec::new();

                // 1. Built-in key-value store
                let mem = load_memory(&path);
                if !mem.is_empty() {
                    let kv_list: Vec<_> = mem.keys().map(|k| k.as_str()).collect();
                    parts.push(format!("Key-value store ({} entries): {}", kv_list.len(), kv_list.join(", ")));
                }

                // 2. Knowledge graph
                let kg_list = list_knowledge_graph();
                if !kg_list.is_empty() {
                    parts.push(kg_list);
                }

                if parts.is_empty() {
                    Ok(ToolResult::success("No memories stored in any source."))
                } else {
                    Ok(ToolResult::success(parts.join("\n\n")))
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
        assert!(tool.description().contains("Unified"));
        assert!(tool.description().contains("knowledge graph"));
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
        ..ToolContext::default()
        };
        let tool = MemoryTool;

        // Set
        let result = tool.execute(serde_json::json!({"action": "set", "key": "name", "value": "Alice"}), &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("Alice"));

        // Get
        let result = tool.execute(serde_json::json!({"action": "get", "key": "name"}), &ctx).await.unwrap();
        assert!(result.output.contains("Alice"));

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

    #[test]
    fn test_search_knowledge_graph_no_file() {
        // Should return empty string when file doesn't exist
        let result = search_knowledge_graph("test");
        // May or may not find results depending on whether the file exists on this machine
        // Just verify it doesn't panic
        let _ = result;
    }
}
