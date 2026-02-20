use anyhow::Result;
use sqlx::PgPool;

/// Persist a metrics snapshot to the database
pub async fn record_snapshot(
    pool: &PgPool,
    host: &str,
    telegram_requests: i64,
    discord_requests: i64,
    telegram_errors: i64,
    discord_errors: i64,
    rate_limited: i64,
    concurrency_rejected: i64,
    completed_requests: i64,
    total_latency_ms: i64,
    agent_turns: i64,
    tool_calls: i64,
    webhook_requests: i64,
    tasks_cancelled: i64,
    agent_timeouts: i64,
    process_rss_bytes: i64,
    uptime_seconds: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO metrics_snapshots (
            host, telegram_requests, discord_requests, telegram_errors, discord_errors,
            rate_limited, concurrency_rejected, completed_requests, total_latency_ms,
            agent_turns, tool_calls, webhook_requests, tasks_cancelled, agent_timeouts,
            process_rss_bytes, uptime_seconds
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)"
    )
    .bind(host)
    .bind(telegram_requests)
    .bind(discord_requests)
    .bind(telegram_errors)
    .bind(discord_errors)
    .bind(rate_limited)
    .bind(concurrency_rejected)
    .bind(completed_requests)
    .bind(total_latency_ms)
    .bind(agent_turns)
    .bind(tool_calls)
    .bind(webhook_requests)
    .bind(tasks_cancelled)
    .bind(agent_timeouts)
    .bind(process_rss_bytes)
    .bind(uptime_seconds)
    .execute(pool)
    .await?;

    Ok(())
}
