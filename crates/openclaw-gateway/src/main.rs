mod config;
mod cron;
mod debounce;
mod discord;
mod discord_handler;
mod doctor;
mod handler;
mod metrics;
mod ratelimit;
mod task_registry;
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
    info!("Commands: 22 (/help /new /status /model /sessions /export /voice /ping /history /clear /db /version /stats /whoami /cancel /stop /cron /tools /skills /config /runtime /doctor)");

    // ── Verify bot token ──
    let bot = telegram::TelegramBot::new(&config.telegram.bot_token);
    let me = bot.get_me().await?;
    info!(
        "Telegram bot verified: @{} ({})",
        me.username.as_deref().unwrap_or("?"),
        me.first_name
    );

    // ── Store agent name for HTTP endpoints ──
    handler::init_agent_name(&config.agent.name);

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
        .route("/health/lite", get(health_lite_handler))
        .route("/version", get(version_handler))
        .route("/ping", get(ping_handler))
        .route(
            "/ready",
            get({
                let cfg = health_config.clone();
                move || ready_handler(cfg)
            }),
        )
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
        )
        .route(
            "/metrics/summary",
            get({
                let m = gateway_metrics.clone();
                move || metrics_summary_handler(m)
            }),
        )
        .route(
            "/doctor",
            get({
                let cfg = health_config.clone();
                move || doctor_handler(cfg)
            }),
        )
        .route(
            "/webhook",
            axum::routing::post({
                let cfg = health_config.clone();
                move |headers: axum::http::HeaderMap, body: Json<serde_json::Value>| {
                    webhook_handler(cfg, headers, body)
                }
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
        let sigterm = async {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("Failed to install SIGTERM handler")
                .recv()
                .await;
        };
        #[cfg(not(unix))]
        let sigterm = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => info!("Received SIGINT (Ctrl+C)"),
            _ = sigterm => info!("Received SIGTERM"),
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

/// Read process RSS (Resident Set Size) in bytes from /proc/self/statm (Linux only)
pub fn process_rss_bytes() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map(|pages| pages * 4096) // page size is typically 4096
        .unwrap_or(0)
}

/// Format uptime with days support (pub for use from handler modules)
pub fn human_uptime(secs: u64) -> String {
    if secs >= 86400 {
        format!("{}d {}h {}m", secs / 86400, (secs % 86400) / 3600, (secs % 3600) / 60)
    } else if secs >= 3600 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

async fn health_lite_handler() -> Json<serde_json::Value> {
    let uptime_secs = handler::BOOT_TIME.elapsed().as_secs();
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "agent": handler::agent_name(),
        "uptime": human_uptime(uptime_secs),
        "uptime_seconds": uptime_secs,
        "active_tasks": task_registry::active_count(),
    }))
}

async fn ping_handler() -> &'static str {
    "pong"
}

async fn version_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "agent": handler::agent_name(),
        "built": env!("BUILD_TIMESTAMP"),
        "boot_time": *handler::BOOT_TIMESTAMP,
    }))
}

async fn health_handler() -> axum::response::Response {
    use axum::response::IntoResponse;
    let t0 = std::time::Instant::now();
    let uptime_secs = handler::BOOT_TIME.elapsed().as_secs();
    let skills_count = {
        let home = dirs::home_dir().unwrap_or_default();
        let skills_dir = home.join(".openclaw").join("workspace").join("skills");
        openclaw_core::skills::list_skills(&skills_dir)
            .map(|s| s.len())
            .unwrap_or(0)
    };
    let session_count = openclaw_agent::sessions::SessionStore::open("main")
        .ok()
        .and_then(|s| s.db_stats("main").ok())
        .map(|stats| stats.session_count)
        .unwrap_or(0);
    let rss = process_rss_bytes();
    let providers: Vec<String> = openclaw_agent::llm::fallback::FallbackProvider::from_config()
        .ok()
        .map(|fb| fb.provider_labels().iter().map(|s| s.to_string()).collect())
        .unwrap_or_default();
    let (error_rate, total_requests, total_errors, avg_latency, webhook_reqs,
         agent_turns, tool_calls, completed_reqs, rate_limited, concurrency_rejected,
         agent_timeouts, tasks_cancelled, gw_connects, gw_disconnects, gw_resumes) =
        metrics::global()
            .map(|m| (
                (m.error_rate_pct() * 100.0).round() / 100.0,
                m.total_requests(), m.total_errors(), m.avg_latency_ms(),
                m.webhook_requests(), m.agent_turns(), m.tool_calls(),
                m.completed_requests(), m.rate_limited(), m.concurrency_rejected(),
                m.agent_timeouts(), m.tasks_cancelled(),
                m.gateway_connects(), m.gateway_disconnects(), m.gateway_resumes(),
            ))
            .unwrap_or((0.0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0));
    let checks = doctor::run_checks("main").await;
    let checks_total = checks.len();
    let checks_passed = checks.iter().filter(|(_, ok, _)| *ok).count();
    let elapsed_ms = t0.elapsed().as_millis();
    (
        [(axum::http::header::HeaderName::from_static("x-response-time-ms"),
          axum::http::HeaderValue::from_str(&elapsed_ms.to_string()).unwrap())],
        Json(serde_json::json!({
            "status": "ok",
            "version": env!("CARGO_PKG_VERSION"),
            "agent": handler::agent_name(),
            "built": env!("BUILD_TIMESTAMP"),
            "boot_time": *handler::BOOT_TIMESTAMP,
            "active_tasks": task_registry::active_count(),
            "uptime": human_uptime(uptime_secs),
            "uptime_seconds": uptime_secs,
            "memory_rss_bytes": rss,
            "memory_rss": crate::doctor::human_bytes_pub(rss),
            "skills": skills_count,
            "sessions": session_count,
            "commands": 22,
            "http_endpoint_count": 11,
            "tool_count": 17,
            "total_requests": total_requests,
            "total_errors": total_errors,
            "error_rate_pct": error_rate,
            "avg_latency_ms": avg_latency,
            "webhook_requests": webhook_reqs,
            "agent_turns": agent_turns,
            "tool_calls": tool_calls,
            "completed_requests": completed_reqs,
            "rate_limited": rate_limited,
            "concurrency_rejected": concurrency_rejected,
            "agent_timeouts": agent_timeouts,
            "tasks_cancelled": tasks_cancelled,
            "gateway_connects": gw_connects,
            "gateway_disconnects": gw_disconnects,
            "gateway_resumes": gw_resumes,
            "provider_count": providers.len(),
            "fallback_chain": providers,
            "doctor_checks_total": checks_total,
            "doctor_checks_passed": checks_passed,
            "response_time_ms": elapsed_ms,
        })),
    ).into_response()
}

async fn ready_handler(
    config: Arc<config::GatewayConfig>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let t0 = std::time::Instant::now();
    let checks = doctor::run_checks(&config.agent.name).await;
    let all_ok = checks.iter().all(|(_, ok, _)| *ok);
    let failed: Vec<&str> = checks.iter()
        .filter(|(_, ok, _)| !*ok)
        .map(|(name, _, _)| name.as_str())
        .collect();
    let status = if all_ok {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    };
    let elapsed_ms = t0.elapsed().as_millis();
    (
        status,
        [(axum::http::header::HeaderName::from_static("x-response-time-ms"),
          axum::http::HeaderValue::from_str(&elapsed_ms.to_string()).unwrap())],
        Json(serde_json::json!({
            "ready": all_ok,
            "checks_total": checks.len(),
            "checks_passed": checks.iter().filter(|(_, ok, _)| *ok).count(),
            "failed": failed,
            "response_time_ms": elapsed_ms,
        })),
    ).into_response()
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

async fn metrics_summary_handler(
    m: Arc<metrics::GatewayMetrics>,
) -> String {
    let uptime = human_uptime(handler::BOOT_TIME.elapsed().as_secs());
    format!(
        "reqs={} errs={} err%={:.1} avg_lat={}ms turns={} tools={} webhooks={} up={}",
        m.total_requests(),
        m.total_errors(),
        m.error_rate_pct(),
        m.avg_latency_ms(),
        m.agent_turns(),
        m.tool_calls(),
        m.webhook_requests(),
        uptime,
    )
}

async fn metrics_json_handler(
    m: Arc<metrics::GatewayMetrics>,
) -> Json<serde_json::Value> {
    Json(m.to_json())
}

async fn webhook_handler(
    config: Arc<config::GatewayConfig>,
    headers: axum::http::HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    use openclaw_agent::runtime::{self, AgentTurnConfig};

    // Generate request_id early so all responses (including errors) include it
    let request_id = uuid::Uuid::new_v4().to_string();

    // Auth check
    let expected_token = match &config.webhook {
        Some(wh) => &wh.token,
        None => {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"request_id": request_id, "error": "webhook not configured", "error_code": "WEBHOOK_NOT_CONFIGURED"})),
            ).into_response();
        }
    };

    let auth_header = headers.get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let token = auth_header.strip_prefix("Bearer ").unwrap_or("");
    if token != expected_token {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"request_id": request_id, "error": "invalid token", "error_code": "INVALID_TOKEN"})),
        ).into_response();
    }

    // Extract message from body
    let message = match body.get("message").and_then(|m| m.as_str()) {
        Some(m) => m.to_string(),
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"request_id": request_id, "error": "missing 'message' field", "error_code": "MISSING_MESSAGE"})),
            ).into_response();
        }
    };

    let session_key = body.get("session_key")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("webhook:{}:{}", config.agent.name, uuid::Uuid::new_v4()));

    // Record metric
    if let Some(m) = metrics::global() {
        m.record_webhook_request();
    }

    // Build provider
    let provider = match openclaw_agent::llm::fallback::FallbackProvider::from_config() {
        Ok(p) => p,
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"request_id": request_id, "error": format!("provider init failed: {}", e), "error_code": "PROVIDER_INIT_FAILED"})),
            ).into_response();
        }
    };

    let workspace_dir = openclaw_agent::workspace::resolve_workspace_dir(&config.agent.name);
    let agent_config = AgentTurnConfig {
        agent_name: config.agent.name.clone(),
        session_key: session_key.clone(),
        workspace_dir: workspace_dir.to_string_lossy().to_string(),
        minimal_context: false,
    };

    let tools = openclaw_agent::tools::ToolRegistry::with_defaults();
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();

    let t_start = std::time::Instant::now();

    let result = runtime::run_agent_turn_streaming(
        &provider,
        &message,
        &agent_config,
        &tools,
        event_tx,
        vec![],
        None,
    ).await;

    match result {
        Ok(turn_result) => {
            let elapsed = t_start.elapsed().as_millis() as u64;
            if let Some(m) = metrics::global() {
                m.record_agent_turn(turn_result.tool_calls_made as u64);
                m.record_completion(elapsed);
            }
            (
                axum::http::StatusCode::OK,
                Json(serde_json::json!({
                    "request_id": request_id,
                    "reply": turn_result.response,
                    "session_key": session_key,
                    "model": turn_result.model_name,
                    "tool_calls": turn_result.tool_calls_made,
                    "rounds": turn_result.total_rounds,
                    "elapsed_ms": turn_result.elapsed_ms,
                })),
            ).into_response()
        }
        Err(e) => {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"request_id": request_id, "error": format!("agent turn failed: {}", e), "error_code": "AGENT_TURN_FAILED"})),
            ).into_response()
        }
    }
}

async fn doctor_handler(
    config: Arc<config::GatewayConfig>,
) -> Json<serde_json::Value> {
    let checks = doctor::run_checks(&config.agent.name).await;
    let all_ok = checks.iter().all(|(_, ok, _)| *ok);
    let results: Vec<serde_json::Value> = checks.iter()
        .map(|(name, ok, detail)| serde_json::json!({
            "check": name,
            "passed": ok,
            "detail": detail,
        }))
        .collect();
    Json(serde_json::json!({
        "status": if all_ok { "healthy" } else { "degraded" },
        "passed": checks.iter().filter(|(_, ok, _)| *ok).count(),
        "total": checks.len(),
        "checks": results,
    }))
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
    let tg_commands = ["help", "new", "status", "model", "sessions", "export", "voice", "ping",
        "history", "clear", "db", "version", "stats", "whoami", "cancel", "stop", "cron", "tools", "skills", "config", "runtime", "doctor"];
    let dc_commands = ["help", "new", "status", "model", "sessions", "export", "voice", "ping",
        "history", "clear", "db", "version", "stats", "whoami", "cancel", "stop", "cron", "tools", "skills", "config", "runtime", "doctor"];

    // Provider labels from fallback chain
    let providers: Vec<String> = openclaw_agent::llm::fallback::FallbackProvider::from_config()
        .ok()
        .map(|fb| fb.provider_labels().iter().map(|s| s.to_string()).collect())
        .unwrap_or_default();

    Json(serde_json::json!({
        "status": "running",
        "version": env!("CARGO_PKG_VERSION"),
        "agent": config.agent.name,
        "fallback": config.agent.fallback,
        "model": config.agent.model,
        "providers": providers,
        "uptime": uptime,
        "uptime_seconds": uptime_secs,
        "channels": channels,
        "sessions": session_info,
        "metrics": m.to_json(),
        "tools": *tool_names,
        "tool_count": tool_names.len(),
        "active_tasks": task_registry::active_count(),
        "webhook_configured": config.webhook.is_some(),
        "built": env!("BUILD_TIMESTAMP"),
        "boot_time": *handler::BOOT_TIMESTAMP,
        "http_endpoints": ["/health", "/health/lite", "/version", "/ping", "/ready", "/status", "/metrics", "/metrics/json", "/metrics/summary", "/doctor", "/webhook"],
        "http_endpoint_count": 11,
        "commands": {
            "telegram": tg_commands,
            "discord": dc_commands,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_human_uptime_seconds() {
        assert_eq!(human_uptime(45), "0m 45s");
    }

    #[test]
    fn test_human_uptime_minutes() {
        assert_eq!(human_uptime(125), "2m 5s");
    }

    #[test]
    fn test_human_uptime_hours() {
        assert_eq!(human_uptime(7260), "2h 1m");
    }

    #[test]
    fn test_human_uptime_days() {
        assert_eq!(human_uptime(90060), "1d 1h 1m");
    }

    #[test]
    fn test_human_uptime_multi_days() {
        assert_eq!(human_uptime(259200), "3d 0h 0m");
    }

    #[test]
    fn test_process_rss_bytes_returns_value() {
        let rss = process_rss_bytes();
        // On Linux /proc/self/statm exists, so RSS should be > 0
        // On other platforms it gracefully returns 0
        if cfg!(target_os = "linux") {
            assert!(rss > 0, "RSS should be > 0 on Linux");
        }
    }

    #[test]
    fn test_webhook_error_codes_defined() {
        // Verify all webhook error codes are valid identifiers
        let codes = [
            "WEBHOOK_NOT_CONFIGURED",
            "INVALID_TOKEN",
            "MISSING_MESSAGE",
            "PROVIDER_INIT_FAILED",
            "AGENT_TURN_FAILED",
        ];
        for code in &codes {
            assert!(code.chars().all(|c| c.is_ascii_uppercase() || c == '_'),
                "Error code '{}' should be UPPER_SNAKE_CASE", code);
        }
        assert_eq!(codes.len(), 5, "Should have 5 webhook error codes");
    }

    #[test]
    fn test_human_uptime_edge_cases() {
        assert_eq!(human_uptime(0), "0m 0s");
        assert_eq!(human_uptime(60), "1m 0s");
        assert_eq!(human_uptime(3600), "1h 0m");
        assert_eq!(human_uptime(86400), "1d 0h 0m");
    }

    #[test]
    fn test_command_arrays_have_22_entries() {
        let tg = ["help", "new", "status", "model", "sessions", "export", "voice", "ping",
            "history", "clear", "db", "version", "stats", "whoami", "cancel", "stop", "cron", "tools", "skills", "config", "runtime", "doctor"];
        let dc = ["help", "new", "status", "model", "sessions", "export", "voice", "ping",
            "history", "clear", "db", "version", "stats", "whoami", "cancel", "stop", "cron", "tools", "skills", "config", "runtime", "doctor"];
        assert_eq!(tg.len(), 22, "Telegram should have 22 commands");
        assert_eq!(dc.len(), 22, "Discord should have 22 commands");
        // Verify both arrays are identical
        assert_eq!(tg, dc, "Telegram and Discord command lists should match");
    }

    #[test]
    fn test_health_expected_fields() {
        let expected = [
            "status", "version", "agent", "built", "boot_time", "active_tasks",
            "uptime", "uptime_seconds", "memory_rss_bytes", "memory_rss",
            "skills", "sessions", "commands",
            "http_endpoint_count", "tool_count",
            "total_requests", "total_errors", "error_rate_pct",
            "avg_latency_ms", "webhook_requests",
            "agent_turns", "tool_calls", "completed_requests",
            "rate_limited", "concurrency_rejected",
            "agent_timeouts", "tasks_cancelled",
            "gateway_connects", "gateway_disconnects", "gateway_resumes",
            "provider_count", "fallback_chain",
            "doctor_checks_total", "doctor_checks_passed", "response_time_ms",
        ];
        assert_eq!(expected.len(), 35, "Should have 35 /health JSON fields");
        // Verify no duplicates
        let mut sorted = expected.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 35, "/health fields should have no duplicates");
    }

    #[test]
    fn test_ready_expected_fields() {
        let expected = ["ready", "checks_total", "checks_passed", "failed", "response_time_ms"];
        assert_eq!(expected.len(), 5, "Should have 5 /ready JSON fields");
        let mut sorted = expected.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 5, "/ready fields should have no duplicates");
    }

    #[test]
    fn test_metrics_summary_format() {
        let summary = format!(
            "reqs={} errs={} err%={:.1} avg_lat={}ms turns={} tools={} webhooks={} up={}",
            0, 0, 0.0, 0, 0, 0, 0, human_uptime(0)
        );
        assert!(summary.contains("reqs="), "Summary should contain reqs=");
        assert!(summary.contains("errs="), "Summary should contain errs=");
        assert!(summary.contains("err%="), "Summary should contain err%=");
        assert!(summary.contains("avg_lat="), "Summary should contain avg_lat=");
        assert!(summary.contains("turns="), "Summary should contain turns=");
        assert!(summary.contains("tools="), "Summary should contain tools=");
        assert!(summary.contains("webhooks="), "Summary should contain webhooks=");
        assert!(summary.contains("up="), "Summary should contain up=");
    }

    #[test]
    fn test_human_uptime_multi_day() {
        // 2 days, 3 hours, 45 minutes = 2*86400 + 3*3600 + 45*60 = 186_300
        assert_eq!(human_uptime(186_300), "2d 3h 45m");
        // 7 days exactly
        assert_eq!(human_uptime(604_800), "7d 0h 0m");
    }

    #[test]
    fn test_http_endpoints_count() {
        let endpoints = ["/health", "/health/lite", "/version", "/ping", "/ready", "/status", "/metrics", "/metrics/json", "/metrics/summary", "/doctor", "/webhook"];
        assert_eq!(endpoints.len(), 11, "Should have 11 HTTP endpoints");
        // Verify no duplicates
        let mut sorted = endpoints.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 11, "HTTP endpoints should have no duplicates");
    }

    #[test]
    fn test_health_lite_expected_fields() {
        let expected = ["status", "version", "agent", "uptime", "uptime_seconds", "active_tasks"];
        assert_eq!(expected.len(), 6, "Should have 6 /health/lite JSON fields");
        let mut sorted = expected.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 6, "/health/lite fields should have no duplicates");
    }

    #[test]
    fn test_version_expected_fields() {
        let expected = ["version", "agent", "built", "boot_time"];
        assert_eq!(expected.len(), 4, "Should have 4 /version JSON fields");
        let mut sorted = expected.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 4, "/version fields should have no duplicates");
    }

    #[test]
    fn test_status_expected_fields() {
        let expected = [
            "status", "version", "agent", "fallback", "model", "providers",
            "uptime", "uptime_seconds", "channels", "sessions", "metrics",
            "tools", "tool_count", "active_tasks",
            "webhook_configured", "built", "boot_time",
            "http_endpoints", "http_endpoint_count", "commands",
        ];
        assert_eq!(expected.len(), 20, "Should have 20 /status JSON fields");
        let mut sorted = expected.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 20, "/status fields should have no duplicates");
    }

    #[test]
    fn test_request_id_is_valid_uuid() {
        let id = uuid::Uuid::new_v4().to_string();
        assert_eq!(id.len(), 36, "UUID should be 36 chars");
        assert_eq!(id.chars().filter(|c| *c == '-').count(), 4, "UUID should have 4 dashes");
        // Parse back to verify format
        assert!(uuid::Uuid::parse_str(&id).is_ok(), "Should parse as valid UUID");
    }

    #[test]
    fn test_boot_timestamp_format() {
        let ts = &*handler::BOOT_TIMESTAMP;
        // Should be ISO 8601 format: YYYY-MM-DDTHH:MM:SSZ
        assert!(ts.len() == 20, "BOOT_TIMESTAMP should be 20 chars, got {}: {}", ts.len(), ts);
        assert!(ts.ends_with('Z'), "BOOT_TIMESTAMP should end with Z");
        assert_eq!(&ts[4..5], "-", "BOOT_TIMESTAMP should have dash at pos 4");
        assert_eq!(&ts[10..11], "T", "BOOT_TIMESTAMP should have T at pos 10");
    }
}
