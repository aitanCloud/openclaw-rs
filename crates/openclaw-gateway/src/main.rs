mod config;
mod cron;
mod debounce;
mod discord;
mod discord_handler;
mod handler;
mod metrics;
mod ratelimit;
mod telegram;

use axum::{routing::get, Json, Router};
use std::sync::Arc;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "openclaw_gateway=info".into()),
        )
        .init();

    let config = config::GatewayConfig::from_file_or_env("/etc/openclaw-gateway/config.json")?;

    // ── Startup banner ──
    info!("╔══════════════════════════════════════════╗");
    info!("║  openclaw-gateway v{}              ║", env!("CARGO_PKG_VERSION"));
    info!("╚══════════════════════════════════════════╝");
    info!("Agent: {} | Fallback: {}", config.agent.name, config.agent.fallback);
    info!("Telegram allowed users: {:?}", config.telegram.allowed_user_ids);
    if let Some(ref dc) = config.discord {
        info!("Discord enabled | allowed users: {:?}", dc.allowed_user_ids);
    }
    info!("Commands: 14 (/help /new /status /model /sessions /export /voice /ping /history /clear /db /version /stats /cron)");

    // ── Verify bot token ──
    let bot = telegram::TelegramBot::new(&config.telegram.bot_token);
    let me = bot.get_me().await?;
    info!(
        "Telegram bot verified: @{} ({})",
        me.username.as_deref().unwrap_or("?"),
        me.first_name
    );

    // ── Session maintenance on startup ──
    match openclaw_agent::sessions::SessionStore::open(&config.agent.name) {
        Ok(store) => {
            // Migrate old session keys (v0.15 → v0.16 format)
            match store.migrate_old_session_keys() {
                Ok(0) => {}
                Ok(n) => info!("Migrated {} old session key(s) to user-based format", n),
                Err(e) => warn!("Session key migration failed: {}", e),
            }
            // Prune sessions older than 30 days
            match store.prune_old_sessions(30) {
                Ok(0) => {}
                Ok(n) => info!("Pruned {} stale session(s) older than 30 days", n),
                Err(e) => warn!("Session pruning failed: {}", e),
            }
            // Log DB stats
            if let Ok(stats) = store.db_stats(&config.agent.name) {
                let size = if stats.db_size_bytes > 1_048_576 {
                    format!("{:.1} MB", stats.db_size_bytes as f64 / 1_048_576.0)
                } else {
                    format!("{:.1} KB", stats.db_size_bytes as f64 / 1024.0)
                };
                info!("Session DB: {} sessions, {} messages, {}", stats.session_count, stats.message_count, size);
            }
        }
        Err(e) => warn!("Could not open session store for maintenance: {}", e),
    }

    // ── Start cron executor ──
    let cron_bot = Arc::new(telegram::TelegramBot::new(&config.telegram.bot_token));
    let cron_config = Arc::new(config.clone());
    cron::spawn_cron_executor(cron_config, cron_bot);

    // ── Metrics (single instance shared via Arc + global static) ──
    let gateway_metrics = Arc::new(metrics::GatewayMetrics::new());
    // Safety: Arc keeps it alive for program lifetime; leak a clone for global handler access
    let metrics_ref: &'static metrics::GatewayMetrics = unsafe {
        &*(Arc::as_ptr(&gateway_metrics))
    };
    metrics::init_global(metrics_ref);

    // ── Start health check HTTP server ──
    let start_time = std::time::Instant::now();
    let health_config = Arc::new(config.clone());
    let tools = openclaw_agent::tools::ToolRegistry::with_defaults();
    let tool_names: Vec<String> = tools.tool_names().iter().map(|s| s.to_string()).collect();
    let tool_names = Arc::new(tool_names);
    let app = Router::new()
        .route("/health", get(health_handler))
        .route(
            "/status",
            get({
                let cfg = health_config.clone();
                let tnames = tool_names.clone();
                let m = gateway_metrics.clone();
                move || status_handler(cfg, tnames, start_time, m)
            }),
        )
        .route(
            "/metrics",
            get({
                let m = gateway_metrics.clone();
                move || metrics_handler(m)
            }),
        )
        .route(
            "/metrics/json",
            get({
                let m = gateway_metrics.clone();
                move || metrics_json_handler(m)
            }),
        );

    let http_port: u16 = std::env::var("HTTP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3100);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", http_port)).await?;
    info!("Health endpoint listening on :{}", http_port);

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            error!("HTTP server error: {}", e);
        }
    });

    // ── Message debouncer (collect rapid messages) ──
    let debouncer = Arc::new(debounce::MessageDebouncer::new(1500, 5));

    // ── Rate limiter (config-driven) ──
    let rl_msgs = config.agent.sandbox.as_ref()
        .and_then(|s| s.rate_limit_messages).unwrap_or(10);
    let rl_window = config.agent.sandbox.as_ref()
        .and_then(|s| s.rate_limit_window_secs).unwrap_or(60);
    let rate_limiter = Arc::new(ratelimit::RateLimiter::new(rl_msgs, rl_window));

    // ── Active task tracker (config-driven concurrency) ──
    let max_concurrent = config.agent.sandbox.as_ref()
        .and_then(|s| s.max_concurrent).unwrap_or(5);
    let active_tasks: Arc<tokio::sync::Semaphore> = Arc::new(tokio::sync::Semaphore::new(max_concurrent));

    // ── Start Discord gateway (if configured) ──
    if let Some(ref discord_config) = config.discord {
        info!("Discord bot configured, starting gateway...");
        let discord_bot = Arc::new(discord::DiscordBot::new(&discord_config.bot_token));

        // Verify Discord bot token
        match discord_bot.get_me().await {
            Ok(me) => {
                info!("Discord bot verified: @{}", me.username);
            }
            Err(e) => {
                error!("Discord bot verification failed: {}", e);
            }
        }

        let discord_config_clone = config.clone();
        let discord_rate_limiter = rate_limiter.clone();
        let discord_active_tasks = active_tasks.clone();
        let discord_metrics = gateway_metrics.clone();

        tokio::spawn(async move {
            match discord_bot.clone().connect_gateway().await {
                Ok(mut msg_rx) => {
                    info!("Discord Gateway connected, listening for messages...");
                    while let Some(msg) = msg_rx.recv().await {
                        let user_id: i64 = msg.author.id.parse().unwrap_or(0);
                        discord_metrics.record_discord_request();

                        // Rate limit check
                        if let Err(wait_secs) = discord_rate_limiter.check(user_id) {
                            discord_metrics.record_rate_limited();
                            let rl_bot = discord::DiscordBot::new(
                                &discord_config_clone.discord.as_ref().unwrap().bot_token,
                            );
                            warn!("Rate limited Discord user {} for {}s", user_id, wait_secs);
                            let _ = rl_bot
                                .send_reply(
                                    &msg.channel_id,
                                    &msg.id,
                                    &format!("⏳ Rate limited. Try again in {}s.", wait_secs),
                                )
                                .await;
                            continue;
                        }

                        // Concurrency control
                        let permit = match discord_active_tasks.clone().try_acquire_owned() {
                            Ok(p) => p,
                            Err(_) => {
                                discord_metrics.record_concurrency_rejected();
                                let busy_bot = discord::DiscordBot::new(
                                    &discord_config_clone.discord.as_ref().unwrap().bot_token,
                                );
                                let _ = busy_bot
                                    .send_reply(
                                        &msg.channel_id,
                                        &msg.id,
                                        "🔄 Server busy — too many concurrent requests. Try again shortly.",
                                    )
                                    .await;
                                continue;
                            }
                        };

                        let bot_clone = discord::DiscordBot::new(
                            &discord_config_clone.discord.as_ref().unwrap().bot_token,
                        );
                        let config_clone = discord_config_clone.clone();
                        let task_metrics = discord_metrics.clone();

                        tokio::spawn(async move {
                            let _permit = permit;
                            let start = std::time::Instant::now();
                            if let Err(e) =
                                discord_handler::handle_discord_message(&bot_clone, &msg, &config_clone)
                                    .await
                            {
                                task_metrics.record_discord_error();
                                error!("Discord handler error: {}", e);
                            }
                            task_metrics.record_completion(start.elapsed().as_millis() as u64);
                        });
                    }
                }
                Err(e) => {
                    error!("Failed to connect Discord Gateway: {}", e);
                }
            }
        });
    }

    // ── Telegram long-polling loop with graceful shutdown ──
    info!("Starting Telegram long-polling...");
    let mut offset: i64 = 0;

    let shutdown = async {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("Failed to register SIGTERM handler");
        #[cfg(unix)]
        let sigterm_fut = sigterm.recv();
        #[cfg(not(unix))]
        let sigterm_fut = std::future::pending::<Option<()>>();

        tokio::select! {
            _ = ctrl_c => info!("Received SIGINT (Ctrl+C)"),
            _ = sigterm_fut => info!("Received SIGTERM"),
        }
    };
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("Shutdown signal received, draining active tasks...");
                // Wait for active tasks to finish (up to 30s)
                let drain_start = std::time::Instant::now();
                while active_tasks.available_permits() < max_concurrent {
                    if drain_start.elapsed().as_secs() > 30 {
                        warn!("Drain timeout — {} tasks still active, forcing shutdown",
                            max_concurrent - active_tasks.available_permits());
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                info!("Graceful shutdown complete");
                return Ok(());
            }
            result = bot.get_updates(offset, 30) => {
                match result {
                    Ok(updates) => {
                        for update in updates {
                            offset = update.update_id + 1;

                            if let Some(msg) = update.message {
                                let user_id = msg.from.as_ref().map(|u| u.id).unwrap_or(0);
                                gateway_metrics.record_telegram_request();

                                // Rate limit check
                                if let Err(wait_secs) = rate_limiter.check(user_id) {
                                    gateway_metrics.record_rate_limited();
                                    let rl_bot = telegram::TelegramBot::new(&config.telegram.bot_token);
                                    let chat_id = msg.chat.id;
                                    warn!("Rate limited user {} for {}s", user_id, wait_secs);
                                    let _ = rl_bot.send_message(
                                        chat_id,
                                        &format!("⏳ Rate limited. Try again in {}s.", wait_secs),
                                    ).await;
                                    continue;
                                }

                                // Acquire semaphore permit for concurrency control
                                let permit = match active_tasks.clone().try_acquire_owned() {
                                    Ok(p) => p,
                                    Err(_) => {
                                        gateway_metrics.record_concurrency_rejected();
                                        let busy_bot = telegram::TelegramBot::new(&config.telegram.bot_token);
                                        let _ = busy_bot.send_message(
                                            msg.chat.id,
                                            "🔄 Server busy — too many concurrent requests. Try again shortly.",
                                        ).await;
                                        continue;
                                    }
                                };

                                let bot_clone = telegram::TelegramBot::new(&config.telegram.bot_token);
                                let config_clone = config.clone();
                                let tg_metrics = gateway_metrics.clone();

                                tokio::spawn(async move {
                                    let _permit = permit; // held until task completes
                                    let start = std::time::Instant::now();
                                    if let Err(e) =
                                        handler::handle_message(&bot_clone, &msg, &config_clone).await
                                    {
                                        tg_metrics.record_telegram_error();
                                        error!("Handler error: {}", e);
                                    }
                                    tg_metrics.record_completion(start.elapsed().as_millis() as u64);
                                });
                            }
                        }
                    }
                    Err(e) => {
                        error!("Polling error: {}", e);
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }
        }
    }
}

async fn health_handler() -> &'static str {
    "ok"
}

async fn metrics_handler(
    m: Arc<metrics::GatewayMetrics>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        [(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        m.to_prometheus(),
    ).into_response()
}

async fn metrics_json_handler(
    m: Arc<metrics::GatewayMetrics>,
) -> Json<serde_json::Value> {
    Json(m.to_json())
}

async fn status_handler(
    config: Arc<config::GatewayConfig>,
    tool_names: Arc<Vec<String>>,
    start_time: std::time::Instant,
    m: Arc<metrics::GatewayMetrics>,
) -> Json<serde_json::Value> {
    let uptime_secs = start_time.elapsed().as_secs();
    let uptime = if uptime_secs >= 86400 {
        format!("{}d {}h {}m", uptime_secs / 86400, (uptime_secs % 86400) / 3600, (uptime_secs % 3600) / 60)
    } else if uptime_secs >= 3600 {
        format!("{}h {}m", uptime_secs / 3600, (uptime_secs % 3600) / 60)
    } else {
        format!("{}m {}s", uptime_secs / 60, uptime_secs % 60)
    };

    // Session stats
    let session_info = match openclaw_agent::sessions::SessionStore::open(&config.agent.name) {
        Ok(store) => {
            let sessions = store.list_sessions(&config.agent.name, 1000).unwrap_or_default();
            let total_messages: i64 = sessions.iter().map(|s| s.message_count).sum();
            let total_tokens: i64 = sessions.iter().map(|s| s.total_tokens).sum();
            let tg_sessions = sessions.iter().filter(|s| s.session_key.starts_with("tg:")).count();
            let dc_sessions = sessions.iter().filter(|s| s.session_key.starts_with("dc:")).count();
            serde_json::json!({
                "total": sessions.len(),
                "telegram": tg_sessions,
                "discord": dc_sessions,
                "total_messages": total_messages,
                "total_tokens": total_tokens,
            })
        }
        Err(_) => serde_json::json!({"error": "failed to open session store"}),
    };

    // Channels info
    let channels = serde_json::json!({
        "telegram": {
            "enabled": true,
            "allowed_users": config.telegram.allowed_user_ids.len(),
        },
        "discord": {
            "enabled": config.discord.is_some(),
            "allowed_users": config.discord.as_ref()
                .map(|d| d.allowed_user_ids.len())
                .unwrap_or(0),
        },
    });

    // Commands
    let tg_commands = ["help", "new", "status", "model", "sessions", "export", "voice", "cron"];
    let dc_commands = ["help", "new", "status", "model", "sessions", "export", "voice", "cron"];

    Json(serde_json::json!({
        "status": "running",
        "version": env!("CARGO_PKG_VERSION"),
        "agent": config.agent.name,
        "fallback": config.agent.fallback,
        "model": config.agent.model,
        "uptime": uptime,
        "uptime_seconds": uptime_secs,
        "channels": channels,
        "sessions": session_info,
        "metrics": m.to_json(),
        "tools": *tool_names,
        "tool_count": tool_names.len(),
        "commands": {
            "telegram": tg_commands,
            "discord": dc_commands,
        },
    }))
}
