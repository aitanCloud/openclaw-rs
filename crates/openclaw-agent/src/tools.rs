pub mod browser;
pub mod claude_code;
pub mod cron;
pub mod delegate;
pub mod exec;
pub mod find;
pub mod grep;
pub mod image;
pub mod list_dir;
pub mod mcp_bridge;
pub mod memory;
pub mod patch;
pub mod process;
pub mod read;
pub mod script_plugin;
pub mod sessions;
pub mod tasks;
pub mod tts;
pub mod web_fetch;
pub mod web_search;
pub mod write;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

use crate::llm::{FunctionDefinition, ToolDefinition};
use crate::sandbox::SandboxPolicy;

pub use tasks::{TaskInfo, TaskQueryFn};
/// Callback type for cancelling a task by ID. Returns true if cancelled.
pub type TaskCancelFn = Arc<dyn Fn(u64) -> bool + Send + Sync>;

/// A delegate request sent from the agent to the gateway for background execution.
#[derive(Debug, Clone)]
pub struct DelegateRequest {
    pub task: String,
    pub context: String,
    pub agent_name: String,
    pub workspace_dir: String,
    pub chat_id: i64,
}

/// Sender half for dispatching delegate requests to the gateway.
pub type DelegateTx = tokio::sync::mpsc::UnboundedSender<DelegateRequest>;

/// Context passed to every tool execution.
#[derive(Clone)]
pub struct ToolContext {
    pub workspace_dir: String,
    pub agent_name: String,
    pub session_key: String,
    pub sandbox: SandboxPolicy,
    /// Chat ID for sending background task updates (0 = unknown).
    pub chat_id: i64,
    /// If set, delegate tool dispatches async instead of blocking.
    pub delegate_tx: Option<DelegateTx>,
    /// If set, tasks tool can query running subagents.
    pub task_query_fn: Option<TaskQueryFn>,
    /// If set, tasks tool can cancel subagents by ID.
    pub task_cancel_fn: Option<TaskCancelFn>,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("workspace_dir", &self.workspace_dir)
            .field("agent_name", &self.agent_name)
            .field("session_key", &self.session_key)
            .field("chat_id", &self.chat_id)
            .field("has_delegate_tx", &self.delegate_tx.is_some())
            .field("has_task_query_fn", &self.task_query_fn.is_some())
            .finish()
    }
}

impl Default for ToolContext {
    fn default() -> Self {
        Self {
            workspace_dir: String::new(),
            agent_name: String::new(),
            session_key: String::new(),
            sandbox: SandboxPolicy::default(),
            chat_id: 0,
            delegate_tx: None,
            task_query_fn: None,
            task_cancel_fn: None,
        }
    }
}

/// Result of a tool execution
#[derive(Debug)]
pub struct ToolResult {
    pub output: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: false,
        }
    }

    pub fn error(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: true,
        }
    }
}

/// Trait for all agent tools
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Value;
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult>;
}

/// Registry of available tools
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }

    /// Create a registry with the default built-in tools
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.register(Box::new(exec::ExecTool));
        registry.register(Box::new(read::ReadTool));
        registry.register(Box::new(write::WriteTool));
        registry.register(Box::new(list_dir::ListDirTool));
        registry.register(Box::new(patch::PatchTool));
        registry.register(Box::new(grep::GrepTool));
        registry.register(Box::new(find::FindTool));
        registry.register(Box::new(web_search::WebSearchTool));
        registry.register(Box::new(web_fetch::WebFetchTool));
        registry.register(Box::new(process::ProcessTool));
        registry.register(Box::new(image::ImageTool));
        registry.register(Box::new(cron::CronTool));
        registry.register(Box::new(sessions::SessionsTool));
        registry.register(Box::new(tts::TtsTool));
        registry.register(Box::new(browser::BrowserTool));
        registry.register(Box::new(delegate::DelegateTool));
        registry.register(Box::new(tasks::TasksTool));
        registry.register(Box::new(memory::MemoryTool));
        registry.register(Box::new(claude_code::ClaudeCodeTool));
        registry
    }

    /// Create a new registry from an existing one, excluding a specific tool by name.
    /// Used to prevent subagents from spawning further subagents (no delegate recursion).
    pub fn without_tool(source: Self, exclude_name: &str) -> Self {
        let tools = source
            .tools
            .into_iter()
            .filter(|t| t.name() != exclude_name)
            .collect();
        Self { tools }
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.push(tool);
    }

    /// Get tool definitions for sending to the LLM
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .iter()
            .map(|t| ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: t.name().to_string(),
                    description: t.description().to_string(),
                    parameters: t.parameters(),
                },
            })
            .collect()
    }

    /// Execute a tool by name
    pub async fn execute(
        &self,
        name: &str,
        args: Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult> {
        let tool = self
            .tools
            .iter()
            .find(|t| t.name() == name)
            .ok_or_else(|| anyhow::anyhow!("Unknown tool: {}", name))?;

        tool.execute(args, ctx).await
    }

    /// Execute multiple tool calls concurrently using futures::join_all.
    /// Returns results in the same order as the input calls.
    pub async fn execute_parallel(
        &self,
        calls: &[(String, Value, String)], // (tool_name, args, call_id)
        ctx: &ToolContext,
    ) -> Vec<(String, Result<ToolResult>)> {
        let futures: Vec<_> = calls
            .iter()
            .map(|(tool_name, args, _call_id)| {
                let tool = self.tools.iter().find(|t| t.name() == tool_name);
                let ctx = ctx.clone();
                let args = args.clone();
                let name = tool_name.clone();

                async move {
                    match tool {
                        Some(t) => (name, t.execute(args, &ctx).await),
                        None => (name.clone(), Err(anyhow::anyhow!("Unknown tool: {}", name))),
                    }
                }
            })
            .collect();

        futures::future::join_all(futures).await
    }

    /// Load and register script plugins from the workspace plugins directory.
    /// Returns the number of plugins loaded.
    pub fn load_plugins(&mut self, workspace_dir: &std::path::Path) -> usize {
        let plugins_dir = script_plugin::plugins_dir(workspace_dir);
        let plugins = script_plugin::load_plugins(&plugins_dir);
        let count = plugins.len();
        for plugin in plugins {
            self.register(Box::new(plugin));
        }
        count
    }

    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.iter().map(|t| t.name()).collect()
    }

    /// Connect to external MCP servers and register their tools.
    /// Returns the number of MCP tools loaded.
    pub async fn load_mcp_tools(
        &mut self,
        configs: &[mcp_bridge::McpServerConfig],
    ) -> usize {
        let tools = mcp_bridge::load_mcp_tools(configs).await;
        let count = tools.len();
        for tool in tools {
            self.register(tool);
        }
        count
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_defaults() {
        let registry = ToolRegistry::with_defaults();
        let names = registry.tool_names();
        assert!(names.contains(&"exec"));
        assert!(names.contains(&"read"));
        assert!(names.contains(&"write"));
        assert!(names.contains(&"list_dir"));
        assert!(names.contains(&"patch"));
        assert!(names.contains(&"grep"));
        assert!(names.contains(&"find"));
        assert!(names.contains(&"web_search"));
        assert!(names.contains(&"web_fetch"));
        assert!(names.contains(&"process"));
        assert!(names.contains(&"image"));
        assert!(names.contains(&"cron"));
        assert!(names.contains(&"sessions"));
        assert!(names.contains(&"tts"));
        assert!(names.contains(&"browser"));
        assert!(names.contains(&"delegate"));
        assert!(names.contains(&"memory"));
        assert_eq!(names.len(), 18);
    }

    #[test]
    fn test_definitions_format() {
        let registry = ToolRegistry::with_defaults();
        let defs = registry.definitions();
        assert_eq!(defs.len(), 18);
        for def in &defs {
            assert_eq!(def.tool_type, "function");
            assert!(!def.function.name.is_empty());
            assert!(!def.function.description.is_empty());
        }
    }

    #[test]
    fn test_without_tool_removes_delegate() {
        let registry = ToolRegistry::with_defaults();
        assert!(registry.tool_names().contains(&"delegate"));
        assert_eq!(registry.tool_names().len(), 18);

        let filtered = ToolRegistry::without_tool(registry, "delegate");
        assert!(!filtered.tool_names().contains(&"delegate"));
        assert_eq!(filtered.tool_names().len(), 17);
    }
}
