use anyhow::Result;
use sqlx::PgPool;

/// Persist a message (user, assistant, system, or tool) to Postgres by session ID
pub async fn record_message(
    pool: &PgPool,
    session_id: i64,
    role: &str,
    content: Option<&str>,
    reasoning_content: Option<&str>,
    tool_calls_json: Option<&serde_json::Value>,
    tool_call_id: Option<&str>,
) -> Result<i64> {
    let row: (i64,) = sqlx::query_as::<_, (i64,)>(
        "INSERT INTO messages (session_id, role, content, reasoning_content, tool_calls_json, tool_call_id)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING id"
    )
    .bind(session_id)
    .bind(role)
    .bind(content)
    .bind(reasoning_content)
    .bind(tool_calls_json)
    .bind(tool_call_id)
    .fetch_one(pool)
    .await?;

    Ok(row.0)
}

/// Persist a message by session key (upserts session if needed).
/// Convenience wrapper that resolves session_key → session_id automatically.
pub async fn append_message(
    pool: &PgPool,
    session_key: &str,
    agent_name: &str,
    model: &str,
    role: &str,
    content: Option<&str>,
    reasoning_content: Option<&str>,
    tool_calls_json: Option<&serde_json::Value>,
    tool_call_id: Option<&str>,
) -> Result<i64> {
    let sid = crate::sessions::upsert_session(pool, session_key, agent_name, model, None, None).await?;
    record_message(pool, sid, role, content, reasoning_content, tool_calls_json, tool_call_id).await
}
