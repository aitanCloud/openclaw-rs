use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tracing::{info, warn};

use openclaw_agent::llm::fallback::FallbackProvider;
use openclaw_agent::llm::streaming::StreamEvent;
use openclaw_agent::llm::LlmProvider;
use openclaw_agent::runtime::{self, AgentTurnConfig};
use openclaw_agent::sessions::SessionStore;
use openclaw_agent::tools::ToolRegistry;
use openclaw_agent::workspace;

use crate::config::GatewayConfig;
use crate::discord::{DiscordBot, DiscordMessage};

/// Minimum chars between Discord message edits
const EDIT_MIN_CHARS: usize = 60;
/// Minimum ms between Discord message edits
const EDIT_MIN_MS: u64 = 500;

/// Handle an incoming Discord message
pub async fn handle_discord_message(
    bot: &DiscordBot,
    msg: &DiscordMessage,
    config: &GatewayConfig,
) -> Result<()> {
    let channel_id = &msg.channel_id;
    let user_id = &msg.author.id;
    let user_name = &msg.author.username;

    let text = msg.content.trim();
    let has_image = msg.attachments.iter().any(|a| {
        a.content_type
            .as_deref()
            .map(|ct| ct.starts_with("image/"))
            .unwrap_or(false)
    });

    if text.is_empty() && !has_image {
        return Ok(());
    }

    // ── Access control ──
    if let Some(ref discord_config) = config.discord {
        if !discord_config.allowed_user_ids.is_empty() {
            let user_id_num: i64 = user_id.parse().unwrap_or(0);
            if !discord_config.allowed_user_ids.contains(&user_id_num) {
                warn!("Unauthorized Discord user {} ({})", user_id, user_name);
                bot.send_reply(channel_id, &msg.id, "⛔ Unauthorized. This bot is private.")
                    .await?;
                return Ok(());
            }
        }
    }

    info!(
        "Discord message from {} ({}): {}{}",
        user_name,
        user_id,
        &text[..text.len().min(100)],
        if has_image { " [+image]" } else { "" }
    );

    // ── Handle commands ──
    if (text.starts_with('/') || text.starts_with('!')) && !has_image {
        return handle_command(bot, channel_id, &msg.id, user_id, text, config, msg).await;
    }

    // ── Check for bot mention (strip it from text) ──
    let clean_text = strip_bot_mention(text);
    if clean_text.is_empty() && !has_image {
        return Ok(());
    }

    // ── Download image attachments ──
    let mut image_urls: Vec<String> = Vec::new();
    if has_image {
        for attachment in &msg.attachments {
            let is_image = attachment
                .content_type
                .as_deref()
                .map(|ct| ct.starts_with("image/"))
                .unwrap_or(false);
            if !is_image {
                continue;
            }
            match download_and_encode_attachment(&attachment.url, attachment.content_type.as_deref()).await {
                Ok(data_url) => {
                    info!("Downloaded Discord attachment: {} ({} bytes encoded)", attachment.filename, data_url.len());
                    image_urls.push(data_url);
                }
                Err(e) => {
                    warn!("Failed to download Discord attachment {}: {}", attachment.filename, e);
                }
            }
        }
    }

    let user_text = if clean_text.is_empty() && has_image {
        "What's in this image?".to_string()
    } else {
        clean_text.clone()
    };

    // ── Send typing indicator + placeholder ──
    bot.send_typing(channel_id).await.ok();
    let placeholder_id = bot
        .send_reply(channel_id, &msg.id, "🧠 ...")
        .await?;

    // ── Keep typing indicator alive ──
    let typing_token = tokio_util::sync::CancellationToken::new();
    let typing_cancel = typing_token.clone();
    let typing_bot_token = config
        .discord
        .as_ref()
        .map(|d| d.bot_token.clone())
        .unwrap_or_default();
    let typing_channel = channel_id.to_string();
    tokio::spawn(async move {
        let typing_bot = DiscordBot::new(&typing_bot_token);
        loop {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(8)) => {
                    typing_bot.send_typing(&typing_channel).await.ok();
                }
                _ = typing_cancel.cancelled() => break,
            }
        }
    });

    // ── Resolve workspace ──
    let workspace_dir = workspace::resolve_workspace_dir(&config.agent.name);
    if !workspace_dir.exists() {
        bot.edit_message(channel_id, &placeholder_id, "❌ Workspace not found.")
            .await?;
        return Ok(());
    }

    // ── Resolve provider ──
    let provider: Box<dyn LlmProvider> = if config.agent.fallback {
        match FallbackProvider::from_config() {
            Ok(fb) => Box::new(fb),
            Err(e) => {
                bot.edit_message(
                    channel_id,
                    &placeholder_id,
                    &format!("❌ Provider error: {}", e),
                )
                .await?;
                return Ok(());
            }
        }
    } else if let Some(ref model) = config.agent.model {
        match crate::handler_utils::resolve_single_provider(model) {
            Ok(p) => Box::new(p),
            Err(e) => {
                bot.edit_message(
                    channel_id,
                    &placeholder_id,
                    &format!("❌ Provider error: {}", e),
                )
                .await?;
                return Ok(());
            }
        }
    } else {
        match FallbackProvider::from_config() {
            Ok(fb) => Box::new(fb),
            Err(e) => {
                bot.edit_message(
                    channel_id,
                    &placeholder_id,
                    &format!("❌ Provider error: {}", e),
                )
                .await?;
                return Ok(());
            }
        }
    };

    // ── Session ──
    let session_key = format!("dc:{}:{}:{}", config.agent.name, user_id, channel_id);
    let store = match SessionStore::open(&config.agent.name) {
        Ok(s) => s,
        Err(e) => {
            bot.edit_message(
                channel_id,
                &placeholder_id,
                &format!("❌ Session error: {}", e),
            )
            .await?;
            return Ok(());
        }
    };
    store.create_session(&session_key, &config.agent.name, provider.name())?;

    // ── Tools + config (load plugins + MCP client tools) ──
    let tools = crate::handler::build_tool_registry(&workspace_dir).await;
    let dc_chat_id: i64 = channel_id.parse().unwrap_or(0);
    let task_chat_id = dc_chat_id;
    let task_query_fn: Option<openclaw_agent::tools::TaskQueryFn> = Some(std::sync::Arc::new(move |_cid| {
        crate::subagent_registry::list_tasks()
            .into_iter()
            .filter(|t| t.chat_id == task_chat_id)
            .map(|t| openclaw_agent::tools::TaskInfo {
                id: t.id,
                description: t.description.clone(),
                status: t.status.to_string(),
                elapsed_secs: t.started_at.elapsed().as_secs(),
                chat_id: t.chat_id,
            })
            .collect()
    }));
    let task_cancel_fn: Option<openclaw_agent::tools::TaskCancelFn> = Some(std::sync::Arc::new(|id| {
        crate::subagent_registry::cancel_subagent(id)
    }));
    let agent_config = AgentTurnConfig {
        agent_name: config.agent.name.clone(),
        session_key: session_key.clone(),
        workspace_dir: workspace_dir.to_string_lossy().to_string(),
        minimal_context: false,
        chat_id: dc_chat_id,
        task_query_fn,
        task_cancel_fn,
    ..AgentTurnConfig::default()
    };

    // ── Set up streaming channel ──
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<StreamEvent>();

    let t_start = std::time::Instant::now();

    // ── Register task for cancellation support ──
    let task_key = format!("dc:{}:{}", user_id, channel_id);
    let cancel_token = crate::task_registry::register_task(&task_key);
    let task_key_cleanup = task_key.clone();

    // ── Spawn agent turn with activity-based watchdog ──
    let watchdog = openclaw_agent::watchdog::ActivityWatchdog::new(
        std::time::Duration::from_secs(60),  // idle timeout: cancel if no activity for 60s
        std::time::Duration::from_secs(600), // max wall-clock safety net: 10 minutes
        cancel_token.clone(),
    );
    let watchdog_handle = watchdog.spawn("dc-agent");

    let user_text_owned = user_text.clone();
    let agent_handle = tokio::spawn(async move {
        let result = runtime::run_agent_turn_streaming(
            provider.as_ref(),
            &user_text_owned,
            &agent_config,
            &tools,
            event_tx,
            image_urls,
            Some(cancel_token),
        )
        .await;
        crate::task_registry::unregister_task(&task_key_cleanup);
        result
    });

    // ── Stream loop: receive events, edit Discord message in real-time ──
    let stream_bot_token = config
        .discord
        .as_ref()
        .map(|d| d.bot_token.clone())
        .unwrap_or_default();
    let stream_bot = DiscordBot::new(&stream_bot_token);
    let stream_channel = channel_id.to_string();
    let stream_placeholder = placeholder_id.clone();

    let mut accumulated = String::new();
    let mut last_edit_len: usize = 0;
    let mut last_edit_time = std::time::Instant::now();
    let mut tool_status = String::new();

    while let Some(event) = event_rx.recv().await {
        watchdog.touch(); // Signal activity to prevent idle timeout
        match event {
            StreamEvent::ContentDelta(delta) => {
                accumulated.push_str(&delta);

                let chars_since_edit = accumulated.len() - last_edit_len;
                let ms_since_edit = last_edit_time.elapsed().as_millis() as u64;

                if chars_since_edit >= EDIT_MIN_CHARS || ms_since_edit >= EDIT_MIN_MS {
                    let display = if tool_status.is_empty() {
                        accumulated.clone()
                    } else {
                        format!("{}\n\n{}", tool_status, accumulated)
                    };

                    stream_bot
                        .edit_message(&stream_channel, &stream_placeholder, &display)
                        .await
                        .ok();

                    last_edit_len = accumulated.len();
                    last_edit_time = std::time::Instant::now();
                }
            }

            StreamEvent::ReasoningDelta(_) => {}

            StreamEvent::ToolCallStart { name } => {
                tool_status = format!("🔧 Calling {}...", name);
                let display = if accumulated.is_empty() {
                    tool_status.clone()
                } else {
                    format!("{}\n\n{}", accumulated, tool_status)
                };
                stream_bot
                    .edit_message(&stream_channel, &stream_placeholder, &display)
                    .await
                    .ok();
            }

            StreamEvent::ToolExec { name, args_summary, .. } => {
                tool_status = if args_summary.is_empty() {
                    format!("⚙️ Running {}...", name)
                } else {
                    format!("⚙️ Running {} — {}...", name, args_summary)
                };
                let display = if accumulated.is_empty() {
                    tool_status.clone()
                } else {
                    format!("{}\n\n{}", accumulated, tool_status)
                };
                stream_bot
                    .edit_message(&stream_channel, &stream_placeholder, &display)
                    .await
                    .ok();
            }

            StreamEvent::ToolResult { name, success, .. } => {
                let icon = if success { "✅" } else { "❌" };
                tool_status = format!("{} {}", icon, name);
                let display = if accumulated.is_empty() {
                    tool_status.clone()
                } else {
                    format!("{}\n\n{}", accumulated, tool_status)
                };
                stream_bot
                    .edit_message(&stream_channel, &stream_placeholder, &display)
                    .await
                    .ok();
            }

            StreamEvent::RoundStart { round } => {
                tool_status = String::new();
                accumulated.clear();
                last_edit_len = 0;

                let display = format!("🔄 Round {}...", round);
                stream_bot
                    .edit_message(&stream_channel, &stream_placeholder, &display)
                    .await
                    .ok();
            }

            StreamEvent::Done => {
                break;
            }
        }
    }

    // ── Wait for agent turn to finish ──
    watchdog_handle.abort(); // Agent turn done, stop the watchdog
    let result = match agent_handle.await? {
        Ok(r) => r,
        Err(e) => {
            typing_token.cancel();
            warn!("Agent turn failed: {}", e);
            let error_msg = format!("❌ {}", e);
            stream_bot.edit_message(&stream_channel, &stream_placeholder, &error_msg).await?;
            return Ok(());
        }
    };
    typing_token.cancel();
    let elapsed = t_start.elapsed().as_millis();

    // ── Persist messages (SQLite + Postgres) ──
    store.append_message(
        &session_key,
        &openclaw_agent::llm::Message::user(&user_text),
    )?;
    // Save ALL turn messages (tool calls, tool results, final assistant) to SQLite
    for turn_msg in &result.turn_messages {
        store.append_message(&session_key, turn_msg)?;
    }
    if result.turn_messages.is_empty() && !result.response.is_empty() {
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
        let uid = user_id.to_string();
        let u_msg = user_text.clone();
        let turn_msgs = result.turn_messages.clone();
        let b_msg = result.response.clone();
        let tokens = result.total_usage.total_tokens as i64;
        tokio::spawn(async move {
            if let Ok(sid) = openclaw_db::sessions::upsert_session(
                &pool, &sk, &agent, &model, Some("discord"), Some(&uid),
            ).await {
                let _ = openclaw_db::messages::record_message(
                    &pool, sid, "user", Some(&u_msg), None, None, None,
                ).await;
                for turn_msg in &turn_msgs {
                    let role = match turn_msg.role {
                        openclaw_agent::llm::Role::Assistant => "assistant",
                        openclaw_agent::llm::Role::Tool => "tool",
                        openclaw_agent::llm::Role::User => "user",
                        openclaw_agent::llm::Role::System => "system",
                    };
                    let tc_json = turn_msg.tool_calls.as_ref()
                        .map(|tc| serde_json::to_value(tc).unwrap_or_default());
                    let _ = openclaw_db::messages::record_message(
                        &pool, sid, role,
                        turn_msg.content.as_deref(),
                        turn_msg.reasoning_content.as_deref(),
                        tc_json.as_ref(),
                        turn_msg.tool_call_id.as_deref(),
                    ).await;
                }
                if turn_msgs.is_empty() && !b_msg.is_empty() {
                    let _ = openclaw_db::messages::record_message(
                        &pool, sid, "assistant", Some(&b_msg), None, None, None,
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

    let stats = format!(
        "\n\n*{}ms · {} round(s) · {} tool(s) · {} tokens · {}*",
        elapsed,
        result.total_rounds,
        result.tool_calls_made,
        result.total_usage.total_tokens,
        result.model_name,
    );

    let full_response = format!("{}{}", response, stats);

    // Discord max is 2000 chars per message
    if full_response.len() <= 2000 {
        stream_bot
            .edit_message(&stream_channel, &stream_placeholder, &full_response)
            .await?;
    } else {
        // Edit first 2000 chars, send rest as new messages
        let first = &full_response[..2000.min(full_response.len())];
        stream_bot
            .edit_message(&stream_channel, &stream_placeholder, first)
            .await?;
        if full_response.len() > 2000 {
            let rest = &full_response[2000..];
            let overflow_bot = DiscordBot::new(&stream_bot_token);
            for chunk in crate::handler_utils::split_message(rest, 2000) {
                overflow_bot.send_message(&stream_channel, &chunk).await?;
            }
        }
    }

    info!(
        "Discord reply sent: {}ms, {} rounds, {} tools, {} tokens, model={}",
        elapsed,
        result.total_rounds,
        result.tool_calls_made,
        result.total_usage.total_tokens,
        result.model_name,
    );

    if let Some(m) = crate::metrics::global() {
        m.record_agent_turn(result.tool_calls_made as u64);
    }

    Ok(())
}

async fn handle_command(
    bot: &DiscordBot,
    channel_id: &str,
    reply_to: &str,
    user_id: &str,
    text: &str,
    config: &GatewayConfig,
    msg: &DiscordMessage,
) -> Result<()> {
    let cmd = text.split_whitespace().next().unwrap_or("");
    // Normalize: both /cmd and !cmd work
    let cmd = cmd.trim_start_matches('/').trim_start_matches('!');

    match cmd {
        "start" | "help" => {
            bot.send_embed(
                channel_id, Some(reply_to),
                "🦀 Rustbot Help",
                "Rust-powered OpenClaw agent. Send a message or use a command.\nUse `!` prefix instead of `/`. Send images for vision.",
                0x5865F2, // Discord blurple
                &[
                    ("Session", "`/new` `/clear` `/sessions` `/export`", false),
                    ("Info", "`/status` `/model` `/version` `/whoami` `/db`", false),
                    ("Monitoring", "`/stats` `/ping` `/history [N]` `/doctor` `/logs [N]`", false),
                    ("Control", "`/cancel` `/stop` `/voice` `/cron` `/tools` `/skills` `/config` `/runtime`", false),
                    ("Commands", "23", true),
                ],
            ).await?;
        }
        "ping" => {
            let start = std::time::Instant::now();
            let ms = start.elapsed().as_millis();
            let color = if ms < 100 { 0x57F287 } else if ms < 500 { 0xFEE75C } else { 0xED4245 };
            bot.send_embed(
                channel_id, Some(reply_to),
                "🏓 Pong!",
                &format!("Latency: **{}ms**", ms),
                color,
                &[],
            ).await?;
        }
        "cancel" | "stop" => {
            let task_key = format!("dc:{}:{}", user_id, channel_id);
            if crate::task_registry::cancel_task(&task_key) {
                if let Some(m) = crate::metrics::global() { m.record_task_cancelled(); }
                bot.send_embed(
                    channel_id, Some(reply_to),
                    "⛔ Task Cancelled",
                    "The running agent task has been stopped.",
                    0xED4245, // Discord red
                    &[
                        ("Active Tasks", &crate::task_registry::active_count().to_string(), true),
                    ],
                ).await?;
            } else {
                bot.send_reply(channel_id, reply_to, "ℹ️ No task is currently running.").await?;
            }
        }
        "stats" => {
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
                let session_str = session_count.to_string();
                let webhook_str = webhooks.to_string();
                let llm_stats = openclaw_agent::llm_log::stats();
                let llm_calls_str = llm_stats.total_recorded.to_string();
                let llm_errors_str = llm_stats.errors.to_string();
                let llm_avg_str = format!("{}ms", llm_stats.avg_latency_ms);
                bot.send_embed(
                    channel_id, Some(reply_to),
                    "📊 Gateway Stats",
                    &format!("Uptime: {}", uptime_str),
                    0x5865F2, // Discord blurple
                    &[
                        ("Telegram", &format!("{} req / {} err", tg_req, tg_err), true),
                        ("Discord", &format!("{} req / {} err", dc_req, dc_err), true),
                        ("Webhooks", &webhook_str, true),
                        ("Rate Limited", &rl.to_string(), true),
                        ("Completed", &completed.to_string(), true),
                        ("Agent Turns", &turns.to_string(), true),
                        ("Tool Calls", &tool_calls.to_string(), true),
                        ("Sessions", &session_str, true),
                        ("Avg Latency", &format!("{}ms", avg), true),
                        ("Error Rate", &format!("{:.1}%", err_rate), true),
                        ("Cancelled", &cancelled.to_string(), true),
                        ("Timeouts", &timeouts.to_string(), true),
                        ("Active Tasks", &crate::task_registry::active_count().to_string(), true),
                        ("LLM Calls", &llm_calls_str, true),
                        ("LLM Errors", &llm_errors_str, true),
                        ("LLM Avg Latency", &llm_avg_str, true),
                    ],
                ).await?;
            } else {
                bot.send_reply(channel_id, reply_to, "📊 Metrics not available.").await?;
            }
        }
        "version" => {
            let uptime_str = crate::human_uptime(crate::handler::BOOT_TIME.elapsed().as_secs());
            bot.send_embed(
                channel_id, Some(reply_to),
                "🦀 openclaw-gateway",
                &format!("v{}", env!("CARGO_PKG_VERSION")),
                0xF74C00, // Rust orange
                &[
                    ("Uptime", &uptime_str, true),
                    ("Agent", &config.agent.name, true),
                    ("Commands", "23", true),
                ],
            ).await?;
        }
        "doctor" => {
            let checks = crate::doctor::run_checks(&config.agent.name).await;
            let all_ok = checks.iter().all(|(_, ok, _)| *ok);
            let color = if all_ok { 0x57F287 } else { 0xED4245 }; // green or red
            let title = if all_ok { "🩺 Doctor — All Clear" } else { "🩺 Doctor — Issues Found" };
            let fields: Vec<(&str, String, bool)> = checks.iter()
                .map(|(name, ok, detail)| {
                    let status = if *ok { "✅" } else { "❌" };
                    (name.as_str(), format!("{} {}", status, detail), false)
                })
                .collect();
            let field_refs: Vec<(&str, &str, bool)> = fields.iter()
                .map(|(n, d, i)| (*n, d.as_str(), *i))
                .collect();
            bot.send_embed(
                channel_id, Some(reply_to),
                title,
                &format!("{}/{} checks passed", checks.iter().filter(|(_, ok, _)| *ok).count(), checks.len()),
                color,
                &field_refs,
            ).await?;
        }
        "tools" => {
            let workspace_dir = workspace::resolve_workspace_dir(&config.agent.name);
            let tools = crate::handler::build_tool_registry(&workspace_dir).await;
            let names = tools.tool_names();
            let tool_list = names.iter()
                .map(|n| format!("`{}`", n))
                .collect::<Vec<_>>()
                .join(" · ");
            bot.send_embed(
                channel_id, Some(reply_to),
                "🔧 Tools",
                &format!("{} tools available", names.len()),
                0x5865F2,
                &[("Tools", &tool_list, false)],
            ).await?;
        }
        "runtime" => {
            let pid = std::process::id();
            let uptime_secs = crate::handler::BOOT_TIME.elapsed().as_secs();
            let uptime_str = crate::human_uptime(uptime_secs);
            let rss = crate::doctor::human_bytes_pub(crate::process_rss_bytes());
            let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
            bot.send_embed(
                channel_id, Some(reply_to),
                "🖥️ Runtime Info",
                &format!("v{}", env!("CARGO_PKG_VERSION")),
                0x2ECC71, // Green
                &[
                    ("Built", env!("BUILD_TIMESTAMP"), true),
                    ("Started", &*crate::handler::BOOT_TIMESTAMP, true),
                    ("Profile", profile, true),
                    ("PID", &pid.to_string(), true),
                    ("Memory", &rss, true),
                    ("Uptime", &uptime_str, true),
                    ("OS", std::env::consts::OS, true),
                    ("Arch", std::env::consts::ARCH, true),
                ],
            ).await?;
        }
        "config" => {
            let discord_status = if config.discord.is_some() { "Enabled" } else { "Disabled" };
            let webhook_status = if config.webhook.is_some() { "Configured" } else { "Not configured" };
            let sandbox_info = config.agent.sandbox.as_ref()
                .map(|sb| format!("timeout={}s, max_concurrent={}",
                    sb.turn_timeout_secs.unwrap_or(120),
                    sb.max_concurrent.unwrap_or(4)))
                .unwrap_or_else(|| "defaults".to_string());
            let providers = openclaw_agent::llm::fallback::FallbackProvider::from_config()
                .ok()
                .map(|fb| fb.provider_labels().iter().map(|s| s.to_string()).collect::<Vec<_>>())
                .unwrap_or_default();
            let provider_str = if providers.is_empty() { "none".to_string() } else { providers.join(", ") };
            bot.send_embed(
                channel_id, Some(reply_to),
                "⚙️ Gateway Config",
                &format!("v{}", env!("CARGO_PKG_VERSION")),
                0x95A5A6, // Gray
                &[
                    ("Agent", &config.agent.name, true),
                    ("Fallback", &config.agent.fallback.to_string(), true),
                    ("Model", config.agent.model.as_deref().unwrap_or("(default)"), true),
                    ("Discord", discord_status, true),
                    ("Webhook", webhook_status, true),
                    ("Sandbox", &sandbox_info, true),
                    ("Providers", &provider_str, false),
                ],
            ).await?;
        }
        "skills" => {
            let workspace_dir = openclaw_agent::workspace::resolve_workspace_dir(&config.agent.name);
            let skills_dir = workspace_dir.join("skills");
            let skills = openclaw_core::skills::list_skills(&skills_dir).unwrap_or_default();
            if skills.is_empty() {
                bot.send_embed(
                    channel_id, Some(reply_to),
                    "📚 Skills",
                    "No skills found in workspace.",
                    0x5865F2,
                    &[],
                ).await?;
            } else {
                let mut fields: Vec<(String, String, bool)> = Vec::new();
                for skill in &skills {
                    let desc = skill.description.as_deref().unwrap_or("(no description)");
                    fields.push((skill.name.clone(), desc.to_string(), true));
                }
                let field_refs: Vec<(&str, &str, bool)> = fields.iter()
                    .map(|(n, d, i)| (n.as_str(), d.as_str(), *i))
                    .collect();
                bot.send_embed(
                    channel_id, Some(reply_to),
                    "📚 Skills",
                    &format!("{} skill(s) available", skills.len()),
                    0x9B59B6, // Purple
                    &field_refs,
                ).await?;
            }
        }
        "whoami" => {
            let session_key = format!("dc:{}:{}:{}", config.agent.name, user_id, channel_id);
            let is_allowed = config.discord.as_ref()
                .map(|dc| dc.allowed_user_ids.is_empty() || dc.allowed_user_ids.contains(&user_id.parse::<i64>().unwrap_or(0)))
                .unwrap_or(false);
            bot.send_embed(
                channel_id, Some(reply_to),
                "👤 Who Am I",
                &format!("**{}** ({})", msg.author.username, user_id),
                0x57F287, // Green
                &[
                    ("Channel", channel_id, true),
                    ("Session Key", &session_key, false),
                    ("Authorized", if is_allowed { "✅ Yes" } else { "❌ No" }, true),
                ],
            ).await?;
        }
        "clear" => {
            let store = SessionStore::open(&config.agent.name)?;
            let session_key = format!("dc:{}:{}:{}", config.agent.name, user_id, channel_id);
            let active_key = store.find_latest_session(&session_key)?
                .unwrap_or(session_key.clone());
            let deleted = store.delete_session(&active_key)?;
            bot.send_embed(
                channel_id, Some(reply_to),
                "🗑️ Session Cleared",
                &format!("Deleted {} message(s)", deleted),
                0x57F287, // Discord green
                &[
                    ("Session", &session_key, false),
                ],
            ).await?;
        }
        cmd if cmd == "history" || cmd.starts_with("history ") => {
            let count: usize = text.split_whitespace().nth(1)
                .and_then(|n| n.parse().ok())
                .unwrap_or(5)
                .min(20);
            let store = SessionStore::open(&config.agent.name)?;
            let session_key = format!("dc:{}:{}:{}", config.agent.name, user_id, channel_id);
            let active_key = store.find_latest_session(&session_key)?
                .unwrap_or(session_key);
            let msgs = store.load_messages(&active_key)?;
            let recent: Vec<_> = msgs.iter().rev().take(count).collect();

            if recent.is_empty() {
                bot.send_embed(
                    channel_id, Some(reply_to),
                    "📜 History",
                    "No messages in current session.",
                    0x5865F2,
                    &[],
                ).await?;
            } else {
                let mut fields: Vec<(String, String, bool)> = Vec::new();
                for msg in recent.iter().rev() {
                    let role = match msg.role.as_str() {
                        "user" => "👤 User",
                        "assistant" => "🤖 Assistant",
                        "system" => "⚙️ System",
                        "tool" => "🔧 Tool",
                        _ => "❓ Unknown",
                    };
                    let content = msg.content.as_deref().unwrap_or("[no content]");
                    let preview = if content.len() > 100 {
                        format!("{}...", &content[..100])
                    } else {
                        content.to_string()
                    };
                    fields.push((role.to_string(), preview, false));
                }
                let field_refs: Vec<(&str, &str, bool)> = fields.iter()
                    .map(|(n, d, i)| (n.as_str(), d.as_str(), *i))
                    .collect();
                bot.send_embed(
                    channel_id, Some(reply_to),
                    "📜 History",
                    &format!("Last {} of {} messages", recent.len(), msgs.len()),
                    0x5865F2, // Discord blurple
                    &field_refs,
                ).await?;
            }
        }
        "db" => {
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
                    bot.send_embed(
                        channel_id, Some(reply_to),
                        "🗄️ Session Database",
                        &format!("Agent: {}", config.agent.name),
                        0xE67E22, // Orange
                        &[
                            ("Sessions", &stats.session_count.to_string(), true),
                            ("Messages", &stats.message_count.to_string(), true),
                            ("Avg/Session", &avg_msgs, true),
                            ("Tokens", &stats.total_tokens.to_string(), true),
                            ("DB Size", &size, true),
                            ("Oldest", &oldest, true),
                            ("Newest", &newest, true),
                        ],
                    ).await?;
                }
                Err(e) => {
                    bot.send_reply(channel_id, reply_to, &format!("❌ Failed to get DB stats: {}", e)).await?;
                }
            }
        }
        "new" | "reset" => {
            let store = SessionStore::open(&config.agent.name)?;
            let old_key = format!("dc:{}:{}:{}", config.agent.name, user_id, channel_id);
            let old_msgs = store.load_messages(&old_key)?;
            let msg_count = old_msgs.len();

            let new_key = format!(
                "dc:{}:{}:{}:{}",
                config.agent.name,
                user_id,
                channel_id,
                uuid::Uuid::new_v4()
            );
            store.create_session(&new_key, &config.agent.name, "pending")?;

            bot.send_embed(
                channel_id, Some(reply_to),
                "🔄 New Session",
                "Starting fresh with a clean context.",
                0x57F287, // Discord green
                &[
                    ("Previous Messages", &msg_count.to_string(), true),
                    ("Agent", &config.agent.name, true),
                ],
            ).await?;
        }
        "status" => {
            let store = SessionStore::open(&config.agent.name)?;
            let session_key = format!("dc:{}:{}:{}", config.agent.name, user_id, channel_id);
            let sessions = store.list_sessions(&config.agent.name, 100)?;
            let current = sessions.iter().find(|s| s.session_key == session_key);

            let msg_count = current.map(|s| s.message_count).unwrap_or(0);
            let tokens = current.map(|s| s.total_tokens).unwrap_or(0);

            let model_info = if config.agent.fallback {
                match FallbackProvider::from_config() {
                    Ok(fb) => fb.provider_labels().join(" → "),
                    Err(_) => "(error)".to_string(),
                }
            } else {
                config.agent.model.as_deref().unwrap_or("default").to_string()
            };

            bot.send_embed(
                channel_id, Some(reply_to),
                "🦀 Rustbot Status",
                &format!("Agent: `{}`", config.agent.name),
                0x57F287, // Discord green
                &[
                    ("Model", &model_info, true),
                    ("Fallback", if config.agent.fallback { "✅" } else { "❌" }, true),
                    ("Active Tasks", &crate::task_registry::active_count().to_string(), true),
                    ("Messages", &msg_count.to_string(), true),
                    ("Tokens", &tokens.to_string(), true),
                    ("Sessions", &sessions.len().to_string(), true),
                ],
            ).await?;
        }
        "model" => {
            if config.agent.fallback {
                match FallbackProvider::from_config() {
                    Ok(fb) => {
                        let labels = fb.provider_labels();
                        let mut chain_desc = String::new();
                        for (i, label) in labels.iter().enumerate() {
                            let marker = if i == 0 { "🥇" } else if i == 1 { "🥈" } else { "🥉" };
                            chain_desc.push_str(&format!("{} `{}`\n", marker, label));
                        }
                        bot.send_embed(
                            channel_id, Some(reply_to),
                            "🔗 Fallback Chain",
                            &chain_desc,
                            0xF74C00, // Rust orange
                            &[
                                ("Providers", &labels.len().to_string(), true),
                                ("Mode", "First available", true),
                                ("Circuit Breaker", ">3 failures", true),
                            ],
                        ).await?;
                    }
                    Err(e) => {
                        bot.send_reply(channel_id, reply_to, &format!("❌ Error: {}", e)).await?;
                    }
                }
            } else {
                bot.send_embed(
                    channel_id, Some(reply_to),
                    "🤖 Model",
                    &format!("`{}`", config.agent.model.as_deref().unwrap_or("default")),
                    0xF74C00, // Rust orange
                    &[("Fallback", "❌ Disabled", true)],
                ).await?;
            }
        }
        "sessions" => {
            let store = SessionStore::open(&config.agent.name)?;
            let sessions = store.list_sessions(&config.agent.name, 10)?;

            if sessions.is_empty() {
                bot.send_reply(channel_id, reply_to, "No sessions found.")
                    .await?;
            } else {
                let total_msgs: i64 = sessions.iter().map(|s| s.message_count).sum();
                let total_tokens: i64 = sessions.iter().map(|s| s.total_tokens).sum();
                let mut desc = String::new();
                for (i, s) in sessions.iter().enumerate() {
                    let age = chrono::Utc::now().timestamp_millis() - s.updated_at_ms;
                    desc.push_str(&format!(
                        "{}. `{}` — {} msgs, {} tokens, {}\n",
                        i + 1,
                        &s.session_key[..s.session_key.len().min(30)],
                        s.message_count,
                        s.total_tokens,
                        crate::handler_utils::format_duration(age),
                    ));
                }
                bot.send_embed(
                    channel_id, Some(reply_to),
                    "📋 Recent Sessions",
                    &desc,
                    0x5865F2, // Discord blurple
                    &[
                        ("Total", &sessions.len().to_string(), true),
                        ("Messages", &total_msgs.to_string(), true),
                        ("Tokens", &total_tokens.to_string(), true),
                    ],
                ).await?;
            }
        }
        "export" => {
            let store = SessionStore::open(&config.agent.name)?;
            let session_key = format!("dc:{}:{}:{}", config.agent.name, user_id, channel_id);
            let messages = store.load_messages(&session_key)?;

            if messages.is_empty() {
                bot.send_reply(channel_id, reply_to, "No messages in current session to export.")
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
                    md.push_str(&format!(
                        "### {} {}\n\n{}\n\n---\n\n",
                        role_icon,
                        msg.role,
                        msg.content.as_deref().unwrap_or("(empty)"),
                    ));
                }
                md.push_str(&format!("*Exported {} messages*", messages.len()));

                let chunks = crate::handler_utils::split_message(&md, 2000);
                for chunk in chunks {
                    bot.send_message(channel_id, &chunk).await?;
                }
            }
        }
        "voice" => {
            let voice_text = text.split_whitespace().skip(1).collect::<Vec<&str>>().join(" ");
            if voice_text.is_empty() {
                bot.send_reply(channel_id, reply_to, "Usage: `/voice <text to speak>`\n\nI'll respond and send it as a voice message.").await?;
            } else {
                bot.send_typing(channel_id).await.ok();

                let provider: Box<dyn LlmProvider> = match FallbackProvider::from_config() {
                    Ok(fb) => Box::new(fb),
                    Err(e) => {
                        bot.send_reply(channel_id, reply_to, &format!("❌ Provider error: {}", e)).await?;
                        return Ok(());
                    }
                };

                let workspace_dir = workspace::resolve_workspace_dir(&config.agent.name);
                let session_key = format!("dc:{}:{}:{}", config.agent.name, user_id, channel_id);
                let store = SessionStore::open(&config.agent.name)?;
                store.create_session(&session_key, &config.agent.name, provider.name())?;

                let tools = crate::handler::build_tool_registry(&workspace_dir).await;
                let agent_config = AgentTurnConfig {
                    agent_name: config.agent.name.clone(),
                    session_key: session_key.clone(),
                    workspace_dir: workspace_dir.to_string_lossy().to_string(),
                    minimal_context: true,
                ..AgentTurnConfig::default()
                };

                let (event_tx, _event_rx) = mpsc::unbounded_channel::<StreamEvent>();
                let result = runtime::run_agent_turn_streaming(
                    provider.as_ref(),
                    &voice_text,
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
                store.append_message(&session_key, &openclaw_agent::llm::Message::user(&voice_text))?;
                store.append_message(&session_key, &openclaw_agent::llm::Message::assistant(&response_text))?;

                // Generate TTS via Piper
                let tts_dir = workspace_dir.join("tts-output");
                std::fs::create_dir_all(&tts_dir)?;
                let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
                let wav_path = tts_dir.join(format!("voice_{}.wav", ts));
                let ogg_path = tts_dir.join(format!("voice_{}.ogg", ts));

                let home = dirs::home_dir()
                    .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
                let model_path = home
                    .join(".openclaw/projects/voice-control/models/en_GB-jenny_dioco-medium.onnx");

                if !model_path.exists() {
                    bot.send_reply(channel_id, reply_to, &format!("🗣️ {}\n\n_(Piper TTS model not found)_", response_text)).await?;
                    return Ok(());
                }

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
                                let caption = if response_text.len() > 200 {
                                    Some(format!("{}...", &response_text[..197]))
                                } else {
                                    Some(response_text.clone())
                                };
                                bot.send_file(channel_id, &ogg_path, "voice.ogg", caption.as_deref()).await?;
                                let _ = std::fs::remove_file(&wav_path);
                                let _ = std::fs::remove_file(&ogg_path);
                            }
                            _ => {
                                warn!("ffmpeg conversion failed, sending text response");
                                bot.send_reply(channel_id, reply_to, &format!("🗣️ {}\n\n_(Voice conversion failed)_", response_text)).await?;
                                let _ = std::fs::remove_file(&wav_path);
                            }
                        }
                    }
                    _ => {
                        warn!("Piper TTS failed, sending text response");
                        bot.send_reply(channel_id, reply_to, &format!("🗣️ {}\n\n_(TTS generation failed)_", response_text)).await?;
                    }
                }
            }
        }
        "cron" => {
            let cron_path = openclaw_core::paths::cron_jobs_path();
            if !cron_path.exists() {
                bot.send_reply(channel_id, reply_to, "No cron jobs file found.")
                    .await?;
            } else {
                let parts: Vec<&str> = text.split_whitespace().collect();

                match parts.get(1).map(|s| *s) {
                    Some("enable") | Some("disable") => {
                        let action = parts[1];
                        let target = parts.get(2).map(|s| *s).unwrap_or("");
                        if target.is_empty() {
                            bot.send_reply(
                                channel_id,
                                reply_to,
                                "Usage: `/cron enable <name>` or `/cron disable <name>`",
                            )
                            .await?;
                        } else {
                            match crate::handler_utils::toggle_cron_job(&cron_path, target, action == "enable") {
                                Ok(job_name) => {
                                    let icon = if action == "enable" { "✅" } else { "⏸️" };
                                    bot.send_reply(
                                        channel_id,
                                        reply_to,
                                        &format!("{} Cron job '{}' {}d.", icon, job_name, action),
                                    )
                                    .await?;
                                }
                                Err(e) => {
                                    bot.send_reply(
                                        channel_id,
                                        reply_to,
                                        &format!("❌ {}", e),
                                    )
                                    .await?;
                                }
                            }
                        }
                    }
                    _ => {
                        match openclaw_core::cron::load_cron_jobs(&cron_path) {
                            Ok(cron_file) => {
                                if cron_file.jobs.is_empty() {
                                    bot.send_embed(
                                        channel_id, Some(reply_to),
                                        "⏰ Cron Jobs",
                                        "No cron jobs configured.",
                                        0x5865F2,
                                        &[],
                                    ).await?;
                                } else {
                                    let mut fields: Vec<(String, String, bool)> = Vec::new();
                                    for job in &cron_file.jobs {
                                        let status = if job.enabled { "✅" } else { "⏸️" };
                                        let last_run = job
                                            .state
                                            .as_ref()
                                            .and_then(|s| s.last_run_at_ms)
                                            .map(|ms| {
                                                let age = chrono::Utc::now().timestamp_millis()
                                                    - ms as i64;
                                                crate::handler_utils::format_duration(age)
                                            })
                                            .unwrap_or_else(|| "never".to_string());
                                        let last_status = job
                                            .state
                                            .as_ref()
                                            .and_then(|s| s.last_status.as_deref())
                                            .unwrap_or("—");
                                        let duration = job
                                            .state
                                            .as_ref()
                                            .and_then(|s| s.last_duration_ms)
                                            .map(|ms| format!("{:.1}s", ms as f64 / 1000.0))
                                            .unwrap_or_else(|| "—".to_string());

                                        fields.push((
                                            format!("{} {}", status, job.name),
                                            format!("`{}` · Last: {} ({}) · {}", job.schedule, last_run, last_status, duration),
                                            false,
                                        ));
                                    }
                                    let field_refs: Vec<(&str, &str, bool)> = fields.iter()
                                        .map(|(n, d, i)| (n.as_str(), d.as_str(), *i))
                                        .collect();
                                    bot.send_embed(
                                        channel_id, Some(reply_to),
                                        "⏰ Cron Jobs",
                                        &format!("{} job(s) · Use `/cron enable|disable <name>`", cron_file.jobs.len()),
                                        0xFA9A28, // Orange
                                        &field_refs,
                                    ).await?;
                                }
                            }
                            Err(e) => {
                                bot.send_reply(
                                    channel_id,
                                    reply_to,
                                    &format!("❌ Failed to load cron jobs: {}", e),
                                )
                                .await?;
                            }
                        }
                    }
                }
            }
        }
        "logs" => {
            let count: usize = text.split_whitespace().nth(1)
                .and_then(|n| n.parse().ok())
                .unwrap_or(5)
                .min(20);
            let entries = openclaw_agent::llm_log::recent(count);
            let total = openclaw_agent::llm_log::total_count();

            if entries.is_empty() {
                bot.send_embed(
                    channel_id, Some(reply_to),
                    "📋 LLM Activity Log",
                    "No LLM activity recorded yet.",
                    0x5865F2,
                    &[],
                ).await?;
            } else {
                let mut fields: Vec<(String, String, bool)> = Vec::new();
                for (i, entry) in entries.iter().enumerate() {
                    let status = if entry.error.is_some() { "❌" } else { "✅" };
                    let content_preview = entry.response_content.as_deref()
                        .map(|c| if c.len() > 80 { format!("{}…", &c[..77]) } else { c.to_string() })
                        .unwrap_or_else(|| {
                            if entry.response_tool_calls > 0 {
                                format!("[{} tool(s): {}]", entry.response_tool_calls, entry.tool_call_names.join(", "))
                            } else if let Some(ref err) = entry.error {
                                format!("ERR: {}", &err[..err.len().min(60)])
                            } else {
                                "(no content)".to_string()
                            }
                        });
                    fields.push((
                        format!("{}. {} `{}`", i + 1, status, entry.model),
                        format!("{}ms · {}tok · {}", entry.latency_ms, entry.usage_total_tokens, content_preview),
                        false,
                    ));
                }
                let field_refs: Vec<(&str, &str, bool)> = fields.iter()
                    .map(|(n, d, i)| (n.as_str(), d.as_str(), *i))
                    .collect();
                bot.send_embed(
                    channel_id, Some(reply_to),
                    "📋 LLM Activity Log",
                    &format!("Showing {} of {} total · Use `/logs N` for more (max 20)", entries.len(), total),
                    0x2ECC71, // Green
                    &field_refs,
                ).await?;
            }
        }
        _ => {
            bot.send_reply(
                channel_id,
                reply_to,
                "Unknown command. Try `/help` for available commands.",
            )
            .await?;
        }
    }

    Ok(())
}

/// Strip bot mention from message text (e.g. "<@123456789> hello" -> "hello")
fn strip_bot_mention(text: &str) -> String {
    let stripped = if text.starts_with("<@") {
        if let Some(end) = text.find('>') {
            text[end + 1..].trim().to_string()
        } else {
            text.to_string()
        }
    } else {
        text.to_string()
    };
    stripped
}

/// Download a Discord attachment and encode it as a base64 data URL
async fn download_and_encode_attachment(url: &str, content_type: Option<&str>) -> Result<String> {
    use base64::Engine;

    let client = reqwest::Client::new();
    let resp = client
        .get(url)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .context("Failed to download Discord attachment")?;

    if !resp.status().is_success() {
        anyhow::bail!("Attachment download failed: {}", resp.status());
    }

    let bytes = resp.bytes().await.context("Failed to read attachment bytes")?;

    let mime = content_type.unwrap_or_else(|| {
        if url.ends_with(".png") {
            "image/png"
        } else if url.ends_with(".gif") {
            "image/gif"
        } else if url.ends_with(".webp") {
            "image/webp"
        } else {
            "image/jpeg"
        }
    });

    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{};base64,{}", mime, b64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_bot_mention() {
        assert_eq!(strip_bot_mention("<@123456789> hello"), "hello");
        assert_eq!(strip_bot_mention("<@!123456789> hello world"), "hello world");
        assert_eq!(strip_bot_mention("hello"), "hello");
        assert_eq!(strip_bot_mention("<@123> "), "");
    }

    #[test]
    fn test_split_message_discord() {
        let short = "hello";
        assert_eq!(crate::handler_utils::split_message(short, 2000), vec!["hello"]);

        let long = "a\n".repeat(1500);
        let chunks = crate::handler_utils::split_message(&long, 2000);
        for chunk in &chunks {
            assert!(chunk.len() <= 2000);
        }
    }
}
