mod config;
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

    // ── Start health check HTTP server ──
    let health_config = Arc::new(config.clone());
    let app = Router::new()
        .route("/health", get(health_handler))
        .route(
            "/status",
            get({
                let cfg = health_config.clone();
                move || status_handler(cfg)
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

    // ── Rate limiter: 10 messages per 60 seconds per user ──
    let rate_limiter = Arc::new(ratelimit::RateLimiter::new(10, 60));

    // ── Active task tracker for graceful shutdown ──
    let active_tasks: Arc<tokio::sync::Semaphore> = Arc::new(tokio::sync::Semaphore::new(5));

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
                while active_tasks.available_permits() < 5 {
                    if drain_start.elapsed().as_secs() > 30 {
                        warn!("Drain timeout — {} tasks still active, forcing shutdown",
                            5 - active_tasks.available_permits());
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

async fn status_handler(config: Arc<config::GatewayConfig>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "running",
        "version": env!("CARGO_PKG_VERSION"),
        "agent": config.agent.name,
        "fallback": config.agent.fallback,
    }))
}
