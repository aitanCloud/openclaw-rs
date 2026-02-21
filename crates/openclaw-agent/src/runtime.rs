use anyhow::{Context, Result};
use std::path::Path;
use std::time::Instant;
use tracing::{debug, info, warn};

use crate::llm::streaming::StreamEvent;
use crate::llm::{Completion, LlmProvider, Message, UsageStats};
use crate::loop_detection::{LoopDetector, LoopVerdict};
use crate::sessions::SessionStore;
use crate::tools::{ToolContext, ToolRegistry};
use crate::workspace;

const MAX_TOOL_ROUNDS: usize = 20;
const MAX_HISTORY_MESSAGES: usize = 40;
/// Approximate max tokens for context history (leave room for system prompt + current turn)
const MAX_HISTORY_TOKENS: usize = 12000;
/// Max characters of tool output to send to the LLM (prevents token waste on huge outputs)
const MAX_TOOL_OUTPUT_CHARS: usize = 32000;

/// Detect if the LLM is fabricating tool actions in a text response.
/// Returns true if the response looks like it's claiming to dispatch/execute something
/// without actually calling a tool.
fn detect_fabrication(content: &str) -> bool {
    let lower = content.to_lowercase();
    let fabrication_patterns = [
        "task dispatched",
        "task has been dispatched",
        "dispatched to background",
        "dispatched successfully",
        "subagent is running",
        "subagent has been",
        "delegated to",
        "i've dispatched",
        "i have dispatched",
        "running in background",
        "check status: /tasks",
        "check status with /tasks",
    ];
    fabrication_patterns.iter().any(|p| lower.contains(p))
}

/// Create a human-readable summary of tool arguments for activity display.
fn summarize_tool_args(tool_name: &str, args: &serde_json::Value) -> String {
    match tool_name {
        "web_fetch" | "web_search" => {
            if let Some(url) = args.get("url").or(args.get("query")).and_then(|v| v.as_str()) {
                let truncated = if url.len() > 120 { format!("{}...", &url[..117]) } else { url.to_string() };
                return truncated;
            }
        }
        "exec" => {
            if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
                let truncated = if cmd.len() > 120 { format!("{}...", &cmd[..117]) } else { cmd.to_string() };
                return format!("`{}`", truncated);
            }
        }
        "read" | "write" | "patch" | "list_dir" | "grep" | "find" => {
            if let Some(path) = args.get("path").or(args.get("file")).and_then(|v| v.as_str()) {
                return path.to_string();
            }
        }
        "browser" => {
            if let Some(url) = args.get("url").and_then(|v| v.as_str()) {
                let truncated = if url.len() > 120 { format!("{}...", &url[..117]) } else { url.to_string() };
                return truncated;
            }
            if let Some(action) = args.get("action").and_then(|v| v.as_str()) {
                return action.to_string();
            }
        }
        "delegate" => {
            if let Some(task) = args.get("task").and_then(|v| v.as_str()) {
                let truncated = if task.len() > 100 { format!("{}...", &task[..97]) } else { task.to_string() };
                return truncated;
            }
        }
        _ => {}
    }
    // Fallback: compact JSON, truncated
    let s = args.to_string();
    if s.len() > 120 { format!("{}...", &s[..117]) } else { s }
}

/// Create a truncated preview of tool output for activity display.
fn preview_tool_output(output: &str) -> String {
    let first_line = output.lines().take(3).collect::<Vec<_>>().join(" ");
    if first_line.len() > 200 {
        format!("{}...", &first_line[..197])
    } else {
        first_line
    }
}

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
    pub chat_id: i64,
    pub delegate_tx: Option<crate::tools::DelegateTx>,
    pub task_query_fn: Option<crate::tools::TaskQueryFn>,
    pub task_cancel_fn: Option<crate::tools::TaskCancelFn>,
}

impl Default for AgentTurnConfig {
    fn default() -> Self {
        Self {
            agent_name: String::new(),
            session_key: String::new(),
            workspace_dir: String::new(),
            minimal_context: false,
            chat_id: 0,
            delegate_tx: None,
            task_query_fn: None,
            task_cancel_fn: None,
        }
    }
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

            // ── Sanitize: drop empty user messages (e.g. from photo messages
            // where image_urls weren't persisted to session history) ──
            kept.retain(|msg| {
                if matches!(msg.role, crate::llm::Role::User) {
                    let content = msg.content.as_deref().unwrap_or("");
                    !content.is_empty()
                } else {
                    true
                }
            });

            // ── Sanitize: strip orphaned tool messages at the start ──
            // Pruning may cut in the middle of a tool-call sequence, leaving
            // Tool-role messages without a preceding Assistant message that
            // contains tool_calls.  LLM APIs reject these with errors like
            // "tool_call_id is not found" or "Messages with role 'tool' must
            // be a response to a preceding message with 'tool_calls'".
            while let Some(first) = kept.first() {
                if matches!(first.role, crate::llm::Role::Tool) {
                    kept.remove(0);
                } else {
                    break;
                }
            }

            // Also strip a leading Assistant message that has tool_calls but
            // whose corresponding Tool results were just removed above.
            if let Some(first) = kept.first() {
                if matches!(first.role, crate::llm::Role::Assistant) && first.tool_calls.is_some() {
                    // Check if the next message is a Tool result for this call
                    let has_tool_response = kept.get(1).map_or(false, |m| matches!(m.role, crate::llm::Role::Tool));
                    if !has_tool_response {
                        kept.remove(0);
                    }
                }
            }

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
    /// All messages generated during this turn (tool calls + tool results + final assistant).
    /// Excludes the system prompt and loaded history — only new messages from this turn.
    pub turn_messages: Vec<Message>,
}

/// Run a single agent turn: assemble context, call LLM, execute tools, loop until text response
pub async fn run_agent_turn(
    provider: &dyn LlmProvider,
    user_message: &str,
    config: &AgentTurnConfig,
    tools: &ToolRegistry,
) -> Result<AgentTurnResult> {
    let t_start = Instant::now();

    // Set session context for LLM log tagging
    crate::llm_log::set_session_context(&config.session_key);

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
        chat_id: config.chat_id,
        delegate_tx: config.delegate_tx.clone(),
        task_query_fn: config.task_query_fn.clone(),
        task_cancel_fn: config.task_cancel_fn.clone(),
        stream_tx: None,
    };

    let mut total_usage = UsageStats::default();
    let mut tool_calls_made = 0;
    let mut rounds = 0;
    let mut loop_detector = LoopDetector::new();
    let turn_start_idx = messages.len(); // track where new messages begin

    loop {
        rounds += 1;
        if rounds > MAX_TOOL_ROUNDS {
            warn!("Agent hit max tool rounds ({}), forcing stop", MAX_TOOL_ROUNDS);
            crate::llm_log::clear_session_context();
            return Ok(AgentTurnResult {
                response: "(Agent reached maximum tool call rounds)".to_string(),
                reasoning: None,
                model_name: provider.name().to_string(),
                tool_calls_made,
                total_rounds: rounds,
                total_usage,
                elapsed_ms: t_start.elapsed().as_millis(),
                turn_messages: messages[turn_start_idx..].to_vec(),
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
                // ── Fabrication detection ──
                if tool_calls_made == 0 && rounds == 1 && detect_fabrication(&content) {
                    warn!("Fabrication detected: LLM generated action text without tool calls, retrying");
                    messages.push(Message::assistant(&content));
                    messages.push(Message::user(
                        "[SYSTEM] Your previous response was REJECTED. You claimed to dispatch a task \
                         or perform an action, but you did NOT call any tool. Text responses cannot \
                         dispatch tasks or run commands. You MUST use the appropriate tool (e.g. \
                         `delegate` for subagent tasks, `exec` for commands). Try again — call the \
                         tool this time."
                    ));
                    continue;
                }

                info!(
                    "Agent turn complete: {} rounds, {} tool calls, {:.0}ms",
                    rounds,
                    tool_calls_made,
                    t_start.elapsed().as_millis()
                );

                crate::llm_log::clear_session_context();
                return Ok(AgentTurnResult {
                    response: content,
                    reasoning,
                    model_name: provider.name().to_string(),
                    tool_calls_made,
                    total_rounds: rounds,
                    total_usage,
                    elapsed_ms: t_start.elapsed().as_millis(),
                    turn_messages: messages[turn_start_idx..].to_vec(),
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

                // ── Loop detection: check each call before executing ──
                let mut execute_list: Vec<(usize, bool, String)> = Vec::new();
                for (idx, (name, args, _call_id)) in prepared.iter().enumerate() {
                    let verdict = loop_detector.check(name, args);
                    match verdict {
                        LoopVerdict::Allow => {
                            loop_detector.record_call(name, args);
                            execute_list.push((idx, true, String::new()));
                        }
                        LoopVerdict::Warn { message, detector, count } => {
                            warn!("Loop warning for {} ({}): count={}", name, detector, count);
                            loop_detector.record_call(name, args);
                            execute_list.push((idx, true, message));
                        }
                        LoopVerdict::Block { message, detector, count } => {
                            warn!("Loop BLOCKED {} ({}): count={}", name, detector, count);
                            loop_detector.record_block();
                            execute_list.push((idx, false, message));
                        }
                    }
                }

                let to_execute: Vec<(String, serde_json::Value, String)> = execute_list.iter()
                    .filter(|(_, should_exec, _)| *should_exec)
                    .map(|(idx, _, _)| prepared[*idx].clone())
                    .collect();

                let exec_results = if !to_execute.is_empty() {
                    tools.execute_parallel(&to_execute, &tool_ctx).await
                } else {
                    Vec::new()
                };

                let mut exec_iter = exec_results.into_iter();
                for (idx, should_execute, block_msg) in &execute_list {
                    let call_id = &calls[*idx].id;
                    let (name, args, _) = &prepared[*idx];

                    let (tool_name, result) = if *should_execute {
                        let (n, r) = exec_iter.next().unwrap_or_else(|| {
                            (name.clone(), Err(anyhow::anyhow!("Tool execution missing")))
                        });
                        (n, r)
                    } else {
                        (name.clone(), Ok(crate::tools::ToolResult::error(block_msg.clone())))
                    };

                    let result = match result {
                        Ok(r) => r,
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

                    if *should_execute {
                        loop_detector.record_outcome(&tool_name, args, &output);
                    }

                    let output = if !block_msg.is_empty() && *should_execute {
                        format!("{}\n\n[SYSTEM: {}]", output, block_msg)
                    } else {
                        output
                    };

                    debug!(
                        "Tool {} result: {} bytes, error={}",
                        tool_name,
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
/// If `cancel_token` is provided, the turn will abort when the token is cancelled.
pub async fn run_agent_turn_streaming(
    provider: &dyn LlmProvider,
    user_message: &str,
    config: &AgentTurnConfig,
    tools: &ToolRegistry,
    event_tx: tokio::sync::mpsc::UnboundedSender<StreamEvent>,
    image_urls: Vec<String>,
    cancel_token: Option<tokio_util::sync::CancellationToken>,
) -> Result<AgentTurnResult> {
    let t_start = Instant::now();

    // Set session context for LLM log tagging
    crate::llm_log::set_session_context(&config.session_key);

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
        // Inject a hint so the LLM knows the image is already in its context
        let image_hint = format!(
            "[SYSTEM: {} image(s) attached inline below. You can see them directly — \
             analyze them from your visual input. Do NOT use the `image` tool or try to \
             open a file path. The image data is already in this message.]\n\n{}",
            image_urls.len(),
            user_message,
        );
        messages.push(Message::user_with_images(&image_hint, image_urls));
    }

    let tool_defs = tools.definitions();
    let tool_ctx = ToolContext {
        workspace_dir: config.workspace_dir.clone(),
        agent_name: config.agent_name.clone(),
        session_key: config.session_key.clone(),
        sandbox: crate::sandbox::SandboxPolicy::default(),
        chat_id: config.chat_id,
        delegate_tx: config.delegate_tx.clone(),
        task_query_fn: config.task_query_fn.clone(),
        task_cancel_fn: config.task_cancel_fn.clone(),
        stream_tx: Some(event_tx.clone()),
    };

    let mut total_usage = UsageStats::default();
    let mut tool_calls_made = 0;
    let mut rounds = 0;
    let mut loop_detector = LoopDetector::new();
    let turn_start_idx = messages.len(); // track where new messages begin

    loop {
        // ── Check cancellation before each round ──
        if let Some(ref ct) = cancel_token {
            if ct.is_cancelled() {
                warn!("Agent turn cancelled by user before round {}", rounds + 1);
                let _ = event_tx.send(StreamEvent::Done);
                crate::llm_log::clear_session_context();
                return Ok(AgentTurnResult {
                    response: "⛔ Cancelled by user.".to_string(),
                    reasoning: None,
                    model_name: provider.name().to_string(),
                    tool_calls_made,
                    total_rounds: rounds,
                    total_usage,
                    elapsed_ms: t_start.elapsed().as_millis(),
                    turn_messages: messages[turn_start_idx..].to_vec(),
                });
            }
        }

        rounds += 1;
        if rounds > MAX_TOOL_ROUNDS {
            warn!("Agent hit max tool rounds ({}), forcing stop", MAX_TOOL_ROUNDS);
            let _ = event_tx.send(StreamEvent::Done);
            crate::llm_log::clear_session_context();
            return Ok(AgentTurnResult {
                response: "(Agent reached maximum tool call rounds)".to_string(),
                reasoning: None,
                model_name: provider.name().to_string(),
                tool_calls_made,
                total_rounds: rounds,
                total_usage,
                elapsed_ms: t_start.elapsed().as_millis(),
                turn_messages: messages[turn_start_idx..].to_vec(),
            });
        }

        if rounds > 1 {
            let _ = event_tx.send(StreamEvent::RoundStart { round: rounds });
        }

        debug!("Streaming round {} — sending {} messages to LLM", rounds, messages.len());

        // ── LLM call with cancellation support ──
        let llm_result = if let Some(ref ct) = cancel_token {
            tokio::select! {
                result = provider.complete_streaming(&messages, &tool_defs, event_tx.clone()) => result,
                _ = ct.cancelled() => {
                    warn!("Agent turn cancelled during LLM streaming on round {}", rounds);
                    let _ = event_tx.send(StreamEvent::Done);
                    crate::llm_log::clear_session_context();
                    return Ok(AgentTurnResult {
                        response: "⛔ Cancelled by user.".to_string(),
                        reasoning: None,
                        model_name: provider.name().to_string(),
                        tool_calls_made,
                        total_rounds: rounds,
                        total_usage,
                        elapsed_ms: t_start.elapsed().as_millis(),
                        turn_messages: messages[turn_start_idx..].to_vec(),
                    });
                }
            }
        } else {
            provider.complete_streaming(&messages, &tool_defs, event_tx.clone()).await
        };

        let (completion, usage) = llm_result
            .with_context(|| format!("LLM streaming call failed on round {}", rounds))?;

        total_usage.prompt_tokens += usage.prompt_tokens;
        total_usage.completion_tokens += usage.completion_tokens;
        total_usage.total_tokens += usage.total_tokens;

        match completion {
            Completion::Text { content, reasoning } => {
                // ── Fabrication detection: catch LLM claiming tool actions in text ──
                if tool_calls_made == 0 && rounds == 1 && detect_fabrication(&content) {
                    warn!("Fabrication detected: LLM generated action text without tool calls, retrying");
                    messages.push(Message::assistant(&content));
                    messages.push(Message::user(
                        "[SYSTEM] Your previous response was REJECTED. You claimed to dispatch a task \
                         or perform an action, but you did NOT call any tool. Text responses cannot \
                         dispatch tasks or run commands. You MUST use the appropriate tool (e.g. \
                         `delegate` for subagent tasks, `exec` for commands). Try again — call the \
                         tool this time."
                    ));
                    continue; // retry the round
                }

                info!(
                    "Streaming agent turn complete: {} rounds, {} tool calls, {:.0}ms",
                    rounds, tool_calls_made, t_start.elapsed().as_millis()
                );

                let _ = event_tx.send(StreamEvent::Done);

                crate::llm_log::clear_session_context();
                return Ok(AgentTurnResult {
                    response: content,
                    reasoning,
                    model_name: provider.name().to_string(),
                    tool_calls_made,
                    total_rounds: rounds,
                    total_usage,
                    elapsed_ms: t_start.elapsed().as_millis(),
                    turn_messages: messages[turn_start_idx..].to_vec(),
                });
            }

            Completion::ToolCalls { calls, reasoning } => {
                info!(
                    "Streaming round {}: LLM requested {} tool call(s){}",
                    rounds, calls.len(),
                    if calls.len() > 1 { " (parallel)" } else { "" }
                );

                messages.push(Message::assistant_tool_calls(calls.clone(), reasoning));

                // Emit tool exec events for all calls (with args summary for visibility)
                for call in &calls {
                    let args_val: serde_json::Value = serde_json::from_str(&call.function.arguments)
                        .unwrap_or(serde_json::json!({}));
                    let _ = event_tx.send(StreamEvent::ToolExec {
                        name: call.function.name.clone(),
                        call_id: call.id.clone(),
                        args_summary: summarize_tool_args(&call.function.name, &args_val),
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

                // ── Loop detection: check each call before executing ──
                let mut execute_list: Vec<(usize, bool, String)> = Vec::new(); // (index, should_execute, block_msg)
                for (idx, (name, args, _call_id)) in prepared.iter().enumerate() {
                    let verdict = loop_detector.check(name, args);
                    match verdict {
                        LoopVerdict::Allow => {
                            loop_detector.record_call(name, args);
                            execute_list.push((idx, true, String::new()));
                        }
                        LoopVerdict::Warn { message, detector, count } => {
                            warn!("Loop warning for {} ({}): {} — count={}", name, detector, message, count);
                            loop_detector.record_call(name, args);
                            execute_list.push((idx, true, message));
                        }
                        LoopVerdict::Block { message, detector, count } => {
                            warn!("Loop BLOCKED {} ({}): {} — count={}", name, detector, message, count);
                            loop_detector.record_block();
                            execute_list.push((idx, false, message));
                        }
                    }
                }

                // Only execute non-blocked calls
                let to_execute: Vec<(String, serde_json::Value, String)> = execute_list.iter()
                    .filter(|(_, should_exec, _)| *should_exec)
                    .map(|(idx, _, _)| prepared[*idx].clone())
                    .collect();

                let exec_results = if !to_execute.is_empty() {
                    tools.execute_parallel(&to_execute, &tool_ctx).await
                } else {
                    Vec::new()
                };

                // Merge results back: blocked calls get error messages, executed calls get real results
                let mut exec_iter = exec_results.into_iter();
                for (idx, should_execute, block_msg) in &execute_list {
                    let call_id = &calls[*idx].id;
                    let (name, args, _) = &prepared[*idx];

                    let (tool_name, result) = if *should_execute {
                        let (n, r) = exec_iter.next().unwrap_or_else(|| {
                            (name.clone(), Err(anyhow::anyhow!("Tool execution missing")))
                        });
                        (n, r)
                    } else {
                        (name.clone(), Ok(crate::tools::ToolResult::error(block_msg.clone())))
                    };

                    let result = match result {
                        Ok(r) => r,
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

                    let _ = event_tx.send(StreamEvent::ToolResult {
                        name: tool_name.clone(),
                        success: !result.is_error,
                        output_preview: preview_tool_output(&output),
                    });

                    // Record outcome for no-progress detection (only for executed calls)
                    if *should_execute {
                        loop_detector.record_outcome(&tool_name, args, &output);
                    }

                    // Append warning to output if loop was detected but not blocked
                    let output = if !block_msg.is_empty() && *should_execute {
                        format!("{}\n\n[SYSTEM: {}]", output, block_msg)
                    } else {
                        output
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
        ..AgentTurnConfig::default()
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

    #[test]
    fn test_detect_fabrication_positive() {
        assert!(detect_fabrication("**Task dispatched.**\n\nBoston forecast running."));
        assert!(detect_fabrication("Task dispatched successfully."));
        assert!(detect_fabrication("I've dispatched the subagent."));
        assert!(detect_fabrication("The subagent is running in background."));
        assert!(detect_fabrication("Check status: /tasks"));
        assert!(detect_fabrication("Delegated to a background subagent."));
        assert!(detect_fabrication("Running in background now."));
    }

    #[test]
    fn test_detect_fabrication_negative() {
        assert!(!detect_fabrication("The weather in Boston is 45°F."));
        assert!(!detect_fabrication("I'll search for that information."));
        assert!(!detect_fabrication("Here are the results:"));
        assert!(!detect_fabrication("Let me check the current tasks."));
        assert!(!detect_fabrication(""));
    }

    /// Mock LLM provider that sleeps before responding — used to test cancellation.
    struct SlowMockProvider;

    #[async_trait::async_trait]
    impl LlmProvider for SlowMockProvider {
        fn name(&self) -> &str { "slow-mock" }

        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[crate::llm::ToolDefinition],
        ) -> Result<(Completion, UsageStats)> {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            Ok((Completion::Text { content: "done".into(), reasoning: None }, UsageStats::default()))
        }

        async fn complete_streaming(
            &self,
            _messages: &[Message],
            _tools: &[crate::llm::ToolDefinition],
            _event_tx: tokio::sync::mpsc::UnboundedSender<StreamEvent>,
        ) -> Result<(Completion, UsageStats)> {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            Ok((Completion::Text { content: "done".into(), reasoning: None }, UsageStats::default()))
        }
    }

    #[tokio::test]
    async fn test_cancellation_aborts_streaming_turn() {
        let provider = SlowMockProvider;
        let config = AgentTurnConfig {
            agent_name: "test-cancel".to_string(),
            session_key: "cancel-test-session".to_string(),
            workspace_dir: "/tmp".to_string(),
            minimal_context: true,
        ..AgentTurnConfig::default()
        };
        let tools = crate::tools::ToolRegistry::new();
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();

        let cancel_token = tokio_util::sync::CancellationToken::new();
        let ct = cancel_token.clone();

        // Cancel after 50ms
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            ct.cancel();
        });

        let result = run_agent_turn_streaming(
            &provider,
            "hello",
            &config,
            &tools,
            event_tx,
            Vec::new(),
            Some(cancel_token),
        ).await.unwrap();

        assert!(result.response.contains("Cancelled"));
        assert!(result.elapsed_ms < 5000); // Should abort quickly, not wait 10s
    }
}
