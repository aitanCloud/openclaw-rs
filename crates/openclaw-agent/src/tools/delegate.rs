use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use tracing::info;

use super::{Tool, ToolContext, ToolResult};

/// Tool that allows the agent to delegate a subtask to a subagent.
/// The subagent runs a single turn with the given prompt and returns the result.
pub struct DelegateTool;

#[async_trait]
impl Tool for DelegateTool {
    fn name(&self) -> &str {
        "delegate"
    }

    fn description(&self) -> &str {
        "Delegate a subtask to a subagent. The subagent runs one turn with the given prompt \
         and returns its response. Use this for complex tasks that benefit from focused, \
         isolated reasoning — e.g. code review, summarization, research."
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

        let prompt = if context.is_empty() {
            task.to_string()
        } else {
            format!("{}\n\n---\nContext:\n{}", task, context)
        };

        info!(
            "Subagent delegated: task={} ({}B context), agent={}, session={}",
            &task[..task.len().min(80)],
            context.len(),
            ctx.agent_name,
            ctx.session_key,
        );

        // Use the subagent registry to run a subagent turn
        let result = crate::subagent::run_subagent_turn(
            &prompt,
            &ctx.agent_name,
            &ctx.workspace_dir,
            None, // cancellation token not available through tool interface
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
