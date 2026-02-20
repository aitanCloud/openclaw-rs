//! MCP Client Bridge — connects to external MCP servers via stdio and
//! exposes their tools as agent Tool trait objects.
//!
//! Uses a global persistent connection pool so MCP server processes are
//! spawned once at startup and reused across all agent turns.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, OnceCell};
use tracing::{debug, error, info, warn};

use super::{Tool, ToolContext, ToolResult};

// ── Config ──

/// Configuration for an external MCP server to connect to
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

// ── Global persistent pool ──

/// Global pool of MCP clients + their discovered tools, initialized once.
static MCP_POOL: OnceCell<McpPool> = OnceCell::const_new();

struct McpPool {
    tools: Vec<PooledTool>,
}

struct PooledTool {
    prefixed_name: String,
    tool_name: String,
    description: String,
    input_schema: Value,
    server_name: String,
    client: Arc<McpClient>,
}

async fn init_pool(configs: &[McpServerConfig]) -> McpPool {
    let mut tools = Vec::new();

    for config in configs {
        match McpClient::connect(config).await {
            Ok(client) => {
                let client = Arc::new(client);
                match client.list_tools().await {
                    Ok(discovered) => {
                        for t in discovered {
                            let prefixed = format!("mcp_{}_{}", t.server_name.replace('-', "_"), t.name);
                            debug!("  → {}: {}", prefixed, t.description);
                            tools.push(PooledTool {
                                prefixed_name: prefixed,
                                tool_name: t.name,
                                description: t.description,
                                input_schema: t.input_schema,
                                server_name: t.server_name,
                                client: Arc::clone(&client),
                            });
                        }
                    }
                    Err(e) => error!("Failed to list tools from MCP '{}': {}", config.name, e),
                }
            }
            Err(e) => error!("Failed to connect to MCP '{}': {}", config.name, e),
        }
    }

    info!("MCP pool: {} tools from {} servers (persistent)", tools.len(), configs.len());
    McpPool { tools }
}

// ── MCP Client (stdio transport) ──

struct McpClientInner {
    child: Child,
    stdin: tokio::process::ChildStdin,
    reader: BufReader<tokio::process::ChildStdout>,
    next_id: u64,
}

pub struct McpClient {
    name: String,
    inner: Arc<Mutex<McpClientInner>>,
}

impl McpClient {
    /// Spawn an MCP server process and connect via stdio
    pub async fn connect(config: &McpServerConfig) -> Result<Self> {
        info!("MCP client connecting to '{}': {} {:?}", config.name, config.command, config.args);

        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        for (k, v) in &config.env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn()
            .with_context(|| format!("Failed to spawn MCP server '{}': {}", config.name, config.command))?;

        let stdin = child.stdin.take()
            .ok_or_else(|| anyhow::anyhow!("No stdin for MCP server '{}'", config.name))?;
        let stdout = child.stdout.take()
            .ok_or_else(|| anyhow::anyhow!("No stdout for MCP server '{}'", config.name))?;

        let client = Self {
            name: config.name.clone(),
            inner: Arc::new(Mutex::new(McpClientInner {
                child,
                stdin,
                reader: BufReader::new(stdout),
                next_id: 1,
            })),
        };

        // Initialize
        let resp = client.request("initialize", Some(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "openclaw-rs", "version": env!("CARGO_PKG_VERSION")}
        }))).await?;

        if let Some(err) = resp.get("error") {
            anyhow::bail!("MCP init failed for '{}': {}", config.name, err);
        }

        // Send initialized notification
        client.notify("notifications/initialized", None).await?;
        info!("MCP client '{}' initialized", config.name);
        Ok(client)
    }

    /// Discover tools from the server
    pub async fn list_tools(&self) -> Result<Vec<DiscoveredTool>> {
        let resp = self.request("tools/list", None).await?;
        let tools_arr = resp.get("result")
            .and_then(|r| r.get("tools"))
            .and_then(|t| t.as_array())
            .ok_or_else(|| anyhow::anyhow!("Invalid tools/list from '{}'", self.name))?;

        let mut tools = Vec::new();
        for t in tools_arr {
            tools.push(DiscoveredTool {
                name: t.get("name").and_then(|v| v.as_str()).unwrap_or("?").to_string(),
                description: t.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                input_schema: t.get("inputSchema").cloned().unwrap_or(json!({"type":"object","properties":{}})),
                server_name: self.name.clone(),
            });
        }
        info!("MCP '{}': {} tools discovered", self.name, tools.len());
        Ok(tools)
    }

    /// Call a tool on the server
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<String> {
        let resp = self.request("tools/call", Some(json!({"name": name, "arguments": arguments}))).await?;

        if let Some(err) = resp.get("error") {
            anyhow::bail!("MCP tool '{}' error: {}", name, err);
        }

        let result = resp.get("result").ok_or_else(|| anyhow::anyhow!("No result"))?;
        let is_error = result.get("isError").and_then(|v| v.as_bool()).unwrap_or(false);

        let mut output = String::new();
        if let Some(items) = result.get("content").and_then(|v| v.as_array()) {
            for item in items {
                if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                    if !output.is_empty() { output.push('\n'); }
                    output.push_str(text);
                }
            }
        }

        if is_error {
            anyhow::bail!("{}", output);
        }
        Ok(output)
    }

    /// Check if the child process is still alive
    async fn is_alive(&self) -> bool {
        let mut inner = self.inner.lock().await;
        matches!(inner.child.try_wait(), Ok(None))
    }

    async fn request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        let mut inner = self.inner.lock().await;
        let id = inner.next_id;
        inner.next_id += 1;

        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params.unwrap_or(json!({}))});
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');

        inner.stdin.write_all(line.as_bytes()).await
            .with_context(|| format!("Write to MCP '{}' failed", self.name))?;
        inner.stdin.flush().await?;

        // Read response, skip notifications
        loop {
            let mut buf = String::new();
            let n = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                inner.reader.read_line(&mut buf),
            ).await
                .with_context(|| format!("Timeout from MCP '{}'", self.name))?
                .with_context(|| format!("Read from MCP '{}' failed", self.name))?;

            if n == 0 { anyhow::bail!("MCP '{}' closed", self.name); }
            let buf = buf.trim();
            if buf.is_empty() { continue; }

            let parsed: Value = serde_json::from_str(buf)?;
            // Skip notifications (no id)
            if parsed.get("id").is_none() || parsed.get("id") == Some(&Value::Null) {
                continue;
            }
            return Ok(parsed);
        }
    }

    async fn notify(&self, method: &str, params: Option<Value>) -> Result<()> {
        let mut inner = self.inner.lock().await;
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params.unwrap_or(json!({}))});
        let mut line = serde_json::to_string(&msg)?;
        line.push('\n');
        inner.stdin.write_all(line.as_bytes()).await?;
        inner.stdin.flush().await?;
        Ok(())
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.inner.try_lock() {
            let _ = inner.child.start_kill();
        }
    }
}

// ── Discovered Tool ──

#[derive(Debug, Clone)]
struct DiscoveredTool {
    name: String,
    description: String,
    input_schema: Value,
    server_name: String,
}

// ── McpTool: wraps a pooled tool as an agent Tool ──

pub struct McpTool {
    prefixed_name: String,
    tool_name: String,
    desc: String,
    schema: Value,
    server_name: String,
    client: Arc<McpClient>,
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.prefixed_name
    }

    fn description(&self) -> &str {
        &self.desc
    }

    fn parameters(&self) -> Value {
        self.schema.clone()
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolResult> {
        debug!("MCP call: {} → {}/{}", self.prefixed_name, self.server_name, self.tool_name);
        match self.client.call_tool(&self.tool_name, args).await {
            Ok(output) => Ok(ToolResult::success(output)),
            Err(e) => Ok(ToolResult::error(format!("MCP error ({}): {}", self.server_name, e))),
        }
    }
}

// ── Public API ──

/// Initialize the global MCP pool (call once at startup).
/// Subsequent calls are no-ops — the pool is initialized exactly once.
pub async fn init_mcp_pool(configs: &[McpServerConfig]) {
    MCP_POOL.get_or_init(|| init_pool(configs)).await;
}

/// Get MCP tools from the persistent pool as boxed Tool objects.
/// Must call `init_mcp_pool` first. Returns empty vec if pool not initialized.
pub async fn load_mcp_tools(configs: &[McpServerConfig]) -> Vec<Box<dyn Tool>> {
    // Ensure pool is initialized (idempotent)
    let pool = MCP_POOL.get_or_init(|| init_pool(configs)).await;

    pool.tools.iter().map(|t| -> Box<dyn Tool> {
        Box::new(McpTool {
            prefixed_name: t.prefixed_name.clone(),
            tool_name: t.tool_name.clone(),
            desc: t.description.clone(),
            schema: t.input_schema.clone(),
            server_name: t.server_name.clone(),
            client: Arc::clone(&t.client),
        })
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_deserialize() {
        let json = r#"{"name": "test", "command": "echo", "args": ["-n"], "env": {"K": "V"}}"#;
        let c: McpServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(c.name, "test");
        assert_eq!(c.args, vec!["-n"]);
        assert_eq!(c.env.get("K").unwrap(), "V");
    }

    #[test]
    fn test_config_minimal() {
        let json = r#"{"name": "x", "command": "y"}"#;
        let c: McpServerConfig = serde_json::from_str(json).unwrap();
        assert!(c.args.is_empty());
        assert!(c.env.is_empty());
    }

    #[test]
    fn test_prefixed_name() {
        assert_eq!(
            format!("mcp_{}_{}", "filesystem".replace('-', "_"), "read_file"),
            "mcp_filesystem_read_file"
        );
        assert_eq!(
            format!("mcp_{}_{}", "my-server".replace('-', "_"), "query"),
            "mcp_my_server_query"
        );
    }
}
