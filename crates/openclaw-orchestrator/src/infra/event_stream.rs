use sqlx::PgPool;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::domain::events::EventEnvelope;
use crate::infra::errors::InfraError;
use crate::infra::event_store::EventRow;

/// Subscribes to events for all instances via Postgres LISTEN/NOTIFY.
/// On notification, replays any events since last known seq per instance.
///
/// Usage:
/// ```ignore
/// let (tx, _rx) = broadcast::channel(1024);
/// let listener = EventStreamListener::new(pool.clone(), tx);
/// tokio::spawn(async move { listener.run().await });
/// ```
pub struct EventStreamListener {
    pool: PgPool,
    tx: broadcast::Sender<EventEnvelope>,
}

impl EventStreamListener {
    pub fn new(pool: PgPool, tx: broadcast::Sender<EventEnvelope>) -> Self {
        Self { pool, tx }
    }

    /// Start listening for notifications. Runs until cancelled.
    /// This should be spawned as a background task.
    pub async fn run(&self) -> Result<(), InfraError> {
        use sqlx::postgres::PgListener;

        let mut listener = PgListener::connect_with(&self.pool)
            .await
            .map_err(|e| InfraError::Database(format!("LISTEN connect: {e}")))?;

        listener
            .listen("orch_events_channel")
            .await
            .map_err(|e| InfraError::Database(format!("LISTEN: {e}")))?;

        tracing::info!("EventStreamListener: listening on orch_events_channel");

        loop {
            match listener.recv().await {
                Ok(notification) => {
                    // Notification payload format: "instance_id:seq"
                    let payload = notification.payload();
                    if let Some((instance_id_str, seq_str)) = payload.split_once(':') {
                        match (Uuid::parse_str(instance_id_str), seq_str.parse::<i64>()) {
                            (Ok(instance_id), Ok(seq)) => {
                                // Replay this specific event and broadcast it
                                if let Err(e) =
                                    self.replay_and_broadcast(instance_id, seq - 1).await
                                {
                                    tracing::warn!(
                                        "EventStreamListener: replay failed: {e}"
                                    );
                                }
                            }
                            _ => {
                                tracing::warn!(
                                    "EventStreamListener: invalid notification payload: {payload}"
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("EventStreamListener: notification error: {e}");
                    // PgListener automatically reconnects, so we just log and continue
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
    }

    /// Replay events since a given seq and broadcast them.
    async fn replay_and_broadcast(
        &self,
        instance_id: Uuid,
        since_seq: i64,
    ) -> Result<(), InfraError> {
        let events = sqlx::query_as::<_, EventRow>(
            r#"
            SELECT event_id, instance_id, seq, event_type, event_version,
                   payload, idempotency_key, correlation_id, causation_id,
                   occurred_at, recorded_at
            FROM orch_events
            WHERE instance_id = $1 AND seq > $2
            ORDER BY seq ASC
            "#,
        )
        .bind(instance_id)
        .bind(since_seq)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| InfraError::Database(format!("replay: {e}")))?;

        for row in events {
            let envelope = EventEnvelope::from(row);
            // Best-effort broadcast -- ok to drop if no subscribers
            let _ = self.tx.send(envelope);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn notification_payload_format() {
        let instance_id =
            uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let seq = 42i64;
        let payload = format!("{instance_id}:{seq}");
        assert_eq!(payload, "550e8400-e29b-41d4-a716-446655440000:42");

        // Parse it back
        let (id_str, seq_str) = payload.split_once(':').unwrap();
        assert_eq!(uuid::Uuid::parse_str(id_str).unwrap(), instance_id);
        assert_eq!(seq_str.parse::<i64>().unwrap(), 42);
    }

    #[test]
    fn invalid_notification_payload_handled() {
        let bad_payloads = vec![
            "",
            "no-colon",
            "not-a-uuid:42",
            "550e8400-e29b-41d4-a716-446655440000:not-a-number",
        ];
        for payload in bad_payloads {
            if let Some((id_str, seq_str)) = payload.split_once(':') {
                // At least one side should fail to parse
                let uuid_ok = uuid::Uuid::parse_str(id_str).is_ok();
                let seq_ok = seq_str.parse::<i64>().is_ok();
                // Both being ok would mean the payload is actually valid
                if uuid_ok && seq_ok {
                    panic!("Expected invalid payload but got valid: {payload}");
                }
            }
            // No colon = split_once returns None, which is handled
        }
    }
}
