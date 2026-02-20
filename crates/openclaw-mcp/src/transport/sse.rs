use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use axum::{
    extract::Query,
    response::{sse::{Event, Sse}, IntoResponse, Response},
    Json,
};
use futures_util::stream::Stream;
use tokio::sync::mpsc;
use tracing::info;
use std::pin::Pin;

use crate::protocol::{JsonRpcRequest, JsonRpcResponse, PARSE_ERROR};
use crate::server::McpServer;

/// Active SSE sessions
pub type SessionMap = Arc<Mutex<HashMap<String, mpsc::UnboundedSender<String>>>>;

/// Create a new session map
pub fn new_session_map() -> SessionMap {
    Arc::new(Mutex::new(HashMap::new()))
}

/// SSE endpoint handler: GET /sse
/// Creates a new session and returns an SSE stream
pub async fn sse_handler(
    sessions: SessionMap,
    _server: McpServer,
) -> Sse<Pin<Box<dyn Stream<Item = Result<Event, std::convert::Infallible>> + Send>>> {
    let session_id = uuid::Uuid::new_v4().to_string();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    // Store session
    {
        let mut map = sessions.lock().await;
        map.insert(session_id.clone(), tx);
    }
    info!("SSE session connected: {}", session_id);

    // Send the endpoint event so the client knows where to POST
    let endpoint_msg = format!("/messages?sessionId={}", session_id);

    let sessions_clone = sessions.clone();
    let sid = session_id.clone();

    let stream = async_stream::stream! {
        // First event: tell client the message endpoint
        yield Ok(Event::default().event("endpoint").data(endpoint_msg));

        // Then stream responses
        loop {
            match rx.recv().await {
                Some(data) => {
                    yield Ok(Event::default().event("message").data(data));
                }
                None => {
                    // Channel closed
                    break;
                }
            }
        }

        // Cleanup
        let mut map = sessions_clone.lock().await;
        map.remove(&sid);
        info!("SSE session disconnected: {}", sid);
    };

    Sse::new(Box::pin(stream))
}

/// Message endpoint handler: POST /messages?sessionId=...
/// Receives JSON-RPC requests and sends responses via SSE
pub async fn messages_handler(
    sessions: SessionMap,
    server: McpServer,
    Query(params): Query<HashMap<String, String>>,
    body: String,
) -> Response {
    let session_id = match params.get("sessionId") {
        Some(id) => id.clone(),
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "missing sessionId parameter"})),
            ).into_response();
        }
    };

    let tx = {
        let map = sessions.lock().await;
        match map.get(&session_id) {
            Some(tx) => tx.clone(),
            None => {
                return (
                    axum::http::StatusCode::NOT_FOUND,
                    Json(serde_json::json!({"error": "session not found"})),
                ).into_response();
            }
        }
    };

    let request: JsonRpcRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            let err = JsonRpcResponse::error(None, PARSE_ERROR, format!("Parse error: {}", e));
            let msg = serde_json::to_string(&err).unwrap();
            let _ = tx.send(msg);
            return (axum::http::StatusCode::ACCEPTED, "").into_response();
        }
    };

    // Notifications don't get responses
    if request.id.is_none() {
        info!("MCP SSE notification: {}", request.method);
        return (axum::http::StatusCode::ACCEPTED, "").into_response();
    }

    let response = server.handle_request(&request).await;
    let msg = serde_json::to_string(&response).unwrap();
    let _ = tx.send(msg);

    (axum::http::StatusCode::ACCEPTED, "").into_response()
}
