mod config;
mod handler;
mod telegram;

use axum::{routing::get, Json, Router};
use std::sync::Arc;
use tracing::{error, info};

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

    // ── Telegram long-polling loop ──
    info!("Starting Telegram long-polling...");
    let mut offset: i64 = 0;

    loop {
        match bot.get_updates(offset, 30).await {
            Ok(updates) => {
                for update in updates {
                    offset = update.update_id + 1;

                    if let Some(msg) = update.message {
                        let bot_clone = telegram::TelegramBot::new(&config.telegram.bot_token);
                        let config_clone = config.clone();

                        // Handle each message in a separate task
                        tokio::spawn(async move {
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
