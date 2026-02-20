use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use tracing::info;

use super::{DelegateRequest, Tool, ToolContext, ToolResult};

/// Tool that allows the agent to delegate a subtask to a subagent.
/// If a delegate_tx channel is available (gateway context), the task runs in the
/// background and the tool returns immediately. Otherwise, falls back to blocking.
pub struct DelegateTool;

#[async_trait]
impl Tool for DelegateTool {
    fn name(&self) -> &str {
        "delegate"
    }

    fn description(&self) -> &str {
        "Delegate a subtask to a background subagent. Returns immediately with a task ID. \
         The subagent runs asynchronously and its result will appear in chat automatically when done. \
         IMPORTANT: After delegating, do NOT poll the tasks tool for status — just tell the user the task \
         was dispatched and move on. The result will be delivered to the chat when ready. \
         Use this for tasks that benefit from focused, isolated reasoning."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "The task description / prompt for the subagent"
                },
                "context": {
                    "type": "string",
                    "description": "Optional additional context to include (e.g. file contents, data)"
                }
            },
            "required": ["task"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let task = args
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'task' parameter"))?;

        let context = args
            .get("context")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        info!(
            "Subagent delegated: task={} ({}B context), agent={}, session={}",
            &task[..task.len().min(80)],
            context.len(),
            ctx.agent_name,
            ctx.session_key,
        );

        // If we have a delegate channel (gateway context), dispatch async
        if let Some(ref tx) = ctx.delegate_tx {
            let req = DelegateRequest {
                task: task.to_string(),
                context: context.to_string(),
                agent_name: ctx.agent_name.clone(),
                workspace_dir: ctx.workspace_dir.clone(),
                chat_id: ctx.chat_id,
            };
            if tx.send(req).is_ok() {
                return Ok(ToolResult::success(format!(
                    "Task dispatched to background. The result will appear in chat automatically when done. \
                     Do NOT call the tasks tool to poll status — just inform the user and move on. Task: {}",
                    &task[..task.len().min(120)]
                )));
            }
        }

        // Fallback: blocking execution (CLI or no channel)
        let prompt = if context.is_empty() {
            task.to_string()
        } else {
            format!("{}\n\n---\nContext:\n{}", task, context)
        };

        let result = crate::subagent::run_subagent_turn(
            &prompt,
            &ctx.agent_name,
            &ctx.workspace_dir,
            None,
            None, // no event forwarding for blocking delegate calls
        )
        .await;

        match result {
            Ok(response) => {
                info!("Subagent completed: {}B response", response.len());
                Ok(ToolResult::success(response))
            }
            Err(e) => {
                Ok(ToolResult::error(format!("Subagent failed: {}", e)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_delegate_tool_definition() {
        let tool = DelegateTool;
        assert_eq!(tool.name(), "delegate");
        assert!(tool.description().contains("subtask"));
        let params = tool.parameters();
        assert!(params["properties"]["task"].is_object());
        assert!(params["properties"]["context"].is_object());
        let required = params["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "task");
    }
}
