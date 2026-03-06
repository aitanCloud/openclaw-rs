use sqlx::PgPool;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::domain::errors::DomainError;
use crate::domain::events::EventEnvelope;
use crate::domain::ports::EventStore;

use super::errors::InfraError;

/// Postgres-backed event store implementing the domain `EventStore` port.
///
/// Events are persisted to the `orch_events` table and broadcast to
/// in-process subscribers via a `tokio::sync::broadcast` channel.
pub struct PgEventStore {
    pool: PgPool,
    tx: broadcast::Sender<EventEnvelope>,
}

impl PgEventStore {
    pub fn new(pool: PgPool) -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self { pool, tx }
    }

    /// Subscribe to new events (in-process).
    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.tx.subscribe()
    }

    /// Return a clone of the broadcast sender.
    ///
    /// Used by the UI crate to store the sender in `AppState` so that
    /// WebSocket handlers can call `sender.subscribe()` without needing
    /// the concrete `PgEventStore` type.
    pub fn sender(&self) -> broadcast::Sender<EventEnvelope> {
        self.tx.clone()
    }

    /// Get the next sequence number for an instance.
    async fn next_seq(&self, instance_id: Uuid) -> Result<i64, InfraError> {
        let row = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT MAX(seq) FROM orch_events WHERE instance_id = $1",
        )
        .bind(instance_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.unwrap_or(0) + 1)
    }
}

#[async_trait::async_trait]
impl EventStore for PgEventStore {
    async fn emit(&self, mut event: EventEnvelope) -> Result<i64, DomainError> {
        // Get next seq for this instance
        let seq = self
            .next_seq(event.instance_id)
            .await
            .map_err(|e| DomainError::Precondition(e.to_string()))?;
        event.seq = seq;

        // Insert event. If idempotency_key conflicts, skip insert (DO NOTHING)
        // and fall back to querying the existing seq. We avoid DO UPDATE because
        // the append-only trigger on orch_events blocks all UPDATEs.
        let inserted = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO orch_events (
                event_id, instance_id, seq, event_type, event_version,
                payload, idempotency_key, correlation_id, causation_id,
                occurred_at, recorded_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now())
            ON CONFLICT (instance_id, idempotency_key)
                WHERE idempotency_key IS NOT NULL
                DO NOTHING
            RETURNING seq
            "#,
        )
        .bind(event.event_id)
        .bind(event.instance_id)
        .bind(seq)
        .bind(&event.event_type)
        .bind(event.event_version as i16)
        .bind(&event.payload)
        .bind(&event.idempotency_key)
        .bind(event.correlation_id)
        .bind(event.causation_id)
        .bind(event.occurred_at)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| DomainError::Precondition(format!("event store: {e}")))?;

        let result = match inserted {
            Some(seq) => seq,
            None => {
                // Idempotency conflict — fetch existing seq
                sqlx::query_scalar::<_, i64>(
                    "SELECT seq FROM orch_events WHERE instance_id = $1 AND idempotency_key = $2",
                )
                .bind(event.instance_id)
                .bind(&event.idempotency_key)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| {
                    DomainError::Precondition(format!("event store idempotency lookup: {e}"))
                })?
            }
        };

        // Update event with actual seq (may differ if idempotency conflict)
        event.seq = result;

        // Notify via Postgres LISTEN/NOTIFY for cross-process subscribers
        let notify_payload = format!("{}:{}", event.instance_id, result);
        sqlx::query("SELECT pg_notify('orch_events_channel', $1)")
            .bind(&notify_payload)
            .execute(&self.pool)
            .await
            .ok(); // Best-effort -- don't fail emit if notify fails

        // Broadcast to in-process subscribers (best-effort, ok to drop)
        let _ = self.tx.send(event);

        Ok(result)
    }

    async fn replay(
        &self,
        instance_id: Uuid,
        since_seq: i64,
    ) -> Result<Vec<EventEnvelope>, DomainError> {
        let rows = sqlx::query_as::<_, EventRow>(
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
        .map_err(|e| DomainError::Precondition(format!("event store replay: {e}")))?;

        Ok(rows.into_iter().map(EventEnvelope::from).collect())
    }

    async fn head_seq(&self, instance_id: Uuid) -> Result<i64, DomainError> {
        let seq = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT MAX(seq) FROM orch_events WHERE instance_id = $1",
        )
        .bind(instance_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| DomainError::Precondition(format!("head_seq query: {e}")))?;
        Ok(seq.unwrap_or(0))
    }
}

/// Row type for sqlx mapping from the `orch_events` table.
#[derive(sqlx::FromRow)]
pub(crate) struct EventRow {
    pub(crate) event_id: Uuid,
    pub(crate) instance_id: Uuid,
    pub(crate) seq: i64,
    pub(crate) event_type: String,
    pub(crate) event_version: i16,
    pub(crate) payload: serde_json::Value,
    pub(crate) idempotency_key: Option<String>,
    pub(crate) correlation_id: Option<Uuid>,
    pub(crate) causation_id: Option<Uuid>,
    pub(crate) occurred_at: chrono::DateTime<chrono::Utc>,
    pub(crate) recorded_at: chrono::DateTime<chrono::Utc>,
}

impl From<EventRow> for EventEnvelope {
    fn from(row: EventRow) -> Self {
        EventEnvelope {
            event_id: row.event_id,
            instance_id: row.instance_id,
            seq: row.seq,
            event_type: row.event_type,
            event_version: row.event_version as u16,
            payload: row.payload,
            idempotency_key: row.idempotency_key,
            correlation_id: row.correlation_id,
            causation_id: row.causation_id,
            occurred_at: row.occurred_at,
            recorded_at: row.recorded_at,
        }
    }
}

/// Canonical advisory lock key derivation -- full 128 bits, no truncation.
/// Byte order: BIG-ENDIAN. UUID bytes[0..8] -> hi, bytes[8..16] -> lo.
/// All implementations MUST use big-endian to avoid cross-platform lock mismatch.
pub fn advisory_lock_key(uuid: Uuid) -> (i64, i64) {
    let bytes = uuid.as_bytes();
    let hi = i64::from_be_bytes(bytes[0..8].try_into().unwrap());
    let lo = i64::from_be_bytes(bytes[8..16].try_into().unwrap());
    (hi, lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advisory_lock_key_deterministic() {
        let uuid = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let (hi, lo) = advisory_lock_key(uuid);
        // Same UUID always produces same keys
        let (hi2, lo2) = advisory_lock_key(uuid);
        assert_eq!(hi, hi2);
        assert_eq!(lo, lo2);
    }

    #[test]
    fn advisory_lock_key_different_uuids() {
        let uuid1 = Uuid::new_v4();
        let uuid2 = Uuid::new_v4();
        let (hi1, lo1) = advisory_lock_key(uuid1);
        let (hi2, lo2) = advisory_lock_key(uuid2);
        // Different UUIDs should (almost certainly) produce different keys
        assert!(hi1 != hi2 || lo1 != lo2);
    }

    #[test]
    fn advisory_lock_key_big_endian() {
        // Known UUID with predictable bytes
        let uuid = Uuid::from_bytes([
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, // hi = 1
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, // lo = 2
        ]);
        let (hi, lo) = advisory_lock_key(uuid);
        assert_eq!(hi, 1);
        assert_eq!(lo, 2);
    }

    #[test]
    fn event_row_to_envelope_conversion() {
        let now = chrono::Utc::now();
        let event_id = Uuid::new_v4();
        let instance_id = Uuid::new_v4();
        let correlation_id = Uuid::new_v4();

        let row = EventRow {
            event_id,
            instance_id,
            seq: 42,
            event_type: "CycleCreated".to_string(),
            event_version: 1,
            payload: serde_json::json!({"prompt": "test"}),
            idempotency_key: Some("idem-1".to_string()),
            correlation_id: Some(correlation_id),
            causation_id: None,
            occurred_at: now,
            recorded_at: now,
        };

        let envelope = EventEnvelope::from(row);
        assert_eq!(envelope.event_id, event_id);
        assert_eq!(envelope.instance_id, instance_id);
        assert_eq!(envelope.seq, 42);
        assert_eq!(envelope.event_type, "CycleCreated");
        assert_eq!(envelope.event_version, 1);
        assert_eq!(envelope.idempotency_key, Some("idem-1".to_string()));
        assert_eq!(envelope.correlation_id, Some(correlation_id));
        assert_eq!(envelope.causation_id, None);
        assert_eq!(envelope.payload, serde_json::json!({"prompt": "test"}));
    }

    #[test]
    fn event_row_version_cast_round_trip() {
        // i16 max is 32767, which is well within u16 range for version numbers
        let now = chrono::Utc::now();
        let row = EventRow {
            event_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            seq: 1,
            event_type: "TestEvent".to_string(),
            event_version: 255,
            payload: serde_json::json!({}),
            idempotency_key: None,
            correlation_id: None,
            causation_id: None,
            occurred_at: now,
            recorded_at: now,
        };
        let envelope = EventEnvelope::from(row);
        assert_eq!(envelope.event_version, 255);
    }
}
