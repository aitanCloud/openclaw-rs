mod config;
mod cron;
mod handler;
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

    info!("Starting openclaw-gateway v{}", env!("CARGO_PKG_VERSION"));
    info!("Agent: {}", config.agent.name);
    info!("Fallback: {}", config.agent.fallback);
    info!(
        "Allowed users: {:?}",
        config.telegram.allowed_user_ids
    );

    // ── Verify bot token ──
    let bot = telegram::TelegramBot::new(&config.telegram.bot_token);
    let me = bot.get_me().await?;
    info!(
        "Telegram bot verified: @{} ({})",
        me.username.as_deref().unwrap_or("?"),
        me.first_name
    );

    // ── Start cron executor ──
    let cron_bot = Arc::new(telegram::TelegramBot::new(&config.telegram.bot_token));
    let cron_config = Arc::new(config.clone());
    cron::spawn_cron_executor(cron_config, cron_bot);

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
                move || status_handler(cfg, tnames, start_time)
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

    // ── Telegram long-polling loop with graceful shutdown ──
    info!("Starting Telegram long-polling...");
    let mut offset: i64 = 0;

    let shutdown = tokio::signal::ctrl_c();
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

                                // Rate limit check
                                if let Err(wait_secs) = rate_limiter.check(user_id) {
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

                                tokio::spawn(async move {
                                    let _permit = permit; // held until task completes
                                    if let Err(e) =
                                        handler::handle_message(&bot_clone, &msg, &config_clone).await
                                    {
                                        error!("Handler error: {}", e);
                                    }
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

async fn status_handler(
    config: Arc<config::GatewayConfig>,
    tool_names: Arc<Vec<String>>,
    start_time: std::time::Instant,
) -> Json<serde_json::Value> {
    let uptime_secs = start_time.elapsed().as_secs();
    let uptime = if uptime_secs >= 86400 {
        format!("{}d {}h {}m", uptime_secs / 86400, (uptime_secs % 86400) / 3600, (uptime_secs % 3600) / 60)
    } else if uptime_secs >= 3600 {
        format!("{}h {}m", uptime_secs / 3600, (uptime_secs % 3600) / 60)
    } else {
        format!("{}m {}s", uptime_secs / 60, uptime_secs % 60)
    };

    Json(serde_json::json!({
        "status": "running",
        "version": env!("CARGO_PKG_VERSION"),
        "agent": config.agent.name,
        "fallback": config.agent.fallback,
        "uptime": uptime,
        "tools": *tool_names,
        "tool_count": tool_names.len(),
    }))
}
