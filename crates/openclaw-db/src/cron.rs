use anyhow::Result;
use sqlx::PgPool;

/// Record a cron job execution start, returns the execution ID
pub async fn record_start(
    pool: &PgPool,
    job_name: &str,
    agent_name: &str,
    model: Option<&str>,
) -> Result<i64> {
    let row: (i64,) = sqlx::query_as::<_, (i64,)>(
        "INSERT INTO cron_executions (job_name, agent_name, model, status)
         VALUES ($1, $2, $3, 'running')
         RETURNING id"
    )
    .bind(job_name)
    .bind(agent_name)
    .bind(model)
    .fetch_one(pool)
    .await?;

    Ok(row.0)
}

/// Mark a cron execution as completed
pub async fn record_complete(
    pool: &PgPool,
    id: i64,
    duration_ms: i32,
    tokens_used: i32,
) -> Result<()> {
    sqlx::query(
        "UPDATE cron_executions SET status = 'completed', duration_ms = $1, tokens_used = $2 WHERE id = $3"
    )
    .bind(duration_ms)
    .bind(tokens_used)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark a cron execution as failed
pub async fn record_failure(
    pool: &PgPool,
    id: i64,
    duration_ms: i32,
    error: &str,
) -> Result<()> {
    sqlx::query(
        "UPDATE cron_executions SET status = 'failed', duration_ms = $1, error = $2 WHERE id = $3"
    )
    .bind(duration_ms)
    .bind(error)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}
