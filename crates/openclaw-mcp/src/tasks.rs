use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use chrono::{DateTime, Utc};
use serde::Serialize;

const MAX_TASKS: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub id: String,
    #[serde(rename = "type")]
    pub task_type: String,
    pub status: TaskStatus,
    #[serde(skip)]
    pub input_message: String,
    #[serde(skip)]
    pub input_session_id: Option<String>,
    pub result: Option<String>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub session_id: Option<String>,
    pub priority: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskStats {
    pub total: usize,
    pub pending: usize,
    pub running: usize,
    pub completed: usize,
    pub failed: usize,
    pub cancelled: usize,
}

#[derive(Clone)]
pub struct TaskManager {
    inner: Arc<Mutex<TaskManagerInner>>,
}

struct TaskManagerInner {
    tasks: HashMap<String, Task>,
    counter: u64,
}

impl TaskManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(TaskManagerInner {
                tasks: HashMap::new(),
                counter: 0,
            })),
        }
    }

    pub async fn create(
        &self,
        message: String,
        session_id: Option<String>,
        priority: i32,
    ) -> anyhow::Result<Task> {
        let mut inner = self.inner.lock().await;
        if inner.tasks.len() >= MAX_TASKS {
            anyhow::bail!("Task limit reached ({}). Wait for tasks to complete or cancel pending ones.", MAX_TASKS);
        }

        inner.counter += 1;
        let timestamp = Utc::now().timestamp().to_string();
        let id = format!("task_{}_{:04}", timestamp, inner.counter);

        let task = Task {
            id: id.clone(),
            task_type: "chat".to_string(),
            status: TaskStatus::Pending,
            input_message: message,
            input_session_id: session_id.clone(),
            result: None,
            error: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            session_id,
            priority,
        };

        inner.tasks.insert(id, task.clone());
        Ok(task)
    }

    pub async fn get(&self, id: &str) -> Option<Task> {
        let inner = self.inner.lock().await;
        inner.tasks.get(id).cloned()
    }

    pub async fn update_status(
        &self,
        id: &str,
        status: TaskStatus,
        result: Option<String>,
        error: Option<String>,
    ) -> bool {
        let mut inner = self.inner.lock().await;
        if let Some(task) = inner.tasks.get_mut(id) {
            task.status = status;
            if status == TaskStatus::Running && task.started_at.is_none() {
                task.started_at = Some(Utc::now());
            }
            if matches!(status, TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled) {
                task.completed_at = Some(Utc::now());
            }
            if let Some(r) = result {
                task.result = Some(r);
            }
            if let Some(e) = error {
                task.error = Some(e);
            }
            true
        } else {
            false
        }
    }

    pub async fn cancel(&self, id: &str) -> bool {
        let mut inner = self.inner.lock().await;
        if let Some(task) = inner.tasks.get_mut(id) {
            if task.status != TaskStatus::Pending {
                return false;
            }
            task.status = TaskStatus::Cancelled;
            task.completed_at = Some(Utc::now());
            true
        } else {
            false
        }
    }

    pub async fn list(&self, status_filter: Option<TaskStatus>, session_filter: Option<&str>) -> Vec<Task> {
        let inner = self.inner.lock().await;
        let mut tasks: Vec<Task> = inner.tasks.values()
            .filter(|t| {
                if let Some(s) = status_filter {
                    if t.status != s { return false; }
                }
                if let Some(sid) = session_filter {
                    if t.session_id.as_deref() != Some(sid) { return false; }
                }
                true
            })
            .cloned()
            .collect();

        tasks.sort_by(|a, b| {
            b.priority.cmp(&a.priority)
                .then(a.created_at.cmp(&b.created_at))
        });
        tasks
    }

    pub async fn next_pending(&self) -> Option<Task> {
        self.list(Some(TaskStatus::Pending), None).await.into_iter().next()
    }

    pub async fn stats(&self) -> TaskStats {
        let inner = self.inner.lock().await;
        let mut stats = TaskStats {
            total: inner.tasks.len(),
            pending: 0,
            running: 0,
            completed: 0,
            failed: 0,
            cancelled: 0,
        };
        for task in inner.tasks.values() {
            match task.status {
                TaskStatus::Pending => stats.pending += 1,
                TaskStatus::Running => stats.running += 1,
                TaskStatus::Completed => stats.completed += 1,
                TaskStatus::Failed => stats.failed += 1,
                TaskStatus::Cancelled => stats.cancelled += 1,
            }
        }
        stats
    }

    pub async fn cleanup(&self, max_age_secs: i64) -> usize {
        let mut inner = self.inner.lock().await;
        let now = Utc::now();
        let before = inner.tasks.len();
        inner.tasks.retain(|_, task| {
            if let Some(completed) = task.completed_at {
                (now - completed).num_seconds() < max_age_secs
            } else {
                true
            }
        });
        before - inner.tasks.len()
    }
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}
