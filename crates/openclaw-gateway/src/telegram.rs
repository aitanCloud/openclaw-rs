use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{error, warn};

/// Telegram Bot API client using raw reqwest (no framework bloat)
pub struct TelegramBot {
    client: reqwest::Client,
    token: String,
    api_base: String,
}

// ── Telegram API types ──

#[derive(Debug, Deserialize)]
pub struct TgResponse<T> {
    pub ok: bool,
    pub result: Option<T>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TgUpdate {
    pub update_id: i64,
    pub message: Option<TgMessage>,
}

#[derive(Debug, Deserialize)]
pub struct TgMessage {
    pub message_id: i64,
    pub from: Option<TgUser>,
    pub chat: TgChat,
    pub text: Option<String>,
    pub date: i64,
}

#[derive(Debug, Deserialize)]
pub struct TgUser {
    pub id: i64,
    pub first_name: String,
    pub last_name: Option<String>,
    pub username: Option<String>,
    pub language_code: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TgChat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
}

#[derive(Debug, Deserialize)]
pub struct TgSentMessage {
    pub message_id: i64,
}

#[derive(Debug, Serialize)]
struct SendMessageRequest {
    chat_id: i64,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parse_mode: Option<String>,
}

#[derive(Debug, Serialize)]
struct EditMessageRequest {
    chat_id: i64,
    message_id: i64,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parse_mode: Option<String>,
}

#[derive(Debug, Serialize)]
struct SendChatActionRequest {
    chat_id: i64,
    action: String,
}

impl TelegramBot {
    pub fn new(token: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            token: token.to_string(),
            api_base: format!("https://api.telegram.org/bot{}", token),
        }
    }

    /// Long-poll for updates
    pub async fn get_updates(&self, offset: i64, timeout: u64) -> Result<Vec<TgUpdate>> {
        let url = format!(
            "{}/getUpdates?offset={}&timeout={}&allowed_updates=[\"message\"]",
            self.api_base, offset, timeout
        );

        let resp: TgResponse<Vec<TgUpdate>> = self
            .client
            .get(&url)
            .timeout(std::time::Duration::from_secs(timeout + 10))
            .send()
            .await
            .context("Failed to poll Telegram")?
            .json()
            .await
            .context("Failed to parse Telegram response")?;

        if !resp.ok {
            anyhow::bail!(
                "Telegram API error: {}",
                resp.description.unwrap_or_default()
            );
        }

        Ok(resp.result.unwrap_or_default())
    }

    /// Send a text message, returning the message_id for later editing
    pub async fn send_message_with_id(&self, chat_id: i64, text: &str) -> Result<i64> {
        let body = SendMessageRequest {
            chat_id,
            text: text.to_string(),
            parse_mode: None,
        };

        let resp = self
            .client
            .post(format!("{}/sendMessage", self.api_base))
            .json(&body)
            .send()
            .await
            .context("Failed to send message")?;

        let resp_body: TgResponse<TgSentMessage> = resp
            .json()
            .await
            .context("Failed to parse sendMessage response")?;

        if let Some(msg) = resp_body.result {
            Ok(msg.message_id)
        } else {
            anyhow::bail!(
                "sendMessage failed: {}",
                resp_body.description.unwrap_or_default()
            )
        }
    }

    /// Send a text message (fire-and-forget, with Markdown fallback)
    pub async fn send_message(&self, chat_id: i64, text: &str) -> Result<()> {
        let chunks = split_message(text, 4000);

        for chunk in chunks {
            let body = SendMessageRequest {
                chat_id,
                text: chunk,
                parse_mode: Some("Markdown".to_string()),
            };

            let resp = self
                .client
                .post(format!("{}/sendMessage", self.api_base))
                .json(&body)
                .send()
                .await;

            match resp {
                Ok(r) => {
                    if !r.status().is_success() {
                        let err_body = r.text().await.unwrap_or_default();
                        warn!("sendMessage failed with Markdown, retrying plain: {}", err_body);
                        let plain_body = SendMessageRequest {
                            chat_id,
                            text: body.text.clone(),
                            parse_mode: None,
                        };
                        let _ = self
                            .client
                            .post(format!("{}/sendMessage", self.api_base))
                            .json(&plain_body)
                            .send()
                            .await;
                    }
                }
                Err(e) => {
                    error!("Failed to send message: {}", e);
                }
            }
        }

        Ok(())
    }

    /// Edit an existing message (for streaming updates)
    pub async fn edit_message(&self, chat_id: i64, message_id: i64, text: &str) -> Result<()> {
        let body = EditMessageRequest {
            chat_id,
            message_id,
            text: text.to_string(),
            parse_mode: None,
        };

        let resp = self
            .client
            .post(format!("{}/editMessageText", self.api_base))
            .json(&body)
            .send()
            .await;

        match resp {
            Ok(r) => {
                if !r.status().is_success() {
                    // Telegram returns 400 if text hasn't changed — ignore
                    let status = r.status();
                    if status.as_u16() != 400 {
                        let err_body = r.text().await.unwrap_or_default();
                        warn!("editMessageText failed ({}): {}", status, err_body);
                    }
                }
            }
            Err(e) => {
                warn!("Failed to edit message: {}", e);
            }
        }

        Ok(())
    }

    /// Edit an existing message with Markdown formatting, falling back to plain text
    pub async fn edit_message_markdown(&self, chat_id: i64, message_id: i64, text: &str) -> Result<()> {
        let body = EditMessageRequest {
            chat_id,
            message_id,
            text: text.to_string(),
            parse_mode: Some("Markdown".to_string()),
        };

        let resp = self
            .client
            .post(format!("{}/editMessageText", self.api_base))
            .json(&body)
            .send()
            .await;

        match resp {
            Ok(r) => {
                if !r.status().is_success() {
                    let status = r.status();
                    if status.as_u16() == 400 {
                        // Could be markdown parse error or "message not modified" — try plain
                        let plain_body = EditMessageRequest {
                            chat_id,
                            message_id,
                            text: text.to_string(),
                            parse_mode: None,
                        };
                        let _ = self
                            .client
                            .post(format!("{}/editMessageText", self.api_base))
                            .json(&plain_body)
                            .send()
                            .await;
                    } else {
                        let err_body = r.text().await.unwrap_or_default();
                        warn!("editMessageText markdown failed ({}): {}", status, err_body);
                    }
                }
            }
            Err(e) => {
                warn!("Failed to edit message (markdown): {}", e);
            }
        }

        Ok(())
    }

    /// Send "typing..." indicator
    pub async fn send_typing(&self, chat_id: i64) -> Result<()> {
        let body = SendChatActionRequest {
            chat_id,
            action: "typing".to_string(),
        };

        let _ = self
            .client
            .post(format!("{}/sendChatAction", self.api_base))
            .json(&body)
            .send()
            .await;

        Ok(())
    }

    /// Get bot info (for startup verification)
    pub async fn get_me(&self) -> Result<TgUser> {
        let resp: TgResponse<TgUser> = self
            .client
            .get(format!("{}/getMe", self.api_base))
            .send()
            .await
            .context("Failed to call getMe")?
            .json()
            .await
            .context("Failed to parse getMe response")?;

        resp.result.context("getMe returned no result")
    }
}

/// Split long messages at newline boundaries
fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            chunks.push(remaining.to_string());
            break;
        }

        let split_at = remaining[..max_len]
            .rfind('\n')
            .unwrap_or(max_len);

        chunks.push(remaining[..split_at].to_string());
        remaining = &remaining[split_at..].trim_start_matches('\n');
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_short_message() {
        let chunks = split_message("hello", 4000);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn test_split_long_message() {
        let text = "line1\nline2\nline3\nline4";
        let chunks = split_message(text, 12);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].len() <= 12);
    }
}
