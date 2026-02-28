use uuid::Uuid;

use super::errors::DomainError;
use super::events::EventEnvelope;
use super::worker::{WorkerError, WorkerHandle, WorkerSpawnConfig};

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

/// Domain interface for worker management. No process types, no PIDs.
///
/// This is a port (hexagonal architecture) — the domain defines the interface,
/// the infrastructure layer provides the implementation.
#[async_trait::async_trait]
pub trait WorkerManager: Send + Sync {
    /// Spawn a worker for a claimed run. Returns handle or error.
    /// MUST be called AFTER RunClaimed event is committed (post-commit side effect).
    async fn spawn(&self, config: WorkerSpawnConfig) -> Result<WorkerHandle, WorkerError>;

    /// Check if a worker is still alive (by logical session id).
    async fn is_alive(&self, session_id: Uuid) -> Result<bool, WorkerError>;

    /// Send cancellation signal to a worker.
    async fn cancel(&self, session_id: Uuid) -> Result<(), WorkerError>;

    /// Kill a worker forcefully (timeout exceeded).
    async fn kill(&self, session_id: Uuid) -> Result<(), WorkerError>;

    /// Attempt to reattach to an orphaned worker after restart.
    /// Returns None if the worker is no longer alive.
    async fn reattach(
        &self,
        session_id: Uuid,
        run_id: Uuid,
    ) -> Result<Option<WorkerHandle>, WorkerError>;
}
