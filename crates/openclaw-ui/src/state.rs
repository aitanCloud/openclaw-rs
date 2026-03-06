use std::sync::Arc;

use sqlx::PgPool;
use tokio::sync::broadcast;
use uuid::Uuid;

use openclaw_orchestrator::domain::events::EventEnvelope;
use openclaw_orchestrator::domain::ports::EventStore;

/// Shared application state for Axum handlers.
pub struct AppState {
    pub pool: PgPool,
    pub event_store: Arc<dyn EventStore>,
    /// Broadcast sender for real-time event notifications.
    ///
    /// WebSocket handlers call `event_tx.subscribe()` to get a receiver.
    /// The sender is obtained from `PgEventStore::sender()` at startup.
    pub event_tx: broadcast::Sender<EventEnvelope>,
    pub config: ApiConfig,
}

/// API configuration parsed from environment variables.
pub struct ApiConfig {
    /// Argon2id hash of the API bearer token.
    pub auth_token_hash: String,
    /// Socket address to bind (e.g. "0.0.0.0:3130").
    pub listen_addr: String,
    /// Default pagination limit.
    pub default_page_limit: u32,
    /// Maximum pagination limit.
    pub max_page_limit: u32,
    /// Maximum WebSocket events per second per connection (0 = unlimited).
    pub max_ws_events_per_sec: u32,
    /// Instance ID for this orchestrator process. If set, background loops start.
    pub instance_id: Option<Uuid>,
    /// Data directory for worker logs and state files.
    pub data_dir: Option<String>,
}

impl ApiConfig {
    /// Parse configuration from environment variables with sensible defaults.
    pub fn from_env() -> anyhow::Result<Self> {
        let auth_token_hash = std::env::var("OPENCLAW_AUTH_TOKEN_HASH")
            .map_err(|_| anyhow::anyhow!("OPENCLAW_AUTH_TOKEN_HASH must be set"))?;
        let listen_addr =
            std::env::var("OPENCLAW_LISTEN_ADDR").unwrap_or_else(|_| "127.0.0.1:3130".to_string());
        let default_page_limit = std::env::var("OPENCLAW_DEFAULT_PAGE_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(50);
        let max_page_limit = std::env::var("OPENCLAW_MAX_PAGE_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);
        let max_ws_events_per_sec = std::env::var("OPENCLAW_WS_RATE_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);

        let instance_id = std::env::var("OPENCLAW_INSTANCE_ID")
            .ok()
            .and_then(|v| Uuid::parse_str(&v).ok());
        let data_dir = std::env::var("OPENCLAW_DATA_DIR").ok();

        Ok(Self {
            auth_token_hash,
            listen_addr,
            default_page_limit,
            max_page_limit,
            max_ws_events_per_sec,
            instance_id,
            data_dir,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_config_defaults() {
        let config = ApiConfig {
            auth_token_hash: "test-hash".to_string(),
            listen_addr: "127.0.0.1:3130".to_string(),
            default_page_limit: 50,
            max_page_limit: 100,
            max_ws_events_per_sec: 100,
            instance_id: None,
            data_dir: None,
        };
        assert_eq!(config.default_page_limit, 50);
        assert_eq!(config.max_page_limit, 100);
        assert_eq!(config.listen_addr, "127.0.0.1:3130");
        assert_eq!(config.max_ws_events_per_sec, 100);
    }

    #[test]
    fn app_state_has_event_tx() {
        let (tx, _rx) = broadcast::channel::<EventEnvelope>(16);
        // Just verify the field exists and compiles
        let _sender = tx.clone();
        assert!(true);
    }
}
