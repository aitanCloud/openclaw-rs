use anyhow::{Context, Result};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::debug;

use super::streaming::StreamEvent;
use super::{Completion, FunctionCall, LlmProvider, Message, Role, ToolCall, ToolDefinition, UsageStats};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    max_tokens: u32,
}

impl AnthropicProvider {
    pub fn new(api_key: &str, model: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            max_tokens: 4096,
        }
    }
}

// ── Convert internal messages to Anthropic format ──

fn convert_messages(messages: &[Message]) -> (Option<String>, Vec<AnthropicMessage>) {
    let mut system_prompt = None;
    let mut anthropic_msgs = Vec::new();

    for msg in messages {
        match msg.role {
            Role::System => {
                // Anthropic uses a top-level system param, not a message
                if let Some(ref content) = msg.content {
                    system_prompt = Some(content.clone());
                }
            }
            Role::User => {
                if !msg.image_urls.is_empty() {
                    // Multimodal: convert image_urls to Anthropic image blocks
                    let mut blocks: Vec<ContentBlock> = Vec::new();
                    for url in &msg.image_urls {
                        if let Some((media_type, data)) = parse_data_url(url) {
                            blocks.push(ContentBlock::Image {
                                source: ImageSource {
                                    source_type: "base64".to_string(),
                                    media_type,
                                    data,
                                },
                            });
                        }
                    }
                    if let Some(ref content) = msg.content {
                        if !content.is_empty() {
                            blocks.push(ContentBlock::Text { text: content.clone() });
                        }
                    }
                    anthropic_msgs.push(AnthropicMessage {
                        role: "user".to_string(),
                        content: AnthropicContent::Blocks(blocks),
                    });
                } else if let Some(ref content) = msg.content {
                    anthropic_msgs.push(AnthropicMessage {
                        role: "user".to_string(),
                        content: AnthropicContent::Text(content.clone()),
                    });
                }
            }
            Role::Assistant => {
                if let Some(ref tool_calls) = msg.tool_calls {
                    // Assistant message with tool_use blocks
                    let mut blocks: Vec<ContentBlock> = Vec::new();
                    if let Some(ref content) = msg.content {
                        if !content.is_empty() {
                            blocks.push(ContentBlock::Text { text: content.clone() });
                        }
                    }
                    for tc in tool_calls {
                        let input: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                            .unwrap_or(serde_json::json!({}));
                        blocks.push(ContentBlock::ToolUse {
                            id: tc.id.clone(),
                            name: tc.function.name.clone(),
                            input,
                        });
                    }
                    anthropic_msgs.push(AnthropicMessage {
                        role: "assistant".to_string(),
                        content: AnthropicContent::Blocks(blocks),
                    });
                } else if let Some(ref content) = msg.content {
                    anthropic_msgs.push(AnthropicMessage {
                        role: "assistant".to_string(),
                        content: AnthropicContent::Text(content.clone()),
                    });
                }
            }
            Role::Tool => {
                // Anthropic expects tool results as user messages with tool_result blocks
                let tool_call_id = msg.tool_call_id.clone().unwrap_or_default();
                let content = msg.content.clone().unwrap_or_default();
                let block = ContentBlock::ToolResult {
                    tool_use_id: tool_call_id,
                    content: content,
                };
                anthropic_msgs.push(AnthropicMessage {
                    role: "user".to_string(),
                    content: AnthropicContent::Blocks(vec![block]),
                });
            }
        }
    }

    (system_prompt, anthropic_msgs)
}

fn convert_tools(tools: &[ToolDefinition]) -> Vec<AnthropicTool> {
    tools
        .iter()
        .map(|t| AnthropicTool {
            name: t.function.name.clone(),
            description: t.function.description.clone(),
            input_schema: t.function.parameters.clone(),
        })
        .collect()
}

// ── Anthropic API types ──

#[derive(Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: String,
    content: AnthropicContent,
}

#[derive(Serialize)]
#[serde(untagged)]
enum AnthropicContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: ImageSource },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Serialize)]
struct ImageSource {
    #[serde(rename = "type")]
    source_type: String,
    media_type: String,
    data: String,
}

/// Parse a data URL like "data:image/jpeg;base64,/9j/4AAQ..." into (media_type, base64_data)
fn parse_data_url(url: &str) -> Option<(String, String)> {
    let url = url.strip_prefix("data:")?;
    let (header, data) = url.split_once(",")?;
    let media_type = header.strip_suffix(";base64")?.to_string();
    Some((media_type, data.to_string()))
}

#[derive(Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

// ── Response types ──

#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<ResponseBlock>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum ResponseBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

#[derive(Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

// ── Streaming event types ──

#[derive(Deserialize)]
struct StreamEventData {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    delta: Option<serde_json::Value>,
    #[serde(default)]
    content_block: Option<serde_json::Value>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
    #[serde(default)]
    message: Option<serde_json::Value>,
}

fn process_response(resp: AnthropicResponse) -> (Completion, UsageStats) {
    let usage = resp
        .usage
        .map(|u| UsageStats {
            prompt_tokens: u.input_tokens,
            completion_tokens: u.output_tokens,
            total_tokens: u.input_tokens + u.output_tokens,
        })
        .unwrap_or_default();

    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for block in resp.content {
        match block {
            ResponseBlock::Text { text } => text_parts.push(text),
            ResponseBlock::ToolUse { id, name, input } => {
                tool_calls.push(ToolCall {
                    id,
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name,
                        arguments: serde_json::to_string(&input).unwrap_or_default(),
                    },
                });
            }
        }
    }

    if !tool_calls.is_empty() {
        (
            Completion::ToolCalls {
                calls: tool_calls,
                reasoning: None,
            },
            usage,
        )
    } else {
        (
            Completion::Text {
                content: text_parts.join(""),
                reasoning: None,
            },
            usage,
        )
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &str {
        &self.model
    }

    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<(Completion, UsageStats)> {
        let (system, anthropic_msgs) = convert_messages(messages);
        let anthropic_tools = convert_tools(tools);

        let request = AnthropicRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            messages: anthropic_msgs,
            system,
            tools: anthropic_tools,
            stream: None,
        };

        let t_start = std::time::Instant::now();
        let mut log_entry = crate::llm_log::LlmLogEntry::new(&self.model);
        log_entry.messages_count = messages.len();
        log_entry.streaming = false;
        log_entry.request_tokens_est = messages
            .iter()
            .map(|m| m.content.as_deref().unwrap_or("").len() as u32 / 4)
            .sum::<u32>()
            .max(1);

        let response = self
            .client
            .post(ANTHROPIC_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send request to Anthropic")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            log_entry.latency_ms = t_start.elapsed().as_millis() as u64;
            log_entry.error = Some(format!("HTTP {}: {}", status, &body[..body.len().min(500)]));
            crate::llm_log::record(log_entry);
            anyhow::bail!("Anthropic API returned {}: {}", status, body);
        }

        let api_response: AnthropicResponse = response
            .json()
            .await
            .context("Failed to parse Anthropic response")?;

        let (completion, usage) = process_response(api_response);

        log_entry.latency_ms = t_start.elapsed().as_millis() as u64;
        log_entry.usage_prompt_tokens = usage.prompt_tokens;
        log_entry.usage_completion_tokens = usage.completion_tokens;
        log_entry.usage_total_tokens = usage.total_tokens;
        match &completion {
            Completion::Text { content, reasoning } => {
                log_entry.response_content = Some(content.clone());
                log_entry.response_reasoning = reasoning.clone();
            }
            Completion::ToolCalls { calls, reasoning } => {
                log_entry.response_tool_calls = calls.len();
                log_entry.tool_call_names = calls.iter().map(|c| c.function.name.clone()).collect();
                log_entry.response_reasoning = reasoning.clone();
            }
        }
        crate::llm_log::record(log_entry);

        Ok((completion, usage))
    }

    async fn complete_streaming(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        event_tx: mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<(Completion, UsageStats)> {
        let (system, anthropic_msgs) = convert_messages(messages);
        let anthropic_tools = convert_tools(tools);

        let request = AnthropicRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            messages: anthropic_msgs,
            system,
            tools: anthropic_tools,
            stream: Some(true),
        };

        let t_start = std::time::Instant::now();
        let mut log_entry = crate::llm_log::LlmLogEntry::new(&self.model);
        log_entry.messages_count = messages.len();
        log_entry.streaming = true;
        log_entry.request_tokens_est = messages
            .iter()
            .map(|m| m.content.as_deref().unwrap_or("").len() as u32 / 4)
            .sum::<u32>()
            .max(1);

        let response = self
            .client
            .post(ANTHROPIC_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send streaming request to Anthropic")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            log_entry.latency_ms = t_start.elapsed().as_millis() as u64;
            log_entry.error = Some(format!("HTTP {}: {}", status, &body[..body.len().min(500)]));
            crate::llm_log::record(log_entry);
            anyhow::bail!("Anthropic streaming API returned {}: {}", status, body);
        }

        let mut content = String::new();
        let mut tool_calls: Vec<PartialAnthropicToolCall> = Vec::new();
        let mut current_tool_index: Option<usize> = None;
        let mut usage = UsageStats::default();

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("Anthropic stream read error")?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim().to_string();
                buffer = buffer[line_end + 1..].to_string();

                if line.is_empty() || line.starts_with("event:") {
                    continue;
                }

                if let Some(json_str) = line.strip_prefix("data: ") {
                    if let Ok(evt) = serde_json::from_str::<StreamEventData>(json_str) {
                        match evt.event_type.as_str() {
                            "content_block_start" => {
                                if let Some(ref block) = evt.content_block {
                                    let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                                    if block_type == "tool_use" {
                                        let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        let idx = tool_calls.len();
                                        tool_calls.push(PartialAnthropicToolCall {
                                            id,
                                            name: name.clone(),
                                            arguments: String::new(),
                                        });
                                        current_tool_index = Some(idx);
                                        let _ = event_tx.send(StreamEvent::ToolCallStart { name });
                                    }
                                }
                            }
                            "content_block_delta" => {
                                if let Some(ref delta) = evt.delta {
                                    let delta_type = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");
                                    match delta_type {
                                        "text_delta" => {
                                            if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                                                content.push_str(text);
                                                let _ = event_tx.send(StreamEvent::ContentDelta(text.to_string()));
                                            }
                                        }
                                        "input_json_delta" => {
                                            if let Some(partial) = delta.get("partial_json").and_then(|v| v.as_str()) {
                                                if let Some(idx) = current_tool_index {
                                                    if let Some(tc) = tool_calls.get_mut(idx) {
                                                        tc.arguments.push_str(partial);
                                                    }
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            "content_block_stop" => {
                                current_tool_index = None;
                            }
                            "message_delta" => {
                                // May contain usage info
                                if let Some(ref delta) = evt.delta {
                                    if let Some(u) = delta.get("usage") {
                                        if let Some(out) = u.get("output_tokens").and_then(|v| v.as_u64()) {
                                            usage.completion_tokens = out as u32;
                                        }
                                    }
                                }
                            }
                            "message_start" => {
                                if let Some(ref msg) = evt.message {
                                    if let Some(u) = msg.get("usage") {
                                        if let Some(inp) = u.get("input_tokens").and_then(|v| v.as_u64()) {
                                            usage.prompt_tokens = inp as u32;
                                        }
                                    }
                                }
                            }
                            "message_stop" => {}
                            _ => {
                                debug!("anthropic: unknown stream event: {}", evt.event_type);
                            }
                        }
                    }
                }
            }
        }

        usage.total_tokens = usage.prompt_tokens + usage.completion_tokens;

        // Fallback token estimation
        if usage.total_tokens == 0 {
            let msg_chars: usize = messages
                .iter()
                .map(|m| m.content.as_deref().unwrap_or("").len())
                .sum();
            usage.prompt_tokens = (msg_chars / 4).max(1) as u32;
            usage.completion_tokens = (content.len() / 4).max(1) as u32;
            usage.total_tokens = usage.prompt_tokens + usage.completion_tokens;
        }

        let _ = event_tx.send(StreamEvent::Done);

        let completion = if !tool_calls.is_empty() {
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
            Completion::ToolCalls {
                calls,
                reasoning: None,
            }
        } else {
            Completion::Text {
                content,
                reasoning: None,
            }
        };

        log_entry.latency_ms = t_start.elapsed().as_millis() as u64;
        log_entry.usage_prompt_tokens = usage.prompt_tokens;
        log_entry.usage_completion_tokens = usage.completion_tokens;
        log_entry.usage_total_tokens = usage.total_tokens;
        match &completion {
            Completion::Text { content, reasoning } => {
                log_entry.response_content = Some(content.clone());
                log_entry.response_reasoning = reasoning.clone();
            }
            Completion::ToolCalls { calls, reasoning } => {
                log_entry.response_tool_calls = calls.len();
                log_entry.tool_call_names = calls.iter().map(|c| c.function.name.clone()).collect();
                log_entry.response_reasoning = reasoning.clone();
            }
        }
        crate::llm_log::record(log_entry);

        Ok((completion, usage))
    }
}

struct PartialAnthropicToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_messages_system_extracted() {
        let msgs = vec![
            Message::system("You are helpful."),
            Message::user("Hello"),
        ];
        let (system, anthropic_msgs) = convert_messages(&msgs);
        assert_eq!(system, Some("You are helpful.".to_string()));
        assert_eq!(anthropic_msgs.len(), 1);
    }

    #[test]
    fn test_convert_tools() {
        let tools = vec![ToolDefinition {
            tool_type: "function".to_string(),
            function: super::super::FunctionDefinition {
                name: "exec".to_string(),
                description: "Run a command".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            },
        }];
        let converted = convert_tools(&tools);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].name, "exec");
    }

    #[test]
    fn test_process_response_text() {
        let resp = AnthropicResponse {
            content: vec![ResponseBlock::Text {
                text: "Hello!".to_string(),
            }],
            usage: Some(AnthropicUsage {
                input_tokens: 10,
                output_tokens: 5,
            }),
            stop_reason: Some("end_turn".to_string()),
        };
        let (completion, usage) = process_response(resp);
        match completion {
            Completion::Text { content, .. } => assert_eq!(content, "Hello!"),
            _ => panic!("Expected text completion"),
        }
        assert_eq!(usage.total_tokens, 15);
    }

    #[test]
    fn test_process_response_tool_use() {
        let resp = AnthropicResponse {
            content: vec![ResponseBlock::ToolUse {
                id: "call_1".to_string(),
                name: "exec".to_string(),
                input: serde_json::json!({"command": "ls"}),
            }],
            usage: Some(AnthropicUsage {
                input_tokens: 20,
                output_tokens: 10,
            }),
            stop_reason: Some("tool_use".to_string()),
        };
        let (completion, usage) = process_response(resp);
        match completion {
            Completion::ToolCalls { calls, .. } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].function.name, "exec");
            }
            _ => panic!("Expected tool calls"),
        }
        assert_eq!(usage.total_tokens, 30);
    }
}
