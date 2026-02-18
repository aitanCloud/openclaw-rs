use anyhow::{Context, Result};
use std::path::Path;
use std::time::Instant;
use tracing::{debug, info, warn};

use crate::llm::streaming::StreamEvent;
use crate::llm::{Completion, LlmProvider, Message, UsageStats};
use crate::sessions::SessionStore;
use crate::tools::{ToolContext, ToolRegistry};
use crate::workspace;

const MAX_TOOL_ROUNDS: usize = 20;
const MAX_HISTORY_MESSAGES: usize = 40;
/// Approximate max tokens for context history (leave room for system prompt + current turn)
const MAX_HISTORY_TOKENS: usize = 12000;
/// Max characters of tool output to send to the LLM (prevents token waste on huge outputs)
const MAX_TOOL_OUTPUT_CHARS: usize = 32000;

/// Truncate tool output if it exceeds the limit, preserving head and tail.
fn truncate_tool_output(output: &str) -> String {
    if output.len() <= MAX_TOOL_OUTPUT_CHARS {
        return output.to_string();
    }
    let head_size = MAX_TOOL_OUTPUT_CHARS * 3 / 4; // 75% head
    let tail_size = MAX_TOOL_OUTPUT_CHARS / 4;       // 25% tail
    let omitted = output.len() - head_size - tail_size;
    format!(
        "{}\n\n... [{} chars truncated] ...\n\n{}",
        &output[..head_size],
        omitted,
        &output[output.len() - tail_size..]
    )
}

/// Rough token estimate: ~4 chars per token for English text.
/// This is intentionally conservative to avoid exceeding context windows.
fn estimate_tokens(text: &str) -> usize {
    // Count chars / 4, with a minimum of 1 token per message
    (text.len() / 4).max(1)
}

fn estimate_message_tokens(msg: &Message) -> usize {
    let base = estimate_tokens(msg.content.as_deref().unwrap_or(""));
    // Role overhead: ~4 tokens for role/formatting
    base + 4
}

/// Configuration for an agent turn
pub struct AgentTurnConfig {
    pub agent_name: String,
    pub session_key: String,
    pub workspace_dir: String,
    pub minimal_context: bool,
}

/// Load recent conversation history from the session store.
/// Returns up to MAX_HISTORY_MESSAGES recent messages (user + assistant only).
fn load_session_history(agent_name: &str, session_key: &str) -> Vec<Message> {
    let store = match SessionStore::open(agent_name) {
        Ok(s) => s,
        Err(e) => {
            debug!("Could not open session store for history: {}", e);
            return Vec::new();
        }
    };

    match store.load_llm_messages(session_key) {
        Ok(msgs) => {
            // Hard cap first
            let start = msgs.len().saturating_sub(MAX_HISTORY_MESSAGES);
            let candidates: Vec<Message> = msgs[start..].to_vec();

            // Token-aware pruning: walk backwards, keep messages until budget exhausted
            let mut token_budget = MAX_HISTORY_TOKENS;
            let mut kept: Vec<Message> = Vec::new();

            for msg in candidates.iter().rev() {
                let msg_tokens = estimate_message_tokens(msg);
                if msg_tokens > token_budget {
                    break;
                }
                token_budget -= msg_tokens;
                kept.push(msg.clone());
            }

            kept.reverse();

            if !kept.is_empty() {
                let total_tokens: usize = kept.iter().map(|m| estimate_message_tokens(m)).sum();
                debug!(
                    "Loaded {} history messages (~{} tokens) for session {} (pruned from {})",
                    kept.len(), total_tokens, session_key, msgs.len()
                );
            }
            kept
        }
        Err(e) => {
            debug!("Could not load session history: {}", e);
            Vec::new()
        }
    }
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

    // Build initial messages with session history
    let history = load_session_history(&config.agent_name, &config.session_key);
    let mut messages = vec![Message::system(&ws.system_prompt)];
    messages.extend(history);
    messages.push(Message::user(user_message));

    // Get tool definitions
    let tool_defs = tools.definitions();
    let tool_ctx = ToolContext {
        workspace_dir: config.workspace_dir.clone(),
        agent_name: config.agent_name.clone(),
        session_key: config.session_key.clone(),
        sandbox: crate::sandbox::SandboxPolicy::default(),
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
                    "Round {}: LLM requested {} tool call(s){}",
                    rounds,
                    calls.len(),
                    if calls.len() > 1 { " (parallel)" } else { "" }
                );

                messages.push(Message::assistant_tool_calls(calls.clone(), reasoning));

                // Prepare tool calls for parallel execution
                let prepared: Vec<(String, serde_json::Value, String)> = calls
                    .iter()
                    .map(|call| {
                        let args: serde_json::Value = serde_json::from_str(&call.function.arguments)
                            .unwrap_or_else(|e| {
                                warn!("Failed to parse tool args for {}: {}", call.function.name, e);
                                serde_json::json!({})
                            });
                        (call.function.name.clone(), args, call.id.clone())
                    })
                    .collect();

                // Execute all tool calls concurrently
                let results = tools.execute_parallel(&prepared, &tool_ctx).await;

                // Process results in order, matching back to call IDs
                for (i, (name, result)) in results.into_iter().enumerate() {
                    let call_id = &calls[i].id;
                    let result = match result {
                        Ok(r) => r,
                        Err(e) => {
                            warn!("Tool {} execution error: {}", name, e);
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
                        name,
                        output.len(),
                        result.is_error
                    );

                    let output = truncate_tool_output(&output);
                    messages.push(Message::tool_result(call_id, &output));
                }
            }
        }
    }
}

/// Run a streaming agent turn — sends StreamEvents via channel as tokens arrive.
/// The caller can use these events to update a Telegram message in real-time.
pub async fn run_agent_turn_streaming(
    provider: &dyn LlmProvider,
    user_message: &str,
    config: &AgentTurnConfig,
    tools: &ToolRegistry,
    event_tx: tokio::sync::mpsc::UnboundedSender<StreamEvent>,
    image_urls: Vec<String>,
) -> Result<AgentTurnResult> {
    let t_start = Instant::now();

    let workspace_dir = Path::new(&config.workspace_dir);
    let ws = workspace::load_workspace(workspace_dir, config.minimal_context)
        .await
        .context("Failed to load workspace")?;

    // Build initial messages with session history
    let history = load_session_history(&config.agent_name, &config.session_key);
    let mut messages = vec![Message::system(&ws.system_prompt)];
    messages.extend(history);

    // Use multimodal message if images are present
    if image_urls.is_empty() {
        messages.push(Message::user(user_message));
    } else {
        messages.push(Message::user_with_images(user_message, image_urls));
    }

    let tool_defs = tools.definitions();
    let tool_ctx = ToolContext {
        workspace_dir: config.workspace_dir.clone(),
        agent_name: config.agent_name.clone(),
        session_key: config.session_key.clone(),
        sandbox: crate::sandbox::SandboxPolicy::default(),
    };

    let mut total_usage = UsageStats::default();
    let mut tool_calls_made = 0;
    let mut rounds = 0;

    loop {
        rounds += 1;
        if rounds > MAX_TOOL_ROUNDS {
            warn!("Agent hit max tool rounds ({}), forcing stop", MAX_TOOL_ROUNDS);
            let _ = event_tx.send(StreamEvent::Done);
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

        if rounds > 1 {
            let _ = event_tx.send(StreamEvent::RoundStart { round: rounds });
        }

        debug!("Streaming round {} — sending {} messages to LLM", rounds, messages.len());

        let (completion, usage) = provider
            .complete_streaming(&messages, &tool_defs, event_tx.clone())
            .await
            .with_context(|| format!("LLM streaming call failed on round {}", rounds))?;

        total_usage.prompt_tokens += usage.prompt_tokens;
        total_usage.completion_tokens += usage.completion_tokens;
        total_usage.total_tokens += usage.total_tokens;

        match completion {
            Completion::Text { content, reasoning } => {
                info!(
                    "Streaming agent turn complete: {} rounds, {} tool calls, {:.0}ms",
                    rounds, tool_calls_made, t_start.elapsed().as_millis()
                );

                let _ = event_tx.send(StreamEvent::Done);

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
                    "Streaming round {}: LLM requested {} tool call(s){}",
                    rounds, calls.len(),
                    if calls.len() > 1 { " (parallel)" } else { "" }
                );

                messages.push(Message::assistant_tool_calls(calls.clone(), reasoning));

                // Emit tool exec events for all calls
                for call in &calls {
                    let _ = event_tx.send(StreamEvent::ToolExec {
                        name: call.function.name.clone(),
                        call_id: call.id.clone(),
                    });
                }

                // Prepare and execute all tool calls concurrently
                let prepared: Vec<(String, serde_json::Value, String)> = calls
                    .iter()
                    .map(|call| {
                        let args: serde_json::Value = serde_json::from_str(&call.function.arguments)
                            .unwrap_or_else(|e| {
                                warn!("Failed to parse tool args for {}: {}", call.function.name, e);
                                serde_json::json!({})
                            });
                        (call.function.name.clone(), args, call.id.clone())
                    })
                    .collect();

                let results = tools.execute_parallel(&prepared, &tool_ctx).await;

                for (i, (name, result)) in results.into_iter().enumerate() {
                    let call_id = &calls[i].id;
                    let result = match result {
                        Ok(r) => r,
                        Err(e) => {
                            warn!("Tool {} execution error: {}", name, e);
                            crate::tools::ToolResult::error(format!("Tool error: {}", e))
                        }
                    };

                    let _ = event_tx.send(StreamEvent::ToolResult {
                        name: name.clone(),
                        success: !result.is_error,
                    });

                    tool_calls_made += 1;

                    let output = if result.is_error {
                        format!("[ERROR] {}", result.output)
                    } else {
                        result.output
                    };

                    let output = truncate_tool_output(&output);
                    messages.push(Message::tool_result(call_id, &output));
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

    #[test]
    fn test_truncate_tool_output_short() {
        let short = "hello world";
        assert_eq!(truncate_tool_output(short), short);
    }

    #[test]
    fn test_truncate_tool_output_long() {
        let long = "x".repeat(50000);
        let result = truncate_tool_output(&long);
        assert!(result.len() < long.len());
        assert!(result.contains("chars truncated"));
    }

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens(""), 1); // minimum 1
        assert_eq!(estimate_tokens("hello world!"), 3); // 12 chars / 4
        assert_eq!(estimate_tokens("a".repeat(100).as_str()), 25);
    }
}
