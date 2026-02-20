use serde_json::{json, Value};
use tracing::info;

use crate::protocol::{ToolCallResult, ToolDefinition};
use crate::tasks::TaskStatus;
use crate::McpContext;

/// Return all tool definitions
pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "openclaw_chat".to_string(),
            description: "Send a message to OpenClaw and get a response".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "The message to send to OpenClaw"
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Optional session ID for conversation context"
                    }
                },
                "required": ["message"]
            }),
        },
        ToolDefinition {
            name: "openclaw_status".to_string(),
            description: "Get OpenClaw gateway status and health information".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        },
        ToolDefinition {
            name: "openclaw_chat_async".to_string(),
            description: "Send a message to OpenClaw asynchronously. Returns a task_id immediately that can be polled for results.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "The message to send to OpenClaw"
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Optional session ID for conversation context"
                    },
                    "priority": {
                        "type": "number",
                        "description": "Task priority (higher = processed first). Default: 0"
                    }
                },
                "required": ["message"]
            }),
        },
        ToolDefinition {
            name: "openclaw_task_status".to_string(),
            description: "Check the status of an async task. Returns status, and result if completed.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "The task ID returned from openclaw_chat_async"
                    }
                },
                "required": ["task_id"]
            }),
        },
        ToolDefinition {
            name: "openclaw_task_list".to_string(),
            description: "List all tasks. Optionally filter by status or session.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["pending", "running", "completed", "failed", "cancelled"],
                        "description": "Filter by task status"
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Filter by session ID"
                    }
                }
            }),
        },
        ToolDefinition {
            name: "openclaw_task_cancel".to_string(),
            description: "Cancel a pending task. Only works for tasks that haven't started yet.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "The task ID to cancel"
                    }
                },
                "required": ["task_id"]
            }),
        },
    ]
}

/// Dispatch a tool call to the appropriate handler
pub async fn handle_tool_call(
    ctx: &McpContext,
    name: &str,
    args: Option<Value>,
) -> ToolCallResult {
    let args = args.unwrap_or(json!({}));
    info!("MCP tool call: {}", name);

    match name {
        "openclaw_chat" => handle_chat(ctx, &args).await,
        "openclaw_status" => handle_status(ctx).await,
        "openclaw_chat_async" => handle_chat_async(ctx, &args).await,
        "openclaw_task_status" => handle_task_status(ctx, &args).await,
        "openclaw_task_list" => handle_task_list(ctx, &args).await,
        "openclaw_task_cancel" => handle_task_cancel(ctx, &args).await,
        _ => ToolCallResult::error(format!("Unknown tool: {}", name)),
    }
}

// ── Validation helpers ──

const MAX_MESSAGE_LEN: usize = 100_000;
const MAX_ID_LEN: usize = 256;

fn validate_message(args: &Value) -> Result<String, String> {
    let msg = args.get("message")
        .and_then(|v| v.as_str())
        .ok_or("message must be a string")?;
    let trimmed = msg.trim();
    if trimmed.is_empty() {
        return Err("message must not be empty".to_string());
    }
    if trimmed.len() > MAX_MESSAGE_LEN {
        return Err(format!("message exceeds maximum length of {} characters", MAX_MESSAGE_LEN));
    }
    Ok(trimmed.to_string())
}

fn validate_id(args: &Value, field: &str) -> Result<String, String> {
    let id = args.get(field)
        .and_then(|v| v.as_str())
        .ok_or(format!("{} must be a string", field))?;
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return Err(format!("{} must not be empty", field));
    }
    if trimmed.len() > MAX_ID_LEN {
        return Err(format!("{} exceeds maximum length of {} characters", field, MAX_ID_LEN));
    }
    Ok(trimmed.to_string())
}

fn optional_session_id(args: &Value) -> Result<Option<String>, String> {
    match args.get("session_id") {
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else if trimmed.len() > MAX_ID_LEN {
                Err(format!("session_id exceeds maximum length of {} characters", MAX_ID_LEN))
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Some(Value::Null) | None => Ok(None),
        _ => Err("session_id must be a string".to_string()),
    }
}

// ── Tool handlers ──

async fn handle_chat(ctx: &McpContext, args: &Value) -> ToolCallResult {
    let message = match validate_message(args) {
        Ok(m) => m,
        Err(e) => return ToolCallResult::error(e),
    };
    let session_id = match optional_session_id(args) {
        Ok(s) => s,
        Err(e) => return ToolCallResult::error(e),
    };

    match run_agent_chat(ctx, &message, session_id.as_deref()).await {
        Ok(response) => ToolCallResult::success(response),
        Err(e) => ToolCallResult::error(format!("Failed to chat with OpenClaw: {}", e)),
    }
}

async fn handle_status(ctx: &McpContext) -> ToolCallResult {
    let status = json!({
        "status": "ok",
        "agent": ctx.agent_name,
        "version": env!("CARGO_PKG_VERSION"),
    });
    ToolCallResult::success(serde_json::to_string_pretty(&status).unwrap())
}

async fn handle_chat_async(ctx: &McpContext, args: &Value) -> ToolCallResult {
    let message = match validate_message(args) {
        Ok(m) => m,
        Err(e) => return ToolCallResult::error(e),
    };
    let session_id = match optional_session_id(args) {
        Ok(s) => s,
        Err(e) => return ToolCallResult::error(e),
    };
    let priority = args.get("priority")
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as i32;

    let task = match ctx.task_manager.create(message.clone(), session_id, priority).await {
        Ok(t) => t,
        Err(e) => return ToolCallResult::error(e.to_string()),
    };

    // Spawn background processing
    let task_id = task.id.clone();
    let ctx_clone = ctx.clone();
    tokio::spawn(async move {
        ctx_clone.task_manager.update_status(&task_id, TaskStatus::Running, None, None).await;
        match run_agent_chat(&ctx_clone, &task.input_message, task.input_session_id.as_deref()).await {
            Ok(response) => {
                ctx_clone.task_manager.update_status(&task_id, TaskStatus::Completed, Some(response), None).await;
            }
            Err(e) => {
                ctx_clone.task_manager.update_status(&task_id, TaskStatus::Failed, None, Some(e.to_string())).await;
            }
        }
    });

    let result = json!({
        "task_id": task.id,
        "status": "pending",
        "message": "Task queued. Use openclaw_task_status to check progress."
    });
    ToolCallResult::success(serde_json::to_string_pretty(&result).unwrap())
}

async fn handle_task_status(ctx: &McpContext, args: &Value) -> ToolCallResult {
    let task_id = match validate_id(args, "task_id") {
        Ok(id) => id,
        Err(e) => return ToolCallResult::error(e),
    };

    let task = match ctx.task_manager.get(&task_id).await {
        Some(t) => t,
        None => return ToolCallResult::error(format!("Task not found: {}", task_id)),
    };

    let mut response = json!({
        "task_id": task.id,
        "type": task.task_type,
        "status": task.status,
        "created_at": task.created_at.to_rfc3339(),
    });

    if let Some(started) = task.started_at {
        response["started_at"] = json!(started.to_rfc3339());
    }
    if let Some(completed) = task.completed_at {
        response["completed_at"] = json!(completed.to_rfc3339());
    }
    if task.status == TaskStatus::Completed {
        if let Some(ref result) = task.result {
            response["result"] = json!(result);
        }
    }
    if task.status == TaskStatus::Failed {
        if let Some(ref error) = task.error {
            response["error"] = json!(error);
        }
    }

    ToolCallResult::success(serde_json::to_string_pretty(&response).unwrap())
}

async fn handle_task_list(ctx: &McpContext, args: &Value) -> ToolCallResult {
    let status_filter = args.get("status")
        .and_then(|v| v.as_str())
        .and_then(|s| match s {
            "pending" => Some(TaskStatus::Pending),
            "running" => Some(TaskStatus::Running),
            "completed" => Some(TaskStatus::Completed),
            "failed" => Some(TaskStatus::Failed),
            "cancelled" => Some(TaskStatus::Cancelled),
            _ => None,
        });

    let session_filter = args.get("session_id").and_then(|v| v.as_str());

    let tasks = ctx.task_manager.list(status_filter, session_filter).await;
    let stats = ctx.task_manager.stats().await;

    let task_list: Vec<Value> = tasks.iter().map(|t| json!({
        "task_id": t.id,
        "type": t.task_type,
        "status": t.status,
        "priority": t.priority,
        "created_at": t.created_at.to_rfc3339(),
        "has_result": t.status == TaskStatus::Completed && t.result.is_some(),
    })).collect();

    let result = json!({
        "stats": stats,
        "tasks": task_list,
    });
    ToolCallResult::success(serde_json::to_string_pretty(&result).unwrap())
}

async fn handle_task_cancel(ctx: &McpContext, args: &Value) -> ToolCallResult {
    let task_id = match validate_id(args, "task_id") {
        Ok(id) => id,
        Err(e) => return ToolCallResult::error(e),
    };

    let task = match ctx.task_manager.get(&task_id).await {
        Some(t) => t,
        None => return ToolCallResult::error(format!("Task not found: {}", task_id)),
    };

    if task.status != TaskStatus::Pending {
        return ToolCallResult::error(format!(
            "Cannot cancel task with status: {:?}. Only pending tasks can be cancelled.",
            task.status
        ));
    }

    if !ctx.task_manager.cancel(&task_id).await {
        return ToolCallResult::error("Failed to cancel task");
    }

    let result = json!({
        "task_id": task_id,
        "status": "cancelled",
        "message": "Task cancelled successfully"
    });
    ToolCallResult::success(serde_json::to_string_pretty(&result).unwrap())
}

// ── Agent integration ──

async fn run_agent_chat(
    ctx: &McpContext,
    message: &str,
    session_id: Option<&str>,
) -> anyhow::Result<String> {
    use openclaw_agent::runtime::{self, AgentTurnConfig};

    let workspace_dir = openclaw_agent::workspace::resolve_workspace_dir(&ctx.agent_name);
    let session_key = session_id
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("mcp:{}:{}", ctx.agent_name, uuid::Uuid::new_v4()));

    let agent_config = AgentTurnConfig {
        agent_name: ctx.agent_name.clone(),
        session_key,
        workspace_dir: workspace_dir.to_string_lossy().to_string(),
        minimal_context: false,
        ..AgentTurnConfig::default()
    };

    let provider = openclaw_agent::llm::fallback::FallbackProvider::from_config()?;
    let tools = openclaw_agent::tools::ToolRegistry::with_defaults();
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();

    let turn_result = runtime::run_agent_turn_streaming(
        &provider,
        message,
        &agent_config,
        &tools,
        event_tx,
        vec![],
        None,
    ).await?;

    Ok(turn_result.response)
}
