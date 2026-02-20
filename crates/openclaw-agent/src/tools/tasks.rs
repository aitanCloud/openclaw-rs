use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

use super::{Tool, ToolContext, ToolResult};

/// Information about a running/completed subagent task.
/// This is a gateway-agnostic representation that the agent crate can use.
#[derive(Debug, Clone)]
pub struct TaskInfo {
    pub id: u64,
    pub description: String,
    pub status: String,
    pub elapsed_secs: u64,
    pub chat_id: i64,
}

/// Callback type for querying tasks from the gateway's subagent registry.
/// The gateway sets this on ToolContext; the tasks tool calls it.
pub type TaskQueryFn = Arc<dyn Fn(i64) -> Vec<TaskInfo> + Send + Sync>;

/// Tool that lets the LLM query and manage its own running subagent tasks.
pub struct TasksTool;

#[async_trait]
impl Tool for TasksTool {
    fn name(&self) -> &str {
        "tasks"
    }

    fn description(&self) -> &str {
        "Query running and recent background subagent tasks. \
         Use this to check task status, see results, or verify that a delegate call succeeded. \
         Actions: list (show all tasks), status (check specific task by ID), cancel (cancel a task by ID)."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "Action to perform: list, status, cancel",
                    "enum": ["list", "status", "cancel"]
                },
                "task_id": {
                    "type": "integer",
                    "description": "Task ID (required for status and cancel actions)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("list");

        let task_query = match &ctx.task_query_fn {
            Some(f) => f,
            None => {
                return Ok(ToolResult::success(
                    "Task query not available (running outside gateway context). \
                     Use /tasks command instead."
                ));
            }
        };

        let task_cancel = &ctx.task_cancel_fn;

        match action {
            "list" => {
                let tasks = task_query(ctx.chat_id);
                if tasks.is_empty() {
                    return Ok(ToolResult::success("No background tasks found."));
                }

                let mut output = format!("{} task(s):\n", tasks.len());
                for t in &tasks {
                    output.push_str(&format!(
                        "\n#{} [{}] — {} ({}s ago)",
                        t.id, t.status, t.description, t.elapsed_secs
                    ));
                }
                Ok(ToolResult::success(output))
            }

            "status" => {
                let task_id = args
                    .get("task_id")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'task_id' for status action"))?;

                let tasks = task_query(ctx.chat_id);
                match tasks.iter().find(|t| t.id == task_id) {
                    Some(t) => Ok(ToolResult::success(format!(
                        "Task #{}: [{}] — {} ({}s elapsed)",
                        t.id, t.status, t.description, t.elapsed_secs
                    ))),
                    None => Ok(ToolResult::error(format!("Task #{} not found", task_id))),
                }
            }

            "cancel" => {
                let task_id = args
                    .get("task_id")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'task_id' for cancel action"))?;

                match task_cancel {
                    Some(cancel_fn) => {
                        if cancel_fn(task_id) {
                            Ok(ToolResult::success(format!("Task #{} cancelled.", task_id)))
                        } else {
                            Ok(ToolResult::error(format!(
                                "Task #{} not found or already finished.",
                                task_id
                            )))
                        }
                    }
                    None => Ok(ToolResult::error(
                        "Task cancellation not available outside gateway context.",
                    )),
                }
            }

            other => Ok(ToolResult::error(format!(
                "Unknown action '{}'. Use: list, status, cancel",
                other
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tasks_tool_definition() {
        let tool = TasksTool;
        assert_eq!(tool.name(), "tasks");
        assert!(tool.description().contains("subagent"));
        let params = tool.parameters();
        assert!(params["properties"]["action"].is_object());
        assert!(params["properties"]["task_id"].is_object());
        let required = params["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "action");
    }

    #[tokio::test]
    async fn test_tasks_list_no_gateway() {
        let tool = TasksTool;
        let ctx = ToolContext::default();
        let result = tool
            .execute(serde_json::json!({"action": "list"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("not available"));
    }

    #[tokio::test]
    async fn test_tasks_list_empty() {
        let tool = TasksTool;
        let mut ctx = ToolContext::default();
        ctx.task_query_fn = Some(Arc::new(|_chat_id| Vec::new()));
        let result = tool
            .execute(serde_json::json!({"action": "list"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("No background tasks"));
    }

    #[tokio::test]
    async fn test_tasks_list_with_tasks() {
        let tool = TasksTool;
        let mut ctx = ToolContext::default();
        ctx.task_query_fn = Some(Arc::new(|_chat_id| {
            vec![TaskInfo {
                id: 1,
                description: "Weather research".to_string(),
                status: "running".to_string(),
                elapsed_secs: 15,
                chat_id: 123,
            }]
        }));
        let result = tool
            .execute(serde_json::json!({"action": "list"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("Weather research"));
        assert!(result.output.contains("running"));
    }

    #[tokio::test]
    async fn test_tasks_cancel() {
        let tool = TasksTool;
        let mut ctx = ToolContext::default();
        ctx.task_query_fn = Some(Arc::new(|_| Vec::new()));
        ctx.task_cancel_fn = Some(Arc::new(|id| id == 1));
        let result = tool
            .execute(serde_json::json!({"action": "cancel", "task_id": 1}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("cancelled"));
    }
}
