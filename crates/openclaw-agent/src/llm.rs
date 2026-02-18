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

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send request to LLM")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("LLM API returned {}: {}", status, body);
        }

        let chat_response: ChatResponse = response
            .json()
            .await
            .context("Failed to parse LLM response")?;

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

        // If the model returned tool calls, return those
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

        // Otherwise it's a text response
        let content = choice.message.content.unwrap_or_default();
        let reasoning = choice.message.reasoning_content;

        Ok((Completion::Text { content, reasoning }, usage))
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
