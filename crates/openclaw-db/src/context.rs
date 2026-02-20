use anyhow::Result;
use sqlx::PgPool;

// ── Deployments ──

pub async fn record_deployment(
    pool: &PgPool,
    host: &str,
    component: &str,
    version: &str,
    binary_path: Option<&str>,
    service_name: Option<&str>,
    git_commit: Option<&str>,
    notes: Option<&str>,
) -> Result<i64> {
    // Mark previous active deployments for this host+component as stopped
    sqlx::query(
        "UPDATE deployments SET status = 'stopped' WHERE host = $1 AND component = $2 AND status = 'active'"
    )
    .bind(host)
    .bind(component)
    .execute(pool)
    .await?;

    let row: (i64,) = sqlx::query_as(
        "INSERT INTO deployments (host, component, version, binary_path, service_name, git_commit, notes)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         RETURNING id"
    )
    .bind(host)
    .bind(component)
    .bind(version)
    .bind(binary_path)
    .bind(service_name)
    .bind(git_commit)
    .bind(notes)
    .fetch_one(pool)
    .await?;

    Ok(row.0)
}

// ── Config Snapshots ──

pub async fn snapshot_config(
    pool: &PgPool,
    config_path: &str,
    config_json: &serde_json::Value,
    changed_keys: &[String],
    change_source: &str,
) -> Result<i64> {
    let row: (i64,) = sqlx::query_as(
        "INSERT INTO config_snapshots (config_path, config_json, changed_keys, change_source)
         VALUES ($1, $2, $3, $4)
         RETURNING id"
    )
    .bind(config_path)
    .bind(config_json)
    .bind(changed_keys)
    .bind(change_source)
    .fetch_one(pool)
    .await?;

    Ok(row.0)
}

// ── Known Issues ──

pub async fn add_issue(
    pool: &PgPool,
    title: &str,
    description: Option<&str>,
    severity: &str,
    component: Option<&str>,
    workaround: Option<&str>,
) -> Result<i64> {
    let row: (i64,) = sqlx::query_as(
        "INSERT INTO known_issues (title, description, severity, component, workaround)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id"
    )
    .bind(title)
    .bind(description)
    .bind(severity)
    .bind(component)
    .bind(workaround)
    .fetch_one(pool)
    .await?;

    Ok(row.0)
}

pub async fn resolve_issue(pool: &PgPool, id: i64) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE known_issues SET status = 'resolved', resolved_at = now() WHERE id = $1 AND status = 'open'"
    )
    .bind(id)
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

// ── Task Log ──

pub async fn log_task(
    pool: &PgPool,
    description: &str,
    category: Option<&str>,
    files_changed: &[String],
    version: Option<&str>,
    git_commit: Option<&str>,
) -> Result<i64> {
    let row: (i64,) = sqlx::query_as(
        "INSERT INTO task_log (description, category, files_changed, version, git_commit)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id"
    )
    .bind(description)
    .bind(category)
    .bind(files_changed)
    .bind(version)
    .bind(git_commit)
    .fetch_one(pool)
    .await?;

    Ok(row.0)
}

// ── Project Context (KV store) ──

pub async fn set_context(
    pool: &PgPool,
    key: &str,
    value: &serde_json::Value,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO project_context (key, value, updated_at)
         VALUES ($1, $2, now())
         ON CONFLICT (key) DO UPDATE SET value = $2, updated_at = now()"
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn get_context(
    pool: &PgPool,
    key: &str,
) -> Result<Option<serde_json::Value>> {
    let row: Option<(serde_json::Value,)> = sqlx::query_as(
        "SELECT value FROM project_context WHERE key = $1"
    )
    .bind(key)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| r.0))
}

pub async fn delete_context(pool: &PgPool, key: &str) -> Result<bool> {
    let result = sqlx::query("DELETE FROM project_context WHERE key = $1")
        .bind(key)
        .execute(pool)
        .await?;

    Ok(result.rows_affected() > 0)
}
