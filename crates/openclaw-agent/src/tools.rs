pub mod exec;
pub mod read;
pub mod web_fetch;
pub mod web_search;
pub mod write;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::llm::{FunctionDefinition, ToolDefinition};

/// Context passed to every tool execution
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub workspace_dir: String,
    pub agent_name: String,
    pub session_key: String,
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
        registry.register(Box::new(web_search::WebSearchTool));
        registry.register(Box::new(web_fetch::WebFetchTool));
        registry
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

    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.iter().map(|t| t.name()).collect()
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
        assert!(names.contains(&"web_search"));
        assert!(names.contains(&"web_fetch"));
        assert_eq!(names.len(), 5);
    }

    #[test]
    fn test_definitions_format() {
        let registry = ToolRegistry::with_defaults();
        let defs = registry.definitions();
        assert_eq!(defs.len(), 5);
        for def in &defs {
            assert_eq!(def.tool_type, "function");
            assert!(!def.function.name.is_empty());
            assert!(!def.function.description.is_empty());
        }
    }
}
