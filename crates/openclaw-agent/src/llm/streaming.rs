use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde::Deserialize;
use std::io::Write;

use super::{Completion, FunctionCall, ToolCall, UsageStats};

/// Stream a chat completion, printing tokens as they arrive.
/// Returns the final Completion once the stream ends.
pub async fn stream_completion(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    messages: &[super::Message],
    tools: &[super::ToolDefinition],
    max_tokens: u32,
    writer: &mut dyn Write,
) -> Result<(Completion, UsageStats)> {
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "max_tokens": max_tokens,
        "stream": true,
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

        // Process complete SSE lines
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

                        // Content tokens
                        if let Some(ref c) = delta.content {
                            content.push_str(c);
                            let _ = writer.write_all(c.as_bytes());
                            let _ = writer.flush();
                        }

                        // Reasoning tokens
                        if let Some(ref r) = delta.reasoning_content {
                            reasoning.push_str(r);
                        }

                        // Tool call deltas
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
                                    }
                                    if let Some(ref args) = func.arguments {
                                        partial.arguments.push_str(args);
                                    }
                                }
                            }
                        }
                    }

                    // Usage in final chunk
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

    // Build final result
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
