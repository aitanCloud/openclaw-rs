use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{info, warn};

use openclaw_agent::llm::fallback::FallbackProvider;
use openclaw_agent::llm::streaming::StreamEvent;
use openclaw_agent::llm::{LlmProvider, Message};
use openclaw_agent::runtime::{self, AgentTurnConfig};
use openclaw_agent::sessions::SessionStore;
use openclaw_agent::tools::ToolRegistry;
use openclaw_agent::workspace;

use crate::config::GatewayConfig;
use crate::telegram::{TelegramBot, TgMessage};

/// Process boot time for /version uptime tracking
pub static BOOT_TIME: std::sync::LazyLock<std::time::Instant> =
    std::sync::LazyLock::new(std::time::Instant::now);

/// ISO 8601 boot timestamp for /health and /status
pub static BOOT_TIMESTAMP: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string());

/// Agent name, set once at startup via init_agent_name()
static AGENT_NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub fn init_agent_name(name: &str) {
    let _ = AGENT_NAME.set(name.to_string());
}

pub fn agent_name() -> &'static str {
    AGENT_NAME.get().map(|s| s.as_str()).unwrap_or("unknown")
}

/// Minimum chars between Telegram message edits (avoid rate limits)
const EDIT_MIN_CHARS: usize = 80;
/// Minimum ms between Telegram message edits
const EDIT_MIN_MS: u64 = 400;

/// Handle an incoming Telegram message
pub async fn handle_message(
    bot: &TelegramBot,
    msg: &TgMessage,
    config: &GatewayConfig,
) -> Result<()> {
    let chat_id = msg.chat.id;
    let user = msg.from.as_ref();
    let user_id = user.map(|u| u.id).unwrap_or(0);
    let user_name = user.map(|u| u.first_name.as_str()).unwrap_or("unknown");

    // Extract text from message or photo caption
    let has_photo = msg.photo.as_ref().map(|p| !p.is_empty()).unwrap_or(false);
    let has_voice = msg.voice.is_some() || msg.audio.is_some();

    let text = if has_voice {
        // Voice/audio message — download and transcribe
        let file_id = msg.voice.as_ref().map(|v| v.file_id.as_str())
            .or_else(|| msg.audio.as_ref().map(|a| a.file_id.as_str()))
            .unwrap_or("");
        let duration = msg.voice.as_ref().map(|v| v.duration)
            .or_else(|| msg.audio.as_ref().map(|a| a.duration))
            .unwrap_or(0);

        info!("Voice message from {} ({}): {}s, file_id={}", user_name, user_id, duration, &file_id[..file_id.len().min(20)]);

        match transcribe_voice_message(bot, file_id).await {
            Ok(transcript) => {
                if transcript.is_empty() {
                    bot.send_message(chat_id, "🎤 I couldn't transcribe that voice message. Please try again or send text.").await?;
                    return Ok(());
                }
                bot.send_message(chat_id, &format!("🎤 _{}_", &transcript[..transcript.len().min(200)])).await?;
                transcript
            }
            Err(e) => {
                warn!("Voice transcription failed: {}", e);
                bot.send_message(chat_id, "🎤 Voice transcription failed. Please send text instead.").await?;
                return Ok(());
            }
        }
    } else {
        msg.text.clone()
            .or_else(|| msg.caption.clone())
            .unwrap_or_default()
    };

    // Must have text or photo
    if text.is_empty() && !has_photo {
        return Ok(());
    }

    // ── Access control ──
    if !config.telegram.allowed_user_ids.is_empty()
        && !config.telegram.allowed_user_ids.contains(&user_id)
    {
        warn!("Unauthorized user {} ({})", user_id, user_name);
        bot.send_message(chat_id, "⛔ Unauthorized. This bot is private.")
            .await?;
        return Ok(());
    }

    info!("Message from {} ({}): {}{}", user_name, user_id,
        &text[..text.len().min(100)],
        if has_photo { " [+photo]" } else { "" });

    // ── Handle commands ──
    if text.starts_with('/') && !has_photo {
        return handle_command(bot, chat_id, user_id, &text, config, msg).await;
    }

    // ── Download photo if present ──
    let mut image_urls: Vec<String> = Vec::new();
    if has_photo {
        if let Some(photos) = &msg.photo {
            // Telegram sends multiple sizes; pick the largest (last)
            if let Some(largest) = photos.last() {
                match download_and_encode_photo(bot, &largest.file_id).await {
                    Ok(data_url) => {
                        info!("Downloaded photo: {} bytes encoded", data_url.len());
                        image_urls.push(data_url);
                    }
                    Err(e) => {
                        warn!("Failed to download photo: {}", e);
                        bot.send_message(chat_id, "⚠️ Failed to download photo.").await?;
                        if text.is_empty() {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    // ── Send typing indicator + initial placeholder ──
    bot.send_typing(chat_id).await.ok();
    let placeholder_id = bot.send_message_with_id(chat_id, "🧠 ...").await?;

    // ── Keep typing indicator alive throughout the agent turn ──
    let typing_token = tokio_util::sync::CancellationToken::new();
    let typing_cancel = typing_token.clone();
    let typing_bot = TelegramBot::new(&config.telegram.bot_token);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(4)) => {
                    typing_bot.send_typing(chat_id).await.ok();
                }
                _ = typing_cancel.cancelled() => break,
            }
        }
    });

    // ── Resolve workspace ──
    let workspace_dir = workspace::resolve_workspace_dir(&config.agent.name);
    if !workspace_dir.exists() {
        bot.edit_message(chat_id, placeholder_id, "❌ Workspace not found.")
            .await?;
        return Ok(());
    }

    // ── Resolve provider ──
    let provider: Box<dyn LlmProvider> = if config.agent.fallback {
        match FallbackProvider::from_config() {
            Ok(fb) => Box::new(fb),
            Err(e) => {
                bot.edit_message(chat_id, placeholder_id, &format!("❌ Provider error: {}", e))
                    .await?;
                return Ok(());
            }
        }
    } else if let Some(ref model) = config.agent.model {
        match crate::handler_utils::resolve_single_provider(model) {
            Ok(p) => Box::new(p),
            Err(e) => {
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

    // ── Session ──
    let session_key = format!("tg:{}:{}:{}", config.agent.name, user_id, chat_id);
    let store = match SessionStore::open(&config.agent.name) {
        Ok(s) => s,
        Err(e) => {
            bot.edit_message(chat_id, placeholder_id, &format!("❌ Session error: {}", e))
                .await?;
            return Ok(());
        }
    };
    store.create_session(&session_key, &config.agent.name, provider.name())?;

    // ── Tools + config (load plugins from workspace) ──
    let mut tools = ToolRegistry::with_defaults();
    let plugin_count = tools.load_plugins(&workspace_dir);
    if plugin_count > 0 {
        info!("Loaded {} plugin tool(s) from workspace", plugin_count);
    }
    // ── Set up delegate channel for async subagent dispatch ──
    let (delegate_tx, mut delegate_rx) = mpsc::unbounded_channel::<openclaw_agent::tools::DelegateRequest>();

    let agent_config = AgentTurnConfig {
        agent_name: config.agent.name.clone(),
        session_key: session_key.clone(),
        workspace_dir: workspace_dir.to_string_lossy().to_string(),
        minimal_context: false,
        chat_id,
        delegate_tx: Some(delegate_tx),
    };

    // ── Spawn delegate listener (background subagent tasks) ──
    let delegate_bot_token = config.telegram.bot_token.clone();
    let subagent_timeout_secs = config.agent.sandbox.as_ref()
        .and_then(|s| s.subagent_timeout_secs).unwrap_or(300);
    tokio::spawn(async move {
        while let Some(req) = delegate_rx.recv().await {
            let bot = TelegramBot::new(&delegate_bot_token);
            let timeout_secs = subagent_timeout_secs;
            tokio::spawn(async move {
                let task_desc = req.task[..req.task.len().min(80)].to_string();
                let (task_id, cancel_token) = crate::subagent_registry::register_subagent(
                    &task_desc, &req.agent_name, req.chat_id,
                );

                let _ = bot.send_message(
                    req.chat_id,
                    &format!("\u{1F680} *Task #{}* started: {}", task_id, task_desc),
                ).await;

                let timeout = std::time::Duration::from_secs(timeout_secs);
                let result = tokio::time::timeout(
                    timeout,
                    openclaw_agent::subagent::run_subagent_turn(
                        &req.task, &req.agent_name, &req.workspace_dir, Some(cancel_token),
                    ),
                ).await;

                match result {
                    Ok(Ok(response)) => {
                        crate::subagent_registry::complete_subagent(task_id);
                        let truncated = if response.len() > 3000 {
                            format!("{}...\n\n_(truncated, {}B total)_", &response[..3000], response.len())
                        } else {
                            response
                        };
                        let _ = bot.send_message(
                            req.chat_id,
                            &format!("\u{2705} *Task #{}* complete:\n\n{}", task_id, truncated),
                        ).await;
                    }
                    Ok(Err(e)) => {
                        crate::subagent_registry::fail_subagent(task_id, &e.to_string());
                        let _ = bot.send_message(
                            req.chat_id,
                            &format!("\u{274C} *Task #{}* failed: {}", task_id, e),
                        ).await;
                    }
                    Err(_) => {
                        let msg = format!("timed out after {}s", timeout_secs);
                        crate::subagent_registry::fail_subagent(task_id, &msg);
                        let _ = bot.send_message(
                            req.chat_id,
                            &format!("\u{23F0} *Task #{}* {}", task_id, msg),
                        ).await;
                    }
                }
                crate::subagent_registry::gc();
            });
        }
    });

    // ── Set up streaming channel ──
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<StreamEvent>();

    let t_start = std::time::Instant::now();

    // ── Register task for cancellation support ──
    let task_key = format!("tg:{}:{}", user_id, chat_id);
    let cancel_token = crate::task_registry::register_task(&task_key);
    let task_key_cleanup = task_key.clone();

    // ── Spawn the agent turn in background (with per-turn timeout) ──
    let turn_timeout = std::time::Duration::from_secs(120);
    let agent_handle = tokio::spawn(async move {
        let result = match tokio::time::timeout(
            turn_timeout,
            runtime::run_agent_turn_streaming(
                provider.as_ref(),
                &text,
                &agent_config,
                &tools,
                event_tx,
                image_urls,
                Some(cancel_token),
            ),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
                if let Some(m) = crate::metrics::global() { m.record_agent_timeout(); }
                Err(anyhow::anyhow!("Agent turn timed out after 120s"))
            }
        };
        crate::task_registry::unregister_task(&task_key_cleanup);
        result
    });

    // ── Stream loop: receive events, edit Telegram message in real-time ──
    let stream_bot = TelegramBot::new(&config.telegram.bot_token);
    let mut accumulated = String::new();
    let mut last_edit_len: usize = 0;
    let mut last_edit_time = std::time::Instant::now();
    let mut tool_status = String::new();
    let mut is_done = false;

    while let Some(event) = event_rx.recv().await {
        match event {
            StreamEvent::ContentDelta(delta) => {
                accumulated.push_str(&delta);

                // Edit message if enough new chars or enough time passed
                let chars_since_edit = accumulated.len() - last_edit_len;
                let ms_since_edit = last_edit_time.elapsed().as_millis() as u64;

                if chars_since_edit >= EDIT_MIN_CHARS || ms_since_edit >= EDIT_MIN_MS {
                    let display = if tool_status.is_empty() {
                        accumulated.clone()
                    } else {
                        format!("{}\n\n{}", tool_status, accumulated)
                    };

                    // Truncate to 4000 chars for Telegram
                    let truncated = if display.len() > 4000 {
                        format!("{}...", &display[..3997])
                    } else {
                        display
                    };

                    stream_bot
                        .edit_message(chat_id, placeholder_id, &truncated)
                        .await
                        .ok();

                    last_edit_len = accumulated.len();
                    last_edit_time = std::time::Instant::now();
                }
            }

            StreamEvent::ReasoningDelta(_) => {
                // Don't show reasoning in Telegram — just the final content
            }

            StreamEvent::ToolCallStart { name } => {
                tool_status = format!("🔧 Calling {}...", name);
                let display = if accumulated.is_empty() {
                    tool_status.clone()
                } else {
                    format!("{}\n\n{}", accumulated, tool_status)
                };
                stream_bot
                    .edit_message(chat_id, placeholder_id, &display)
                    .await
                    .ok();
            }

            StreamEvent::ToolExec { name, .. } => {
                tool_status = format!("⚙️ Running {}...", name);
                let display = if accumulated.is_empty() {
                    tool_status.clone()
                } else {
                    format!("{}\n\n{}", accumulated, tool_status)
                };
                stream_bot
                    .edit_message(chat_id, placeholder_id, &display)
                    .await
                    .ok();
            }

            StreamEvent::ToolResult { name, success } => {
                let icon = if success { "✅" } else { "❌" };
                tool_status = format!("{} {}", icon, name);
                let display = if accumulated.is_empty() {
                    tool_status.clone()
                } else {
                    format!("{}\n\n{}", accumulated, tool_status)
                };
                stream_bot
                    .edit_message(chat_id, placeholder_id, &display)
                    .await
                    .ok();
            }

            StreamEvent::RoundStart { round } => {
                tool_status = String::new();
                // Clear accumulated content for new round — the LLM will generate fresh
                accumulated.clear();
                last_edit_len = 0;

                let display = format!("🔄 Round {}...", round);
                stream_bot
                    .edit_message(chat_id, placeholder_id, &display)
                    .await
                    .ok();
            }

            StreamEvent::Done => {
                is_done = true;
                break;
            }
        }
    }

    // ── Wait for agent turn to finish, stop typing indicator ──
    let result = agent_handle.await??;
    typing_token.cancel();
    let elapsed = t_start.elapsed().as_millis();

    // ── Persist messages (SQLite + Postgres) ──
    let user_text = msg.text.as_deref().unwrap_or("");
    store.append_message(
        &session_key,
        &openclaw_agent::llm::Message::user(user_text),
    )?;
    if !result.response.is_empty() {
        store.append_message(
            &session_key,
            &openclaw_agent::llm::Message::assistant(&result.response),
        )?;
    }
    store.add_tokens(&session_key, result.total_usage.total_tokens as i64)?;

    // Postgres dual-write (fire-and-forget)
    if let Some(pool) = openclaw_db::pool() {
        let pool = pool.clone();
        let sk = session_key.clone();
        let agent = config.agent.name.clone();
        let model = result.model_name.clone();
        let channel = Some("telegram".to_string());
        let uid = user_id.to_string();
        let user_msg = user_text.to_string();
        let bot_msg = result.response.clone();
        let tokens = result.total_usage.total_tokens as i64;
        tokio::spawn(async move {
            if let Ok(sid) = openclaw_db::sessions::upsert_session(
                &pool, &sk, &agent, &model, channel.as_deref(), Some(&uid),
            ).await {
                let _ = openclaw_db::messages::record_message(
                    &pool, sid, "user", Some(&user_msg), None, None, None,
                ).await;
                if !bot_msg.is_empty() {
                    let _ = openclaw_db::messages::record_message(
                        &pool, sid, "assistant", Some(&bot_msg), None, None, None,
                    ).await;
                }
                let _ = openclaw_db::sessions::add_tokens(&pool, &sk, tokens).await;
            }
        });
    }

    // ── Final edit with stats footer ──
    let response = if !result.response.is_empty() {
        result.response.clone()
    } else if let Some(ref reasoning) = result.reasoning {
        reasoning.clone()
    } else {
        "(no response)".to_string()
    };

    // Escape underscores in model name to prevent Telegram Markdown breakage
    let safe_model = result.model_name.replace('_', "\\_");
    let stats = format!(
        "\n\n_{}ms · {} round(s) · {} tool(s) · {} tokens · {}_",
        elapsed,
        result.total_rounds,
        result.tool_calls_made,
        result.total_usage.total_tokens,
        safe_model,
    );

    let full_response = format!("{}{}", response, stats);

    if full_response.len() <= 4000 {
        stream_bot
            .edit_message_markdown(chat_id, placeholder_id, &full_response)
            .await?;
    } else {
        // First 4000 chars in the edited message, rest as new messages
        let first = &full_response[..4000.min(full_response.len())];
        stream_bot
            .edit_message_markdown(chat_id, placeholder_id, first)
            .await?;
        if full_response.len() > 4000 {
            let rest = &full_response[4000..];
            let send_bot = TelegramBot::new(&config.telegram.bot_token);
            for chunk in crate::handler_utils::split_message(rest, 4000) {
                send_bot.send_message(chat_id, &chunk).await?;
            }
        }
    }

    info!(
        "Reply sent: {}ms, {} rounds, {} tools, {} tokens, model={}",
        elapsed, result.total_rounds, result.tool_calls_made,
        result.total_usage.total_tokens, result.model_name,
    );

    if let Some(m) = crate::metrics::global() {
        m.record_agent_turn(result.tool_calls_made as u64);
    }

    Ok(())
}

async fn handle_command(
    bot: &TelegramBot,
    chat_id: i64,
    user_id: i64,
    text: &str,
    config: &GatewayConfig,
    msg: &TgMessage,
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
                /export — export current session as markdown\n\
                /voice — get a voice response (TTS)\n\
                /ping — latency check\n\
                /history [N] — last N messages (default 5, max 20)\n\
                /clear — delete current session\n\
                /db — session database stats\n\
                /version — build info and uptime\n\
                /stats — gateway request stats\n\
                /whoami — show your user info\n\
                /cancel — stop the running task\n\
                /cron — list and manage cron jobs\n\
                /tools — list all built-in agent tools\n\
                /skills — list available workspace skills\n\
                /config — show gateway config (sanitized)\n\
                /runtime — build and runtime info\n\
                /logs [N] — show recent LLM activity (default 5)\n\
                /doctor — run health checks\n\
                /help — show this help\n\n\
                You can also send voice messages — I'll transcribe and respond.",
            )
            .await?;
        }
        "/ping" => {
            let start = std::time::Instant::now();
            bot.send_message(chat_id, &format!("🏓 Pong! ({}ms)", start.elapsed().as_millis())).await?;
        }
        "/cancel" | "/stop" => {
            // /cancel <id> — cancel a specific background task
            if let Some(id_str) = text.split_whitespace().nth(1) {
                if let Ok(id) = id_str.parse::<u64>() {
                    if crate::subagent_registry::cancel_subagent(id) {
                        bot.send_message(chat_id, &format!("⛔ Cancelled task #{}", id)).await?;
                    } else {
                        bot.send_message(chat_id, &format!("ℹ️ No task #{} found.", id)).await?;
                    }
                    return Ok(());
                }
            }
            // /cancel (no args) — cancel the main running task + all subagents for this chat
            let task_key = format!("tg:{}:{}", user_id, chat_id);
            let main_cancelled = crate::task_registry::cancel_task(&task_key);
            let sub_cancelled = crate::subagent_registry::cancel_all_for_chat(chat_id);
            if main_cancelled || sub_cancelled > 0 {
                if let Some(m) = crate::metrics::global() { m.record_task_cancelled(); }
                let mut msg_text = String::from("⛔ Cancelled");
                if main_cancelled { msg_text.push_str(" running task"); }
                if sub_cancelled > 0 {
                    if main_cancelled { msg_text.push_str(" +"); }
                    msg_text.push_str(&format!(" {} background task(s)", sub_cancelled));
                }
                msg_text.push('.');
                bot.send_message(chat_id, &msg_text).await?;
            } else {
                bot.send_message(chat_id, "ℹ️ No tasks are currently running.").await?;
            }
        }
        "/tasks" => {
            let tasks = crate::subagent_registry::list_tasks();
            if tasks.is_empty() {
                bot.send_message(chat_id, "📋 No background tasks.").await?;
            } else {
                let mut msg_text = format!("📋 *Background Tasks* ({} total)\n\n", tasks.len());
                for t in tasks.iter().take(10) {
                    let elapsed = t.started_at.elapsed().as_secs();
                    msg_text.push_str(&format!(
                        "*#{}* {} — {}s\n  _{}_\n\n",
                        t.id, t.status, elapsed,
                        &t.description[..t.description.len().min(60)],
                    ));
                }
                bot.send_message(chat_id, &msg_text).await?;
            }
        }
        "/whoami" => {
            let session_key = format!("tg:{}:{}:{}", config.agent.name, user_id, chat_id);
            let username = msg.from.as_ref()
                .and_then(|u| u.username.as_deref())
                .unwrap_or("unknown");
            let is_allowed = config.telegram.allowed_user_ids.is_empty()
                || config.telegram.allowed_user_ids.contains(&user_id);
            bot.send_message(chat_id, &format!(
                "👤 *Who Am I*\n\n\
                User: @{} ({})\n\
                Chat: {}\n\
                Session key: `{}`\n\
                Authorized: {}",
                username, user_id, chat_id, session_key,
                if is_allowed { "✅ Yes" } else { "❌ No" },
            )).await?;
        }
        "/stats" => {
            if let Some(m) = crate::metrics::global() {
                let tg_req = m.telegram_requests.load(std::sync::atomic::Ordering::Relaxed);
                let dc_req = m.discord_requests.load(std::sync::atomic::Ordering::Relaxed);
                let tg_err = m.telegram_errors.load(std::sync::atomic::Ordering::Relaxed);
                let dc_err = m.discord_errors.load(std::sync::atomic::Ordering::Relaxed);
                let rl = m.rate_limited.load(std::sync::atomic::Ordering::Relaxed);
                let completed = m.completed_requests.load(std::sync::atomic::Ordering::Relaxed);
                let avg = m.avg_latency_ms();
                let uptime_str = crate::human_uptime(crate::handler::BOOT_TIME.elapsed().as_secs());
                let cancelled = m.tasks_cancelled.load(std::sync::atomic::Ordering::Relaxed);
                let timeouts = m.agent_timeouts.load(std::sync::atomic::Ordering::Relaxed);
                let turns = m.agent_turns.load(std::sync::atomic::Ordering::Relaxed);
                let tool_calls = m.tool_calls.load(std::sync::atomic::Ordering::Relaxed);
                let err_rate = m.error_rate_pct();
                let webhooks = m.webhook_requests.load(std::sync::atomic::Ordering::Relaxed);
                let session_count = openclaw_agent::sessions::SessionStore::open(&config.agent.name)
                    .ok()
                    .and_then(|s| s.db_stats(&config.agent.name).ok())
                    .map(|stats| stats.session_count)
                    .unwrap_or(0);
                let llm_stats = openclaw_agent::llm_log::stats();
                bot.send_message(chat_id, &format!(
                    "📊 *Gateway Stats* ({} uptime)\n\n\
                    Telegram: {} requests, {} errors\n\
                    Discord: {} requests, {} errors\n\
                    Webhooks: {}\n\
                    Rate limited: {}\n\
                    Completed: {}\n\
                    Agent turns: {}\n\
                    Tool calls: {}\n\
                    Sessions: {}\n\
                    Cancelled: {}\n\
                    Timeouts: {}\n\
                    Active tasks: {}\n\
                    Avg latency: {}ms\n\
                    Error rate: {:.1}%\n\
                    LLM calls: {}\n\
                    LLM errors: {}\n\
                    LLM avg latency: {}ms",
                    uptime_str, tg_req, tg_err, dc_req, dc_err, webhooks, rl, completed,
                    turns, tool_calls, session_count, cancelled, timeouts,
                    crate::task_registry::active_count(), avg, err_rate,
                    llm_stats.total_recorded, llm_stats.errors, llm_stats.avg_latency_ms,
                )).await?;
            } else {
                bot.send_message(chat_id, "📊 Metrics not available.").await?;
            }
        }
        "/version" => {
            let uptime_str = crate::human_uptime(crate::handler::BOOT_TIME.elapsed().as_secs());
            bot.send_message(chat_id, &format!(
                "🦀 *openclaw-gateway* v{}\n\
                Uptime: {}\n\
                Agent: {}\n\
                Commands: 23",
                env!("CARGO_PKG_VERSION"), uptime_str, config.agent.name,
            )).await?;
        }
        "/doctor" => {
            let checks = crate::doctor::run_checks(&config.agent.name).await;
            let all_ok = checks.iter().all(|(_, ok, _)| *ok);
            let icon = if all_ok { "✅" } else { "⚠️" };
            let mut msg_text = format!("{} *Doctor Report*\n\n", icon);
            for (name, ok, detail) in &checks {
                let status = if *ok { "✅" } else { "❌" };
                msg_text.push_str(&format!("{} {}: {}\n", status, name, detail));
            }
            bot.send_message(chat_id, &msg_text).await?;
        }
        "/tools" => {
            let tools = openclaw_agent::tools::ToolRegistry::with_defaults();
            let names = tools.tool_names();
            let mut msg_text = format!("🔧 *Built-in Tools* ({} total)\n\n", names.len());
            for name in &names {
                msg_text.push_str(&format!("• `{}`\n", name));
            }
            bot.send_message(chat_id, &msg_text).await?;
        }
        "/runtime" => {
            let pid = std::process::id();
            let uptime_secs = crate::handler::BOOT_TIME.elapsed().as_secs();
            let uptime_str = crate::human_uptime(uptime_secs);
            let rss = crate::doctor::human_bytes_pub(crate::process_rss_bytes());
            bot.send_message(chat_id, &format!(
                "🖥️ *Runtime Info*\n\n\
                Version: v{}\n\
                Built: {}\n\
                Started: {}\n\
                Profile: {}\n\
                PID: {}\n\
                Memory: {}\n\
                Uptime: {}\n\
                OS: {}\n\
                Arch: {}",
                env!("CARGO_PKG_VERSION"),
                env!("BUILD_TIMESTAMP"),
                *crate::handler::BOOT_TIMESTAMP,
                if cfg!(debug_assertions) { "debug" } else { "release" },
                pid,
                rss,
                uptime_str,
                std::env::consts::OS,
                std::env::consts::ARCH,
            )).await?;
        }
        "/config" => {
            let mut lines = Vec::new();
            lines.push(format!("⚙️ *Gateway Config*\n"));
            lines.push(format!("Agent: {}", config.agent.name));
            lines.push(format!("Fallback: {}", config.agent.fallback));
            lines.push(format!("Model: {}", config.agent.model.as_deref().unwrap_or("(default)")));
            lines.push(format!("Telegram users: {}", config.telegram.allowed_user_ids.len()));
            lines.push(format!("Discord: {}", if config.discord.is_some() { "enabled" } else { "disabled" }));
            if let Some(ref dc) = config.discord {
                lines.push(format!("Discord users: {}", dc.allowed_user_ids.len()));
            }
            lines.push(format!("Webhook: {}", if config.webhook.is_some() { "configured" } else { "not configured" }));
            if let Some(ref sb) = config.agent.sandbox {
                lines.push(format!("Sandbox: timeout={}s, max_concurrent={}",
                    sb.turn_timeout_secs.unwrap_or(120),
                    sb.max_concurrent.unwrap_or(4)));
            } else {
                lines.push("Sandbox: defaults".to_string());
            }
            let providers = openclaw_agent::llm::fallback::FallbackProvider::from_config()
                .ok()
                .map(|fb| fb.provider_labels().iter().map(|s| s.to_string()).collect::<Vec<_>>())
                .unwrap_or_default();
            if !providers.is_empty() {
                lines.push(format!("Providers: {}", providers.join(", ")));
            }
            bot.send_message(chat_id, &lines.join("\n")).await?;
        }
        "/skills" => {
            let workspace_dir = openclaw_agent::workspace::resolve_workspace_dir(&config.agent.name);
            let skills_dir = workspace_dir.join("skills");
            let skills = openclaw_core::skills::list_skills(&skills_dir).unwrap_or_default();
            if skills.is_empty() {
                bot.send_message(chat_id, "📚 No skills found in workspace.").await?;
            } else {
                let mut msg_text = format!("📚 *Skills* ({} available)\n\n", skills.len());
                for skill in &skills {
                    let desc = skill.description.as_deref().unwrap_or("(no description)");
                    msg_text.push_str(&format!("• *{}* — {}\n", skill.name, desc));
                }
                bot.send_message(chat_id, &msg_text).await?;
            }
        }
        "/clear" => {
            let store = SessionStore::open(&config.agent.name)?;
            let session_key = format!("tg:{}:{}:{}", config.agent.name, user_id, chat_id);
            let active_key = store.find_latest_session(&session_key)?
                .unwrap_or(session_key);
            let deleted = store.delete_session(&active_key)?;
            bot.send_message(chat_id, &format!(
                "🗑️ Session cleared. Deleted {} message(s).", deleted
            )).await?;
        }
        cmd if cmd.starts_with("/history") => {
            let count: usize = text.split_whitespace().nth(1)
                .and_then(|n| n.parse().ok())
                .unwrap_or(5)
                .min(20);
            let store = SessionStore::open(&config.agent.name)?;
            let session_key = format!("tg:{}:{}:{}", config.agent.name, user_id, chat_id);
            let active_key = store.find_latest_session(&session_key)?
                .unwrap_or(session_key);
            let msgs = store.load_messages(&active_key)?;
            let recent: Vec<_> = msgs.iter().rev().take(count).collect();

            if recent.is_empty() {
                bot.send_message(chat_id, "📜 No messages in current session.").await?;
            } else {
                let mut text = format!("📜 *Last {} messages:*\n\n", recent.len());
                for msg in recent.iter().rev() {
                    let role = match msg.role.as_str() {
                        "user" => "👤",
                        "assistant" => "🤖",
                        "system" => "⚙️",
                        "tool" => "🔧",
                        _ => "❓",
                    };
                    let content = msg.content.as_deref().unwrap_or("[no content]");
                    let preview = if content.len() > 200 {
                        format!("{}...", &content[..200])
                    } else {
                        content.to_string()
                    };
                    text.push_str(&format!("{} {}\n", role, preview));
                }
                bot.send_message(chat_id, &text).await?;
            }
        }
        "/db" => {
            let store = SessionStore::open(&config.agent.name)?;
            match store.db_stats(&config.agent.name) {
                Ok(stats) => {
                    let size = if stats.db_size_bytes > 1_048_576 {
                        format!("{:.1} MB", stats.db_size_bytes as f64 / 1_048_576.0)
                    } else {
                        format!("{:.1} KB", stats.db_size_bytes as f64 / 1024.0)
                    };
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
                    let oldest = stats.oldest_ms
                        .map(|ms| crate::handler_utils::format_duration(now - ms))
                        .unwrap_or_else(|| "n/a".to_string());
                    let newest = stats.newest_ms
                        .map(|ms| crate::handler_utils::format_duration(now - ms))
                        .unwrap_or_else(|| "n/a".to_string());
                    let avg_msgs = if stats.session_count > 0 {
                        format!("{:.1}", stats.message_count as f64 / stats.session_count as f64)
                    } else {
                        "0".to_string()
                    };
                    bot.send_message(chat_id, &format!(
                        "🗄️ *Session Database*\n\n\
                        Sessions: {}\n\
                        Messages: {}\n\
                        Avg msgs/session: {}\n\
                        Tokens: {}\n\
                        DB size: {}\n\
                        Oldest: {}\n\
                        Newest: {}",
                        stats.session_count, stats.message_count, avg_msgs,
                        stats.total_tokens, size, oldest, newest,
                    )).await?;
                }
                Err(e) => {
                    bot.send_message(chat_id, &format!("❌ Failed to get DB stats: {}", e)).await?;
                }
            }
        }
        "/new" | "/reset" => {
            let store = SessionStore::open(&config.agent.name)?;
            let old_key = format!("tg:{}:{}:{}", config.agent.name, user_id, chat_id);
            let old_msgs = store.load_messages(&old_key)?;
            let msg_count = old_msgs.iter()
                .filter(|m| m.role == "user" || m.role == "assistant")
                .count();

            let new_key = format!("tg:{}:{}:{}:{}", config.agent.name, user_id, chat_id, uuid::Uuid::new_v4());
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
            let session_key = format!("tg:{}:{}:{}", config.agent.name, user_id, chat_id);
            let sessions = store.list_sessions(&config.agent.name, 100)?;
            let current = sessions.iter().find(|s| s.session_key == session_key);

            let msg_count = current.map(|s| s.message_count).unwrap_or(0);
            let tokens = current.map(|s| s.total_tokens).unwrap_or(0);

            let chain_info = if config.agent.fallback {
                match FallbackProvider::from_config() {
                    Ok(fb) => format!("Chain: {}", fb.provider_labels().join(" → ")),
                    Err(_) => "Chain: (error loading)".to_string(),
                }
            } else {
                format!("Model: {}", config.agent.model.as_deref().unwrap_or("default"))
            };

            bot.send_message(
                chat_id,
                &format!(
                    "🦀 *Rustbot Status*\n\n\
                    Agent: `{}`\n\
                    Fallback: {}\n\
                    {}\n\
                    Session: `{}`\n\
                    Messages: {}\n\
                    Tokens used: {}\n\
                    Total sessions: {}\n\
                    Active tasks: {}",
                    config.agent.name,
                    if config.agent.fallback { "✅ enabled" } else { "❌ disabled" },
                    chain_info,
                    session_key,
                    msg_count,
                    tokens,
                    sessions.len(),
                    crate::task_registry::active_count(),
                ),
            )
            .await?;
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
                        info.push_str("\nFirst available model is used. Circuit breaker at >3 failures.");
                        info
                    }
                    Err(e) => format!("❌ Error: {}", e),
                }
            } else {
                format!("🤖 *Model:* `{}`", config.agent.model.as_deref().unwrap_or("default"))
            };
            bot.send_message(chat_id, &chain_info).await?;
        }
        "/export" => {
            let store = SessionStore::open(&config.agent.name)?;
            let session_key = format!("tg:{}:{}:{}", config.agent.name, user_id, chat_id);
            let messages = store.load_messages(&session_key)?;

            if messages.is_empty() {
                bot.send_message(chat_id, "No messages in current session to export.")
                    .await?;
            } else {
                let mut md = String::from("# Session Export\n\n");
                for msg in &messages {
                    let role_icon = match msg.role.as_str() {
                        "user" => "👤",
                        "assistant" => "🤖",
                        "system" => "⚙️",
                        "tool" => "🔧",
                        _ => "❓",
                    };
                    md.push_str(&format!("### {} {}\n\n{}\n\n---\n\n",
                        role_icon,
                        msg.role,
                        msg.content.as_deref().unwrap_or("(empty)"),
                    ));
                }
                md.push_str(&format!("_Exported {} messages_", messages.len()));

                // Send as chunks if needed
                let chunks = crate::handler_utils::split_message(&md, 4000);
                for chunk in chunks {
                    bot.send_message(chat_id, &chunk).await?;
                }
            }
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
                    msg.push_str(&format!(
                        "{}. `{}` — {} msgs, {} tokens, {}\n",
                        i + 1,
                        &s.session_key[..s.session_key.len().min(30)],
                        s.message_count,
                        s.total_tokens,
                        crate::handler_utils::format_duration(age),
                    ));
                }
                bot.send_message(chat_id, &msg).await?;
            }
        }
        "/voice" => {
            // /voice <text> — get LLM response, convert to speech, send as voice message
            let voice_text = text.strip_prefix("/voice").unwrap_or("").trim();
            if voice_text.is_empty() {
                bot.send_message(chat_id, "Usage: /voice <text to speak>\n\nI'll respond and send it as a voice message.").await?;
            } else {
                bot.send_typing(chat_id).await.ok();

                // Get LLM response for the text
                let provider: Box<dyn LlmProvider> = match FallbackProvider::from_config() {
                    Ok(fb) => Box::new(fb),
                    Err(e) => {
                        bot.send_message(chat_id, &format!("❌ Provider error: {}", e)).await?;
                        return Ok(());
                    }
                };

                let workspace_dir = workspace::resolve_workspace_dir(&config.agent.name);
                let session_key = format!("tg:{}:{}:{}", config.agent.name, user_id, chat_id);
                let store = SessionStore::open(&config.agent.name)?;
                store.create_session(&session_key, &config.agent.name, provider.name())?;

                let tools = ToolRegistry::with_defaults();
                let agent_config = AgentTurnConfig {
                    agent_name: config.agent.name.clone(),
                    session_key: session_key.clone(),
                    workspace_dir: workspace_dir.to_string_lossy().to_string(),
                    minimal_context: true,
                ..AgentTurnConfig::default()
                };

                let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();
                let result = runtime::run_agent_turn_streaming(
                    provider.as_ref(),
                    voice_text,
                    &agent_config,
                    &tools,
                    event_tx,
                    Vec::new(),
                    None,
                ).await?;

                let response_text = if !result.response.is_empty() {
                    result.response.clone()
                } else {
                    "I have nothing to say.".to_string()
                };

                // Persist messages
                store.append_message(&session_key, &Message::user(voice_text))?;
                store.append_message(&session_key, &Message::assistant(&response_text))?;

                // Generate TTS via Piper
                let tts_dir = workspace_dir.join("tts-output");
                std::fs::create_dir_all(&tts_dir)?;
                let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
                let wav_path = tts_dir.join(format!("voice_{}.wav", ts));
                let ogg_path = tts_dir.join(format!("voice_{}.ogg", ts));

                // Resolve Piper model
                let home = dirs::home_dir()
                    .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
                let model_path = home
                    .join(".openclaw/projects/voice-control/models/en_GB-jenny_dioco-medium.onnx");

                if !model_path.exists() {
                    // Fallback: send text response instead
                    bot.send_message(chat_id, &format!("🗣️ {}\n\n_(Piper TTS model not found for voice)_", response_text)).await?;
                    return Ok(());
                }

                // Run Piper to generate WAV
                let mut piper_cmd = tokio::process::Command::new("piper");
                piper_cmd
                    .arg("--model").arg(&model_path)
                    .arg("--output_file").arg(&wav_path)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped());

                let mut piper_child = piper_cmd.spawn().map_err(|e| {
                    anyhow::anyhow!("Failed to spawn piper: {}", e)
                })?;

                if let Some(mut stdin) = piper_child.stdin.take() {
                    use tokio::io::AsyncWriteExt;
                    // Replace "AItan" with phonetic hint for correct pronunciation
                    let tts_text = response_text.replace("AItan", "Ay-tawn");
                    stdin.write_all(tts_text.as_bytes()).await?;
                    drop(stdin);
                }

                let piper_output = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    piper_child.wait_with_output(),
                ).await;

                match piper_output {
                    Ok(Ok(output)) if output.status.success() => {
                        // Convert WAV to OGG/Opus using ffmpeg
                        let ffmpeg_result = tokio::process::Command::new("ffmpeg")
                            .args(["-y", "-i"])
                            .arg(&wav_path)
                            .args(["-c:a", "libopus", "-b:a", "64k", "-vbr", "on", "-application", "voip"])
                            .arg(&ogg_path)
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .status()
                            .await;

                        match ffmpeg_result {
                            Ok(status) if status.success() && ogg_path.exists() => {
                                // Send as voice message
                                let caption = if response_text.len() > 200 {
                                    Some(format!("{}...", &response_text[..197]))
                                } else {
                                    Some(response_text.clone())
                                };
                                bot.send_voice(chat_id, &ogg_path, caption.as_deref()).await?;

                                // Cleanup temp files
                                let _ = std::fs::remove_file(&wav_path);
                                let _ = std::fs::remove_file(&ogg_path);
                            }
                            _ => {
                                // ffmpeg failed — send text fallback
                                warn!("ffmpeg conversion failed, sending text response");
                                bot.send_message(chat_id, &format!("🗣️ {}\n\n_(Voice conversion failed)_", response_text)).await?;
                                let _ = std::fs::remove_file(&wav_path);
                            }
                        }
                    }
                    _ => {
                        warn!("Piper TTS failed, sending text response");
                        bot.send_message(chat_id, &format!("🗣️ {}\n\n_(TTS generation failed)_", response_text)).await?;
                    }
                }
            }
        }
        "/cron" => {
            let cron_path = openclaw_core::paths::cron_jobs_path();
            if !cron_path.exists() {
                bot.send_message(chat_id, "No cron jobs file found.").await?;
            } else {
                let parts: Vec<&str> = text.split_whitespace().collect();

                match parts.get(1).map(|s| *s) {
                    Some("enable") | Some("disable") => {
                        let action = parts[1];
                        let target = parts.get(2).map(|s| *s).unwrap_or("");
                        if target.is_empty() {
                            bot.send_message(chat_id, "Usage: /cron enable <name> or /cron disable <name>").await?;
                        } else {
                            match crate::handler_utils::toggle_cron_job(&cron_path, target, action == "enable") {
                                Ok(job_name) => {
                                    let icon = if action == "enable" { "✅" } else { "⏸️" };
                                    bot.send_message(chat_id, &format!("{} Cron job '{}' {}d.", icon, job_name, action)).await?;
                                }
                                Err(e) => {
                                    bot.send_message(chat_id, &format!("❌ {}", e)).await?;
                                }
                            }
                        }
                    }
                    _ => {
                        // Default: list all cron jobs
                        match openclaw_core::cron::load_cron_jobs(&cron_path) {
                            Ok(cron_file) => {
                                if cron_file.jobs.is_empty() {
                                    bot.send_message(chat_id, "No cron jobs configured.").await?;
                                } else {
                                    let mut msg = String::from("⏰ *Cron Jobs:*\n\n");
                                    for job in &cron_file.jobs {
                                        let status = if job.enabled { "✅" } else { "⏸️" };
                                        let last_run = job.state.as_ref()
                                            .and_then(|s| s.last_run_at_ms)
                                            .map(|ms| {
                                                let age = chrono::Utc::now().timestamp_millis() - ms as i64;
                                                crate::handler_utils::format_duration(age)
                                            })
                                            .unwrap_or_else(|| "never".to_string());
                                        let last_status = job.state.as_ref()
                                            .and_then(|s| s.last_status.as_deref())
                                            .unwrap_or("—");
                                        let duration = job.state.as_ref()
                                            .and_then(|s| s.last_duration_ms)
                                            .map(|ms| format!("{:.1}s", ms as f64 / 1000.0))
                                            .unwrap_or_else(|| "—".to_string());

                                        msg.push_str(&format!(
                                            "{} *{}*\n  Schedule: `{}`\n  Last: {} ({}), took {}\n\n",
                                            status, job.name, job.schedule,
                                            last_run, last_status, duration,
                                        ));
                                    }
                                    msg.push_str("_Use /cron enable <name> or /cron disable <name>_");
                                    bot.send_message(chat_id, &msg).await?;
                                }
                            }
                            Err(e) => {
                                bot.send_message(chat_id, &format!("❌ Failed to load cron jobs: {}", e)).await?;
                            }
                        }
                    }
                }
            }
        }
        "/logs" => {
            let count: usize = text.split_whitespace().nth(1)
                .and_then(|n| n.parse().ok())
                .unwrap_or(5)
                .min(20);
            let entries = openclaw_agent::llm_log::recent(count);
            let total = openclaw_agent::llm_log::total_count();

            if entries.is_empty() {
                bot.send_message(chat_id, "📋 No LLM activity recorded yet.").await?;
            } else {
                let mut msg_text = format!(
                    "📋 *LLM Activity Log* (last {} of {} total)\n\n",
                    entries.len(), total,
                );
                for (i, entry) in entries.iter().enumerate() {
                    let status = if entry.error.is_some() { "❌" } else { "✅" };
                    let content_preview = entry.response_content.as_deref()
                        .map(|c| if c.len() > 60 { format!("{}…", &c[..57]) } else { c.to_string() })
                        .unwrap_or_else(|| {
                            if entry.response_tool_calls > 0 {
                                format!("[{} tool(s): {}]", entry.response_tool_calls, entry.tool_call_names.join(", "))
                            } else if let Some(ref err) = entry.error {
                                format!("ERR: {}", &err[..err.len().min(50)])
                            } else {
                                "(no content)".to_string()
                            }
                        });
                    msg_text.push_str(&format!(
                        "{}. {} `{}` {}ms {}tok\n   {}\n\n",
                        i + 1, status, entry.model, entry.latency_ms,
                        entry.usage_total_tokens, content_preview,
                    ));
                }
                msg_text.push_str("_Use /logs N to show more (max 20)_");
                let chunks = crate::handler_utils::split_message(&msg_text, 4000);
                for chunk in chunks {
                    bot.send_message(chat_id, &chunk).await?;
                }
            }
        }
        _ => {
            bot.send_message(chat_id, "Unknown command. Try /help for available commands.")
                .await?;
        }
    }

    Ok(())
}

/// Download a Telegram voice message and transcribe it via Whisper
async fn transcribe_voice_message(bot: &TelegramBot, file_id: &str) -> Result<String> {
    let file = bot.get_file(file_id).await?;
    let file_path = file.file_path
        .ok_or_else(|| anyhow::anyhow!("Telegram getFile returned no file_path"))?;

    let bytes = bot.download_file(&file_path).await?;

    // Save to temp file
    let tmp_dir = std::env::temp_dir().join("openclaw-voice");
    std::fs::create_dir_all(&tmp_dir)?;
    let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f");
    let ogg_path = tmp_dir.join(format!("voice_{}.ogg", ts));
    let wav_path = tmp_dir.join(format!("voice_{}.wav", ts));

    std::fs::write(&ogg_path, &bytes)?;

    // Convert OGG to WAV via ffmpeg (Whisper needs WAV/MP3)
    let ffmpeg_status = tokio::process::Command::new("ffmpeg")
        .args(["-y", "-i"])
        .arg(&ogg_path)
        .args(["-ar", "16000", "-ac", "1", "-c:a", "pcm_s16le"])
        .arg(&wav_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;

    let _ = std::fs::remove_file(&ogg_path);

    match ffmpeg_status {
        Ok(status) if status.success() && wav_path.exists() => {}
        _ => {
            anyhow::bail!("ffmpeg conversion failed for voice message");
        }
    }

    // Try whisper-cpp first, then whisper CLI
    let transcript = try_whisper_transcribe(&wav_path).await;
    let _ = std::fs::remove_file(&wav_path);

    transcript
}

/// Try to transcribe a WAV file using available Whisper implementations
async fn try_whisper_transcribe(wav_path: &std::path::Path) -> Result<String> {
    // Try whisper-cpp (whisper-cpp binary)
    let result = tokio::process::Command::new("whisper-cpp")
        .arg("--model")
        .arg(resolve_whisper_model_path()?)
        .arg("--file")
        .arg(wav_path)
        .arg("--no-timestamps")
        .arg("--output-txt")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .await;

    if let Ok(output) = result {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !text.is_empty() {
                return Ok(text);
            }
            // whisper-cpp may write to .txt file
            let txt_path = wav_path.with_extension("wav.txt");
            if txt_path.exists() {
                let text = std::fs::read_to_string(&txt_path).unwrap_or_default().trim().to_string();
                let _ = std::fs::remove_file(&txt_path);
                if !text.is_empty() {
                    return Ok(text);
                }
            }
        }
    }

    // Try Python whisper CLI
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        tokio::process::Command::new("whisper")
            .arg(wav_path)
            .args(["--model", "base", "--language", "en", "--output_format", "txt"])
            .arg("--output_dir")
            .arg(wav_path.parent().unwrap_or(std::path::Path::new("/tmp")))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output(),
    ).await;

    if let Ok(Ok(output)) = result {
        if output.status.success() {
            // Python whisper writes to <filename>.txt
            let txt_path = wav_path.with_extension("txt");
            if txt_path.exists() {
                let text = std::fs::read_to_string(&txt_path).unwrap_or_default().trim().to_string();
                let _ = std::fs::remove_file(&txt_path);
                if !text.is_empty() {
                    return Ok(text);
                }
            }
        }
    }

    anyhow::bail!("No Whisper implementation available (tried whisper-cpp and whisper CLI)")
}

/// Resolve the Whisper model path
fn resolve_whisper_model_path() -> Result<String> {
    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;

    // Check common locations
    let candidates = [
        home.join(".openclaw/models/ggml-base.en.bin"),
        home.join(".local/share/whisper-cpp/ggml-base.en.bin"),
        std::path::PathBuf::from("/usr/share/whisper-cpp/models/ggml-base.en.bin"),
    ];

    for path in &candidates {
        if path.exists() {
            return Ok(path.to_string_lossy().to_string());
        }
    }

    // Default — let whisper-cpp handle model resolution
    Ok("base.en".to_string())
}

/// Download a Telegram photo and encode it as a base64 data URL
async fn download_and_encode_photo(bot: &TelegramBot, file_id: &str) -> Result<String> {
    use base64::Engine;

    let file = bot.get_file(file_id).await?;
    let file_path = file.file_path
        .ok_or_else(|| anyhow::anyhow!("Telegram getFile returned no file_path"))?;

    let bytes = bot.download_file(&file_path).await?;

    // Detect MIME type from extension
    let mime = if file_path.ends_with(".png") {
        "image/png"
    } else if file_path.ends_with(".gif") {
        "image/gif"
    } else if file_path.ends_with(".webp") {
        "image/webp"
    } else {
        "image/jpeg"
    };

    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{};base64,{}", mime, b64))
}
