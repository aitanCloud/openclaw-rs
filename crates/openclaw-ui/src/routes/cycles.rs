use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use openclaw_orchestrator::domain::errors::DomainError;
use openclaw_orchestrator::domain::events::EventEnvelope;

use crate::errors::ApiError;
use crate::pagination::{
    build_page, clamp_limit, EntityCursor, PaginatedResponse, PaginationParams,
};
use crate::state::AppState;

/// Cycle row from the `orch_cycles` table.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct CycleRow {
    pub id: Uuid,
    pub instance_id: Uuid,
    pub state: String,
    pub prompt: String,
    pub plan: Option<serde_json::Value>,
    pub block_reason: Option<String>,
    pub failure_reason: Option<String>,
    pub cancel_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// GET /api/v1/instances/:id/cycles — list cycles for an instance (paginated).
pub async fn list_cycles(
    State(state): State<Arc<AppState>>,
    Path(instance_id): Path<Uuid>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<PaginatedResponse<CycleRow>>, ApiError> {
    let limit = clamp_limit(
        params.limit,
        state.config.default_page_limit,
        state.config.max_page_limit,
    );
    let fetch_limit = (limit + 1) as i64;

    let rows = if let Some(ref cursor_str) = params.cursor {
        let cursor = EntityCursor::decode(cursor_str).ok_or_else(|| {
            ApiError::from(openclaw_orchestrator::app::errors::AppError::Domain(
                DomainError::Precondition("invalid cursor".into()),
            ))
        })?;
        sqlx::query_as::<_, CycleRow>(
            r#"
            SELECT id, instance_id, state, prompt, plan, block_reason,
                   failure_reason, cancel_reason, created_at, updated_at
            FROM orch_cycles
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
    } else {
        sqlx::query_as::<_, CycleRow>(
            r#"
            SELECT id, instance_id, state, prompt, plan, block_reason,
                   failure_reason, cancel_reason, created_at, updated_at
            FROM orch_cycles
            WHERE instance_id = $1
            ORDER BY created_at ASC, id ASC
            LIMIT $2
            "#,
        )
        .bind(instance_id)
        .bind(fetch_limit)
        .fetch_all(&state.pool)
        .await?
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

/// GET /api/v1/instances/:id/cycles/:cycle_id — get cycle detail (includes plan JSON).
pub async fn get_cycle(
    State(state): State<Arc<AppState>>,
    Path((instance_id, cycle_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<CycleRow>, ApiError> {
    let row = sqlx::query_as::<_, CycleRow>(
        r#"
        SELECT id, instance_id, state, prompt, plan, block_reason,
               failure_reason, cancel_reason, created_at, updated_at
        FROM orch_cycles
        WHERE instance_id = $1 AND id = $2
        "#,
    )
    .bind(instance_id)
    .bind(cycle_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| {
        ApiError::from(DomainError::NotFound {
            entity: "Cycle".into(),
            id: cycle_id.to_string(),
        })
    })?;

    Ok(Json(row))
}

/// Request body for POST /api/v1/instances/:id/cycles.
#[derive(Debug, Deserialize)]
pub struct CreateCycleRequest {
    pub prompt: String,
}

/// Response body for cycle creation.
#[derive(Debug, Serialize)]
pub struct CreateCycleResponse {
    pub cycle_id: Uuid,
    pub state: String,
}

/// POST /api/v1/instances/:id/cycles — create a new cycle.
///
/// Emits `CycleCreated` + `PlanRequested` events via the event store.
pub async fn create_cycle(
    State(state): State<Arc<AppState>>,
    Path(instance_id): Path<Uuid>,
    Json(body): Json<CreateCycleRequest>,
) -> Result<(axum::http::StatusCode, Json<CreateCycleResponse>), ApiError> {
    if body.prompt.trim().is_empty() {
        return Err(DomainError::Precondition("prompt must not be empty".into()).into());
    }

    let cycle_id = Uuid::new_v4();
    let now = Utc::now();

    // Emit CycleCreated event
    let cycle_created = EventEnvelope {
        event_id: Uuid::new_v4(),
        instance_id,
        seq: 0, // assigned by store
        event_type: "CycleCreated".to_string(),
        event_version: 1,
        payload: serde_json::json!({
            "cycle_id": cycle_id,
            "prompt": body.prompt,
            "actor": { "kind": "Human" },
        }),
        idempotency_key: None,
        correlation_id: None,
        causation_id: None,
        occurred_at: now,
        recorded_at: now,
    };
    state
        .event_store
        .emit(cycle_created)
        .await
        .map_err(|e| ApiError::from(openclaw_orchestrator::app::errors::AppError::Domain(e)))?;

    // Emit PlanRequested event
    let plan_requested = EventEnvelope {
        event_id: Uuid::new_v4(),
        instance_id,
        seq: 0,
        event_type: "PlanRequested".to_string(),
        event_version: 1,
        payload: serde_json::json!({
            "cycle_id": cycle_id,
            "objective": body.prompt,
            "actor": { "kind": "System" },
        }),
        idempotency_key: Some(format!("plan-requested-{cycle_id}")),
        correlation_id: None,
        causation_id: None,
        occurred_at: now,
        recorded_at: now,
    };
    state
        .event_store
        .emit(plan_requested)
        .await
        .map_err(|e| ApiError::from(openclaw_orchestrator::app::errors::AppError::Domain(e)))?;

    tracing::info!(
        instance_id = %instance_id,
        cycle_id = %cycle_id,
        "cycle created and plan requested"
    );

    Ok((
        axum::http::StatusCode::CREATED,
        Json(CreateCycleResponse {
            cycle_id,
            state: "planning".to_string(),
        }),
    ))
}

/// Request body for POST /api/v1/instances/:id/cycles/:cycle_id/approve.
#[derive(Debug, Deserialize)]
pub struct ApprovePlanRequest {
    pub approved_by: String,
}

/// Response body for plan approval.
#[derive(Debug, Serialize)]
pub struct ApprovePlanResponse {
    pub cycle_id: Uuid,
    pub state: String,
}

/// POST /api/v1/instances/:id/cycles/:cycle_id/approve — approve a plan.
///
/// Emits `PlanApproved` event via the event store.
pub async fn approve_plan(
    State(state): State<Arc<AppState>>,
    Path((instance_id, cycle_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<ApprovePlanRequest>,
) -> Result<Json<ApprovePlanResponse>, ApiError> {
    if body.approved_by.trim().is_empty() {
        return Err(DomainError::Precondition("approved_by must not be empty".into()).into());
    }

    let now = Utc::now();

    let envelope = EventEnvelope {
        event_id: Uuid::new_v4(),
        instance_id,
        seq: 0,
        event_type: "PlanApproved".to_string(),
        event_version: 1,
        payload: serde_json::json!({
            "cycle_id": cycle_id,
            "approved_by": body.approved_by,
            "actor": { "kind": "Human", "actor_id": body.approved_by },
        }),
        idempotency_key: Some(format!("plan-approved-{cycle_id}")),
        correlation_id: None,
        causation_id: None,
        occurred_at: now,
        recorded_at: now,
    };

    state
        .event_store
        .emit(envelope)
        .await
        .map_err(|e| ApiError::from(openclaw_orchestrator::app::errors::AppError::Domain(e)))?;

    tracing::info!(
        instance_id = %instance_id,
        cycle_id = %cycle_id,
        approved_by = %body.approved_by,
        "plan approved"
    );

    Ok(Json(ApprovePlanResponse {
        cycle_id,
        state: "approved".to_string(),
    }))
}
