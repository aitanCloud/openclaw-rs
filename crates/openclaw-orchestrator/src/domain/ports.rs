use uuid::Uuid;

use super::errors::DomainError;
use super::events::EventEnvelope;

/// Event store port — domain-facing, knows nothing about Postgres.
#[async_trait::async_trait]
pub trait EventStore: Send + Sync {
    /// Emit an event, return its sequence number.
    async fn emit(&self, event: EventEnvelope) -> Result<i64, DomainError>;

    /// Replay events for an instance since a given sequence number.
    async fn replay(
        &self,
        instance_id: Uuid,
        since_seq: i64,
    ) -> Result<Vec<EventEnvelope>, DomainError>;
}
