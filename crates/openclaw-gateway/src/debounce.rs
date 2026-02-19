use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{Duration, Instant};

/// Collects rapid messages from the same user+channel into a single batch.
/// After `debounce_ms` of silence, the batch is flushed via the callback.
pub struct MessageDebouncer {
    debounce_ms: u64,
    max_collect: usize,
    pending: Arc<Mutex<HashMap<String, PendingBatch>>>,
}

struct PendingBatch {
    messages: Vec<String>,
    last_received: Instant,
    flush_handle: Option<tokio::task::JoinHandle<()>>,
}

impl MessageDebouncer {
    pub fn new(debounce_ms: u64, max_collect: usize) -> Self {
        Self {
            debounce_ms,
            max_collect,
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Add a message for a given key (e.g. "tg:user:chat").
    /// Returns Some(collected_text) if the batch should be flushed immediately
    /// (max_collect reached), or None if the message was queued for debounce.
    pub async fn add_message(&self, key: &str, text: &str) -> Option<String> {
        let mut pending = self.pending.lock().await;

        let batch = pending.entry(key.to_string()).or_insert_with(|| PendingBatch {
            messages: Vec::new(),
            last_received: Instant::now(),
            flush_handle: None,
        });

        batch.messages.push(text.to_string());
        batch.last_received = Instant::now();

        // If max_collect reached, flush immediately
        if batch.messages.len() >= self.max_collect {
            let collected = batch.messages.join("\n\n");
            if let Some(handle) = batch.flush_handle.take() {
                handle.abort();
            }
            pending.remove(key);
            return Some(collected);
        }

        // Cancel previous flush timer
        if let Some(handle) = batch.flush_handle.take() {
            handle.abort();
        }

        None
    }

    /// Start a debounce timer for a key. Returns the collected text after the
    /// debounce period elapses without new messages.
    pub async fn wait_for_flush(&self, key: &str) -> Option<String> {
        let debounce = Duration::from_millis(self.debounce_ms);

        loop {
            tokio::time::sleep(debounce).await;

            let mut pending = self.pending.lock().await;
            if let Some(batch) = pending.get(key) {
                // Check if enough time has passed since last message
                if batch.last_received.elapsed() >= debounce {
                    let collected = batch.messages.join("\n\n");
                    pending.remove(key);
                    return Some(collected);
                }
                // Otherwise, another message arrived — loop and wait again
            } else {
                // Already flushed (max_collect hit)
                return None;
            }
        }
    }

    /// Check if there's a pending batch for this key
    pub async fn has_pending(&self, key: &str) -> bool {
        let pending = self.pending.lock().await;
        pending.contains_key(key)
    }

    /// Get the number of pending batches
    pub async fn pending_count(&self) -> usize {
        let pending = self.pending.lock().await;
        pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_debounce_immediate_on_max() {
        let d = MessageDebouncer::new(1000, 3);

        assert!(d.add_message("k1", "msg1").await.is_none());
        assert!(d.add_message("k1", "msg2").await.is_none());

        // Third message should trigger immediate flush
        let result = d.add_message("k1", "msg3").await;
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "msg1\n\nmsg2\n\nmsg3");
    }

    #[tokio::test]
    async fn test_debounce_timer_flush() {
        let d = MessageDebouncer::new(50, 10); // 50ms debounce

        d.add_message("k1", "hello").await;
        d.add_message("k1", "world").await;

        // Wait for debounce
        let result = d.wait_for_flush("k1").await;
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "hello\n\nworld");
    }

    #[tokio::test]
    async fn test_debounce_separate_keys() {
        let d = MessageDebouncer::new(1000, 2);

        d.add_message("k1", "a").await;
        d.add_message("k2", "b").await;

        assert_eq!(d.pending_count().await, 2);

        // Flush k1
        let r1 = d.add_message("k1", "c").await;
        assert!(r1.is_some());
        assert_eq!(r1.unwrap(), "a\n\nc");

        // k2 still pending
        assert!(d.has_pending("k2").await);
        assert!(!d.has_pending("k1").await);
    }

    #[tokio::test]
    async fn test_debounce_single_message() {
        let d = MessageDebouncer::new(30, 10);

        d.add_message("k1", "solo").await;

        let result = d.wait_for_flush("k1").await;
        assert_eq!(result.unwrap(), "solo");
    }
}
