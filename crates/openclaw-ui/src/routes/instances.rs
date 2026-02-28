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

/// Request body for POST /api/v1/instances.
#[derive(Debug, Deserialize)]
pub struct CreateInstanceRequest {
    pub name: String,
    pub project_id: Uuid,
    pub data_dir: String,
    pub token: String,
}

/// Response body for instance creation.
#[derive(Debug, Serialize)]
pub struct CreateInstanceResponse {
    pub instance_id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub state: String,
}

/// POST /api/v1/instances — create a new instance.
///
/// Emits an `InstanceCreated` event via the event store.
pub async fn create_instance(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateInstanceRequest>,
) -> Result<(axum::http::StatusCode, Json<CreateInstanceResponse>), ApiError> {
    if body.name.trim().is_empty() {
        return Err(DomainError::Precondition("name must not be empty".into()).into());
    }
    if body.data_dir.trim().is_empty() {
        return Err(DomainError::Precondition("data_dir must not be empty".into()).into());
    }
    if body.token.is_empty() {
        return Err(DomainError::Precondition("token must not be empty".into()).into());
    }

    // Hash the provided token with argon2id
    let token_hash = {
        use argon2::password_hash::{PasswordHasher, SaltString};
        use argon2::Argon2;
        let salt = SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);
        Argon2::default()
            .hash_password(body.token.as_bytes(), &salt)
            .map_err(|e| DomainError::Precondition(format!("failed to hash token: {e}")))?
            .to_string()
    };

    let instance_id = Uuid::new_v4();
    let project_id = body.project_id;
    let now = Utc::now();

    let envelope = EventEnvelope {
        event_id: Uuid::new_v4(),
        instance_id,
        seq: 0, // assigned by store
        event_type: "InstanceCreated".to_string(),
        event_version: 1,
        payload: serde_json::json!({
            "instance_id": instance_id,
            "project_id": project_id,
            "name": body.name,
            "data_dir": body.data_dir,
            "token_hash": token_hash,
        }),
        idempotency_key: None,
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
        project_id = %project_id,
        name = %body.name,
        "instance created"
    );

    Ok((
        axum::http::StatusCode::CREATED,
        Json(CreateInstanceResponse {
            instance_id,
            project_id,
            name: body.name,
            state: "provisioning".to_string(),
        }),
    ))
}
