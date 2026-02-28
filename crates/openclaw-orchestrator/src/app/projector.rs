use crate::app::errors::AppError;
use crate::domain::events::EventEnvelope;

/// Projector processes events and updates projection tables.
/// Each projector handles a subset of event types.
///
/// Contract (from Architecture Section 31.6):
/// - Handler MUST be idempotent: processing the same event_id twice = no-op
/// - Idempotence is by event_id, NOT by payload content
/// - For StateChanging events: projector MUST have a handler
/// - For Informational events: handler is optional (missing = silently skip)
#[async_trait::async_trait]
pub trait Projector: Send + Sync {
    /// Name of this projector (for logging/metrics).
    fn name(&self) -> &'static str;

    /// Which event types this projector handles.
    fn handles(&self) -> &[&'static str];

    /// Handle a single event. Must be idempotent by event_id.
    async fn handle(&self, event: &EventEnvelope, pool: &sqlx::PgPool) -> Result<(), AppError>;
}
