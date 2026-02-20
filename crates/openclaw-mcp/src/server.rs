use serde_json::json;
use tracing::info;

use crate::protocol::*;
use crate::tools;
use crate::McpContext;

/// MCP Server — handles JSON-RPC requests
#[derive(Clone)]
pub struct McpServer {
    ctx: McpContext,
}

impl McpServer {
    pub fn new(ctx: McpContext) -> Self {
        Self { ctx }
    }

    /// Handle a single JSON-RPC request and return a response
    pub async fn handle_request(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        match request.method.as_str() {
            "initialize" => self.handle_initialize(request),
            "ping" => self.handle_ping(request),
            "tools/list" => self.handle_tools_list(request),
            "tools/call" => self.handle_tools_call(request).await,
            "shutdown" => self.handle_shutdown(request),
            _ => {
                info!("MCP unknown method: {}", request.method);
                JsonRpcResponse::error(
                    request.id.clone(),
                    METHOD_NOT_FOUND,
                    format!("Method not found: {}", request.method),
                )
            }
        }
    }

    fn handle_initialize(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        info!("MCP initialize");
        let result = InitializeResult {
            protocol_version: "2024-11-05".to_string(),
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability {
                    list_changed: Some(false),
                }),
            },
            server_info: ServerInfo {
                name: "openclaw-mcp".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };
        JsonRpcResponse::success(
            request.id.clone(),
            serde_json::to_value(result).unwrap(),
        )
    }

    fn handle_ping(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        JsonRpcResponse::success(request.id.clone(), json!({}))
    }

    fn handle_tools_list(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        let defs = tools::tool_definitions();
        let result = ToolsListResult { tools: defs };
        JsonRpcResponse::success(
            request.id.clone(),
            serde_json::to_value(result).unwrap(),
        )
    }

    async fn handle_tools_call(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        let params = match &request.params {
            Some(p) => p,
            None => {
                return JsonRpcResponse::error(
                    request.id.clone(),
                    INVALID_PARAMS,
                    "Missing params",
                );
            }
        };

        let name = match params.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => {
                return JsonRpcResponse::error(
                    request.id.clone(),
                    INVALID_PARAMS,
                    "Missing tool name",
                );
            }
        };

        let arguments = params.get("arguments").cloned();
        let result = tools::handle_tool_call(&self.ctx, name, arguments).await;

        JsonRpcResponse::success(
            request.id.clone(),
            serde_json::to_value(result).unwrap(),
        )
    }

    fn handle_shutdown(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        info!("MCP shutdown");
        JsonRpcResponse::success(request.id.clone(), json!({}))
    }
}
