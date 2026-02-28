/// Domain errors for the OpenClaw orchestrator.
///
/// These are pure domain errors — no framework types, no HTTP status codes.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    InvalidTransition {
        entity: String,
        from: String,
        trigger: String,
    },
    InvalidState(String),
    NotFound {
        entity: String,
        id: String,
    },
    AlreadyExists {
        entity: String,
        id: String,
    },
    BudgetExceeded {
        available_cents: i64,
        requested_cents: i64,
    },
    Precondition(String),
}

impl std::fmt::Display for DomainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTransition {
                entity,
                from,
                trigger,
            } => {
                write!(
                    f,
                    "invalid transition: {entity} cannot go from {from} via {trigger}"
                )
            }
            Self::InvalidState(msg) => write!(f, "invalid state: {msg}"),
            Self::NotFound { entity, id } => write!(f, "{entity} not found: {id}"),
            Self::AlreadyExists { entity, id } => write!(f, "{entity} already exists: {id}"),
            Self::BudgetExceeded {
                available_cents,
                requested_cents,
            } => {
                write!(
                    f,
                    "budget exceeded: available={available_cents} requested={requested_cents}"
                )
            }
            Self::Precondition(msg) => write!(f, "precondition failed: {msg}"),
        }
    }
}

impl std::error::Error for DomainError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_invalid_transition() {
        let err = DomainError::InvalidTransition {
            entity: "CycleState".to_string(),
            from: "Created".to_string(),
            trigger: "Complete".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "invalid transition: CycleState cannot go from Created via Complete"
        );
    }

    #[test]
    fn display_invalid_state() {
        let err = DomainError::InvalidState("bad state".to_string());
        assert_eq!(err.to_string(), "invalid state: bad state");
    }

    #[test]
    fn display_not_found() {
        let err = DomainError::NotFound {
            entity: "Cycle".to_string(),
            id: "abc-123".to_string(),
        };
        assert_eq!(err.to_string(), "Cycle not found: abc-123");
    }

    #[test]
    fn display_already_exists() {
        let err = DomainError::AlreadyExists {
            entity: "Instance".to_string(),
            id: "def-456".to_string(),
        };
        assert_eq!(err.to_string(), "Instance already exists: def-456");
    }

    #[test]
    fn display_budget_exceeded() {
        let err = DomainError::BudgetExceeded {
            available_cents: 100,
            requested_cents: 200,
        };
        assert_eq!(
            err.to_string(),
            "budget exceeded: available=100 requested=200"
        );
    }

    #[test]
    fn display_precondition() {
        let err = DomainError::Precondition("must be active".to_string());
        assert_eq!(err.to_string(), "precondition failed: must be active");
    }

    #[test]
    fn error_trait_implemented() {
        let err: Box<dyn std::error::Error> = Box::new(DomainError::InvalidState("x".into()));
        assert!(err.to_string().contains("invalid state"));
    }

    #[test]
    fn clone_and_eq() {
        let err = DomainError::NotFound {
            entity: "Run".to_string(),
            id: "123".to_string(),
        };
        let cloned = err.clone();
        assert_eq!(err, cloned);
    }
}
