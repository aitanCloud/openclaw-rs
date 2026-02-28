/// State enums and validated transition logic for the OpenClaw orchestrator.
///
/// Each entity has a state enum, a trigger enum, and a `transition()` method
/// that enforces the canonical state machine. Invalid transitions return
/// `DomainError::InvalidTransition`.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::errors::DomainError;

// ── Helpers ──

fn invalid_transition(entity: &str, from: &str, trigger: &str) -> DomainError {
    DomainError::InvalidTransition {
        entity: entity.to_string(),
        from: from.to_string(),
        trigger: trigger.to_string(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// InstanceState
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceState {
    Provisioning,
    Active,
    Blocked(String),
    Suspended,
    ProvisioningFailed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceTrigger {
    Provisioned,
    ProvisioningFailed(String),
    Block(String),
    Suspend,
    Unblock,
    Resume,
}

impl InstanceState {
    pub fn transition(self, trigger: InstanceTrigger) -> Result<InstanceState, DomainError> {
        use InstanceState::*;
        use InstanceTrigger as T;

        let from_label = format!("{:?}", &self);
        let trigger_label = format!("{:?}", &trigger);

        match (self, trigger) {
            (Provisioning, T::Provisioned) => Ok(Active),
            (Provisioning, T::ProvisioningFailed(reason)) => Ok(ProvisioningFailed(reason)),
            (Active, T::Block(reason)) => Ok(Blocked(reason)),
            (Active, T::Suspend) => Ok(Suspended),
            (Blocked(_), T::Unblock) => Ok(Active),
            (Suspended, T::Resume) => Ok(Active),
            _ => Err(invalid_transition("InstanceState", &from_label, &trigger_label)),
        }
    }

    /// Returns true if this state is terminal (no further transitions possible).
    pub fn is_terminal(&self) -> bool {
        matches!(self, InstanceState::ProvisioningFailed(_))
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// CycleState
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CycleState {
    Created,
    Planning,
    PlanReady,
    Approved,
    Running,
    Blocked(String),
    Completing,
    Completed,
    Failed(String),
    Cancelled(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CycleTrigger {
    PlanRequested,
    PlanGenerated,
    PlanApproved,
    Cancel(String),
    Start,
    Block(String),
    Unblock,
    BeginCompleting,
    Complete,
    Fail(String),
}

impl CycleState {
    pub fn transition(self, trigger: CycleTrigger) -> Result<CycleState, DomainError> {
        use CycleState::*;
        use CycleTrigger as T;

        let from_label = format!("{:?}", &self);
        let trigger_label = format!("{:?}", &trigger);

        match (self, trigger) {
            (Created, T::PlanRequested) => Ok(Planning),
            (Planning, T::PlanGenerated) => Ok(PlanReady),
            (Planning, T::Fail(reason)) => Ok(Failed(reason)),
            (PlanReady, T::PlanApproved) => Ok(Approved),
            (PlanReady, T::Cancel(reason)) => Ok(Cancelled(reason)),
            (Approved, T::Start) => Ok(Running),
            (Running, T::Block(reason)) => Ok(Blocked(reason)),
            (Running, T::BeginCompleting) => Ok(Completing),
            (Running, T::Fail(reason)) => Ok(Failed(reason)),
            (Running, T::Cancel(reason)) => Ok(Cancelled(reason)),
            (Blocked(_), T::Unblock) => Ok(Running),
            (Completing, T::Complete) => Ok(Completed),
            (Completing, T::Fail(reason)) => Ok(Failed(reason)),
            _ => Err(invalid_transition("CycleState", &from_label, &trigger_label)),
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            CycleState::Completed | CycleState::Failed(_) | CycleState::Cancelled(_)
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// TaskState
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskState {
    Scheduled,
    Active { active_run_id: Uuid, attempt: u32 },
    Verifying { run_id: Uuid },
    Passed { evidence_run_id: Uuid },
    Failed { reason: String, total_attempts: u32 },
    Cancelled(String),
    Skipped(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskTrigger {
    Claim { run_id: Uuid, attempt: u32 },
    RunCompleted { run_id: Uuid },
    RetryScheduled,
    Pass { evidence_run_id: Uuid },
    Fail { reason: String, total_attempts: u32 },
    Cancel(String),
    Skip(String),
}

impl TaskState {
    pub fn transition(self, trigger: TaskTrigger) -> Result<TaskState, DomainError> {
        use TaskState::*;
        use TaskTrigger as T;

        let from_label = format!("{:?}", &self);
        let trigger_label = format!("{:?}", &trigger);

        match (self, trigger) {
            (Scheduled, T::Claim { run_id, attempt }) => Ok(Active {
                active_run_id: run_id,
                attempt,
            }),
            (Active { .. }, T::RunCompleted { run_id }) => Ok(Verifying { run_id }),
            (Active { .. }, T::RetryScheduled) => Ok(Scheduled),
            (Active { .. }, T::Fail {
                reason,
                total_attempts,
            }) => Ok(Failed {
                reason,
                total_attempts,
            }),
            (Active { .. }, T::Cancel(reason)) => Ok(Cancelled(reason)),
            (Verifying { .. }, T::Pass { evidence_run_id }) => Ok(Passed { evidence_run_id }),
            (Verifying { .. }, T::RetryScheduled) => Ok(Scheduled),
            (Verifying { .. }, T::Fail {
                reason,
                total_attempts,
            }) => Ok(Failed {
                reason,
                total_attempts,
            }),
            (Scheduled, T::Cancel(reason)) => Ok(Cancelled(reason)),
            (Scheduled, T::Skip(reason)) => Ok(Skipped(reason)),
            _ => Err(invalid_transition("TaskState", &from_label, &trigger_label)),
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TaskState::Passed { .. }
                | TaskState::Failed { .. }
                | TaskState::Cancelled(_)
                | TaskState::Skipped(_)
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// RunState
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureCategory {
    Transient,
    Permanent,
    ResourceExhausted,
    Timeout,
    SecurityViolation,
    SpawnFailure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunState {
    Claimed,
    Running,
    Completed { exit_code: i32 },
    Failed(FailureCategory),
    TimedOut,
    Cancelled(String),
    Abandoned(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunTrigger {
    Start,
    Complete { exit_code: i32 },
    Fail(FailureCategory),
    TimeOut,
    Cancel(String),
    Abandon(String),
}

impl RunState {
    pub fn transition(self, trigger: RunTrigger) -> Result<RunState, DomainError> {
        use RunState::*;
        use RunTrigger as T;

        let from_label = format!("{:?}", &self);
        let trigger_label = format!("{:?}", &trigger);

        match (self, trigger) {
            (Claimed, T::Start) => Ok(Running),
            (Claimed, T::Fail(cat)) => Ok(Failed(cat)),
            (Claimed, T::Cancel(reason)) => Ok(Cancelled(reason)),
            (Running, T::Complete { exit_code }) => Ok(Completed { exit_code }),
            (Running, T::Fail(cat)) => Ok(Failed(cat)),
            (Running, T::TimeOut) => Ok(TimedOut),
            (Running, T::Cancel(reason)) => Ok(Cancelled(reason)),
            (Running, T::Abandon(reason)) => Ok(Abandoned(reason)),
            _ => Err(invalid_transition("RunState", &from_label, &trigger_label)),
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            RunState::Completed { .. }
                | RunState::Failed(_)
                | RunState::TimedOut
                | RunState::Cancelled(_)
                | RunState::Abandoned(_)
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// MergeState
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeState {
    Pending,
    Attempted,
    Succeeded,
    Conflicted { conflict_count: u32 },
    FailedTransient { category: FailureCategory },
    FailedPermanent { category: FailureCategory },
    Skipped(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeTrigger {
    Attempt,
    Succeed,
    Conflict { conflict_count: u32 },
    FailTransient { category: FailureCategory },
    FailPermanent { category: FailureCategory },
    Skip(String),
}

impl MergeState {
    pub fn transition(self, trigger: MergeTrigger) -> Result<MergeState, DomainError> {
        use MergeState::*;
        use MergeTrigger as T;

        let from_label = format!("{:?}", &self);
        let trigger_label = format!("{:?}", &trigger);

        match (self, trigger) {
            (Pending, T::Attempt) => Ok(Attempted),
            (Pending, T::Skip(reason)) => Ok(Skipped(reason)),
            (Attempted, T::Succeed) => Ok(Succeeded),
            (Attempted, T::Conflict { conflict_count }) => Ok(Conflicted { conflict_count }),
            (Attempted, T::FailTransient { category }) => Ok(FailedTransient { category }),
            (Attempted, T::FailPermanent { category }) => Ok(FailedPermanent { category }),
            (FailedTransient { .. }, T::Attempt) => Ok(Attempted),
            (Conflicted { .. }, T::Attempt) => Ok(Attempted),
            _ => Err(invalid_transition("MergeState", &from_label, &trigger_label)),
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            MergeState::Succeeded | MergeState::FailedPermanent { .. } | MergeState::Skipped(_)
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── InstanceState tests ──

    mod instance {
        use super::*;

        #[test]
        fn provisioning_to_active() {
            let state = InstanceState::Provisioning;
            let result = state.transition(InstanceTrigger::Provisioned);
            assert_eq!(result.unwrap(), InstanceState::Active);
        }

        #[test]
        fn provisioning_to_failed() {
            let state = InstanceState::Provisioning;
            let result =
                state.transition(InstanceTrigger::ProvisioningFailed("timeout".to_string()));
            assert_eq!(
                result.unwrap(),
                InstanceState::ProvisioningFailed("timeout".to_string())
            );
        }

        #[test]
        fn active_to_blocked() {
            let state = InstanceState::Active;
            let result = state.transition(InstanceTrigger::Block("rate limit".to_string()));
            assert_eq!(
                result.unwrap(),
                InstanceState::Blocked("rate limit".to_string())
            );
        }

        #[test]
        fn active_to_suspended() {
            let state = InstanceState::Active;
            let result = state.transition(InstanceTrigger::Suspend);
            assert_eq!(result.unwrap(), InstanceState::Suspended);
        }

        #[test]
        fn blocked_to_active() {
            let state = InstanceState::Blocked("some reason".to_string());
            let result = state.transition(InstanceTrigger::Unblock);
            assert_eq!(result.unwrap(), InstanceState::Active);
        }

        #[test]
        fn suspended_to_active() {
            let state = InstanceState::Suspended;
            let result = state.transition(InstanceTrigger::Resume);
            assert_eq!(result.unwrap(), InstanceState::Active);
        }

        // Invalid transitions
        #[test]
        fn provisioning_cannot_suspend() {
            let state = InstanceState::Provisioning;
            let result = state.transition(InstanceTrigger::Suspend);
            assert!(result.is_err());
            match result.unwrap_err() {
                DomainError::InvalidTransition { entity, .. } => {
                    assert_eq!(entity, "InstanceState");
                }
                other => panic!("expected InvalidTransition, got: {:?}", other),
            }
        }

        #[test]
        fn active_cannot_provision() {
            let state = InstanceState::Active;
            let result = state.transition(InstanceTrigger::Provisioned);
            assert!(result.is_err());
        }

        #[test]
        fn blocked_cannot_suspend() {
            let state = InstanceState::Blocked("x".into());
            let result = state.transition(InstanceTrigger::Suspend);
            assert!(result.is_err());
        }

        #[test]
        fn suspended_cannot_unblock() {
            let state = InstanceState::Suspended;
            let result = state.transition(InstanceTrigger::Unblock);
            assert!(result.is_err());
        }

        #[test]
        fn provisioning_failed_is_terminal() {
            let state = InstanceState::ProvisioningFailed("error".into());
            assert!(state.is_terminal());
            // Try all triggers
            let triggers = vec![
                InstanceTrigger::Provisioned,
                InstanceTrigger::ProvisioningFailed("x".into()),
                InstanceTrigger::Block("x".into()),
                InstanceTrigger::Suspend,
                InstanceTrigger::Unblock,
                InstanceTrigger::Resume,
            ];
            for trigger in triggers {
                let result = state.clone().transition(trigger);
                assert!(result.is_err(), "terminal state should reject all triggers");
            }
        }

        #[test]
        fn non_terminal_states() {
            assert!(!InstanceState::Provisioning.is_terminal());
            assert!(!InstanceState::Active.is_terminal());
            assert!(!InstanceState::Blocked("x".into()).is_terminal());
            assert!(!InstanceState::Suspended.is_terminal());
        }
    }

    // ── CycleState tests ──

    mod cycle {
        use super::*;

        #[test]
        fn created_to_planning() {
            let result = CycleState::Created.transition(CycleTrigger::PlanRequested);
            assert_eq!(result.unwrap(), CycleState::Planning);
        }

        #[test]
        fn planning_to_plan_ready() {
            let result = CycleState::Planning.transition(CycleTrigger::PlanGenerated);
            assert_eq!(result.unwrap(), CycleState::PlanReady);
        }

        #[test]
        fn planning_to_failed() {
            let result =
                CycleState::Planning.transition(CycleTrigger::Fail("plan generation error".into()));
            assert_eq!(
                result.unwrap(),
                CycleState::Failed("plan generation error".into())
            );
        }

        #[test]
        fn plan_ready_to_approved() {
            let result = CycleState::PlanReady.transition(CycleTrigger::PlanApproved);
            assert_eq!(result.unwrap(), CycleState::Approved);
        }

        #[test]
        fn plan_ready_to_cancelled() {
            let result =
                CycleState::PlanReady.transition(CycleTrigger::Cancel("not needed".into()));
            assert_eq!(
                result.unwrap(),
                CycleState::Cancelled("not needed".into())
            );
        }

        #[test]
        fn approved_to_running() {
            let result = CycleState::Approved.transition(CycleTrigger::Start);
            assert_eq!(result.unwrap(), CycleState::Running);
        }

        #[test]
        fn running_to_blocked() {
            let result =
                CycleState::Running.transition(CycleTrigger::Block("waiting on API".into()));
            assert_eq!(
                result.unwrap(),
                CycleState::Blocked("waiting on API".into())
            );
        }

        #[test]
        fn running_to_completing() {
            let result = CycleState::Running.transition(CycleTrigger::BeginCompleting);
            assert_eq!(result.unwrap(), CycleState::Completing);
        }

        #[test]
        fn blocked_to_running() {
            let result =
                CycleState::Blocked("reason".into()).transition(CycleTrigger::Unblock);
            assert_eq!(result.unwrap(), CycleState::Running);
        }

        #[test]
        fn completing_to_completed() {
            let result = CycleState::Completing.transition(CycleTrigger::Complete);
            assert_eq!(result.unwrap(), CycleState::Completed);
        }

        #[test]
        fn running_to_failed() {
            let result = CycleState::Running.transition(CycleTrigger::Fail("boom".into()));
            assert_eq!(result.unwrap(), CycleState::Failed("boom".into()));
        }

        #[test]
        fn running_to_cancelled() {
            let result =
                CycleState::Running.transition(CycleTrigger::Cancel("user abort".into()));
            assert_eq!(
                result.unwrap(),
                CycleState::Cancelled("user abort".into())
            );
        }

        #[test]
        fn completing_to_failed() {
            let result =
                CycleState::Completing.transition(CycleTrigger::Fail("merge failed".into()));
            assert_eq!(
                result.unwrap(),
                CycleState::Failed("merge failed".into())
            );
        }

        // Invalid transitions
        #[test]
        fn created_cannot_complete() {
            let result = CycleState::Created.transition(CycleTrigger::Complete);
            assert!(result.is_err());
        }

        #[test]
        fn planning_cannot_start() {
            let result = CycleState::Planning.transition(CycleTrigger::Start);
            assert!(result.is_err());
        }

        #[test]
        fn approved_cannot_complete() {
            let result = CycleState::Approved.transition(CycleTrigger::Complete);
            assert!(result.is_err());
        }

        #[test]
        fn completing_cannot_cancel() {
            let result =
                CycleState::Completing.transition(CycleTrigger::Cancel("oops".into()));
            assert!(result.is_err());
        }

        #[test]
        fn blocked_cannot_complete() {
            let result =
                CycleState::Blocked("x".into()).transition(CycleTrigger::Complete);
            assert!(result.is_err());
        }

        // Terminal states
        #[test]
        fn completed_is_terminal() {
            assert!(CycleState::Completed.is_terminal());
        }

        #[test]
        fn failed_is_terminal() {
            assert!(CycleState::Failed("x".into()).is_terminal());
        }

        #[test]
        fn cancelled_is_terminal() {
            assert!(CycleState::Cancelled("x".into()).is_terminal());
        }

        #[test]
        fn completed_rejects_all_triggers() {
            let triggers = vec![
                CycleTrigger::PlanRequested,
                CycleTrigger::PlanGenerated,
                CycleTrigger::PlanApproved,
                CycleTrigger::Start,
                CycleTrigger::Complete,
                CycleTrigger::Fail("x".into()),
                CycleTrigger::Cancel("x".into()),
                CycleTrigger::Block("x".into()),
                CycleTrigger::Unblock,
                CycleTrigger::BeginCompleting,
            ];
            for trigger in triggers {
                let result = CycleState::Completed.transition(trigger);
                assert!(result.is_err(), "terminal state should reject all triggers");
            }
        }

        #[test]
        fn non_terminal_states() {
            assert!(!CycleState::Created.is_terminal());
            assert!(!CycleState::Planning.is_terminal());
            assert!(!CycleState::PlanReady.is_terminal());
            assert!(!CycleState::Approved.is_terminal());
            assert!(!CycleState::Running.is_terminal());
            assert!(!CycleState::Blocked("x".into()).is_terminal());
            assert!(!CycleState::Completing.is_terminal());
        }
    }

    // ── TaskState tests ──

    mod task {
        use super::*;

        fn run_id() -> Uuid {
            Uuid::new_v4()
        }

        #[test]
        fn scheduled_to_active() {
            let rid = run_id();
            let result = TaskState::Scheduled.transition(TaskTrigger::Claim {
                run_id: rid,
                attempt: 1,
            });
            assert_eq!(
                result.unwrap(),
                TaskState::Active {
                    active_run_id: rid,
                    attempt: 1
                }
            );
        }

        #[test]
        fn active_to_verifying() {
            let rid = run_id();
            let rid2 = run_id();
            let state = TaskState::Active {
                active_run_id: rid,
                attempt: 1,
            };
            let result = state.transition(TaskTrigger::RunCompleted { run_id: rid2 });
            assert_eq!(result.unwrap(), TaskState::Verifying { run_id: rid2 });
        }

        #[test]
        fn active_to_scheduled_retry() {
            let state = TaskState::Active {
                active_run_id: run_id(),
                attempt: 1,
            };
            let result = state.transition(TaskTrigger::RetryScheduled);
            assert_eq!(result.unwrap(), TaskState::Scheduled);
        }

        #[test]
        fn active_to_failed() {
            let state = TaskState::Active {
                active_run_id: run_id(),
                attempt: 3,
            };
            let result = state.transition(TaskTrigger::Fail {
                reason: "retries exhausted".into(),
                total_attempts: 3,
            });
            assert_eq!(
                result.unwrap(),
                TaskState::Failed {
                    reason: "retries exhausted".into(),
                    total_attempts: 3
                }
            );
        }

        #[test]
        fn active_to_cancelled() {
            let state = TaskState::Active {
                active_run_id: run_id(),
                attempt: 1,
            };
            let result = state.transition(TaskTrigger::Cancel("user cancel".into()));
            assert_eq!(
                result.unwrap(),
                TaskState::Cancelled("user cancel".into())
            );
        }

        #[test]
        fn verifying_to_passed() {
            let rid = run_id();
            let state = TaskState::Verifying { run_id: rid };
            let result = state.transition(TaskTrigger::Pass {
                evidence_run_id: rid,
            });
            assert_eq!(
                result.unwrap(),
                TaskState::Passed {
                    evidence_run_id: rid
                }
            );
        }

        #[test]
        fn verifying_to_scheduled_retry() {
            let state = TaskState::Verifying { run_id: run_id() };
            let result = state.transition(TaskTrigger::RetryScheduled);
            assert_eq!(result.unwrap(), TaskState::Scheduled);
        }

        #[test]
        fn verifying_to_failed() {
            let state = TaskState::Verifying { run_id: run_id() };
            let result = state.transition(TaskTrigger::Fail {
                reason: "verification failed, no retries".into(),
                total_attempts: 2,
            });
            assert_eq!(
                result.unwrap(),
                TaskState::Failed {
                    reason: "verification failed, no retries".into(),
                    total_attempts: 2
                }
            );
        }

        #[test]
        fn scheduled_to_cancelled() {
            let result = TaskState::Scheduled.transition(TaskTrigger::Cancel("abort".into()));
            assert_eq!(result.unwrap(), TaskState::Cancelled("abort".into()));
        }

        #[test]
        fn scheduled_to_skipped() {
            let result =
                TaskState::Scheduled.transition(TaskTrigger::Skip("not applicable".into()));
            assert_eq!(
                result.unwrap(),
                TaskState::Skipped("not applicable".into())
            );
        }

        // Invalid transitions
        #[test]
        fn scheduled_cannot_pass() {
            let result = TaskState::Scheduled.transition(TaskTrigger::Pass {
                evidence_run_id: run_id(),
            });
            assert!(result.is_err());
        }

        #[test]
        fn scheduled_cannot_run_completed() {
            let result =
                TaskState::Scheduled.transition(TaskTrigger::RunCompleted { run_id: run_id() });
            assert!(result.is_err());
        }

        #[test]
        fn verifying_cannot_claim() {
            let state = TaskState::Verifying { run_id: run_id() };
            let result = state.transition(TaskTrigger::Claim {
                run_id: run_id(),
                attempt: 1,
            });
            assert!(result.is_err());
        }

        #[test]
        fn active_cannot_skip() {
            let state = TaskState::Active {
                active_run_id: run_id(),
                attempt: 1,
            };
            let result = state.transition(TaskTrigger::Skip("x".into()));
            assert!(result.is_err());
        }

        #[test]
        fn active_cannot_pass() {
            let state = TaskState::Active {
                active_run_id: run_id(),
                attempt: 1,
            };
            let result = state.transition(TaskTrigger::Pass {
                evidence_run_id: run_id(),
            });
            assert!(result.is_err());
        }

        // Terminal states
        #[test]
        fn passed_is_terminal() {
            assert!(TaskState::Passed {
                evidence_run_id: run_id()
            }
            .is_terminal());
        }

        #[test]
        fn failed_is_terminal() {
            assert!(TaskState::Failed {
                reason: "x".into(),
                total_attempts: 1
            }
            .is_terminal());
        }

        #[test]
        fn cancelled_is_terminal() {
            assert!(TaskState::Cancelled("x".into()).is_terminal());
        }

        #[test]
        fn skipped_is_terminal() {
            assert!(TaskState::Skipped("x".into()).is_terminal());
        }

        #[test]
        fn passed_rejects_all_triggers() {
            let state = TaskState::Passed {
                evidence_run_id: run_id(),
            };
            let triggers = vec![
                TaskTrigger::Claim {
                    run_id: run_id(),
                    attempt: 1,
                },
                TaskTrigger::RunCompleted { run_id: run_id() },
                TaskTrigger::RetryScheduled,
                TaskTrigger::Pass {
                    evidence_run_id: run_id(),
                },
                TaskTrigger::Fail {
                    reason: "x".into(),
                    total_attempts: 1,
                },
                TaskTrigger::Cancel("x".into()),
                TaskTrigger::Skip("x".into()),
            ];
            for trigger in triggers {
                let result = state.clone().transition(trigger);
                assert!(result.is_err(), "terminal state should reject all triggers");
            }
        }

        #[test]
        fn non_terminal_states() {
            assert!(!TaskState::Scheduled.is_terminal());
            assert!(
                !TaskState::Active {
                    active_run_id: run_id(),
                    attempt: 1
                }
                .is_terminal()
            );
            assert!(!TaskState::Verifying { run_id: run_id() }.is_terminal());
        }
    }

    // ── RunState tests ──

    mod run {
        use super::*;

        #[test]
        fn claimed_to_running() {
            let result = RunState::Claimed.transition(RunTrigger::Start);
            assert_eq!(result.unwrap(), RunState::Running);
        }

        #[test]
        fn running_to_completed() {
            let result = RunState::Running.transition(RunTrigger::Complete { exit_code: 0 });
            assert_eq!(result.unwrap(), RunState::Completed { exit_code: 0 });
        }

        #[test]
        fn running_to_completed_nonzero() {
            let result = RunState::Running.transition(RunTrigger::Complete { exit_code: 1 });
            assert_eq!(result.unwrap(), RunState::Completed { exit_code: 1 });
        }

        #[test]
        fn running_to_failed() {
            let result = RunState::Running.transition(RunTrigger::Fail(FailureCategory::Transient));
            assert_eq!(result.unwrap(), RunState::Failed(FailureCategory::Transient));
        }

        #[test]
        fn running_to_timed_out() {
            let result = RunState::Running.transition(RunTrigger::TimeOut);
            assert_eq!(result.unwrap(), RunState::TimedOut);
        }

        #[test]
        fn running_to_cancelled() {
            let result = RunState::Running.transition(RunTrigger::Cancel("user".into()));
            assert_eq!(result.unwrap(), RunState::Cancelled("user".into()));
        }

        #[test]
        fn running_to_abandoned() {
            let result =
                RunState::Running.transition(RunTrigger::Abandon("reconciler".into()));
            assert_eq!(
                result.unwrap(),
                RunState::Abandoned("reconciler".into())
            );
        }

        #[test]
        fn claimed_to_failed_spawn_failure() {
            let result =
                RunState::Claimed.transition(RunTrigger::Fail(FailureCategory::SpawnFailure));
            assert_eq!(
                result.unwrap(),
                RunState::Failed(FailureCategory::SpawnFailure)
            );
        }

        #[test]
        fn claimed_to_cancelled() {
            let result = RunState::Claimed.transition(RunTrigger::Cancel("abort".into()));
            assert_eq!(result.unwrap(), RunState::Cancelled("abort".into()));
        }

        // Invalid transitions
        #[test]
        fn claimed_cannot_complete() {
            let result = RunState::Claimed.transition(RunTrigger::Complete { exit_code: 0 });
            assert!(result.is_err());
        }

        #[test]
        fn claimed_cannot_time_out() {
            let result = RunState::Claimed.transition(RunTrigger::TimeOut);
            assert!(result.is_err());
        }

        #[test]
        fn claimed_cannot_abandon() {
            let result = RunState::Claimed.transition(RunTrigger::Abandon("x".into()));
            assert!(result.is_err());
        }

        #[test]
        fn running_cannot_start_again() {
            let result = RunState::Running.transition(RunTrigger::Start);
            assert!(result.is_err());
        }

        #[test]
        fn completed_cannot_fail() {
            let result = RunState::Completed { exit_code: 0 }
                .transition(RunTrigger::Fail(FailureCategory::Permanent));
            assert!(result.is_err());
        }

        // Terminal states
        #[test]
        fn completed_is_terminal() {
            assert!(RunState::Completed { exit_code: 0 }.is_terminal());
        }

        #[test]
        fn failed_is_terminal() {
            assert!(RunState::Failed(FailureCategory::Transient).is_terminal());
        }

        #[test]
        fn timed_out_is_terminal() {
            assert!(RunState::TimedOut.is_terminal());
        }

        #[test]
        fn cancelled_is_terminal() {
            assert!(RunState::Cancelled("x".into()).is_terminal());
        }

        #[test]
        fn abandoned_is_terminal() {
            assert!(RunState::Abandoned("x".into()).is_terminal());
        }

        #[test]
        fn completed_rejects_all_triggers() {
            let state = RunState::Completed { exit_code: 0 };
            let triggers: Vec<RunTrigger> = vec![
                RunTrigger::Start,
                RunTrigger::Complete { exit_code: 0 },
                RunTrigger::Fail(FailureCategory::Permanent),
                RunTrigger::TimeOut,
                RunTrigger::Cancel("x".into()),
                RunTrigger::Abandon("x".into()),
            ];
            for trigger in triggers {
                let result = state.clone().transition(trigger);
                assert!(result.is_err(), "terminal state should reject all triggers");
            }
        }

        #[test]
        fn non_terminal_states() {
            assert!(!RunState::Claimed.is_terminal());
            assert!(!RunState::Running.is_terminal());
        }

        #[test]
        fn failure_category_serde_round_trip() {
            let cats = vec![
                FailureCategory::Transient,
                FailureCategory::Permanent,
                FailureCategory::ResourceExhausted,
                FailureCategory::Timeout,
                FailureCategory::SecurityViolation,
                FailureCategory::SpawnFailure,
            ];
            for cat in cats {
                let json = serde_json::to_string(&cat).unwrap();
                let restored: FailureCategory = serde_json::from_str(&json).unwrap();
                assert_eq!(cat, restored);
            }
        }
    }

    // ── MergeState tests ──

    mod merge {
        use super::*;

        #[test]
        fn pending_to_attempted() {
            let result = MergeState::Pending.transition(MergeTrigger::Attempt);
            assert_eq!(result.unwrap(), MergeState::Attempted);
        }

        #[test]
        fn pending_to_skipped() {
            let result =
                MergeState::Pending.transition(MergeTrigger::Skip("no changes".into()));
            assert_eq!(
                result.unwrap(),
                MergeState::Skipped("no changes".into())
            );
        }

        #[test]
        fn attempted_to_succeeded() {
            let result = MergeState::Attempted.transition(MergeTrigger::Succeed);
            assert_eq!(result.unwrap(), MergeState::Succeeded);
        }

        #[test]
        fn attempted_to_conflicted() {
            let result =
                MergeState::Attempted.transition(MergeTrigger::Conflict { conflict_count: 3 });
            assert_eq!(
                result.unwrap(),
                MergeState::Conflicted { conflict_count: 3 }
            );
        }

        #[test]
        fn attempted_to_failed_transient() {
            let result = MergeState::Attempted.transition(MergeTrigger::FailTransient {
                category: FailureCategory::Transient,
            });
            assert_eq!(
                result.unwrap(),
                MergeState::FailedTransient {
                    category: FailureCategory::Transient
                }
            );
        }

        #[test]
        fn attempted_to_failed_permanent() {
            let result = MergeState::Attempted.transition(MergeTrigger::FailPermanent {
                category: FailureCategory::Permanent,
            });
            assert_eq!(
                result.unwrap(),
                MergeState::FailedPermanent {
                    category: FailureCategory::Permanent
                }
            );
        }

        #[test]
        fn failed_transient_to_attempted_retry() {
            let state = MergeState::FailedTransient {
                category: FailureCategory::Transient,
            };
            let result = state.transition(MergeTrigger::Attempt);
            assert_eq!(result.unwrap(), MergeState::Attempted);
        }

        #[test]
        fn conflicted_to_attempted_retry() {
            let state = MergeState::Conflicted { conflict_count: 2 };
            let result = state.transition(MergeTrigger::Attempt);
            assert_eq!(result.unwrap(), MergeState::Attempted);
        }

        // Invalid transitions
        #[test]
        fn pending_cannot_succeed() {
            let result = MergeState::Pending.transition(MergeTrigger::Succeed);
            assert!(result.is_err());
        }

        #[test]
        fn pending_cannot_conflict() {
            let result =
                MergeState::Pending.transition(MergeTrigger::Conflict { conflict_count: 1 });
            assert!(result.is_err());
        }

        #[test]
        fn attempted_cannot_skip() {
            let result =
                MergeState::Attempted.transition(MergeTrigger::Skip("x".into()));
            assert!(result.is_err());
        }

        #[test]
        fn succeeded_cannot_attempt() {
            let result = MergeState::Succeeded.transition(MergeTrigger::Attempt);
            assert!(result.is_err());
        }

        #[test]
        fn failed_permanent_cannot_retry() {
            let state = MergeState::FailedPermanent {
                category: FailureCategory::Permanent,
            };
            let result = state.transition(MergeTrigger::Attempt);
            assert!(result.is_err());
        }

        // Terminal states
        #[test]
        fn succeeded_is_terminal() {
            assert!(MergeState::Succeeded.is_terminal());
        }

        #[test]
        fn failed_permanent_is_terminal() {
            assert!(MergeState::FailedPermanent {
                category: FailureCategory::Permanent
            }
            .is_terminal());
        }

        #[test]
        fn skipped_is_terminal() {
            assert!(MergeState::Skipped("x".into()).is_terminal());
        }

        #[test]
        fn succeeded_rejects_all_triggers() {
            let triggers = vec![
                MergeTrigger::Attempt,
                MergeTrigger::Succeed,
                MergeTrigger::Conflict { conflict_count: 1 },
                MergeTrigger::FailTransient {
                    category: FailureCategory::Transient,
                },
                MergeTrigger::FailPermanent {
                    category: FailureCategory::Permanent,
                },
                MergeTrigger::Skip("x".into()),
            ];
            for trigger in triggers {
                let result = MergeState::Succeeded.transition(trigger);
                assert!(result.is_err(), "terminal state should reject all triggers");
            }
        }

        #[test]
        fn non_terminal_states() {
            assert!(!MergeState::Pending.is_terminal());
            assert!(!MergeState::Attempted.is_terminal());
            assert!(!MergeState::Conflicted { conflict_count: 1 }.is_terminal());
            assert!(
                !MergeState::FailedTransient {
                    category: FailureCategory::Transient
                }
                .is_terminal()
            );
        }
    }
}
