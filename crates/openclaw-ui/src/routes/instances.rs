use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::errors::ApiError;
use crate::pagination::{
    build_page, clamp_limit, EntityCursor, PaginatedResponse, PaginationParams,
};
use crate::state::AppState;

/// Instance row from the `orch_instances` table.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct InstanceRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub state: String,
    pub block_reason: Option<String>,
    pub started_at: DateTime<Utc>,
    pub last_heartbeat: DateTime<Utc>,
}

/// GET /api/v1/instances — list instances (paginated).
pub async fn list_instances(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<PaginatedResponse<InstanceRow>>, ApiError> {
    let limit = clamp_limit(
        params.limit,
        state.config.default_page_limit,
        state.config.max_page_limit,
    );
    let fetch_limit = (limit + 1) as i64;

    let rows = if let Some(ref cursor_str) = params.cursor {
        let cursor = EntityCursor::decode(cursor_str).ok_or_else(|| {
            ApiError::from(openclaw_orchestrator::app::errors::AppError::Domain(
                openclaw_orchestrator::domain::errors::DomainError::Precondition(
                    "invalid cursor".into(),
                ),
            ))
        })?;
        sqlx::query_as::<_, InstanceRow>(
            r#"
            SELECT id, project_id, name, state, block_reason, started_at, last_heartbeat
            FROM orch_instances
            WHERE (started_at, id) > ($1, $2)
            ORDER BY started_at ASC, id ASC
            LIMIT $3
            "#,
        )
        .bind(cursor.created_at)
        .bind(cursor.id)
        .bind(fetch_limit)
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query_as::<_, InstanceRow>(
            r#"
            SELECT id, project_id, name, state, block_reason, started_at, last_heartbeat
            FROM orch_instances
            ORDER BY started_at ASC, id ASC
            LIMIT $1
            "#,
        )
        .bind(fetch_limit)
        .fetch_all(&state.pool)
        .await?
    };

    let page = build_page(rows, limit, |row| {
        EntityCursor {
            created_at: row.started_at,
            id: row.id,
        }
        .encode()
    });

    Ok(Json(page))
}

/// GET /api/v1/instances/:id — get instance detail.
pub async fn get_instance(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<InstanceRow>, ApiError> {
    let row = sqlx::query_as::<_, InstanceRow>(
        r#"
        SELECT id, project_id, name, state, block_reason, started_at, last_heartbeat
        FROM orch_instances
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| {
        ApiError::from(openclaw_orchestrator::domain::errors::DomainError::NotFound {
            entity: "Instance".into(),
            id: id.to_string(),
        })
    })?;

    Ok(Json(row))
}
