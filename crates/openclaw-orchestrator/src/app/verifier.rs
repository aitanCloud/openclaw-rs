use std::time::Duration;

use chrono::Utc;
use uuid::Uuid;

use crate::app::errors::AppError;
use crate::domain::events::EventEnvelope;
use crate::domain::verification::{CheckConfig, CheckResult, VerificationOutcome};

/// Default verification checks for Rust projects.
pub fn default_rust_checks() -> Vec<CheckConfig> {
    vec![
        CheckConfig {
            name: "cargo_check".to_string(),
            command: "cargo".to_string(),
            args: vec!["check".to_string()],
            timeout_secs: 120,
            required: true,
        },
        CheckConfig {
            name: "cargo_test".to_string(),
            command: "cargo".to_string(),
            args: vec!["test".to_string()],
            timeout_secs: 300,
            required: true,
        },
        CheckConfig {
            name: "cargo_fmt".to_string(),
            command: "cargo".to_string(),
            args: vec!["fmt".to_string(), "--check".to_string()],
            timeout_secs: 60,
            required: false, // advisory, not blocking
        },
        CheckConfig {
            name: "cargo_clippy".to_string(),
            command: "cargo".to_string(),
            args: vec![
                "clippy".to_string(),
                "--".to_string(),
                "-D".to_string(),
                "warnings".to_string(),
            ],
            timeout_secs: 120,
            required: false, // advisory
        },
    ]
}

/// The Verifier runs checks in a worktree and produces a VerificationOutcome.
pub struct Verifier;

impl Verifier {
    pub fn new() -> Self {
        Self
    }

    /// Run all checks in the given worktree directory.
    ///
    /// Returns a VerificationOutcome summarizing all results.
    /// Each check runs sequentially (to avoid resource contention).
    pub async fn run_checks(
        &self,
        worktree_path: &str,
        checks: &[CheckConfig],
    ) -> Result<VerificationOutcome, AppError> {
        let mut results = Vec::with_capacity(checks.len());
        let mut all_required_passed = true;

        for check in checks {
            let result = self.run_single_check(worktree_path, check).await;

            if check.required {
                match &result {
                    CheckResult::Passed => {}
                    _ => {
                        all_required_passed = false;
                    }
                }
            }

            results.push((check.name.clone(), result));
        }

        Ok(VerificationOutcome {
            results,
            all_passed: all_required_passed,
        })
    }

    /// Run a single check in the worktree.
    async fn run_single_check(&self, worktree_path: &str, check: &CheckConfig) -> CheckResult {
        let timeout = Duration::from_secs(check.timeout_secs);

        let cmd_result = tokio::time::timeout(timeout, async {
            tokio::process::Command::new(&check.command)
                .args(&check.args)
                .current_dir(worktree_path)
                .output()
                .await
        })
        .await;

        match cmd_result {
            Ok(Ok(output)) => {
                if output.status.success() {
                    CheckResult::Passed
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    // Truncate to 2000 chars to keep event payloads bounded
                    let reason = format!(
                        "exit code {}: {}{}",
                        output.status.code().unwrap_or(-1),
                        &stdout[..stdout.len().min(1000)],
                        &stderr[..stderr.len().min(1000)],
                    );
                    CheckResult::Failed { reason }
                }
            }
            Ok(Err(e)) => CheckResult::Error {
                message: format!("command execution failed: {e}"),
            },
            Err(_) => CheckResult::Failed {
                reason: format!("timed out after {}s", check.timeout_secs),
            },
        }
    }
}

impl Default for Verifier {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a VerifyResult (informational) event envelope for a single check.
pub fn verify_result_event(
    instance_id: Uuid,
    run_id: Uuid,
    task_id: Uuid,
    check_name: &str,
    result: &CheckResult,
) -> EventEnvelope {
    let now = Utc::now();
    EventEnvelope {
        event_id: Uuid::new_v4(),
        instance_id,
        seq: 0,
        event_type: "VerifyResult".to_string(),
        event_version: 1,
        payload: serde_json::json!({
            "run_id": run_id,
            "task_id": task_id,
            "check_name": check_name,
            "result": result,
        }),
        idempotency_key: Some(format!("verify-{run_id}-{check_name}")),
        correlation_id: None,
        causation_id: None,
        occurred_at: now,
        recorded_at: now,
    }
}

/// Build a TaskPassed or TaskFailed event based on verification outcome.
pub fn task_verdict_event(
    instance_id: Uuid,
    run_id: Uuid,
    task_id: Uuid,
    outcome: &VerificationOutcome,
) -> EventEnvelope {
    let now = Utc::now();
    let (event_type, payload) = if outcome.all_passed {
        (
            "TaskPassed".to_string(),
            serde_json::json!({
                "task_id": task_id,
                "run_id": run_id,
            }),
        )
    } else {
        let failed_checks: Vec<&str> = outcome
            .results
            .iter()
            .filter(|(_, r)| !matches!(r, CheckResult::Passed | CheckResult::Skipped { .. }))
            .map(|(name, _)| name.as_str())
            .collect();
        (
            "TaskFailed".to_string(),
            serde_json::json!({
                "task_id": task_id,
                "run_id": run_id,
                "failed_checks": failed_checks,
                "failure_category": "Transient",
            }),
        )
    };

    EventEnvelope {
        event_id: Uuid::new_v4(),
        instance_id,
        seq: 0,
        event_type,
        event_version: 1,
        payload,
        idempotency_key: Some(format!("task-verdict-{run_id}")),
        correlation_id: None,
        causation_id: None,
        occurred_at: now,
        recorded_at: now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_rust_checks_returns_four() {
        let checks = default_rust_checks();
        assert_eq!(checks.len(), 4);
    }

    #[test]
    fn default_rust_checks_first_two_required() {
        let checks = default_rust_checks();
        assert!(checks[0].required, "cargo_check should be required");
        assert!(checks[1].required, "cargo_test should be required");
    }

    #[test]
    fn default_rust_checks_last_two_optional() {
        let checks = default_rust_checks();
        assert!(!checks[2].required, "cargo_fmt should be optional");
        assert!(!checks[3].required, "cargo_clippy should be optional");
    }

    #[test]
    fn default_rust_checks_names() {
        let checks = default_rust_checks();
        assert_eq!(checks[0].name, "cargo_check");
        assert_eq!(checks[1].name, "cargo_test");
        assert_eq!(checks[2].name, "cargo_fmt");
        assert_eq!(checks[3].name, "cargo_clippy");
    }

    #[test]
    fn default_rust_checks_commands_are_cargo() {
        let checks = default_rust_checks();
        for check in &checks {
            assert_eq!(check.command, "cargo");
        }
    }

    #[test]
    fn default_rust_checks_timeouts_reasonable() {
        let checks = default_rust_checks();
        for check in &checks {
            assert!(
                check.timeout_secs >= 60,
                "{} timeout too low: {}",
                check.name,
                check.timeout_secs
            );
            assert!(
                check.timeout_secs <= 600,
                "{} timeout too high: {}",
                check.name,
                check.timeout_secs
            );
        }
    }

    #[test]
    fn verifier_is_constructible() {
        let _v = Verifier::new();
    }

    #[test]
    fn verifier_default_works() {
        let _v = Verifier::default();
    }

    #[tokio::test]
    async fn run_single_check_passing_command() {
        let verifier = Verifier::new();
        let check = CheckConfig {
            name: "true_check".to_string(),
            command: "true".to_string(),
            args: vec![],
            timeout_secs: 10,
            required: true,
        };
        let result = verifier.run_single_check("/tmp", &check).await;
        assert_eq!(result, CheckResult::Passed);
    }

    #[tokio::test]
    async fn run_single_check_failing_command() {
        let verifier = Verifier::new();
        let check = CheckConfig {
            name: "false_check".to_string(),
            command: "false".to_string(),
            args: vec![],
            timeout_secs: 10,
            required: true,
        };
        let result = verifier.run_single_check("/tmp", &check).await;
        assert!(
            matches!(result, CheckResult::Failed { .. }),
            "expected Failed, got {result:?}"
        );
    }

    #[tokio::test]
    async fn run_single_check_nonexistent_command() {
        let verifier = Verifier::new();
        let check = CheckConfig {
            name: "nonexistent".to_string(),
            command: "this-command-does-not-exist-9999".to_string(),
            args: vec![],
            timeout_secs: 10,
            required: true,
        };
        let result = verifier.run_single_check("/tmp", &check).await;
        assert!(
            matches!(result, CheckResult::Error { .. }),
            "expected Error, got {result:?}"
        );
    }

    #[tokio::test]
    async fn run_checks_aggregates_results() {
        let verifier = Verifier::new();
        let checks = vec![
            CheckConfig {
                name: "pass".to_string(),
                command: "true".to_string(),
                args: vec![],
                timeout_secs: 10,
                required: true,
            },
            CheckConfig {
                name: "fail".to_string(),
                command: "false".to_string(),
                args: vec![],
                timeout_secs: 10,
                required: true,
            },
        ];
        let outcome = verifier.run_checks("/tmp", &checks).await.expect("ok");
        assert_eq!(outcome.results.len(), 2);
        assert_eq!(outcome.results[0].0, "pass");
        assert_eq!(outcome.results[0].1, CheckResult::Passed);
        assert!(matches!(outcome.results[1].1, CheckResult::Failed { .. }));
        assert!(!outcome.all_passed, "should fail because required check failed");
    }

    #[tokio::test]
    async fn run_checks_passes_when_only_optional_fails() {
        let verifier = Verifier::new();
        let checks = vec![
            CheckConfig {
                name: "required_pass".to_string(),
                command: "true".to_string(),
                args: vec![],
                timeout_secs: 10,
                required: true,
            },
            CheckConfig {
                name: "optional_fail".to_string(),
                command: "false".to_string(),
                args: vec![],
                timeout_secs: 10,
                required: false,
            },
        ];
        let outcome = verifier.run_checks("/tmp", &checks).await.expect("ok");
        assert!(
            outcome.all_passed,
            "should pass because only optional check failed"
        );
    }

    #[tokio::test]
    async fn run_checks_empty_checks_list() {
        let verifier = Verifier::new();
        let outcome = verifier.run_checks("/tmp", &[]).await.expect("ok");
        assert!(outcome.results.is_empty());
        assert!(outcome.all_passed);
    }

    #[test]
    fn verify_result_event_creates_correct_type() {
        let instance_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();
        let result = CheckResult::Passed;

        let event = verify_result_event(instance_id, run_id, task_id, "cargo_check", &result);
        assert_eq!(event.event_type, "VerifyResult");
        assert_eq!(event.instance_id, instance_id);
        assert_eq!(event.event_version, 1);
    }

    #[test]
    fn verify_result_event_has_idempotency_key() {
        let run_id = Uuid::new_v4();
        let event = verify_result_event(
            Uuid::new_v4(),
            run_id,
            Uuid::new_v4(),
            "cargo_test",
            &CheckResult::Passed,
        );
        let key = event.idempotency_key.expect("should have key");
        assert!(key.starts_with("verify-"));
        assert!(key.contains(&run_id.to_string()));
        assert!(key.contains("cargo_test"));
    }

    #[test]
    fn verify_result_event_payload_structure() {
        let instance_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();
        let result = CheckResult::Failed {
            reason: "exit code 1".to_string(),
        };

        let event = verify_result_event(instance_id, run_id, task_id, "cargo_check", &result);
        let payload = &event.payload;
        assert_eq!(payload["run_id"], run_id.to_string());
        assert_eq!(payload["task_id"], task_id.to_string());
        assert_eq!(payload["check_name"], "cargo_check");
        assert!(payload["result"].is_object());
    }

    #[test]
    fn task_verdict_event_creates_task_passed_when_all_pass() {
        let instance_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();
        let outcome = VerificationOutcome {
            results: vec![
                ("cargo_check".to_string(), CheckResult::Passed),
                ("cargo_test".to_string(), CheckResult::Passed),
            ],
            all_passed: true,
        };

        let event = task_verdict_event(instance_id, run_id, task_id, &outcome);
        assert_eq!(event.event_type, "TaskPassed");
        assert_eq!(event.instance_id, instance_id);
        assert_eq!(event.payload["task_id"], task_id.to_string());
        assert_eq!(event.payload["run_id"], run_id.to_string());
    }

    #[test]
    fn task_verdict_event_creates_task_failed_when_check_fails() {
        let instance_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();
        let outcome = VerificationOutcome {
            results: vec![
                ("cargo_check".to_string(), CheckResult::Passed),
                (
                    "cargo_test".to_string(),
                    CheckResult::Failed {
                        reason: "tests failed".to_string(),
                    },
                ),
            ],
            all_passed: false,
        };

        let event = task_verdict_event(instance_id, run_id, task_id, &outcome);
        assert_eq!(event.event_type, "TaskFailed");
        assert_eq!(event.payload["failure_category"], "Transient");
    }

    #[test]
    fn task_verdict_event_includes_failed_checks_list() {
        let outcome = VerificationOutcome {
            results: vec![
                ("cargo_check".to_string(), CheckResult::Passed),
                (
                    "cargo_test".to_string(),
                    CheckResult::Failed {
                        reason: "test failure".to_string(),
                    },
                ),
                (
                    "cargo_fmt".to_string(),
                    CheckResult::Failed {
                        reason: "formatting".to_string(),
                    },
                ),
            ],
            all_passed: false,
        };

        let event = task_verdict_event(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), &outcome);
        let failed = event.payload["failed_checks"]
            .as_array()
            .expect("should be array");
        assert_eq!(failed.len(), 2);
        assert!(failed.contains(&serde_json::json!("cargo_test")));
        assert!(failed.contains(&serde_json::json!("cargo_fmt")));
    }

    #[test]
    fn task_verdict_event_has_idempotency_key() {
        let run_id = Uuid::new_v4();
        let outcome = VerificationOutcome {
            results: vec![("test".to_string(), CheckResult::Passed)],
            all_passed: true,
        };

        let event = task_verdict_event(Uuid::new_v4(), run_id, Uuid::new_v4(), &outcome);
        let key = event.idempotency_key.expect("should have key");
        assert!(key.starts_with("task-verdict-"));
        assert!(key.contains(&run_id.to_string()));
    }

    #[test]
    fn task_verdict_event_version_is_one() {
        let outcome = VerificationOutcome {
            results: vec![],
            all_passed: true,
        };
        let event = task_verdict_event(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), &outcome);
        assert_eq!(event.event_version, 1);
    }

    #[test]
    fn task_verdict_event_skipped_not_in_failed_list() {
        let outcome = VerificationOutcome {
            results: vec![
                ("cargo_check".to_string(), CheckResult::Passed),
                (
                    "cargo_fmt".to_string(),
                    CheckResult::Skipped {
                        reason: "not applicable".to_string(),
                    },
                ),
            ],
            all_passed: false,
        };

        let event = task_verdict_event(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), &outcome);
        let failed = event.payload["failed_checks"]
            .as_array()
            .expect("should be array");
        // Skipped is excluded from failed_checks per the filter logic
        assert!(
            !failed.contains(&serde_json::json!("cargo_fmt")),
            "Skipped checks should not appear in failed_checks"
        );
    }
}
