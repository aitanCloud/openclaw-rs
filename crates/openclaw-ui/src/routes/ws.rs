//! WebSocket endpoint for real-time event streaming.
//!
//! Protocol:
//! 1. Client connects to `/api/v1/instances/{id}/events/ws`
//! 2. Client sends `{ "type": "auth", "token": "..." }` within 5 seconds
//! 3. Client sends `{ "type": "subscribe", "since_seq": N }`
//! 4. Server replays events with seq > N, then sends `backfill_complete`
//! 5. Server enters live mode: wakes on broadcast, fetches from DB
//! 6. Heartbeat every 30 seconds if idle

use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    response::IntoResponse,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{debug, instrument, warn};
use uuid::Uuid;

use openclaw_orchestrator::domain::events::EventEnvelope;

use futures_util::SinkExt;

use crate::auth::verify_token;
use crate::state::AppState;

// ── Wire-format messages ───────────────────────────────────────────

/// Messages the client sends to the server.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Auth { token: String },
    Subscribe { since_seq: i64 },
}

/// Messages the server sends to the client.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Event {
        seq: i64,
        event: EventEnvelope,
    },
    BackfillComplete {
        last_sent_seq: i64,
        head_seq: i64,
    },
    Heartbeat {
        ts: String,
    },
    Error {
        message: String,
    },
}

impl ServerMessage {
    /// Serialize to a text WebSocket message.
    fn into_ws_message(self) -> Message {
        // serde_json::to_string should not fail on our well-typed enums
        let json = serde_json::to_string(&self).expect("ServerMessage serialization");
        Message::Text(json.into())
    }
}

// ── Constants ──────────────────────────────────────────────────────

/// Time the client has to send the auth message after connecting.
const AUTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Interval between heartbeat messages when idle.
const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

// ── Handler ────────────────────────────────────────────────────────

/// WebSocket upgrade handler.
///
/// This endpoint does NOT use the auth middleware layer — authentication
/// is performed via the first WebSocket message ("first-message auth").
#[instrument(skip(ws, state), fields(%instance_id))]
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Path(instance_id): Path<Uuid>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    debug!("ws upgrade requested");
    ws.on_upgrade(move |socket| handle_socket(socket, instance_id, state))
}

/// Main WebSocket session loop.
async fn handle_socket(mut socket: WebSocket, instance_id: Uuid, state: Arc<AppState>) {
    // ── Phase 1: Auth ──────────────────────────────────────────
    if let Err(msg) = wait_for_auth(&mut socket, &state).await {
        let _ = socket
            .send(ServerMessage::Error { message: msg }.into_ws_message())
            .await;
        let _ = socket.close().await;
        return;
    }
    debug!(instance_id = %instance_id, "ws authenticated");

    // ── Phase 2: Subscribe + backfill ──────────────────────────
    let since_seq = match wait_for_subscribe(&mut socket).await {
        Ok(seq) => seq,
        Err(msg) => {
            let _ = socket
                .send(ServerMessage::Error { message: msg }.into_ws_message())
                .await;
            let _ = socket.close().await;
            return;
        }
    };

    let last_sent_seq = match do_backfill(&mut socket, instance_id, since_seq, &state).await {
        Ok(seq) => seq,
        Err(msg) => {
            let _ = socket
                .send(ServerMessage::Error { message: msg }.into_ws_message())
                .await;
            let _ = socket.close().await;
            return;
        }
    };

    // ── Phase 3: Live streaming ────────────────────────────────
    let rx = state.event_tx.subscribe();
    live_loop(&mut socket, instance_id, last_sent_seq, rx, &state).await;
}

/// Wait for the client's auth message within the timeout.
async fn wait_for_auth(
    socket: &mut WebSocket,
    state: &AppState,
) -> Result<(), String> {
    let msg = tokio::time::timeout(AUTH_TIMEOUT, recv_text(socket))
        .await
        .map_err(|_| "auth timeout: no auth message within 5 seconds".to_string())?
        .map_err(|e| format!("connection error during auth: {e}"))?;

    let client_msg: ClientMessage =
        serde_json::from_str(&msg).map_err(|e| format!("invalid auth message: {e}"))?;

    match client_msg {
        ClientMessage::Auth { token } => {
            if verify_token(&token, &state.config.auth_token_hash) {
                Ok(())
            } else {
                Err("authentication failed".to_string())
            }
        }
        _ => Err("expected auth message as first message".to_string()),
    }
}

/// Wait for the client's subscribe message (reuses auth timeout).
async fn wait_for_subscribe(socket: &mut WebSocket) -> Result<i64, String> {
    let msg = tokio::time::timeout(AUTH_TIMEOUT, recv_text(socket))
        .await
        .map_err(|_| "subscribe timeout: no subscribe message within 5 seconds".to_string())?
        .map_err(|e| format!("connection error during subscribe: {e}"))?;

    let client_msg: ClientMessage =
        serde_json::from_str(&msg).map_err(|e| format!("invalid subscribe message: {e}"))?;

    match client_msg {
        ClientMessage::Subscribe { since_seq } => Ok(since_seq),
        _ => Err("expected subscribe message".to_string()),
    }
}

/// Replay events from DB and send `backfill_complete`.
///
/// Returns the seq of the last event sent (or `since_seq` if no events).
async fn do_backfill(
    socket: &mut WebSocket,
    instance_id: Uuid,
    since_seq: i64,
    state: &AppState,
) -> Result<i64, String> {
    let events = state
        .event_store
        .replay(instance_id, since_seq)
        .await
        .map_err(|e| format!("backfill query failed: {e}"))?;

    let mut last_sent = since_seq;
    for event in &events {
        let msg = ServerMessage::Event {
            seq: event.seq,
            event: event.clone(),
        };
        socket
            .send(msg.into_ws_message())
            .await
            .map_err(|e| format!("send failed during backfill: {e}"))?;
        last_sent = event.seq;
    }

    // head_seq is the max seq we just sent (which is the DB head for this instance)
    let head_seq = last_sent;

    socket
        .send(
            ServerMessage::BackfillComplete {
                last_sent_seq: last_sent,
                head_seq,
            }
            .into_ws_message(),
        )
        .await
        .map_err(|e| format!("send failed for backfill_complete: {e}"))?;

    debug!(
        instance_id = %instance_id,
        since_seq,
        last_sent,
        "backfill complete"
    );

    Ok(last_sent)
}

/// Live streaming loop: wake on broadcast, fetch from DB, send to client.
async fn live_loop(
    socket: &mut WebSocket,
    instance_id: Uuid,
    mut last_sent_seq: i64,
    mut rx: broadcast::Receiver<EventEnvelope>,
    state: &AppState,
) {
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    // First tick fires immediately; skip it.
    heartbeat.tick().await;

    loop {
        tokio::select! {
            // ── Broadcast wake-up ──
            result = rx.recv() => {
                match result {
                    Ok(envelope) => {
                        // Only fetch from DB if the broadcast hints this instance
                        // has new events. The broadcast carries *all* instances'
                        // events, so filter by instance_id.
                        if envelope.instance_id != instance_id {
                            continue;
                        }

                        // Wake-up pattern: always fetch from DB, never send
                        // directly from the broadcast payload.
                        match fetch_and_send(socket, instance_id, &mut last_sent_seq, state).await {
                            Ok(()) => {},
                            Err(e) => {
                                warn!(instance_id = %instance_id, error = %e, "live send error");
                                return;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(instance_id = %instance_id, lagged = n, "broadcast lagged");
                        let _ = socket
                            .send(ServerMessage::Error {
                                message: "lagged".to_string(),
                            }.into_ws_message())
                            .await;
                        let _ = socket.close().await;
                        return;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        debug!(instance_id = %instance_id, "broadcast channel closed");
                        return;
                    }
                }
            }

            // ── Heartbeat tick ──
            _ = heartbeat.tick() => {
                let msg = ServerMessage::Heartbeat {
                    ts: Utc::now().to_rfc3339(),
                };
                if socket.send(msg.into_ws_message()).await.is_err() {
                    return;
                }
            }

            // ── Client message (ping/pong/close) ──
            maybe_msg = socket.recv() => {
                match maybe_msg {
                    Some(Ok(Message::Close(_))) | None => {
                        debug!(instance_id = %instance_id, "client disconnected");
                        return;
                    }
                    Some(Ok(Message::Ping(data))) => {
                        if socket.send(Message::Pong(data)).await.is_err() {
                            return;
                        }
                    }
                    Some(Ok(_)) => {
                        // Ignore other messages in live mode
                    }
                    Some(Err(e)) => {
                        warn!(instance_id = %instance_id, error = %e, "ws recv error");
                        return;
                    }
                }
            }
        }
    }
}

/// Fetch new events from DB and send them over the socket.
async fn fetch_and_send(
    socket: &mut WebSocket,
    instance_id: Uuid,
    last_sent_seq: &mut i64,
    state: &AppState,
) -> Result<(), String> {
    let events = state
        .event_store
        .replay(instance_id, *last_sent_seq)
        .await
        .map_err(|e| format!("live replay failed: {e}"))?;

    for event in &events {
        let msg = ServerMessage::Event {
            seq: event.seq,
            event: event.clone(),
        };
        socket
            .send(msg.into_ws_message())
            .await
            .map_err(|e| format!("live send failed: {e}"))?;
        *last_sent_seq = event.seq;
    }

    Ok(())
}

/// Receive the next text message from the socket, skipping ping/pong.
async fn recv_text(socket: &mut WebSocket) -> Result<String, String> {
    loop {
        match socket.recv().await {
            Some(Ok(Message::Text(text))) => return Ok(text.to_string()),
            Some(Ok(Message::Ping(data))) => {
                // Reply to pings during handshake
                let _ = socket.send(Message::Pong(data)).await;
            }
            Some(Ok(Message::Close(_))) | None => {
                return Err("connection closed".to_string());
            }
            Some(Ok(_)) => {
                // Skip binary, pong, etc.
                continue;
            }
            Some(Err(e)) => {
                return Err(format!("receive error: {e}"));
            }
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── ClientMessage deserialization ──

    #[test]
    fn deserialize_auth_message() {
        let json = r#"{"type": "auth", "token": "my-secret"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::Auth { token } => assert_eq!(token, "my-secret"),
            _ => panic!("expected Auth variant"),
        }
    }

    #[test]
    fn deserialize_subscribe_message() {
        let json = r#"{"type": "subscribe", "since_seq": 42}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::Subscribe { since_seq } => assert_eq!(since_seq, 42),
            _ => panic!("expected Subscribe variant"),
        }
    }

    #[test]
    fn deserialize_subscribe_since_zero() {
        let json = r#"{"type": "subscribe", "since_seq": 0}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::Subscribe { since_seq } => assert_eq!(since_seq, 0),
            _ => panic!("expected Subscribe variant"),
        }
    }

    #[test]
    fn deserialize_invalid_type_fails() {
        let json = r#"{"type": "unknown"}"#;
        let result = serde_json::from_str::<ClientMessage>(json);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_missing_token_fails() {
        let json = r#"{"type": "auth"}"#;
        let result = serde_json::from_str::<ClientMessage>(json);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_missing_since_seq_fails() {
        let json = r#"{"type": "subscribe"}"#;
        let result = serde_json::from_str::<ClientMessage>(json);
        assert!(result.is_err());
    }

    // ── ServerMessage serialization ──

    #[test]
    fn serialize_event_message() {
        let envelope = EventEnvelope {
            event_id: Uuid::nil(),
            instance_id: Uuid::nil(),
            seq: 7,
            event_type: "CycleCreated".to_string(),
            event_version: 1,
            payload: serde_json::json!({"name": "test"}),
            idempotency_key: None,
            correlation_id: None,
            causation_id: None,
            occurred_at: Utc::now(),
            recorded_at: Utc::now(),
        };
        let msg = ServerMessage::Event {
            seq: 7,
            event: envelope,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"event""#));
        assert!(json.contains(r#""seq":7"#));
        assert!(json.contains(r#""event_type":"CycleCreated""#));
    }

    #[test]
    fn serialize_backfill_complete() {
        let msg = ServerMessage::BackfillComplete {
            last_sent_seq: 10,
            head_seq: 10,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"backfill_complete""#));
        assert!(json.contains(r#""last_sent_seq":10"#));
        assert!(json.contains(r#""head_seq":10"#));
    }

    #[test]
    fn serialize_heartbeat() {
        let msg = ServerMessage::Heartbeat {
            ts: "2026-02-27T12:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"heartbeat""#));
        assert!(json.contains(r#""ts":"2026-02-27T12:00:00Z""#));
    }

    #[test]
    fn serialize_error_message() {
        let msg = ServerMessage::Error {
            message: "lagged".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"error""#));
        assert!(json.contains(r#""message":"lagged""#));
    }

    #[test]
    fn server_message_into_ws_message() {
        let msg = ServerMessage::Heartbeat {
            ts: "2026-02-27T12:00:00Z".to_string(),
        };
        let ws_msg = msg.into_ws_message();
        match ws_msg {
            Message::Text(text) => {
                assert!(text.contains("heartbeat"));
            }
            _ => panic!("expected Text message"),
        }
    }

    // ── Constants ──

    #[test]
    fn auth_timeout_is_5_seconds() {
        assert_eq!(AUTH_TIMEOUT, std::time::Duration::from_secs(5));
    }

    #[test]
    fn heartbeat_interval_is_30_seconds() {
        assert_eq!(HEARTBEAT_INTERVAL, std::time::Duration::from_secs(30));
    }
}
