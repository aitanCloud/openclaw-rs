pub mod exec;
pub mod read;
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
        assert_eq!(names.len(), 3);
    }

    #[test]
    fn test_definitions_format() {
        let registry = ToolRegistry::with_defaults();
        let defs = registry.definitions();
        assert_eq!(defs.len(), 3);
        for def in &defs {
            assert_eq!(def.tool_type, "function");
            assert!(!def.function.name.is_empty());
            assert!(!def.function.description.is_empty());
        }
    }
}
