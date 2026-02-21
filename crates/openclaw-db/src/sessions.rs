use anyhow::Result;
use sqlx::PgPool;

/// Session metadata
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub session_key: String,
    pub agent_name: String,
    pub model: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub message_count: i64,
    pub total_tokens: i64,
}

/// A single message loaded from the database
#[derive(Debug, Clone)]
pub struct SessionMessage {
    pub role: String,
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
    pub tool_calls_json: Option<String>,
    pub tool_call_id: Option<String>,
    pub timestamp_ms: i64,
}

/// Database statistics
#[derive(Debug, Clone)]
pub struct DbStats {
    pub session_count: i64,
    pub message_count: i64,
    pub total_tokens: i64,
    pub oldest_ms: Option<i64>,
    pub newest_ms: Option<i64>,
}

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

/// Load all messages for a session as raw SessionMessage structs
pub async fn load_messages(pool: &PgPool, session_key: &str) -> Result<Vec<SessionMessage>> {
    let sid = match get_session_id(pool, session_key).await? {
        Some(id) => id,
        None => return Ok(Vec::new()),
    };

    let rows = sqlx::query_as::<_, (String, Option<String>, Option<String>, Option<serde_json::Value>, Option<String>, chrono::DateTime<chrono::Utc>)>(
        "SELECT role, content, reasoning_content, tool_calls_json, tool_call_id, created_at
         FROM messages WHERE session_id = $1 ORDER BY id ASC"
    )
    .bind(sid)
    .fetch_all(pool)
    .await?;

    let messages = rows.into_iter().map(|(role, content, reasoning, tc_json, tc_id, created_at)| {
        SessionMessage {
            role,
            content,
            reasoning_content: reasoning,
            tool_calls_json: tc_json.map(|v| v.to_string()),
            tool_call_id: tc_id,
            timestamp_ms: created_at.timestamp_millis(),
        }
    }).collect();

    Ok(messages)
}

/// List recent sessions for an agent
pub async fn list_sessions(pool: &PgPool, agent_name: &str, limit: i64) -> Result<Vec<SessionInfo>> {
    let rows = sqlx::query_as::<_, (String, String, String, chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>, i64, i64)>(
        "SELECT s.session_key, s.agent_name, s.model, s.created_at, s.updated_at,
                s.total_tokens, COUNT(m.id) as msg_count
         FROM sessions s
         LEFT JOIN messages m ON m.session_id = s.id
         WHERE s.agent_name = $1
         GROUP BY s.id
         ORDER BY s.updated_at DESC
         LIMIT $2"
    )
    .bind(agent_name)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(|(key, agent, model, created, updated, tokens, msgs)| {
        SessionInfo {
            session_key: key,
            agent_name: agent,
            model,
            created_at_ms: created.timestamp_millis(),
            updated_at_ms: updated.timestamp_millis(),
            total_tokens: tokens,
            message_count: msgs,
        }
    }).collect())
}

/// Find the latest session key matching a prefix
pub async fn find_latest_session(pool: &PgPool, key_prefix: &str) -> Result<Option<String>> {
    let pattern = format!("{}%", key_prefix);
    let row: Option<(String,)> = sqlx::query_as::<_, (String,)>(
        "SELECT session_key FROM sessions WHERE session_key LIKE $1 ORDER BY updated_at DESC LIMIT 1"
    )
    .bind(&pattern)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.0))
}

/// Delete a session and all its messages. Returns number of messages deleted.
pub async fn delete_session(pool: &PgPool, session_key: &str) -> Result<usize> {
    let sid = match get_session_id(pool, session_key).await? {
        Some(id) => id,
        None => return Ok(0),
    };

    let result = sqlx::query("DELETE FROM messages WHERE session_id = $1")
        .bind(sid)
        .execute(pool)
        .await?;
    let deleted = result.rows_affected() as usize;

    sqlx::query("DELETE FROM sessions WHERE id = $1")
        .bind(sid)
        .execute(pool)
        .await?;

    Ok(deleted)
}

/// Get database statistics for an agent
pub async fn db_stats(pool: &PgPool, agent_name: &str) -> Result<DbStats> {
    let row = sqlx::query_as::<_, (i64, i64, i64, Option<chrono::DateTime<chrono::Utc>>, Option<chrono::DateTime<chrono::Utc>>)>(
        "SELECT
            COUNT(DISTINCT s.id) as session_count,
            COUNT(m.id) as message_count,
            COALESCE(SUM(DISTINCT s.total_tokens), 0) as total_tokens,
            MIN(s.created_at) as oldest,
            MAX(s.updated_at) as newest
         FROM sessions s
         LEFT JOIN messages m ON m.session_id = s.id
         WHERE s.agent_name = $1"
    )
    .bind(agent_name)
    .fetch_one(pool)
    .await?;

    Ok(DbStats {
        session_count: row.0,
        message_count: row.1,
        total_tokens: row.2,
        oldest_ms: row.3.map(|dt| dt.timestamp_millis()),
        newest_ms: row.4.map(|dt| dt.timestamp_millis()),
    })
}

/// Delete sessions older than `max_age_days` days. Returns the number of sessions pruned.
pub async fn prune_old_sessions(pool: &PgPool, max_age_days: i32) -> Result<usize> {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(max_age_days as i64);

    // Get session IDs to prune
    let ids: Vec<(i64,)> = sqlx::query_as::<_, (i64,)>(
        "SELECT id FROM sessions WHERE updated_at < $1"
    )
    .bind(cutoff)
    .fetch_all(pool)
    .await?;

    if ids.is_empty() {
        return Ok(0);
    }

    let id_list: Vec<i64> = ids.into_iter().map(|r| r.0).collect();
    let count = id_list.len();

    for id in &id_list {
        sqlx::query("DELETE FROM messages WHERE session_id = $1")
            .bind(id)
            .execute(pool)
            .await?;
        sqlx::query("DELETE FROM sessions WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await?;
    }

    Ok(count)
}
