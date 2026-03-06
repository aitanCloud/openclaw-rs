use std::sync::Arc;

use axum::{
    extract::{Path, State},
    Json,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use openclaw_orchestrator::domain::errors::DomainError;

use crate::errors::ApiError;
use crate::state::AppState;

/// Run row from the `orch_runs` table.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct RunRow {
    pub id: Uuid,
    pub task_id: Uuid,
    pub instance_id: Uuid,
    pub run_number: i32,
    pub state: String,
    pub worker_session_id: Uuid,
    pub exit_code: Option<i32>,
    pub cost_cents: i64,
    pub output_json: Option<serde_json::Value>,
    pub prompt_sent: Option<String>,
    pub failure_category: Option<String>,
    pub cancel_reason: Option<String>,
    pub abandon_reason: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// GET /api/v1/instances/:id/runs/:run_id — get run detail.
pub async fn get_run(
    State(state): State<Arc<AppState>>,
    Path((instance_id, run_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<RunRow>, ApiError> {
    let row = sqlx::query_as::<_, RunRow>(
        r#"
        SELECT id, task_id, instance_id, run_number, state, worker_session_id,
               exit_code, cost_cents, output_json, prompt_sent,
               failure_category, cancel_reason, abandon_reason,
               started_at, finished_at
        FROM orch_runs
        WHERE instance_id = $1 AND id = $2
        "#,
    )
    .bind(instance_id)
    .bind(run_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| {
        ApiError::from(DomainError::NotFound {
            entity: "Run".into(),
            id: run_id.to_string(),
        })
    })?;

    Ok(Json(row))
}

/// GET /api/v1/instances/:id/tasks/:task_id/runs — list runs for a task.
pub async fn list_runs_for_task(
    State(state): State<Arc<AppState>>,
    Path((instance_id, task_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Vec<RunRow>>, ApiError> {
    let rows = sqlx::query_as::<_, RunRow>(
        r#"
        SELECT id, task_id, instance_id, run_number, state, worker_session_id,
               exit_code, cost_cents, output_json, prompt_sent,
               failure_category, cancel_reason, abandon_reason,
               started_at, finished_at
        FROM orch_runs
        WHERE instance_id = $1 AND task_id = $2
        ORDER BY run_number ASC
        "#,
    )
    .bind(instance_id)
    .bind(task_id)
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(rows))
}

/// Response body for the log tail placeholder.
#[derive(Debug, Serialize)]
pub struct RunLogsResponse {
    pub logs: Vec<serde_json::Value>,
    pub message: String,
}

/// GET /api/v1/instances/:id/runs/:run_id/logs — placeholder for streaming log tail.
///
/// Returns 501 Not Implemented. This will be replaced with SSE streaming later.
#[allow(unused_variables)]
pub async fn get_run_logs(
    State(state): State<Arc<AppState>>,
    Path((instance_id, run_id)): Path<(Uuid, Uuid)>,
) -> Result<(axum::http::StatusCode, Json<RunLogsResponse>), ApiError> {
    Ok((
        axum::http::StatusCode::NOT_IMPLEMENTED,
        Json(RunLogsResponse {
            logs: vec![],
            message: "log streaming not yet implemented".to_string(),
        }),
    ))
}
