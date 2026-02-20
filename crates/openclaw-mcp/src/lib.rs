pub mod protocol;
pub mod server;
pub mod tasks;
pub mod tools;
pub mod transport;

use tasks::TaskManager;

/// Shared context for MCP tool handlers
#[derive(Clone)]
pub struct McpContext {
    pub agent_name: String,
    pub task_manager: TaskManager,
}

impl McpContext {
    pub fn new(agent_name: String) -> Self {
        Self {
            agent_name,
            task_manager: TaskManager::new(),
        }
    }
}

/// Create an MCP server with the given agent name
pub fn create_server(agent_name: &str) -> server::McpServer {
    let ctx = McpContext::new(agent_name.to_string());
    server::McpServer::new(ctx)
}

/// Run MCP server in stdio mode (for Claude Desktop, Windsurf, etc.)
pub async fn run_stdio(agent_name: &str) -> anyhow::Result<()> {
    let server = create_server(agent_name);
    transport::stdio::run_stdio(server).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::JsonRpcRequest;
    use serde_json::json;

    fn make_request(method: &str, params: Option<serde_json::Value>) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: method.to_string(),
            params,
        }
    }

    #[tokio::test]
    async fn test_initialize() {
        let server = create_server("test-agent");
        let req = make_request("initialize", Some(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "1.0"}
        })));
        let resp = server.handle_request(&req).await;
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert_eq!(result["serverInfo"]["name"], "openclaw-mcp");
    }

    #[tokio::test]
    async fn test_ping() {
        let server = create_server("test-agent");
        let req = make_request("ping", None);
        let resp = server.handle_request(&req).await;
        assert!(resp.error.is_none());
    }

    #[tokio::test]
    async fn test_tools_list() {
        let server = create_server("test-agent");
        let req = make_request("tools/list", None);
        let resp = server.handle_request(&req).await;
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 6);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"openclaw_chat"));
        assert!(names.contains(&"openclaw_status"));
        assert!(names.contains(&"openclaw_chat_async"));
        assert!(names.contains(&"openclaw_task_status"));
        assert!(names.contains(&"openclaw_task_list"));
        assert!(names.contains(&"openclaw_task_cancel"));
    }

    #[tokio::test]
    async fn test_unknown_method() {
        let server = create_server("test-agent");
        let req = make_request("nonexistent/method", None);
        let resp = server.handle_request(&req).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, crate::protocol::METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn test_tools_call_missing_name() {
        let server = create_server("test-agent");
        let req = make_request("tools/call", Some(json!({})));
        let resp = server.handle_request(&req).await;
        assert!(resp.error.is_some());
    }

    #[tokio::test]
    async fn test_tools_call_unknown_tool() {
        let server = create_server("test-agent");
        let req = make_request("tools/call", Some(json!({
            "name": "nonexistent_tool",
            "arguments": {}
        })));
        let resp = server.handle_request(&req).await;
        assert!(resp.error.is_none()); // Tool errors are returned as ToolCallResult, not JSON-RPC errors
        let result = resp.result.unwrap();
        assert_eq!(result["isError"], true);
    }

    #[tokio::test]
    async fn test_status_tool() {
        let server = create_server("test-agent");
        let req = make_request("tools/call", Some(json!({
            "name": "openclaw_status",
            "arguments": {}
        })));
        let resp = server.handle_request(&req).await;
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("test-agent"));
    }

    #[tokio::test]
    async fn test_task_manager_lifecycle() {
        let tm = TaskManager::new();

        // Create task
        let task = tm.create("hello".to_string(), None, 0).await.unwrap();
        assert_eq!(task.status, tasks::TaskStatus::Pending);

        // List tasks
        let all = tm.list(None, None).await;
        assert_eq!(all.len(), 1);

        // Stats
        let stats = tm.stats().await;
        assert_eq!(stats.pending, 1);
        assert_eq!(stats.total, 1);

        // Cancel
        assert!(tm.cancel(&task.id).await);
        let cancelled = tm.get(&task.id).await.unwrap();
        assert_eq!(cancelled.status, tasks::TaskStatus::Cancelled);

        // Can't cancel again
        assert!(!tm.cancel(&task.id).await);
    }

    #[tokio::test]
    async fn test_task_manager_max_tasks() {
        let tm = TaskManager::new();
        for i in 0..1000 {
            tm.create(format!("msg {}", i), None, 0).await.unwrap();
        }
        // 1001st should fail
        assert!(tm.create("overflow".to_string(), None, 0).await.is_err());
    }

    #[tokio::test]
    async fn test_chat_validation_empty_message() {
        let server = create_server("test-agent");
        let req = make_request("tools/call", Some(json!({
            "name": "openclaw_chat",
            "arguments": {"message": ""}
        })));
        let resp = server.handle_request(&req).await;
        let result = resp.result.unwrap();
        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("empty"));
    }

    #[tokio::test]
    async fn test_chat_validation_missing_message() {
        let server = create_server("test-agent");
        let req = make_request("tools/call", Some(json!({
            "name": "openclaw_chat",
            "arguments": {}
        })));
        let resp = server.handle_request(&req).await;
        let result = resp.result.unwrap();
        assert_eq!(result["isError"], true);
    }

    #[tokio::test]
    async fn test_task_cancel_nonexistent() {
        let server = create_server("test-agent");
        let req = make_request("tools/call", Some(json!({
            "name": "openclaw_task_cancel",
            "arguments": {"task_id": "nonexistent"}
        })));
        let resp = server.handle_request(&req).await;
        let result = resp.result.unwrap();
        assert_eq!(result["isError"], true);
    }

    #[tokio::test]
    async fn test_task_status_nonexistent() {
        let server = create_server("test-agent");
        let req = make_request("tools/call", Some(json!({
            "name": "openclaw_task_status",
            "arguments": {"task_id": "nonexistent"}
        })));
        let resp = server.handle_request(&req).await;
        let result = resp.result.unwrap();
        assert_eq!(result["isError"], true);
    }

    #[tokio::test]
    async fn test_task_list_empty() {
        let server = create_server("test-agent");
        let req = make_request("tools/call", Some(json!({
            "name": "openclaw_task_list",
            "arguments": {}
        })));
        let resp = server.handle_request(&req).await;
        let result = resp.result.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["stats"]["total"], 0);
    }

    #[tokio::test]
    async fn test_shutdown() {
        let server = create_server("test-agent");
        let req = make_request("shutdown", None);
        let resp = server.handle_request(&req).await;
        assert!(resp.error.is_none());
    }
}
