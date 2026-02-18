use anyhow::Result;
use tracing::{error, info, warn};

use openclaw_agent::llm::fallback::FallbackProvider;
use openclaw_agent::llm::{LlmProvider, OpenAiCompatibleProvider};
use openclaw_agent::runtime::{self, AgentTurnConfig};
use openclaw_agent::sessions::SessionStore;
use openclaw_agent::tools::ToolRegistry;
use openclaw_agent::workspace;

use crate::config::GatewayConfig;
use crate::telegram::{TelegramBot, TgMessage};

/// Handle an incoming Telegram message
pub async fn handle_message(
    bot: &TelegramBot,
    msg: &TgMessage,
    config: &GatewayConfig,
) -> Result<()> {
    let chat_id = msg.chat.id;
    let user = msg.from.as_ref();
    let user_id = user.map(|u| u.id).unwrap_or(0);
    let user_name = user
        .map(|u| u.first_name.as_str())
        .unwrap_or("unknown");

    let text = match &msg.text {
        Some(t) if !t.is_empty() => t.clone(),
        _ => return Ok(()),
    };

    // ── Access control ──
    if !config.telegram.allowed_user_ids.is_empty()
        && !config.telegram.allowed_user_ids.contains(&user_id)
    {
        warn!(
            "Unauthorized user {} ({}) tried to send: {}",
            user_id, user_name, &text[..text.len().min(50)]
        );
        bot.send_message(chat_id, "⛔ Unauthorized. This bot is private.")
            .await?;
        return Ok(());
    }

    info!(
        "Message from {} ({}): {}",
        user_name,
        user_id,
        &text[..text.len().min(100)]
    );

    // ── Handle commands ──
    if text.starts_with('/') {
        return handle_command(bot, chat_id, user_id, &text, config).await;
    }

    // ── Send initial "thinking" message ──
    let placeholder_id = bot
        .send_message_with_id(chat_id, "🧠 Thinking...")
        .await?;

    // ── Resolve workspace ──
    let workspace_dir = workspace::resolve_workspace_dir(&config.agent.name);
    if !workspace_dir.exists() {
        bot.edit_message(chat_id, placeholder_id, "❌ Workspace not found. Initialize OpenClaw first.")
            .await?;
        return Ok(());
    }

    // ── Resolve provider ──
    let provider: Box<dyn LlmProvider> = if config.agent.fallback {
        match FallbackProvider::from_config() {
            Ok(fb) => Box::new(fb),
            Err(e) => {
                error!("Failed to build fallback chain: {}", e);
                bot.edit_message(chat_id, placeholder_id, &format!("❌ Provider error: {}", e))
                    .await?;
                return Ok(());
            }
        }
    } else if let Some(ref model) = config.agent.model {
        match resolve_single_provider(model) {
            Ok(p) => Box::new(p),
            Err(e) => {
                error!("Failed to resolve provider: {}", e);
                bot.edit_message(chat_id, placeholder_id, &format!("❌ Provider error: {}", e))
                    .await?;
                return Ok(());
            }
        }
    } else {
        match FallbackProvider::from_config() {
            Ok(fb) => Box::new(fb),
            Err(e) => {
                bot.edit_message(chat_id, placeholder_id, &format!("❌ Provider error: {}", e))
                    .await?;
                return Ok(());
            }
        }
    };

    // ── Session: one per chat_id ──
    let session_key = format!("tg:{}:{}", config.agent.name, chat_id);
    let store = match SessionStore::open(&config.agent.name) {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to open session store: {}", e);
            bot.edit_message(chat_id, placeholder_id, &format!("❌ Session error: {}", e))
                .await?;
            return Ok(());
        }
    };

    store.create_session(&session_key, &config.agent.name, provider.name())?;

    // ── Set up tools and config ──
    let tools = ToolRegistry::with_defaults();
    let agent_config = AgentTurnConfig {
        agent_name: config.agent.name.clone(),
        session_key: session_key.clone(),
        workspace_dir: workspace_dir.to_string_lossy().to_string(),
        minimal_context: false,
    };

    // ── Run agent turn ──
    let t_start = std::time::Instant::now();

    // Keep sending typing every 5s in background
    let bot_token = config.telegram.bot_token.clone();
    let typing_handle = tokio::spawn(async move {
        let typing_bot = crate::telegram::TelegramBot::new(&bot_token);
        loop {
            let _ = typing_bot.send_typing(chat_id).await;
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });

    // Update placeholder with round progress
    let progress_bot = crate::telegram::TelegramBot::new(&config.telegram.bot_token);
    let progress_placeholder = placeholder_id;

    let result = runtime::run_agent_turn(provider.as_ref(), &text, &agent_config, &tools).await;

    typing_handle.abort();

    match result {
        Ok(turn_result) => {
            let elapsed = t_start.elapsed().as_millis();

            // Persist messages
            store.append_message(
                &session_key,
                &openclaw_agent::llm::Message::user(&text),
            )?;
            if !turn_result.response.is_empty() {
                store.append_message(
                    &session_key,
                    &openclaw_agent::llm::Message::assistant(&turn_result.response),
                )?;
            }
            store.add_tokens(&session_key, turn_result.total_usage.total_tokens as i64)?;

            // Build response
            let response = if !turn_result.response.is_empty() {
                turn_result.response.clone()
            } else if let Some(ref reasoning) = turn_result.reasoning {
                reasoning.clone()
            } else {
                "(no response)".to_string()
            };

            // Stats footer with model name
            let stats = format!(
                "\n\n{}ms · {} round(s) · {} tool(s) · {} tokens · {}",
                elapsed,
                turn_result.total_rounds,
                turn_result.tool_calls_made,
                turn_result.total_usage.total_tokens,
                turn_result.model_name,
            );

            let full_response = format!("{}{}", response, stats);

            // Edit the placeholder with the final response
            // For long responses, we need to split and send new messages
            if full_response.len() <= 4000 {
                bot.edit_message(chat_id, placeholder_id, &full_response)
                    .await?;
            } else {
                // Delete placeholder by editing to first chunk, send rest as new messages
                let chunks = split_for_telegram(&full_response, 4000);
                if let Some(first) = chunks.first() {
                    bot.edit_message(chat_id, placeholder_id, first).await?;
                }
                for chunk in chunks.iter().skip(1) {
                    bot.send_message(chat_id, chunk).await?;
                }
            }

            info!(
                "Reply sent: {}ms, {} rounds, {} tools, {} tokens, model={}",
                elapsed,
                turn_result.total_rounds,
                turn_result.tool_calls_made,
                turn_result.total_usage.total_tokens,
                turn_result.model_name,
            );
        }
        Err(e) => {
            error!("Agent turn failed: {}", e);
            bot.edit_message(chat_id, placeholder_id, &format!("❌ Agent error: {}", e))
                .await?;
        }
    }

    Ok(())
}

async fn handle_command(
    bot: &TelegramBot,
    chat_id: i64,
    user_id: i64,
    text: &str,
    config: &GatewayConfig,
) -> Result<()> {
    let cmd = text.split_whitespace().next().unwrap_or("");

    match cmd {
        "/start" | "/help" => {
            bot.send_message(
                chat_id,
                "🦀 *Rustbot* online.\n\n\
                I'm the Rust-powered OpenClaw agent. Send me a message and I'll process it with tools (exec, read, write) and workspace context.\n\n\
                *Commands:*\n\
                /new — start a new session\n\
                /status — show bot status\n\
                /model — show current model info\n\
                /sessions — list recent sessions\n\
                /help — show this help",
            )
            .await?;
        }
        "/new" | "/reset" => {
            let store = SessionStore::open(&config.agent.name)?;
            let old_key = format!("tg:{}:{}", config.agent.name, chat_id);

            // Check if there was an existing session
            let old_msgs = store.load_messages(&old_key)?;
            let msg_count = old_msgs.len();

            // The next message will create a fresh session automatically
            // (since create_session uses INSERT OR IGNORE, we need a new key)
            // We'll use a generation counter approach
            let new_key = format!("tg:{}:{}:{}", config.agent.name, chat_id, uuid::Uuid::new_v4());
            store.create_session(&new_key, &config.agent.name, "pending")?;

            bot.send_message(
                chat_id,
                &format!(
                    "🔄 *New session started.*\n\nPrevious session had {} messages. Starting fresh.",
                    msg_count
                ),
            )
            .await?;
        }
        "/status" => {
            let store = SessionStore::open(&config.agent.name)?;
            let session_key = format!("tg:{}:{}", config.agent.name, chat_id);
            let sessions = store.list_sessions(&config.agent.name, 100)?;
            let current = sessions.iter().find(|s| s.session_key == session_key);

            let msg_count = current.map(|s| s.message_count).unwrap_or(0);
            let tokens = current.map(|s| s.total_tokens).unwrap_or(0);

            // Get fallback chain labels
            let chain_info = if config.agent.fallback {
                match FallbackProvider::from_config() {
                    Ok(fb) => {
                        let labels = fb.provider_labels().join(" → ");
                        format!("Chain: {}", labels)
                    }
                    Err(_) => "Chain: (error loading)".to_string(),
                }
            } else {
                format!("Model: {}", config.agent.model.as_deref().unwrap_or("default"))
            };

            let status = format!(
                "🦀 *Rustbot Status*\n\n\
                Agent: `{}`\n\
                Fallback: {}\n\
                {}\n\
                Session: `{}`\n\
                Messages: {}\n\
                Tokens used: {}\n\
                Total sessions: {}",
                config.agent.name,
                if config.agent.fallback { "✅ enabled" } else { "❌ disabled" },
                chain_info,
                session_key,
                msg_count,
                tokens,
                sessions.len(),
            );

            bot.send_message(chat_id, &status).await?;
        }
        "/model" => {
            let chain_info = if config.agent.fallback {
                match FallbackProvider::from_config() {
                    Ok(fb) => {
                        let labels = fb.provider_labels();
                        let mut info = String::from("🔗 *Fallback Chain:*\n\n");
                        for (i, label) in labels.iter().enumerate() {
                            let marker = if i == 0 { "🥇" } else if i == 1 { "🥈" } else { "🥉" };
                            info.push_str(&format!("{} `{}`\n", marker, label));
                        }
                        info.push_str("\nFirst available model is used. Failed models are skipped (circuit breaker at >3 failures).");
                        info
                    }
                    Err(e) => format!("❌ Error loading providers: {}", e),
                }
            } else {
                format!(
                    "🤖 *Model:* `{}`",
                    config.agent.model.as_deref().unwrap_or("default")
                )
            };

            bot.send_message(chat_id, &chain_info).await?;
        }
        "/sessions" => {
            let store = SessionStore::open(&config.agent.name)?;
            let sessions = store.list_sessions(&config.agent.name, 10)?;

            if sessions.is_empty() {
                bot.send_message(chat_id, "No sessions found.").await?;
            } else {
                let mut msg = String::from("📋 *Recent Sessions:*\n\n");
                for (i, s) in sessions.iter().enumerate() {
                    let age = chrono::Utc::now().timestamp_millis() - s.updated_at_ms;
                    let age_str = format_duration(age);
                    msg.push_str(&format!(
                        "{}. `{}` — {} msgs, {} tokens, {}\n",
                        i + 1,
                        &s.session_key[..s.session_key.len().min(30)],
                        s.message_count,
                        s.total_tokens,
                        age_str,
                    ));
                }
                bot.send_message(chat_id, &msg).await?;
            }
        }
        _ => {
            bot.send_message(
                chat_id,
                "Unknown command. Try /help for available commands.",
            )
            .await?;
        }
    }

    Ok(())
}

fn resolve_single_provider(model_spec: &str) -> Result<OpenAiCompatibleProvider> {
    let config_path = openclaw_core::paths::manual_config_path();
    let content = std::fs::read_to_string(&config_path)?;
    let config: serde_json::Value = serde_json::from_str(&content)?;

    let providers = config
        .get("models")
        .and_then(|m| m.get("providers"))
        .and_then(|p| p.as_object())
        .ok_or_else(|| anyhow::anyhow!("No providers in config"))?;

    for (_name, provider) in providers {
        if let Some(base_url) = provider.get("baseUrl").and_then(|v| v.as_str()) {
            let api_key = provider
                .get("apiKey")
                .and_then(|v| v.as_str())
                .unwrap_or("ollama-local");

            if let Some(models) = provider.get("models").and_then(|m| m.as_array()) {
                for model in models {
                    if let Some(id) = model.get("id").and_then(|v| v.as_str()) {
                        if id == model_spec || format!("{}/{}", _name, id) == model_spec {
                            return Ok(OpenAiCompatibleProvider::new(base_url, api_key, id));
                        }
                    }
                }
            }
        }
    }

    anyhow::bail!("Model '{}' not found in any provider", model_spec)
}

fn split_for_telegram(text: &str, max_len: usize) -> Vec<String> {
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
        remaining = remaining[split_at..].trim_start_matches('\n');
    }

    chunks
}

fn format_duration(ms: i64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}
