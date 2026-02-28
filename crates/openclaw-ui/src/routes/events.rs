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
use crate::pagination::{build_page, clamp_limit, EventCursor, PaginatedResponse};
use crate::state::AppState;

/// Event row from the `orch_events` table.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct EventRow {
    pub event_id: Uuid,
    pub instance_id: Uuid,
    pub seq: i64,
    pub event_type: String,
    pub event_version: i16,
    pub payload: serde_json::Value,
    pub idempotency_key: Option<String>,
    pub correlation_id: Option<Uuid>,
    pub causation_id: Option<Uuid>,
    pub occurred_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
}

/// Query params for events: pagination + optional event_type filter.
#[derive(Debug, Deserialize)]
pub struct EventPaginationParams {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
    pub event_type: Option<String>,
}

/// GET /api/v1/instances/:id/events — list events (paginated by seq, filter by event_type).
pub async fn list_events(
    State(state): State<Arc<AppState>>,
    Path(instance_id): Path<Uuid>,
    Query(params): Query<EventPaginationParams>,
) -> Result<Json<PaginatedResponse<EventRow>>, ApiError> {
    let limit = clamp_limit(
        params.limit,
        state.config.default_page_limit,
        state.config.max_page_limit,
    );
    let fetch_limit = (limit + 1) as i64;

    let since_seq = if let Some(ref cursor_str) = params.cursor {
        let cursor = EventCursor::decode(cursor_str).ok_or_else(|| {
            ApiError::from(DomainError::Precondition("invalid cursor".into()))
        })?;
        cursor.seq
    } else {
        0
    };

    let rows = if let Some(ref event_type) = params.event_type {
        sqlx::query_as::<_, EventRow>(
            r#"
            SELECT event_id, instance_id, seq, event_type, event_version,
                   payload, idempotency_key, correlation_id, causation_id,
                   occurred_at, recorded_at
            FROM orch_events
            WHERE instance_id = $1 AND seq > $2 AND event_type = $3
            ORDER BY seq ASC
            LIMIT $4
            "#,
        )
        .bind(instance_id)
        .bind(since_seq)
        .bind(event_type)
        .bind(fetch_limit)
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query_as::<_, EventRow>(
            r#"
            SELECT event_id, instance_id, seq, event_type, event_version,
                   payload, idempotency_key, correlation_id, causation_id,
                   occurred_at, recorded_at
            FROM orch_events
            WHERE instance_id = $1 AND seq > $2
            ORDER BY seq ASC
            LIMIT $3
            "#,
        )
        .bind(instance_id)
        .bind(since_seq)
        .bind(fetch_limit)
        .fetch_all(&state.pool)
        .await?
    };

    let page = build_page(rows, limit, |row| {
        EventCursor {
            instance_id: row.instance_id,
            seq: row.seq,
        }
        .encode()
    });

    Ok(Json(page))
}
