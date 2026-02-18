use anyhow::{Context, Result};
use std::path::Path;
use std::time::Instant;
use tracing::{debug, info, warn};

use crate::llm::{Completion, LlmProvider, Message, UsageStats};
use crate::tools::{ToolContext, ToolRegistry};
use crate::workspace;

const MAX_TOOL_ROUNDS: usize = 20;

/// Configuration for an agent turn
pub struct AgentTurnConfig {
    pub agent_name: String,
    pub session_key: String,
    pub workspace_dir: String,
    pub minimal_context: bool,
}

/// Result of a complete agent turn
#[derive(Debug)]
pub struct AgentTurnResult {
    pub response: String,
    pub reasoning: Option<String>,
    pub model_name: String,
    pub tool_calls_made: usize,
    pub total_rounds: usize,
    pub total_usage: UsageStats,
    pub elapsed_ms: u128,
}

/// Run a single agent turn: assemble context, call LLM, execute tools, loop until text response
pub async fn run_agent_turn(
    provider: &dyn LlmProvider,
    user_message: &str,
    config: &AgentTurnConfig,
    tools: &ToolRegistry,
) -> Result<AgentTurnResult> {
    let t_start = Instant::now();

    // Load workspace context
    let workspace_dir = Path::new(&config.workspace_dir);
    let ws = workspace::load_workspace(workspace_dir, config.minimal_context)
        .await
        .context("Failed to load workspace")?;

    debug!(
        "Loaded {} bootstrap files from {}",
        ws.bootstrap_files.len(),
        ws.dir.display()
    );

    // Build initial messages
    let mut messages = vec![
        Message::system(&ws.system_prompt),
        Message::user(user_message),
    ];

    // Get tool definitions
    let tool_defs = tools.definitions();
    let tool_ctx = ToolContext {
        workspace_dir: config.workspace_dir.clone(),
        agent_name: config.agent_name.clone(),
        session_key: config.session_key.clone(),
    };

    let mut total_usage = UsageStats::default();
    let mut tool_calls_made = 0;
    let mut rounds = 0;

    loop {
        rounds += 1;
        if rounds > MAX_TOOL_ROUNDS {
            warn!("Agent hit max tool rounds ({}), forcing stop", MAX_TOOL_ROUNDS);
            return Ok(AgentTurnResult {
                response: "(Agent reached maximum tool call rounds)".to_string(),
                reasoning: None,
                model_name: provider.name().to_string(),
                tool_calls_made,
                total_rounds: rounds,
                total_usage,
                elapsed_ms: t_start.elapsed().as_millis(),
            });
        }

        debug!("Round {} — sending {} messages to LLM", rounds, messages.len());

        let (completion, usage) = provider
            .complete(&messages, &tool_defs)
            .await
            .with_context(|| format!("LLM call failed on round {}", rounds))?;

        total_usage.prompt_tokens += usage.prompt_tokens;
        total_usage.completion_tokens += usage.completion_tokens;
        total_usage.total_tokens += usage.total_tokens;

        match completion {
            Completion::Text { content, reasoning } => {
                info!(
                    "Agent turn complete: {} rounds, {} tool calls, {:.0}ms",
                    rounds,
                    tool_calls_made,
                    t_start.elapsed().as_millis()
                );

                return Ok(AgentTurnResult {
                    response: content,
                    reasoning,
                    model_name: provider.name().to_string(),
                    tool_calls_made,
                    total_rounds: rounds,
                    total_usage,
                    elapsed_ms: t_start.elapsed().as_millis(),
                });
            }

            Completion::ToolCalls { calls, reasoning } => {
                info!(
                    "Round {}: LLM requested {} tool call(s)",
                    rounds,
                    calls.len()
                );

                // Add the assistant message with tool calls (and reasoning for models like kimi-k2.5)
                messages.push(Message::assistant_tool_calls(calls.clone(), reasoning));

                // Execute each tool call
                for call in &calls {
                    let tool_name = &call.function.name;
                    let args_str = &call.function.arguments;

                    debug!("Executing tool: {} (call_id: {})", tool_name, call.id);

                    let args: serde_json::Value = serde_json::from_str(args_str)
                        .unwrap_or_else(|e| {
                            warn!("Failed to parse tool args for {}: {}", tool_name, e);
                            serde_json::json!({})
                        });

                    let result = match tools.execute(tool_name, args, &tool_ctx).await {
                        Ok(result) => result,
                        Err(e) => {
                            warn!("Tool {} execution error: {}", tool_name, e);
                            crate::tools::ToolResult::error(format!("Tool error: {}", e))
                        }
                    };

                    tool_calls_made += 1;

                    let output = if result.is_error {
                        format!("[ERROR] {}", result.output)
                    } else {
                        result.output
                    };

                    debug!(
                        "Tool {} result: {} bytes, error={}",
                        tool_name,
                        output.len(),
                        result.is_error
                    );

                    messages.push(Message::tool_result(&call.id, &output));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_turn_config() {
        let config = AgentTurnConfig {
            agent_name: "main".to_string(),
            session_key: "test-session".to_string(),
            workspace_dir: "/tmp".to_string(),
            minimal_context: false,
        };
        assert_eq!(config.agent_name, "main");
    }
}
