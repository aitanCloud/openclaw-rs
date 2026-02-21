use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use super::{Tool, ToolContext, ToolResult};

pub struct SessionsTool;

#[async_trait]
impl Tool for SessionsTool {
    fn name(&self) -> &str {
        "sessions"
    }

    fn description(&self) -> &str {
        "Manage conversation sessions. Actions: list (show recent sessions with stats), history (view messages from a session), send (inject a message into a session)."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "history", "send"],
                    "description": "Action to perform"
                },
                "session_key": {
                    "type": "string",
                    "description": "Session key (for history/send). Use partial match — first matching session is used."
                },
                "message": {
                    "type": "string",
                    "description": "Message text to inject (for 'send' action)"
                },
                "role": {
                    "type": "string",
                    "enum": ["user", "assistant", "system"],
                    "description": "Role for the injected message (for 'send' action, default: 'user')"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max items to return (for 'list': max sessions, default 10; for 'history': max messages, default 50)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("sessions: missing 'action' argument"))?;

        match action {
            "list" => list_sessions(&args, ctx).await,
            "history" => session_history(&args, ctx).await,
            "send" => send_message(&args, ctx).await,
            _ => Ok(ToolResult::error(format!(
                "Unknown action '{}'. Use: list, history, send",
                action
            ))),
        }
    }
}

fn get_pool() -> Result<&'static openclaw_db::PgPool> {
    openclaw_db::pool().ok_or_else(|| anyhow::anyhow!("Database not available"))
}

async fn list_sessions(args: &Value, ctx: &ToolContext) -> Result<ToolResult> {
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(10) as i64;

    let pool = get_pool()?;
    let sessions = openclaw_db::sessions::list_sessions(pool, &ctx.agent_name, limit).await?;

    if sessions.is_empty() {
        return Ok(ToolResult::success("No sessions found."));
    }

    let mut lines = Vec::new();
    lines.push(format!("Found {} session(s):", sessions.len()));
    lines.push(String::new());

    for (i, s) in sessions.iter().enumerate() {
        let age = format_age(s.updated_at_ms);
        let is_current = s.session_key == ctx.session_key;
        let marker = if is_current { " ← current" } else { "" };
        lines.push(format!(
            "{}. {} — {} msgs, {} tokens, model: {}, updated: {}{}",
            i + 1,
            s.session_key,
            s.message_count,
            s.total_tokens,
            s.model,
            age,
            marker,
        ));
    }

    Ok(ToolResult::success(lines.join("\n")))
}

async fn session_history(args: &Value, ctx: &ToolContext) -> Result<ToolResult> {
    let session_key_arg = args.get("session_key").and_then(|v| v.as_str());
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(50) as usize;

    let pool = get_pool()?;

    // Resolve session key: use provided key (with partial match) or current session
    let session_key = match session_key_arg {
        Some(partial) => resolve_session_key(pool, &ctx.agent_name, partial).await?,
        None => ctx.session_key.clone(),
    };

    let messages = openclaw_db::sessions::load_messages(pool, &session_key).await?;

    if messages.is_empty() {
        return Ok(ToolResult::success(format!(
            "Session '{}' has no messages.",
            session_key
        )));
    }

    let total = messages.len();
    let start = total.saturating_sub(limit);
    let shown = &messages[start..];

    let mut lines = Vec::new();
    lines.push(format!(
        "Session: {} — showing {}/{} messages",
        session_key,
        shown.len(),
        total
    ));
    lines.push(String::new());

    for (i, msg) in shown.iter().enumerate() {
        let role_icon = match msg.role.as_str() {
            "user" => "👤",
            "assistant" => "🤖",
            "system" => "⚙️",
            "tool" => "🔧",
            _ => "❓",
        };

        let content = msg.content.as_deref().unwrap_or("(empty)");
        // Truncate long messages for readability
        let display = if content.len() > 500 {
            format!("{}... [{} chars total]", &content[..500], content.len())
        } else {
            content.to_string()
        };

        let ts = format_timestamp(msg.timestamp_ms);
        lines.push(format!(
            "[{}] {} {} {}: {}",
            start + i + 1,
            ts,
            role_icon,
            msg.role,
            display
        ));
    }

    Ok(ToolResult::success(lines.join("\n")))
}

async fn send_message(args: &Value, ctx: &ToolContext) -> Result<ToolResult> {
    let session_key_arg = args.get("session_key").and_then(|v| v.as_str());
    let message = match args.get("message").and_then(|v| v.as_str()) {
        Some(m) => m,
        None => return Ok(ToolResult::error("sessions send: missing 'message'")),
    };
    let role_str = args
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("user");

    let pool = get_pool()?;

    let session_key = match session_key_arg {
        Some(partial) => resolve_session_key(pool, &ctx.agent_name, partial).await?,
        None => ctx.session_key.clone(),
    };

    let role = match role_str {
        "user" | "assistant" | "system" => role_str,
        _ => return Ok(ToolResult::error(format!(
            "Invalid role '{}'. Use: user, assistant, system",
            role_str
        ))),
    };

    openclaw_db::messages::append_message(
        pool, &session_key, &ctx.agent_name, "", role,
        Some(message), None, None, None,
    ).await?;

    Ok(ToolResult::success(format!(
        "Injected {} message into session '{}': \"{}\"",
        role_str,
        session_key,
        if message.len() > 100 {
            format!("{}...", &message[..100])
        } else {
            message.to_string()
        }
    )))
}

/// Resolve a partial session key to a full key by searching existing sessions
async fn resolve_session_key(
    pool: &openclaw_db::PgPool,
    agent_name: &str,
    partial: &str,
) -> Result<String> {
    let sessions = openclaw_db::sessions::list_sessions(pool, agent_name, 100).await?;

    // Exact match first
    if let Some(s) = sessions.iter().find(|s| s.session_key == partial) {
        return Ok(s.session_key.clone());
    }

    // Partial match (contains)
    let partial_lower = partial.to_lowercase();
    let matches: Vec<_> = sessions
        .iter()
        .filter(|s| s.session_key.to_lowercase().contains(&partial_lower))
        .collect();

    match matches.len() {
        0 => anyhow::bail!("No session matching '{}' found", partial),
        1 => Ok(matches[0].session_key.clone()),
        n => {
            let keys: Vec<_> = matches.iter().map(|s| s.session_key.as_str()).collect();
            anyhow::bail!(
                "Ambiguous: '{}' matches {} sessions: {}",
                partial,
                n,
                keys.join(", ")
            )
        }
    }
}

fn format_age(timestamp_ms: i64) -> String {
    let now = chrono::Utc::now().timestamp_millis();
    let diff_secs = (now - timestamp_ms) / 1000;
    if diff_secs < 60 {
        format!("{}s ago", diff_secs)
    } else if diff_secs < 3600 {
        format!("{}m ago", diff_secs / 60)
    } else if diff_secs < 86400 {
        format!("{}h ago", diff_secs / 3600)
    } else {
        format!("{}d ago", diff_secs / 86400)
    }
}

fn format_timestamp(timestamp_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(timestamp_ms)
        .map(|dt| dt.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "??:??:??".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::Message;

    fn test_ctx() -> ToolContext {
        ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test-sessions-tool".to_string(),
            session_key: "test-current-session".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        ..ToolContext::default()
        }
    }

    #[tokio::test]
    async fn test_sessions_list_empty() {
        let tool = SessionsTool;
        let ctx = test_ctx();
        let args = serde_json::json!({"action": "list"});
        // This will try to open a real DB for agent "test-sessions-tool"
        // which may or may not exist — the tool should handle gracefully
        let result = tool.execute(args, &ctx).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_sessions_missing_action() {
        let tool = SessionsTool;
        let ctx = test_ctx();
        let args = serde_json::json!({});
        let result = tool.execute(args, &ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_sessions_unknown_action() {
        let tool = SessionsTool;
        let ctx = test_ctx();
        let args = serde_json::json!({"action": "destroy"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("Unknown action"));
    }

    #[tokio::test]
    async fn test_sessions_send_missing_message() {
        let tool = SessionsTool;
        let ctx = test_ctx();
        let args = serde_json::json!({"action": "send"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("missing"));
    }

    #[tokio::test]
    async fn test_sessions_send_invalid_role() {
        let tool = SessionsTool;
        let ctx = test_ctx();
        let args = serde_json::json!({"action": "send", "message": "hi", "role": "admin"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("Invalid role"));
    }

    #[test]
    fn test_format_age() {
        let now = chrono::Utc::now().timestamp_millis();
        assert!(format_age(now - 30_000).contains("s ago"));
        assert!(format_age(now - 300_000).contains("m ago"));
        assert!(format_age(now - 7_200_000).contains("h ago"));
        assert!(format_age(now - 172_800_000).contains("d ago"));
    }

    #[test]
    fn test_format_timestamp() {
        let ts = format_timestamp(1708300800000); // 2024-02-19 00:00:00 UTC
        assert!(ts.contains(":"));
        assert_eq!(ts.len(), 8); // HH:MM:SS
    }
}
