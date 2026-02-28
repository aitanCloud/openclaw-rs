use serde::{Deserialize, Serialize};

/// Result of a single verification check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckResult {
    Passed,
    Failed { reason: String },
    Skipped { reason: String },
    Error { message: String },
}

/// Configuration for a single verification check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub timeout_secs: u64,
    pub required: bool,
}

/// Outcome of running all verification checks.
#[derive(Debug, Clone)]
pub struct VerificationOutcome {
    pub results: Vec<(String, CheckResult)>,
    pub all_passed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_result_serde_passed() {
        let result = CheckResult::Passed;
        let json = serde_json::to_string(&result).expect("serialize");
        let restored: CheckResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(result, restored);
    }

    #[test]
    fn check_result_serde_failed() {
        let result = CheckResult::Failed {
            reason: "exit code 1: error[E0308]".to_string(),
        };
        let json = serde_json::to_string(&result).expect("serialize");
        let restored: CheckResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(result, restored);
    }

    #[test]
    fn check_result_serde_skipped() {
        let result = CheckResult::Skipped {
            reason: "not applicable".to_string(),
        };
        let json = serde_json::to_string(&result).expect("serialize");
        let restored: CheckResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(result, restored);
    }

    #[test]
    fn check_result_serde_error() {
        let result = CheckResult::Error {
            message: "command not found".to_string(),
        };
        let json = serde_json::to_string(&result).expect("serialize");
        let restored: CheckResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(result, restored);
    }

    #[test]
    fn check_config_serde_round_trip() {
        let config = CheckConfig {
            name: "cargo_check".to_string(),
            command: "cargo".to_string(),
            args: vec!["check".to_string()],
            timeout_secs: 120,
            required: true,
        };
        let json = serde_json::to_string(&config).expect("serialize");
        let restored: CheckConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(config.name, restored.name);
        assert_eq!(config.command, restored.command);
        assert_eq!(config.args, restored.args);
        assert_eq!(config.timeout_secs, restored.timeout_secs);
        assert_eq!(config.required, restored.required);
    }

    #[test]
    fn verification_outcome_all_passed_when_all_required_pass() {
        let outcome = VerificationOutcome {
            results: vec![
                ("cargo_check".to_string(), CheckResult::Passed),
                ("cargo_test".to_string(), CheckResult::Passed),
            ],
            all_passed: true,
        };
        assert!(outcome.all_passed);
    }

    #[test]
    fn verification_outcome_not_passed_when_required_check_fails() {
        let outcome = VerificationOutcome {
            results: vec![
                ("cargo_check".to_string(), CheckResult::Passed),
                (
                    "cargo_test".to_string(),
                    CheckResult::Failed {
                        reason: "test failure".to_string(),
                    },
                ),
            ],
            all_passed: false,
        };
        assert!(!outcome.all_passed);
    }

    #[test]
    fn verification_outcome_passed_when_only_optional_checks_fail() {
        // Simulate: required checks passed, optional check failed.
        // The `all_passed` field reflects only required checks.
        let outcome = VerificationOutcome {
            results: vec![
                ("cargo_check".to_string(), CheckResult::Passed),
                ("cargo_test".to_string(), CheckResult::Passed),
                (
                    "cargo_fmt".to_string(),
                    CheckResult::Failed {
                        reason: "formatting issues".to_string(),
                    },
                ),
            ],
            all_passed: true, // only required checks matter
        };
        assert!(outcome.all_passed);
    }

    #[test]
    fn check_result_debug() {
        let result = CheckResult::Passed;
        let debug = format!("{result:?}");
        assert!(debug.contains("Passed"));
    }

    #[test]
    fn check_result_clone() {
        let result = CheckResult::Failed {
            reason: "test".to_string(),
        };
        let cloned = result.clone();
        assert_eq!(result, cloned);
    }

    #[test]
    fn check_config_clone() {
        let config = CheckConfig {
            name: "test".to_string(),
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
            timeout_secs: 30,
            required: false,
        };
        let cloned = config.clone();
        assert_eq!(config.name, cloned.name);
        assert_eq!(config.command, cloned.command);
        assert_eq!(config.args, cloned.args);
        assert_eq!(config.timeout_secs, cloned.timeout_secs);
        assert_eq!(config.required, cloned.required);
    }

    #[test]
    fn check_config_debug() {
        let config = CheckConfig {
            name: "cargo_check".to_string(),
            command: "cargo".to_string(),
            args: vec!["check".to_string()],
            timeout_secs: 120,
            required: true,
        };
        let debug = format!("{config:?}");
        assert!(debug.contains("CheckConfig"));
        assert!(debug.contains("cargo_check"));
    }

    #[test]
    fn verification_outcome_debug() {
        let outcome = VerificationOutcome {
            results: vec![("test".to_string(), CheckResult::Passed)],
            all_passed: true,
        };
        let debug = format!("{outcome:?}");
        assert!(debug.contains("VerificationOutcome"));
        assert!(debug.contains("all_passed"));
    }

    #[test]
    fn verification_outcome_clone() {
        let outcome = VerificationOutcome {
            results: vec![("test".to_string(), CheckResult::Passed)],
            all_passed: true,
        };
        let cloned = outcome.clone();
        assert_eq!(outcome.results.len(), cloned.results.len());
        assert_eq!(outcome.all_passed, cloned.all_passed);
    }

    #[test]
    fn check_result_eq() {
        assert_eq!(CheckResult::Passed, CheckResult::Passed);
        assert_ne!(
            CheckResult::Passed,
            CheckResult::Failed {
                reason: "x".to_string()
            }
        );
        assert_ne!(
            CheckResult::Failed {
                reason: "a".to_string()
            },
            CheckResult::Failed {
                reason: "b".to_string()
            }
        );
    }

    #[test]
    fn check_config_serde_with_empty_args() {
        let config = CheckConfig {
            name: "simple".to_string(),
            command: "true".to_string(),
            args: vec![],
            timeout_secs: 10,
            required: false,
        };
        let json = serde_json::to_string(&config).expect("serialize");
        let restored: CheckConfig = serde_json::from_str(&json).expect("deserialize");
        assert!(restored.args.is_empty());
    }

    #[test]
    fn verification_outcome_empty_results() {
        let outcome = VerificationOutcome {
            results: vec![],
            all_passed: true,
        };
        assert!(outcome.results.is_empty());
        assert!(outcome.all_passed);
    }
}
