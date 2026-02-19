use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

/// Discord bot client using raw HTTP + WebSocket Gateway
pub struct DiscordBot {
    client: reqwest::Client,
    token: String,
    api_base: String,
}

// ── Discord API types ──

#[derive(Debug, Clone, Deserialize)]
pub struct DiscordMessage {
    pub id: String,
    pub channel_id: String,
    pub content: String,
    pub author: DiscordUser,
    #[serde(default)]
    pub guild_id: Option<String>,
    #[serde(default)]
    pub attachments: Vec<DiscordAttachment>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiscordUser {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub discriminator: Option<String>,
    #[serde(default)]
    pub bot: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiscordAttachment {
    pub id: String,
    pub filename: String,
    pub url: String,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct GatewayResponse {
    url: String,
}

#[derive(Debug, Deserialize)]
struct GatewayPayload {
    op: u8,
    #[serde(default)]
    d: Option<Value>,
    #[serde(default)]
    s: Option<u64>,
    #[serde(default)]
    t: Option<String>,
}

#[derive(Debug, Serialize)]
struct GatewayIdentify {
    op: u8,
    d: IdentifyData,
}

#[derive(Debug, Serialize)]
struct IdentifyData {
    token: String,
    intents: u64,
    properties: IdentifyProperties,
}

#[derive(Debug, Serialize)]
struct IdentifyProperties {
    os: String,
    browser: String,
    device: String,
}

#[derive(Debug, Serialize)]
struct HeartbeatPayload {
    op: u8,
    d: Option<u64>,
}

#[derive(Debug, Serialize)]
struct CreateMessageRequest {
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_reference: Option<MessageReference>,
}

#[derive(Debug, Serialize)]
struct MessageReference {
    message_id: String,
}

#[derive(Debug, Serialize)]
struct EditMessageRequest {
    content: String,
}

/// Discord Gateway intents
const INTENT_GUILDS: u64 = 1 << 0;
const INTENT_GUILD_MESSAGES: u64 = 1 << 9;
const INTENT_MESSAGE_CONTENT: u64 = 1 << 15;
const INTENT_DIRECT_MESSAGES: u64 = 1 << 12;

impl DiscordBot {
    pub fn new(token: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            token: token.to_string(),
            api_base: "https://discord.com/api/v10".to_string(),
        }
    }

    /// Send a message to a channel, returns message ID
    pub async fn send_message(&self, channel_id: &str, content: &str) -> Result<String> {
        let chunks = split_message(content, 2000);
        let mut last_id = String::new();

        for chunk in chunks {
            let body = CreateMessageRequest {
                content: chunk,
                message_reference: None,
            };

            let resp = self
                .client
                .post(format!("{}/channels/{}/messages", self.api_base, channel_id))
                .header("Authorization", format!("Bot {}", self.token))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .context("Failed to send Discord message")?;

            if !resp.status().is_success() {
                let status = resp.status();
                let err_body = resp.text().await.unwrap_or_default();
                anyhow::bail!("Discord sendMessage failed ({}): {}", status, err_body);
            }

            let msg: Value = resp.json().await?;
            last_id = msg["id"].as_str().unwrap_or("").to_string();
        }

        Ok(last_id)
    }

    /// Send a reply to a specific message
    pub async fn send_reply(
        &self,
        channel_id: &str,
        reply_to: &str,
        content: &str,
    ) -> Result<String> {
        let chunks = split_message(content, 2000);
        let mut last_id = String::new();

        for (i, chunk) in chunks.iter().enumerate() {
            let body = CreateMessageRequest {
                content: chunk.clone(),
                message_reference: if i == 0 {
                    Some(MessageReference {
                        message_id: reply_to.to_string(),
                    })
                } else {
                    None
                },
            };

            let resp = self
                .client
                .post(format!("{}/channels/{}/messages", self.api_base, channel_id))
                .header("Authorization", format!("Bot {}", self.token))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .context("Failed to send Discord reply")?;

            if !resp.status().is_success() {
                let status = resp.status();
                let err_body = resp.text().await.unwrap_or_default();
                warn!("Discord reply failed ({}): {}", status, err_body);
            } else {
                let msg: Value = resp.json().await?;
                last_id = msg["id"].as_str().unwrap_or("").to_string();
            }
        }

        Ok(last_id)
    }

    /// Edit an existing message
    pub async fn edit_message(
        &self,
        channel_id: &str,
        message_id: &str,
        content: &str,
    ) -> Result<()> {
        // Discord max message length is 2000
        let truncated = if content.len() > 2000 {
            format!("{}...", &content[..1997])
        } else {
            content.to_string()
        };

        let body = EditMessageRequest { content: truncated };

        let resp = self
            .client
            .patch(format!(
                "{}/channels/{}/messages/{}",
                self.api_base, channel_id, message_id
            ))
            .header("Authorization", format!("Bot {}", self.token))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await;

        match resp {
            Ok(r) => {
                if !r.status().is_success() {
                    let status = r.status();
                    if status.as_u16() != 400 {
                        let err_body = r.text().await.unwrap_or_default();
                        warn!("Discord editMessage failed ({}): {}", status, err_body);
                    }
                }
            }
            Err(e) => {
                warn!("Failed to edit Discord message: {}", e);
            }
        }

        Ok(())
    }

    /// Send a file (e.g. voice OGG) to a channel with optional text content
    pub async fn send_file(
        &self,
        channel_id: &str,
        file_path: &std::path::Path,
        filename: &str,
        content: Option<&str>,
    ) -> Result<String> {
        let file_bytes = tokio::fs::read(file_path).await
            .context("Failed to read file for Discord upload")?;

        let file_part = reqwest::multipart::Part::bytes(file_bytes)
            .file_name(filename.to_string());

        let mut form = reqwest::multipart::Form::new()
            .part("files[0]", file_part);

        if let Some(text) = content {
            form = form.text("content", text.to_string());
        }

        let resp = self
            .client
            .post(format!("{}/channels/{}/messages", self.api_base, channel_id))
            .header("Authorization", format!("Bot {}", self.token))
            .multipart(form)
            .send()
            .await
            .context("Failed to send Discord file")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Discord file upload failed ({}): {}", status, err_body);
        }

        let msg: serde_json::Value = resp.json().await?;
        Ok(msg["id"].as_str().unwrap_or("").to_string())
    }

    /// Send typing indicator
    pub async fn send_typing(&self, channel_id: &str) -> Result<()> {
        let _ = self
            .client
            .post(format!(
                "{}/channels/{}/typing",
                self.api_base, channel_id
            ))
            .header("Authorization", format!("Bot {}", self.token))
            .send()
            .await;
        Ok(())
    }

    /// Get current bot user info
    pub async fn get_me(&self) -> Result<DiscordUser> {
        let resp = self
            .client
            .get(format!("{}/users/@me", self.api_base))
            .header("Authorization", format!("Bot {}", self.token))
            .send()
            .await
            .context("Failed to call Discord /users/@me")?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Discord auth failed: {}", err);
        }

        resp.json().await.context("Failed to parse Discord user")
    }

    /// Get the Gateway WebSocket URL
    async fn get_gateway_url(&self) -> Result<String> {
        let resp: GatewayResponse = self
            .client
            .get(format!("{}/gateway/bot", self.api_base))
            .header("Authorization", format!("Bot {}", self.token))
            .send()
            .await
            .context("Failed to get Discord gateway URL")?
            .json()
            .await
            .context("Failed to parse gateway response")?;

        Ok(format!("{}/?v=10&encoding=json", resp.url))
    }

    /// Connect to the Discord Gateway via WebSocket and stream messages.
    /// Returns a receiver that yields DiscordMessage events.
    pub async fn connect_gateway(
        self: Arc<Self>,
    ) -> Result<mpsc::UnboundedReceiver<DiscordMessage>> {
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();

        let gateway_url = self.get_gateway_url().await?;
        info!("Connecting to Discord Gateway: {}", gateway_url);

        let token = self.token.clone();

        tokio::spawn(async move {
            if let Err(e) = run_gateway_loop(&gateway_url, &token, msg_tx).await {
                error!("Discord Gateway error: {}", e);
            }
        });

        Ok(msg_rx)
    }
}

/// State for Discord Gateway Resume (avoids re-Identify on reconnect)
struct ResumeState {
    session_id: String,
    sequence: u64,
    resume_url: Option<String>,
}

/// Main Gateway WebSocket loop with reconnection and exponential backoff
async fn run_gateway_loop(
    gateway_url: &str,
    token: &str,
    msg_tx: mpsc::UnboundedSender<DiscordMessage>,
) -> Result<()> {
    let mut reconnect_delay = 1u64;
    let mut consecutive_failures = 0u32;
    let mut resume_state: Option<ResumeState> = None;

    loop {
        let session_start = std::time::Instant::now();
        let connect_url = resume_state.as_ref()
            .and_then(|r| r.resume_url.clone())
            .unwrap_or_else(|| gateway_url.to_string());

        match run_gateway_session(&connect_url, token, &msg_tx, &mut resume_state).await {
            Ok(()) => {
                // Clean close — still reconnect (Discord may close for maintenance)
                info!("Discord Gateway session ended cleanly, reconnecting in 5s...");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;

                // Reset backoff if session lasted > 30s (was healthy)
                if session_start.elapsed().as_secs() > 30 {
                    reconnect_delay = 1;
                    consecutive_failures = 0;
                }
            }
            Err(e) => {
                consecutive_failures += 1;
                let err_str = e.to_string();
                error!(
                    "Discord Gateway session error (attempt {}): {}. Reconnecting in {}s...",
                    consecutive_failures, err_str, reconnect_delay
                );

                // Invalid session means we must re-Identify
                if err_str.contains("Invalid session") {
                    info!("Clearing resume state — will re-Identify");
                    resume_state = None;
                }

                tokio::time::sleep(std::time::Duration::from_secs(reconnect_delay)).await;
                reconnect_delay = (reconnect_delay * 2).min(120);

                // Reset backoff if session lasted > 60s (was healthy before failing)
                if session_start.elapsed().as_secs() > 60 {
                    reconnect_delay = 1;
                    consecutive_failures = 0;
                }
            }
        }

        // Safety: if receiver is closed, stop reconnecting
        if msg_tx.is_closed() {
            info!("Discord message channel closed, stopping gateway loop");
            break;
        }
    }

    Ok(())
}

/// Single Gateway WebSocket session
async fn run_gateway_session(
    gateway_url: &str,
    token: &str,
    msg_tx: &mpsc::UnboundedSender<DiscordMessage>,
    resume_state: &mut Option<ResumeState>,
) -> Result<()> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(gateway_url)
        .await
        .context("Failed to connect to Discord Gateway WebSocket")?;

    let (mut ws_write, mut ws_read) = ws_stream.split();

    // Wait for Hello (op 10) to get heartbeat interval
    let hello = ws_read
        .next()
        .await
        .ok_or_else(|| anyhow::anyhow!("Gateway closed before Hello"))??;

    let hello_payload: GatewayPayload =
        serde_json::from_str(&hello.to_text().unwrap_or("{}"))
            .context("Failed to parse Hello")?;

    if hello_payload.op != 10 {
        anyhow::bail!("Expected Hello (op 10), got op {}", hello_payload.op);
    }

    let heartbeat_interval = hello_payload
        .d
        .as_ref()
        .and_then(|d| d["heartbeat_interval"].as_u64())
        .unwrap_or(41250);

    info!(
        "Discord Gateway Hello, heartbeat interval: {}ms",
        heartbeat_interval
    );

    // Resume or Identify
    if let Some(ref state) = resume_state {
        info!("Attempting Gateway Resume (session_id={}, seq={})", &state.session_id[..8.min(state.session_id.len())], state.sequence);
        let resume = serde_json::json!({
            "op": 6,
            "d": {
                "token": token,
                "session_id": state.session_id,
                "seq": state.sequence,
            }
        });
        ws_write
            .send(tokio_tungstenite::tungstenite::Message::Text(resume.to_string().into()))
            .await
            .context("Failed to send Resume")?;
    } else {
        let identify = GatewayIdentify {
            op: 2,
            d: IdentifyData {
                token: token.to_string(),
                intents: INTENT_GUILDS
                    | INTENT_GUILD_MESSAGES
                    | INTENT_MESSAGE_CONTENT
                    | INTENT_DIRECT_MESSAGES,
                properties: IdentifyProperties {
                    os: "linux".to_string(),
                    browser: "openclaw-rs".to_string(),
                    device: "openclaw-rs".to_string(),
                },
            },
        };
        let identify_json = serde_json::to_string(&identify)?;
        ws_write
            .send(tokio_tungstenite::tungstenite::Message::Text(identify_json.into()))
            .await
            .context("Failed to send Identify")?;
    }

    // Heartbeat + message loop
    let mut sequence: Option<u64> = resume_state.as_ref().map(|r| r.sequence);
    let mut session_id: Option<String> = resume_state.as_ref().map(|r| r.session_id.clone());
    let mut resume_gateway_url: Option<String> = None;
    let mut heartbeat_interval =
        tokio::time::interval(std::time::Duration::from_millis(heartbeat_interval));
    let mut last_heartbeat_ack = Instant::now();
    let mut awaiting_ack = false;
    let heartbeat_timeout = std::time::Duration::from_secs(45);

    loop {
        tokio::select! {
            _ = heartbeat_interval.tick() => {
                // Check for missed ACK before sending next heartbeat
                if awaiting_ack && last_heartbeat_ack.elapsed() > heartbeat_timeout {
                    warn!("Heartbeat ACK timeout ({}s) — forcing reconnect",
                        last_heartbeat_ack.elapsed().as_secs());
                    // Save resume state
                    if let (Some(sid), Some(seq)) = (&session_id, sequence) {
                        *resume_state = Some(ResumeState {
                            session_id: sid.clone(),
                            sequence: seq,
                            resume_url: resume_gateway_url.clone(),
                        });
                    }
                    return Err(anyhow::anyhow!("Heartbeat ACK timeout"));
                }

                let hb = HeartbeatPayload { op: 1, d: sequence };
                let hb_json = serde_json::to_string(&hb)?;
                ws_write
                    .send(tokio_tungstenite::tungstenite::Message::Text(hb_json.into()))
                    .await
                    .context("Failed to send heartbeat")?;
                awaiting_ack = true;
                debug!("Sent heartbeat (seq: {:?})", sequence);
            }
            msg = ws_read.next() => {
                match msg {
                    Some(Ok(ws_msg)) => {
                        if ws_msg.is_close() {
                            info!("Discord Gateway closed");
                            return Ok(());
                        }
                        if let Ok(text) = ws_msg.to_text() {
                            if let Ok(payload) = serde_json::from_str::<GatewayPayload>(text) {
                                if let Some(s) = payload.s {
                                    sequence = Some(s);
                                }

                                match payload.op {
                                    0 => {
                                        // Dispatch event
                                        if let Some(ref event_name) = payload.t {
                                            if event_name == "MESSAGE_CREATE" {
                                                if let Some(ref d) = payload.d {
                                                    match serde_json::from_value::<DiscordMessage>(d.clone()) {
                                                        Ok(discord_msg) => {
                                                            // Skip bot messages
                                                            if discord_msg.author.bot.unwrap_or(false) {
                                                                continue;
                                                            }
                                                            let _ = msg_tx.send(discord_msg);
                                                        }
                                                        Err(e) => {
                                                            debug!("Failed to parse MESSAGE_CREATE: {}", e);
                                                        }
                                                    }
                                                }
                                            } else if event_name == "READY" {
                                                if let Some(ref d) = payload.d {
                                                    let user = d["user"]["username"].as_str().unwrap_or("?");
                                                    if let Some(sid) = d["session_id"].as_str() {
                                                        session_id = Some(sid.to_string());
                                                    }
                                                    if let Some(url) = d["resume_gateway_url"].as_str() {
                                                        resume_gateway_url = Some(format!("{}/?v=10&encoding=json", url));
                                                    }
                                                    info!("Discord Gateway READY as {} (session={})", user,
                                                        session_id.as_deref().unwrap_or("?"));
                                                }
                                            } else if event_name == "RESUMED" {
                                                info!("Discord Gateway RESUMED successfully");
                                            }
                                        }
                                    }
                                    11 => {
                                        // Heartbeat ACK
                                        last_heartbeat_ack = Instant::now();
                                        awaiting_ack = false;
                                        debug!("Heartbeat ACK received");
                                    }
                                    7 => {
                                        // Reconnect requested — save state for Resume
                                        if let (Some(sid), Some(seq)) = (&session_id, sequence) {
                                            *resume_state = Some(ResumeState {
                                                session_id: sid.clone(),
                                                sequence: seq,
                                                resume_url: resume_gateway_url.clone(),
                                            });
                                        }
                                        info!("Discord Gateway requested reconnect");
                                        return Err(anyhow::anyhow!("Reconnect requested"));
                                    }
                                    9 => {
                                        // Invalid session — check if resumable
                                        let resumable = payload.d.as_ref()
                                            .and_then(|d| d.as_bool())
                                            .unwrap_or(false);
                                        if !resumable {
                                            warn!("Discord Gateway: invalid session (not resumable)");
                                            *resume_state = None;
                                        } else {
                                            warn!("Discord Gateway: invalid session (resumable)");
                                        }
                                        return Err(anyhow::anyhow!("Invalid session"));
                                    }
                                    _ => {
                                        debug!("Gateway op {}", payload.op);
                                    }
                                }
                            }
                        }
                    }
                    Some(Err(e)) => {
                        return Err(anyhow::anyhow!("WebSocket error: {}", e));
                    }
                    None => {
                        // Save resume state before returning
                        if let (Some(sid), Some(seq)) = (&session_id, sequence) {
                            *resume_state = Some(ResumeState {
                                session_id: sid.clone(),
                                sequence: seq,
                                resume_url: resume_gateway_url.clone(),
                            });
                        }
                        return Ok(());
                    }
                }
            }
        }
    }
}

/// Split long messages at newline boundaries (Discord max 2000 chars)
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
        let split_at = remaining[..max_len].rfind('\n').unwrap_or(max_len);
        chunks.push(remaining[..split_at].to_string());
        remaining = remaining[split_at..].trim_start_matches('\n');
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_short_message() {
        let chunks = split_message("hello", 2000);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn test_split_long_message() {
        let text = "line1\nline2\nline3\nline4";
        let chunks = split_message(text, 12);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].len() <= 12);
    }

    #[test]
    fn test_intents() {
        let intents =
            INTENT_GUILDS | INTENT_GUILD_MESSAGES | INTENT_MESSAGE_CONTENT | INTENT_DIRECT_MESSAGES;
        // Should include message content intent (privileged)
        assert!(intents & INTENT_MESSAGE_CONTENT != 0);
        assert!(intents & INTENT_GUILD_MESSAGES != 0);
    }
}
