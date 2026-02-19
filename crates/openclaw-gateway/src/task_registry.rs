use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use tokio_util::sync::CancellationToken;

/// Global registry of active agent tasks, keyed by chat identifier.
/// Used to cancel running tasks via /cancel or /stop commands,
/// and to auto-cancel stale tasks when a new message arrives.
static REGISTRY: OnceLock<Mutex<HashMap<String, CancellationToken>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<String, CancellationToken>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register a new task for a chat. Returns the CancellationToken to pass into the agent runtime.
/// If a task is already running for this chat, it is cancelled first.
pub fn register_task(chat_key: &str) -> CancellationToken {
    let token = CancellationToken::new();
    let mut map = registry().lock().unwrap();

    // Cancel any existing task for this chat
    if let Some(old_token) = map.remove(chat_key) {
        old_token.cancel();
        tracing::info!("Auto-cancelled previous task for {}", chat_key);
    }

    map.insert(chat_key.to_string(), token.clone());
    token
}

/// Cancel the running task for a chat. Returns true if a task was found and cancelled.
pub fn cancel_task(chat_key: &str) -> bool {
    let mut map = registry().lock().unwrap();
    if let Some(token) = map.remove(chat_key) {
        token.cancel();
        true
    } else {
        false
    }
}

/// Remove a task from the registry (called when a task completes normally).
pub fn unregister_task(chat_key: &str) {
    let mut map = registry().lock().unwrap();
    map.remove(chat_key);
}

/// Check if a task is currently running for a chat.
pub fn has_active_task(chat_key: &str) -> bool {
    let map = registry().lock().unwrap();
    map.contains_key(chat_key)
}

/// Count of currently active tasks.
pub fn active_count() -> usize {
    let map = registry().lock().unwrap();
    map.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_cancel() {
        let token = register_task("test:chat:1");
        assert!(!token.is_cancelled());
        assert!(has_active_task("test:chat:1"));

        let cancelled = cancel_task("test:chat:1");
        assert!(cancelled);
        assert!(token.is_cancelled());
        assert!(!has_active_task("test:chat:1"));
    }

    #[test]
    fn test_cancel_nonexistent() {
        let cancelled = cancel_task("test:chat:nonexistent");
        assert!(!cancelled);
    }

    #[test]
    fn test_auto_cancel_on_reregister() {
        let token1 = register_task("test:chat:2");
        assert!(!token1.is_cancelled());

        let token2 = register_task("test:chat:2");
        assert!(token1.is_cancelled()); // old task auto-cancelled
        assert!(!token2.is_cancelled()); // new task is active

        unregister_task("test:chat:2");
        assert!(!has_active_task("test:chat:2"));
    }

    #[test]
    fn test_unregister() {
        let _token = register_task("test:chat:3");
        assert!(has_active_task("test:chat:3"));

        unregister_task("test:chat:3");
        assert!(!has_active_task("test:chat:3"));
    }

    #[test]
    fn test_active_count() {
        let initial = active_count();
        let _t1 = register_task("test:count:a");
        let _t2 = register_task("test:count:b");
        assert_eq!(active_count(), initial + 2);

        unregister_task("test:count:a");
        unregister_task("test:count:b");
    }
}
