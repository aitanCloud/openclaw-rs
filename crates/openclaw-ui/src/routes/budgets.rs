use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use openclaw_orchestrator::domain::errors::DomainError;

use crate::errors::ApiError;
use crate::pagination::{
    build_page, clamp_limit, EntityCursor, PaginatedResponse, PaginationParams,
};
use crate::state::AppState;

/// Budget ledger entry from the `orch_budget_ledger` table.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct BudgetLedgerRow {
    pub id: Uuid,
    pub instance_id: Uuid,
    pub event_type: String,
    pub amount_cents: i64,
    pub balance_after: i64,
    pub created_at: DateTime<Utc>,
}

/// GET /api/v1/instances/:id/budgets — list budget ledger entries (paginated).
pub async fn list_budgets(
    State(state): State<Arc<AppState>>,
    Path(instance_id): Path<Uuid>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<PaginatedResponse<BudgetLedgerRow>>, ApiError> {
    let limit = clamp_limit(
        params.limit,
        state.config.default_page_limit,
        state.config.max_page_limit,
    );
    let fetch_limit = (limit + 1) as i64;

    let rows = if let Some(ref cursor_str) = params.cursor {
        let cursor = EntityCursor::decode(cursor_str).ok_or_else(|| {
            ApiError::from(DomainError::Precondition("invalid cursor".into()))
        })?;
        sqlx::query_as::<_, BudgetLedgerRow>(
            r#"
            SELECT id, instance_id, event_type, amount_cents, balance_after,
                   created_at
            FROM orch_budget_ledger
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
        sqlx::query_as::<_, BudgetLedgerRow>(
            r#"
            SELECT id, instance_id, event_type, amount_cents, balance_after,
                   created_at
            FROM orch_budget_ledger
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
