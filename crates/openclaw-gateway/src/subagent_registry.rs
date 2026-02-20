use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use tokio_util::sync::CancellationToken;

/// Global registry of background subagent tasks.
static REGISTRY: OnceLock<Mutex<SubagentRegistry>> = OnceLock::new();

fn registry() -> &'static Mutex<SubagentRegistry> {
    REGISTRY.get_or_init(|| Mutex::new(SubagentRegistry::default()))
}

#[derive(Clone, Debug)]
pub struct SubagentTask {
    pub id: u64,
    pub description: String,
    pub agent_name: String,
    pub chat_id: i64,
    pub status: TaskStatus,
    pub started_at: Instant,
    pub cancel_token: CancellationToken,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TaskStatus {
    Running,
    Completed,
    Failed(String),
    Cancelled,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatus::Running => write!(f, "running"),
            TaskStatus::Completed => write!(f, "completed"),
            TaskStatus::Failed(e) => write!(f, "failed: {}", e),
            TaskStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

#[derive(Default)]
struct SubagentRegistry {
    tasks: HashMap<u64, SubagentTask>,
    next_id: u64,
}

/// Register a new background subagent task. Returns (task_id, cancel_token).
pub fn register_subagent(
    description: &str,
    agent_name: &str,
    chat_id: i64,
) -> (u64, CancellationToken) {
    let mut reg = registry().lock().unwrap();
    reg.next_id += 1;
    let id = reg.next_id;
    let cancel_token = CancellationToken::new();
    let task = SubagentTask {
        id,
        description: description.to_string(),
        agent_name: agent_name.to_string(),
        chat_id,
        status: TaskStatus::Running,
        started_at: Instant::now(),
        cancel_token: cancel_token.clone(),
    };
    reg.tasks.insert(id, task);
    (id, cancel_token)
}

/// Mark a task as completed.
pub fn complete_subagent(id: u64) {
    let mut reg = registry().lock().unwrap();
    if let Some(task) = reg.tasks.get_mut(&id) {
        task.status = TaskStatus::Completed;
    }
}

/// Mark a task as failed.
pub fn fail_subagent(id: u64, error: &str) {
    let mut reg = registry().lock().unwrap();
    if let Some(task) = reg.tasks.get_mut(&id) {
        task.status = TaskStatus::Failed(error.to_string());
    }
}

/// Cancel a subagent task by ID. Returns true if found.
pub fn cancel_subagent(id: u64) -> bool {
    let mut reg = registry().lock().unwrap();
    if let Some(task) = reg.tasks.get_mut(&id) {
        task.cancel_token.cancel();
        task.status = TaskStatus::Cancelled;
        true
    } else {
        false
    }
}

/// Cancel all running subagent tasks for a given chat_id. Returns count cancelled.
pub fn cancel_all_for_chat(chat_id: i64) -> usize {
    let mut reg = registry().lock().unwrap();
    let mut count = 0;
    for task in reg.tasks.values_mut() {
        if task.chat_id == chat_id && task.status == TaskStatus::Running {
            task.cancel_token.cancel();
            task.status = TaskStatus::Cancelled;
            count += 1;
        }
    }
    count
}

/// List all tasks (running first, then recent completed/failed).
pub fn list_tasks() -> Vec<SubagentTask> {
    let reg = registry().lock().unwrap();
    let mut tasks: Vec<SubagentTask> = reg.tasks.values().cloned().collect();
    tasks.sort_by(|a, b| {
        let a_running = a.status == TaskStatus::Running;
        let b_running = b.status == TaskStatus::Running;
        b_running.cmp(&a_running).then(b.id.cmp(&a.id))
    });
    tasks
}

/// Count of currently running tasks.
pub fn running_count() -> usize {
    let reg = registry().lock().unwrap();
    reg.tasks
        .values()
        .filter(|t| t.status == TaskStatus::Running)
        .count()
}

/// Clean up old completed tasks (keep last 20).
pub fn gc() {
    let mut reg = registry().lock().unwrap();
    let mut finished: Vec<u64> = reg
        .tasks
        .iter()
        .filter(|(_, t)| t.status != TaskStatus::Running)
        .map(|(id, _)| *id)
        .collect();
    finished.sort();
    if finished.len() > 20 {
        let to_remove = finished.len() - 20;
        for id in finished.into_iter().take(to_remove) {
            reg.tasks.remove(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_complete() {
        let (id, _token) = register_subagent("test task", "main", 12345);
        assert!(id > 0);

        complete_subagent(id);
        gc();
    }

    #[test]
    fn test_cancel() {
        let (id, token) = register_subagent("cancel test", "main", 12345);
        assert!(!token.is_cancelled());

        let cancelled = cancel_subagent(id);
        assert!(cancelled);
        assert!(token.is_cancelled());

        gc();
    }
}
