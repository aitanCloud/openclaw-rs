use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};

use openclaw_orchestrator::app::errors::AppError;
use openclaw_orchestrator::domain::errors::DomainError;

/// HTTP API error type wrapping AppError.
///
/// Maps domain and infra errors to appropriate HTTP status codes.
/// Internal details are never exposed for infrastructure errors.
pub struct ApiError(pub AppError);

impl From<AppError> for ApiError {
    fn from(e: AppError) -> Self {
        Self(e)
    }
}

impl From<DomainError> for ApiError {
    fn from(e: DomainError) -> Self {
        Self(AppError::Domain(e))
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        Self(AppError::Infra(
            openclaw_orchestrator::infra::errors::InfraError::Database(e.to_string()),
        ))
    }
}

/// Map an AppError variant to (status_code, error_code, message).
pub fn classify_error(err: &AppError) -> (StatusCode, &'static str, String) {
    match err {
        AppError::Domain(domain_err) => match domain_err {
            DomainError::Precondition(msg) => {
                (StatusCode::BAD_REQUEST, "PRECONDITION_FAILED", msg.clone())
            }
            DomainError::NotFound { entity, id } => (
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                format!("{entity} not found: {id}"),
            ),
            DomainError::InvalidTransition {
                entity,
                from,
                trigger,
            } => (
                StatusCode::CONFLICT,
                "INVALID_TRANSITION",
                format!("{entity} cannot transition from {from} via {trigger}"),
            ),
            DomainError::InvalidState(msg) => {
                (StatusCode::BAD_REQUEST, "INVALID_STATE", msg.clone())
            }
            DomainError::AlreadyExists { entity, id } => (
                StatusCode::CONFLICT,
                "ALREADY_EXISTS",
                format!("{entity} already exists: {id}"),
            ),
            DomainError::BudgetExceeded {
                available_cents,
                requested_cents,
            } => (
                StatusCode::BAD_REQUEST,
                "BUDGET_EXCEEDED",
                format!(
                    "budget exceeded: available={available_cents} requested={requested_cents}"
                ),
            ),
        },
        AppError::Infra(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            "an internal error occurred".to_string(),
        ),
        AppError::ConcurrencyConflict(msg) => {
            (StatusCode::CONFLICT, "IDEMPOTENCY_CONFLICT", msg.clone())
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = classify_error(&self.0);

        // Log internal errors at error level; domain errors at debug
        match &self.0 {
            AppError::Infra(e) => {
                tracing::error!(error = %e, "internal error");
            }
            _ => {
                tracing::debug!(code = code, message = %message, "api error");
            }
        }

        let body = serde_json::json!({
            "error": {
                "code": code,
                "message": message,
            }
        });
        (status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openclaw_orchestrator::infra::errors::InfraError;

    // ── classify_error ──

    #[test]
    fn precondition_maps_to_400() {
        let err = AppError::Domain(DomainError::Precondition("must be active".into()));
        let (status, code, msg) = classify_error(&err);
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "PRECONDITION_FAILED");
        assert_eq!(msg, "must be active");
    }

    #[test]
    fn not_found_maps_to_404() {
        let err = AppError::Domain(DomainError::NotFound {
            entity: "Instance".into(),
            id: "abc-123".into(),
        });
        let (status, code, msg) = classify_error(&err);
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(code, "NOT_FOUND");
        assert!(msg.contains("Instance"));
        assert!(msg.contains("abc-123"));
    }

    #[test]
    fn invalid_transition_maps_to_409() {
        let err = AppError::Domain(DomainError::InvalidTransition {
            entity: "CycleState".into(),
            from: "Created".into(),
            trigger: "Complete".into(),
        });
        let (status, code, _msg) = classify_error(&err);
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code, "INVALID_TRANSITION");
    }

    #[test]
    fn invalid_state_maps_to_400() {
        let err = AppError::Domain(DomainError::InvalidState("bad state".into()));
        let (status, code, _) = classify_error(&err);
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "INVALID_STATE");
    }

    #[test]
    fn already_exists_maps_to_409() {
        let err = AppError::Domain(DomainError::AlreadyExists {
            entity: "Instance".into(),
            id: "def".into(),
        });
        let (status, code, _) = classify_error(&err);
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code, "ALREADY_EXISTS");
    }

    #[test]
    fn budget_exceeded_maps_to_400() {
        let err = AppError::Domain(DomainError::BudgetExceeded {
            available_cents: 100,
            requested_cents: 200,
        });
        let (status, code, _) = classify_error(&err);
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "BUDGET_EXCEEDED");
    }

    #[test]
    fn infra_error_maps_to_500_no_details() {
        let err = AppError::Infra(InfraError::Database(
            "connection refused to 10.0.0.1:5432".into(),
        ));
        let (status, code, msg) = classify_error(&err);
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(code, "INTERNAL_ERROR");
        // Must NOT leak internal details
        assert!(!msg.contains("10.0.0.1"));
        assert!(!msg.contains("connection"));
        assert_eq!(msg, "an internal error occurred");
    }

    #[test]
    fn concurrency_conflict_maps_to_409() {
        let err = AppError::ConcurrencyConflict("stale version".into());
        let (status, code, msg) = classify_error(&err);
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code, "IDEMPOTENCY_CONFLICT");
        assert_eq!(msg, "stale version");
    }

    // ── From conversions ──

    #[test]
    fn from_app_error() {
        let app_err = AppError::ConcurrencyConflict("test".into());
        let api_err: ApiError = app_err.into();
        let (status, _, _) = classify_error(&api_err.0);
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[test]
    fn from_domain_error() {
        let domain_err = DomainError::NotFound {
            entity: "Run".into(),
            id: "123".into(),
        };
        let api_err: ApiError = domain_err.into();
        let (status, _, _) = classify_error(&api_err.0);
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
