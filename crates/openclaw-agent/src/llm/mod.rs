pub mod fallback;
pub mod streaming;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ── Message types ──

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

impl Message {
    pub fn system(content: &str) -> Self {
        Self {
            role: Role::System,
            content: Some(content.to_string()),
            reasoning_content: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }

    pub fn user(content: &str) -> Self {
        Self {
            role: Role::User,
            content: Some(content.to_string()),
            reasoning_content: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }

    pub fn assistant(content: &str) -> Self {
        Self {
            role: Role::Assistant,
            content: Some(content.to_string()),
            reasoning_content: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }

    pub fn assistant_tool_calls(calls: Vec<ToolCall>, reasoning: Option<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: None,
            reasoning_content: reasoning,
            tool_call_id: None,
            tool_calls: Some(calls),
        }
    }

    pub fn tool_result(call_id: &str, content: &str) -> Self {
        Self {
            role: Role::Tool,
            content: Some(content.to_string()),
            reasoning_content: None,
            tool_call_id: Some(call_id.to_string()),
            tool_calls: None,
        }
    }
}

// ── Tool call types ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

// ── Tool definition (sent to LLM) ──

#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDefinition,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

// ── Completion result ──

#[derive(Debug)]
pub enum Completion {
    /// LLM responded with text (final answer)
    Text {
        content: String,
        reasoning: Option<String>,
    },
    /// LLM wants to call tools (with optional reasoning to echo back)
    ToolCalls {
        calls: Vec<ToolCall>,
        reasoning: Option<String>,
    },
}

// ── Usage stats ──

#[derive(Debug, Default, Clone)]
pub struct UsageStats {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

// ── Provider trait ──

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &str;

    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<(Completion, UsageStats)>;

    /// Streaming completion — sends events via channel as tokens arrive.
    /// Default implementation falls back to non-streaming complete().
    async fn complete_streaming(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        event_tx: tokio::sync::mpsc::UnboundedSender<streaming::StreamEvent>,
    ) -> Result<(Completion, UsageStats)> {
        let result = self.complete(messages, tools).await?;
        // Emit the full content as a single delta for non-streaming providers
        if let Completion::Text { ref content, .. } = result.0 {
            let _ = event_tx.send(streaming::StreamEvent::ContentDelta(content.clone()));
        }
        let _ = event_tx.send(streaming::StreamEvent::Done);
        Ok(result)
    }
}

// ── OpenAI-compatible provider ──

pub struct OpenAiCompatibleProvider {
    pub client: reqwest::Client,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub max_tokens: u32,
}

impl OpenAiCompatibleProvider {
    pub fn new(base_url: &str, api_key: &str, model: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            max_tokens: 4096,
        }
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn max_tokens(&self) -> u32 {
        self.max_tokens
    }

    /// Process a ChatResponse into a Completion + UsageStats
    fn process_chat_response(&self, chat_response: ChatResponse) -> Result<(Completion, UsageStats)> {
        let usage = chat_response
            .usage
            .map(|u| UsageStats {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            })
            .unwrap_or_default();

        let choice = chat_response
            .choices
            .into_iter()
            .next()
            .context("LLM returned no choices")?;

        if let Some(tool_calls) = choice.message.tool_calls {
            if !tool_calls.is_empty() {
                return Ok((
                    Completion::ToolCalls {
                        calls: tool_calls,
                        reasoning: choice.message.reasoning_content,
                    },
                    usage,
                ));
            }
        }

        let content = choice.message.content.unwrap_or_default();
        let reasoning = choice.message.reasoning_content;

        Ok((Completion::Text { content, reasoning }, usage))
    }
}

// ── API request/response types ──

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ToolDefinition>,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<ApiUsage>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Deserialize)]
struct ApiUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
}

#[async_trait]
impl LlmProvider for OpenAiCompatibleProvider {
    fn name(&self) -> &str {
        &self.model
    }

    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<(Completion, UsageStats)> {
        let request = ChatRequest {
            model: self.model.clone(),
            messages: messages.to_vec(),
            max_tokens: self.max_tokens,
            tools: tools.to_vec(),
        };

        // Retry with exponential backoff for transient errors (429, 502, 503, 504)
        let max_retries = 3;
        let mut last_error = None;

        for attempt in 0..=max_retries {
            if attempt > 0 {
                let delay_ms = 1000 * (1 << (attempt - 1)); // 1s, 2s, 4s
                tracing::warn!(
                    "Retrying LLM request (attempt {}/{}) after {}ms",
                    attempt + 1, max_retries + 1, delay_ms
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }

            let response = match self
                .client
                .post(format!("{}/chat/completions", self.base_url))
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(&request)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    last_error = Some(anyhow::anyhow!("Failed to send request to LLM: {}", e));
                    continue;
                }
            };

            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                let is_transient = matches!(status.as_u16(), 429 | 502 | 503 | 504);
                if is_transient && attempt < max_retries {
                    last_error = Some(anyhow::anyhow!("LLM API returned {}: {}", status, body));
                    continue;
                }
                anyhow::bail!("LLM API returned {}: {}", status, body);
            }

            let chat_response: ChatResponse = response
                .json()
                .await
                .context("Failed to parse LLM response")?;

            return self.process_chat_response(chat_response);
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("LLM request failed after retries")))
    }

    async fn complete_streaming(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        event_tx: tokio::sync::mpsc::UnboundedSender<streaming::StreamEvent>,
    ) -> Result<(Completion, UsageStats)> {
        streaming::stream_completion(
            &self.client,
            &self.base_url,
            &self.api_key,
            &self.model,
            messages,
            tools,
            self.max_tokens,
            Some(event_tx),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_serialization() {
        let msg = Message::system("You are helpful.");
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"role\":\"system\""));
        assert!(json.contains("You are helpful."));
        // tool_call_id should be omitted
        assert!(!json.contains("tool_call_id"));
    }

    #[test]
    fn test_tool_result_message() {
        let msg = Message::tool_result("call_123", "file contents here");
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"role\":\"tool\""));
        assert!(json.contains("call_123"));
    }

    #[test]
    fn test_tool_definition_serialization() {
        let def = ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: "exec".to_string(),
                description: "Execute a shell command".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "description": "The command to run"}
                    },
                    "required": ["command"]
                }),
            },
        };
        let json = serde_json::to_string(&def).unwrap();
        assert!(json.contains("\"type\":\"function\""));
        assert!(json.contains("\"name\":\"exec\""));
    }
}
