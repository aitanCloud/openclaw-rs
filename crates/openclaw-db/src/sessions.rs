use anyhow::Result;
use sqlx::PgPool;

/// Create or get a session, returning its database ID
pub async fn upsert_session(
    pool: &PgPool,
    session_key: &str,
    agent_name: &str,
    model: &str,
    channel: Option<&str>,
    user_id: Option<&str>,
) -> Result<i64> {
    let row: (i64,) = sqlx::query_as::<_, (i64,)>(
        "INSERT INTO sessions (session_key, agent_name, model, channel, user_id)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (session_key) DO UPDATE SET
            model = EXCLUDED.model,
            updated_at = now()
         RETURNING id"
    )
    .bind(session_key)
    .bind(agent_name)
    .bind(model)
    .bind(channel)
    .bind(user_id)
    .fetch_one(pool)
    .await?;

    Ok(row.0)
}

/// Add tokens to a session's running total
pub async fn add_tokens(pool: &PgPool, session_key: &str, tokens: i64) -> Result<()> {
    sqlx::query(
        "UPDATE sessions SET total_tokens = total_tokens + $1, updated_at = now() WHERE session_key = $2"
    )
    .bind(tokens)
    .bind(session_key)
    .execute(pool)
    .await?;
    Ok(())
}

/// Get session ID by key
pub async fn get_session_id(pool: &PgPool, session_key: &str) -> Result<Option<i64>> {
    let row: Option<(i64,)> = sqlx::query_as::<_, (i64,)>(
        "SELECT id FROM sessions WHERE session_key = $1"
    )
    .bind(session_key)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.0))
}
