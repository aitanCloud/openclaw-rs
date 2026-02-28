use std::sync::Arc;

use axum::{
    middleware,
    routing::{any, get, post},
    Router,
};

use crate::auth::auth_middleware;
use crate::routes::{budgets, cycles, events, instances, runs, tasks, ws};
use crate::state::AppState;

/// Build the API routes (nested under /api/v1).
fn api_routes() -> Router<Arc<AppState>> {
    Router::new()
        // Instances
        .route(
            "/instances",
            get(instances::list_instances).post(instances::create_instance),
        )
        .route("/instances/{id}", get(instances::get_instance))
        // Cycles
        .route(
            "/instances/{id}/cycles",
            get(cycles::list_cycles).post(cycles::create_cycle),
        )
        .route(
            "/instances/{id}/cycles/{cycle_id}",
            get(cycles::get_cycle),
        )
        .route(
            "/instances/{id}/cycles/{cycle_id}/approve",
            post(cycles::approve_plan),
        )
        .route(
            "/instances/{id}/cycles/{cycle_id}/merge",
            post(cycles::trigger_merge),
        )
        // Tasks
        .route("/instances/{id}/tasks", get(tasks::list_tasks))
        // Runs
        .route("/instances/{id}/runs/{run_id}", get(runs::get_run))
        .route(
            "/instances/{id}/runs/{run_id}/logs",
            get(runs::get_run_logs),
        )
        // Budgets
        .route("/instances/{id}/budgets", get(budgets::list_budgets))
        // Events
        .route("/instances/{id}/events", get(events::list_events))
}

/// Routes that bypass the auth middleware (first-message auth via WebSocket).
fn ws_routes() -> Router<Arc<AppState>> {
    Router::new().route(
        "/instances/{id}/events/ws",
        any(ws::ws_handler),
    )
}

/// Build the full Axum router with auth middleware and CORS.
///
/// The WebSocket route uses first-message auth and is mounted OUTSIDE
/// the bearer-token auth middleware layer.
pub fn build_router(state: AppState) -> Router {
    let shared_state = Arc::new(state);

    // Authenticated routes (bearer token in header)
    let authed = Router::new()
        .nest("/api/v1", api_routes())
        .layer(middleware::from_fn_with_state(
            shared_state.clone(),
            auth_middleware,
        ));

    // Unauthenticated routes (WS uses first-message auth)
    let unauthed = Router::new().nest("/api/v1", ws_routes());

    // Merge: unauthed routes first, then authed routes.
    // Axum merges without applying the layer from one router to the other.
    authed
        .merge(unauthed)
        .with_state(shared_state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::state::ApiConfig;
    use openclaw_orchestrator::domain::errors::DomainError;
    use openclaw_orchestrator::domain::events::EventEnvelope;
    use openclaw_orchestrator::domain::ports::EventStore;
    use tokio::sync::broadcast;
    use uuid::Uuid;

    /// Minimal event store for route registration tests.
    struct NoopEventStore;

    #[async_trait::async_trait]
    impl EventStore for NoopEventStore {
        async fn emit(&self, _event: EventEnvelope) -> Result<i64, DomainError> {
            Ok(1)
        }
        async fn replay(
            &self,
            _instance_id: Uuid,
            _since_seq: i64,
        ) -> Result<Vec<EventEnvelope>, DomainError> {
            Ok(vec![])
        }
    }

    fn make_test_state() -> AppState {
        use argon2::password_hash::{PasswordHasher, SaltString};
        use argon2::Argon2;

        let salt = SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);
        let hash = Argon2::default()
            .hash_password(b"test-token", &salt)
            .unwrap()
            .to_string();

        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://dummy:dummy@localhost:5432/dummy")
            .expect("connect_lazy should not fail");

        let (event_tx, _) = broadcast::channel(16);

        AppState {
            pool,
            event_store: Arc::new(NoopEventStore),
            event_tx,
            config: ApiConfig {
                auth_token_hash: hash,
                listen_addr: "127.0.0.1:0".to_string(),
                default_page_limit: 50,
                max_page_limit: 100,
                max_ws_events_per_sec: 100,
            },
        }
    }

    /// Verify all routes are mounted by sending requests without auth
    /// and expecting 401 (not 404).
    #[tokio::test]
    async fn all_routes_return_401_without_auth() {
        let state = make_test_state();
        let app = build_router(state);

        let paths = vec![
            ("GET", "/api/v1/instances"),
            (
                "GET",
                "/api/v1/instances/00000000-0000-0000-0000-000000000001",
            ),
            (
                "GET",
                "/api/v1/instances/00000000-0000-0000-0000-000000000001/cycles",
            ),
            (
                "GET",
                "/api/v1/instances/00000000-0000-0000-0000-000000000001/cycles/00000000-0000-0000-0000-000000000002",
            ),
            (
                "GET",
                "/api/v1/instances/00000000-0000-0000-0000-000000000001/tasks",
            ),
            (
                "GET",
                "/api/v1/instances/00000000-0000-0000-0000-000000000001/runs/00000000-0000-0000-0000-000000000003",
            ),
            (
                "GET",
                "/api/v1/instances/00000000-0000-0000-0000-000000000001/runs/00000000-0000-0000-0000-000000000003/logs",
            ),
            (
                "GET",
                "/api/v1/instances/00000000-0000-0000-0000-000000000001/budgets",
            ),
            (
                "GET",
                "/api/v1/instances/00000000-0000-0000-0000-000000000001/events",
            ),
        ];

        for (method, path) in paths {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .unwrap();

            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "expected 401 for {method} {path}, got {}",
                response.status()
            );
        }
    }

    #[tokio::test]
    async fn post_routes_return_401_without_auth() {
        let state = make_test_state();
        let app = build_router(state);

        let paths = vec![
            "/api/v1/instances",
            "/api/v1/instances/00000000-0000-0000-0000-000000000001/cycles",
            "/api/v1/instances/00000000-0000-0000-0000-000000000001/cycles/00000000-0000-0000-0000-000000000002/approve",
            "/api/v1/instances/00000000-0000-0000-0000-000000000001/cycles/00000000-0000-0000-0000-000000000002/merge",
        ];

        for path in paths {
            let request = Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap();

            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "expected 401 for POST {path}, got {}",
                response.status()
            );
        }
    }

    #[tokio::test]
    async fn unknown_route_returns_404() {
        let state = make_test_state();
        let app = build_router(state);

        let request = Request::builder()
            .method("GET")
            .uri("/api/v1/nonexistent")
            .header("authorization", "Bearer test-token")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// The WebSocket route is mounted and does NOT go through auth middleware.
    /// A non-upgrade request should fail with a WebSocket-related error, not 401 or 404.
    #[tokio::test]
    async fn ws_route_is_mounted_outside_auth() {
        let state = make_test_state();
        let app = build_router(state);

        // A plain GET without WebSocket upgrade headers should NOT return 404 (route exists)
        // and should NOT return 401 (no auth middleware on this route).
        let request = Request::builder()
            .method("GET")
            .uri("/api/v1/instances/00000000-0000-0000-0000-000000000001/events/ws")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        // Without upgrade headers, axum's WebSocketUpgrade extractor rejects.
        // This should NOT be 401 (auth bypass confirmed) or 404 (route exists).
        let status = response.status();
        assert_ne!(status, StatusCode::UNAUTHORIZED, "ws route should not require auth middleware");
        assert_ne!(status, StatusCode::NOT_FOUND, "ws route should be mounted");
    }
}
