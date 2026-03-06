use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use openclaw_orchestrator::domain::errors::DomainError;

use crate::errors::ApiError;
use crate::pagination::{
    build_page, clamp_limit, EntityCursor, PaginatedResponse, PaginationParams,
};
use crate::state::AppState;

/// Task row from the `orch_tasks` table.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct TaskRow {
    pub id: Uuid,
    pub cycle_id: Uuid,
    pub instance_id: Uuid,
    pub task_key: String,
    pub phase: String,
    pub ordinal: i32,
    pub state: String,
    pub title: String,
    pub description: String,
    pub acceptance: Option<serde_json::Value>,
    pub current_attempt: i32,
    pub max_retries: i32,
    pub failure_reason: Option<String>,
    pub cancel_reason: Option<String>,
    pub skip_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Extended query params: standard pagination + optional cycle_id filter.
#[derive(Debug, Deserialize)]
pub struct TaskPaginationParams {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
    pub cycle_id: Option<Uuid>,
}

impl From<&TaskPaginationParams> for PaginationParams {
    fn from(p: &TaskPaginationParams) -> Self {
        PaginationParams {
            cursor: p.cursor.clone(),
            limit: p.limit,
        }
    }
}

/// GET /api/v1/instances/:id/tasks — list tasks for an instance (paginated, filter by cycle_id).
pub async fn list_tasks(
    State(state): State<Arc<AppState>>,
    Path(instance_id): Path<Uuid>,
    Query(params): Query<TaskPaginationParams>,
) -> Result<Json<PaginatedResponse<TaskRow>>, ApiError> {
    let limit = clamp_limit(
        params.limit,
        state.config.default_page_limit,
        state.config.max_page_limit,
    );
    let fetch_limit = (limit + 1) as i64;

    let rows = match (&params.cursor, &params.cycle_id) {
        // No cursor, no filter
        (None, None) => {
            sqlx::query_as::<_, TaskRow>(
                r#"
                SELECT id, cycle_id, instance_id, task_key, phase, ordinal, state,
                       title, description, acceptance, current_attempt, max_retries,
                       failure_reason, cancel_reason, skip_reason,
                       created_at, updated_at
                FROM orch_tasks
                WHERE instance_id = $1
                ORDER BY created_at ASC, id ASC
                LIMIT $2
                "#,
            )
            .bind(instance_id)
            .bind(fetch_limit)
            .fetch_all(&state.pool)
            .await?
        }
        // No cursor, with cycle_id filter
        (None, Some(cycle_id)) => {
            sqlx::query_as::<_, TaskRow>(
                r#"
                SELECT id, cycle_id, instance_id, task_key, phase, ordinal, state,
                       title, description, acceptance, current_attempt, max_retries,
                       failure_reason, cancel_reason, skip_reason,
                       created_at, updated_at
                FROM orch_tasks
                WHERE instance_id = $1 AND cycle_id = $2
                ORDER BY created_at ASC, id ASC
                LIMIT $3
                "#,
            )
            .bind(instance_id)
            .bind(cycle_id)
            .bind(fetch_limit)
            .fetch_all(&state.pool)
            .await?
        }
        // With cursor, no filter
        (Some(cursor_str), None) => {
            let cursor = EntityCursor::decode(cursor_str).ok_or_else(|| {
                ApiError::from(DomainError::Precondition("invalid cursor".into()))
            })?;
            sqlx::query_as::<_, TaskRow>(
                r#"
                SELECT id, cycle_id, instance_id, task_key, phase, ordinal, state,
                       title, description, acceptance, current_attempt, max_retries,
                       failure_reason, cancel_reason, skip_reason,
                       created_at, updated_at
                FROM orch_tasks
                WHERE instance_id = $1 AND (created_at, id) > ($2, $3)
                ORDER BY created_at ASC, id ASC
                LIMIT $4
                "#,
            )
            .bind(instance_id)
            .bind(cursor.created_at)
            .bind(cursor.id)
            .bind(fetch_limit)
            .fetch_all(&state.pool)
            .await?
        }
        // With cursor, with cycle_id filter
        (Some(cursor_str), Some(cycle_id)) => {
            let cursor = EntityCursor::decode(cursor_str).ok_or_else(|| {
                ApiError::from(DomainError::Precondition("invalid cursor".into()))
            })?;
            sqlx::query_as::<_, TaskRow>(
                r#"
                SELECT id, cycle_id, instance_id, task_key, phase, ordinal, state,
                       title, description, acceptance, current_attempt, max_retries,
                       failure_reason, cancel_reason, skip_reason,
                       created_at, updated_at
                FROM orch_tasks
                WHERE instance_id = $1 AND cycle_id = $2 AND (created_at, id) > ($3, $4)
                ORDER BY created_at ASC, id ASC
                LIMIT $5
                "#,
            )
            .bind(instance_id)
            .bind(cycle_id)
            .bind(cursor.created_at)
            .bind(cursor.id)
            .bind(fetch_limit)
            .fetch_all(&state.pool)
            .await?
        }
    };

    let page = build_page(rows, limit, |row| {
        EntityCursor {
            created_at: row.created_at,
            id: row.id,
        }
        .encode()
    });

    Ok(Json(page))
}
