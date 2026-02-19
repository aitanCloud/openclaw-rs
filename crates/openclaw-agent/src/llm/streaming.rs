use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde::Deserialize;
use tokio::sync::mpsc;

use super::{Completion, FunctionCall, ToolCall, UsageStats};

/// Events emitted during streaming
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A chunk of content text arrived
    ContentDelta(String),
    /// A chunk of reasoning text arrived
    ReasoningDelta(String),
    /// Tool call is being assembled (name known)
    ToolCallStart { name: String },
    /// Tool is being executed
    ToolExec { name: String, call_id: String },
    /// Tool execution finished
    ToolResult { name: String, success: bool },
    /// New round starting (after tool calls)
    RoundStart { round: usize },
    /// Stream finished — final completion
    Done,
}

/// Stream a chat completion, sending events via channel as tokens arrive.
/// Returns the final Completion once the stream ends.
pub async fn stream_completion(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    messages: &[super::Message],
    tools: &[super::ToolDefinition],
    max_tokens: u32,
    event_tx: Option<mpsc::UnboundedSender<StreamEvent>>,
) -> Result<(Completion, UsageStats)> {
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "max_tokens": max_tokens,
        "stream": true,
        "stream_options": {"include_usage": true},
    });

    if !tools.is_empty() {
        body["tools"] = serde_json::to_value(tools)?;
    }

    let response = client
        .post(format!("{}/chat/completions", base_url))
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .context("Failed to send streaming request")?;

    let status = response.status();
    if !status.is_success() {
        let err_body = response.text().await.unwrap_or_default();
        anyhow::bail!("LLM API returned {}: {}", status, err_body);
    }

    let mut content = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<PartialToolCall> = Vec::new();
    let mut usage = UsageStats::default();

    let mut stream = response.bytes_stream();
    let mut buffer = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("Stream read error")?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(line_end) = buffer.find('\n') {
            let line = buffer[..line_end].trim().to_string();
            buffer = buffer[line_end + 1..].to_string();

            if line.is_empty() || line == "data: [DONE]" {
                continue;
            }

            if let Some(json_str) = line.strip_prefix("data: ") {
                if let Ok(chunk) = serde_json::from_str::<StreamChunk>(json_str) {
                    if let Some(choice) = chunk.choices.first() {
                        let delta = &choice.delta;

                        if let Some(ref c) = delta.content {
                            content.push_str(c);
                            if let Some(ref tx) = event_tx {
                                let _ = tx.send(StreamEvent::ContentDelta(c.clone()));
                            }
                        }

                        if let Some(ref r) = delta.reasoning_content {
                            reasoning.push_str(r);
                            if let Some(ref tx) = event_tx {
                                let _ = tx.send(StreamEvent::ReasoningDelta(r.clone()));
                            }
                        }

                        if let Some(ref tc_deltas) = delta.tool_calls {
                            for tc_delta in tc_deltas {
                                let idx = tc_delta.index as usize;
                                while tool_calls.len() <= idx {
                                    tool_calls.push(PartialToolCall::default());
                                }
                                let partial = &mut tool_calls[idx];
                                if let Some(ref id) = tc_delta.id {
                                    partial.id = id.clone();
                                }
                                if let Some(ref func) = tc_delta.function {
                                    if let Some(ref name) = func.name {
                                        partial.name = name.clone();
                                        if let Some(ref tx) = event_tx {
                                            let _ = tx.send(StreamEvent::ToolCallStart {
                                                name: name.clone(),
                                            });
                                        }
                                    }
                                    if let Some(ref args) = func.arguments {
                                        partial.arguments.push_str(args);
                                    }
                                }
                            }
                        }
                    }

                    if let Some(u) = chunk.usage {
                        usage = UsageStats {
                            prompt_tokens: u.prompt_tokens,
                            completion_tokens: u.completion_tokens,
                            total_tokens: u.total_tokens,
                        };
                    }
                }
            }
        }
    }

    // Fallback: estimate tokens if the API didn't report them (common in streaming)
    if usage.total_tokens == 0 {
        let msg_chars: usize = messages.iter()
            .map(|m| m.content.as_deref().unwrap_or("").len())
            .sum();
        let prompt_est = (msg_chars / 4).max(1) as u32;
        let completion_est = ((content.len() + reasoning.len()) / 4).max(1) as u32;
        usage = UsageStats {
            prompt_tokens: prompt_est,
            completion_tokens: completion_est,
            total_tokens: prompt_est + completion_est,
        };
    }

    if let Some(ref tx) = event_tx {
        let _ = tx.send(StreamEvent::Done);
    }

    if !tool_calls.is_empty() {
        let calls = tool_calls
            .into_iter()
            .map(|tc| ToolCall {
                id: tc.id,
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: tc.name,
                    arguments: tc.arguments,
                },
            })
            .collect();

        let reasoning_opt = if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        };

        Ok((
            Completion::ToolCalls {
                calls,
                reasoning: reasoning_opt,
            },
            usage,
        ))
    } else {
        let reasoning_opt = if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        };

        Ok((
            Completion::Text {
                content,
                reasoning: reasoning_opt,
            },
            usage,
        ))
    }
}

// ── SSE chunk types ──

#[derive(Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<StreamUsage>,
}

#[derive(Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCallDelta>>,
}

#[derive(Deserialize)]
struct ToolCallDelta {
    index: u32,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<FunctionDelta>,
}

#[derive(Deserialize)]
struct FunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct StreamUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
}

#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}
