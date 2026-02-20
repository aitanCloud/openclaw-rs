use anyhow::Result;
use sqlx::PgPool;

/// Persist a message (user, assistant, system, or tool) to Postgres
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
