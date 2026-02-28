/// Infrastructure errors -- DB, IO, serialization, lock conflicts.
/// NOT serializable. Does NOT leave the infra layer.
#[derive(Debug)]
pub enum InfraError {
    Database(String),
    Io(String),
    Serialization(String),
    LockConflict(String),
}

impl std::fmt::Display for InfraError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(msg) => write!(f, "database error: {msg}"),
            Self::Io(msg) => write!(f, "IO error: {msg}"),
            Self::Serialization(msg) => write!(f, "serialization error: {msg}"),
            Self::LockConflict(msg) => write!(f, "lock conflict: {msg}"),
        }
    }
}

impl std::error::Error for InfraError {}

impl From<sqlx::Error> for InfraError {
    fn from(err: sqlx::Error) -> Self {
        Self::Database(err.to_string())
    }
}

impl From<serde_json::Error> for InfraError {
    fn from(err: serde_json::Error) -> Self {
        Self::Serialization(err.to_string())
    }
}

impl From<std::io::Error> for InfraError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_database_error() {
        let err = InfraError::Database("connection refused".to_string());
        assert_eq!(err.to_string(), "database error: connection refused");
    }

    #[test]
    fn display_io_error() {
        let err = InfraError::Io("file not found".to_string());
        assert_eq!(err.to_string(), "IO error: file not found");
    }

    #[test]
    fn display_serialization_error() {
        let err = InfraError::Serialization("invalid JSON".to_string());
        assert_eq!(err.to_string(), "serialization error: invalid JSON");
    }

    #[test]
    fn display_lock_conflict() {
        let err = InfraError::LockConflict("instance locked".to_string());
        assert_eq!(err.to_string(), "lock conflict: instance locked");
    }

    #[test]
    fn from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let infra_err = InfraError::from(io_err);
        assert!(matches!(infra_err, InfraError::Io(_)));
        assert!(infra_err.to_string().contains("missing"));
    }

    #[test]
    fn from_serde_json_error() {
        let json_err = serde_json::from_str::<serde_json::Value>("{{bad}}")
            .expect_err("should fail");
        let infra_err = InfraError::from(json_err);
        assert!(matches!(infra_err, InfraError::Serialization(_)));
    }

    #[test]
    fn error_trait_implemented() {
        let err: Box<dyn std::error::Error> =
            Box::new(InfraError::Database("test".to_string()));
        assert!(err.to_string().contains("database error"));
    }
}
