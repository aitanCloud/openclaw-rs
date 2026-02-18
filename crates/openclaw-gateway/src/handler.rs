use anyhow::Result;
use std::sync::Arc;
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
        _ => return Ok(()), // Ignore non-text messages
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
        return handle_command(bot, chat_id, &text, config).await;
    }

    // ── Send typing indicator ──
    bot.send_typing(chat_id).await?;

    // ── Resolve workspace ──
    let workspace_dir = workspace::resolve_workspace_dir(&config.agent.name);
    if !workspace_dir.exists() {
        bot.send_message(
            chat_id,
            "❌ Workspace not found. Initialize OpenClaw first.",
        )
        .await?;
        return Ok(());
    }

    // ── Resolve provider ──
    let provider: Box<dyn LlmProvider> = if config.agent.fallback {
        match FallbackProvider::from_config() {
            Ok(fb) => Box::new(fb),
            Err(e) => {
                error!("Failed to build fallback chain: {}", e);
                bot.send_message(chat_id, &format!("❌ Provider error: {}", e))
                    .await?;
                return Ok(());
            }
        }
    } else if let Some(ref model) = config.agent.model {
        // Use specific model from config
        match resolve_single_provider(model) {
            Ok(p) => Box::new(p),
            Err(e) => {
                error!("Failed to resolve provider: {}", e);
                bot.send_message(chat_id, &format!("❌ Provider error: {}", e))
                    .await?;
                return Ok(());
            }
        }
    } else {
        match FallbackProvider::from_config() {
            Ok(fb) => Box::new(fb),
            Err(e) => {
                bot.send_message(chat_id, &format!("❌ Provider error: {}", e))
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
            bot.send_message(chat_id, &format!("❌ Session error: {}", e))
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

            // Build response with stats footer
            let response = if !turn_result.response.is_empty() {
                turn_result.response.clone()
            } else if let Some(ref reasoning) = turn_result.reasoning {
                reasoning.clone()
            } else {
                "(no response)".to_string()
            };

            let stats = format!(
                "\n\n_{}ms · {} round(s) · {} tool(s) · {} tokens_",
                elapsed,
                turn_result.total_rounds,
                turn_result.tool_calls_made,
                turn_result.total_usage.total_tokens,
            );

            bot.send_message(chat_id, &format!("{}{}", response, stats))
                .await?;

            info!(
                "Reply sent: {}ms, {} rounds, {} tools, {} tokens",
                elapsed,
                turn_result.total_rounds,
                turn_result.tool_calls_made,
                turn_result.total_usage.total_tokens,
            );
        }
        Err(e) => {
            error!("Agent turn failed: {}", e);
            bot.send_message(chat_id, &format!("❌ Agent error: {}", e))
                .await?;
        }
    }

    Ok(())
}

async fn handle_command(
    bot: &TelegramBot,
    chat_id: i64,
    text: &str,
    config: &GatewayConfig,
) -> Result<()> {
    let cmd = text.split_whitespace().next().unwrap_or("");

    match cmd {
        "/start" => {
            bot.send_message(
                chat_id,
                "🦀 *Rustbot* online.\n\nI'm the Rust-powered OpenClaw agent. Send me a message and I'll process it with tools (exec, read, write) and workspace context.\n\n/status — show bot status\n/reset — clear session history",
            )
            .await?;
        }
        "/status" => {
            let store = SessionStore::open(&config.agent.name)?;
            let session_key = format!("tg:{}:{}", config.agent.name, chat_id);
            let sessions = store.list_sessions(&config.agent.name, 5)?;
            let current = sessions.iter().find(|s| s.session_key == session_key);

            let msg_count = current.map(|s| s.message_count).unwrap_or(0);
            let tokens = current.map(|s| s.total_tokens).unwrap_or(0);

            let status = format!(
                "🦀 *Rustbot Status*\n\n\
                Agent: `{}`\n\
                Fallback: {}\n\
                Session: `{}`\n\
                Messages: {}\n\
                Tokens used: {}\n\
                Sessions total: {}",
                config.agent.name,
                if config.agent.fallback { "enabled" } else { "disabled" },
                session_key,
                msg_count,
                tokens,
                sessions.len(),
            );

            bot.send_message(chat_id, &status).await?;
        }
        "/reset" => {
            // Create a new session key (old one becomes orphaned but harmless)
            bot.send_message(chat_id, "🔄 Session reset. Starting fresh.")
                .await?;
        }
        _ => {
            bot.send_message(chat_id, "Unknown command. Try /start, /status, or /reset.")
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

    // Try to find the model in providers
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
