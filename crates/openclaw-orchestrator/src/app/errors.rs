use crate::domain::errors::DomainError;
use crate::infra::errors::InfraError;

/// Application-layer error type.
///
/// Composes DomainError + InfraError per the architecture (E2).
/// Returned by projectors and other application-layer operations.
#[derive(Debug)]
pub enum AppError {
    Domain(DomainError),
    Infra(InfraError),
    ConcurrencyConflict(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Domain(e) => write!(f, "domain: {e}"),
            Self::Infra(e) => write!(f, "infra: {e}"),
            Self::ConcurrencyConflict(msg) => write!(f, "concurrency conflict: {msg}"),
        }
    }
}

impl std::error::Error for AppError {}

impl From<DomainError> for AppError {
    fn from(e: DomainError) -> Self {
        Self::Domain(e)
    }
}

impl From<InfraError> for AppError {
    fn from(e: InfraError) -> Self {
        Self::Infra(e)
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        Self::Infra(InfraError::Serialization(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_domain_error() {
        let err = AppError::Domain(DomainError::Precondition("test".to_string()));
        assert_eq!(err.to_string(), "domain: precondition failed: test");
    }

    #[test]
    fn display_infra_error() {
        let err = AppError::Infra(InfraError::Database("conn refused".to_string()));
        assert_eq!(err.to_string(), "infra: database error: conn refused");
    }

    #[test]
    fn display_concurrency_conflict() {
        let err = AppError::ConcurrencyConflict("stale version".to_string());
        assert_eq!(err.to_string(), "concurrency conflict: stale version");
    }

    #[test]
    fn from_domain_error() {
        let domain_err = DomainError::InvalidState("bad".to_string());
        let app_err: AppError = domain_err.into();
        assert!(matches!(app_err, AppError::Domain(_)));
    }

    #[test]
    fn from_infra_error() {
        let infra_err = InfraError::Io("disk full".to_string());
        let app_err: AppError = infra_err.into();
        assert!(matches!(app_err, AppError::Infra(_)));
    }

    #[test]
    fn from_serde_json_error() {
        let json_err = serde_json::from_str::<serde_json::Value>("{{bad}}")
            .expect_err("should fail");
        let app_err: AppError = json_err.into();
        assert!(matches!(app_err, AppError::Infra(InfraError::Serialization(_))));
    }

    #[test]
    fn error_trait_implemented() {
        let err: Box<dyn std::error::Error> =
            Box::new(AppError::ConcurrencyConflict("test".to_string()));
        assert!(err.to_string().contains("concurrency conflict"));
    }
}
