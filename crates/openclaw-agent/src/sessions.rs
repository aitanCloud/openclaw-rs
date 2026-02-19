use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const SCHEMA_VERSION: i32 = 1;

/// A single message in a session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    pub role: String,
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
    pub tool_calls_json: Option<String>,
    pub tool_call_id: Option<String>,
    pub timestamp_ms: i64,
}

/// Session metadata
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub session_key: String,
    pub agent_name: String,
    pub model: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub message_count: i64,
    pub total_tokens: i64,
}

/// SQLite-backed session store
pub struct SessionStore {
    conn: Connection,
}

impl SessionStore {
    /// Open or create the session database
    pub fn open(agent_name: &str) -> Result<Self> {
        let db_path = resolve_db_path(agent_name)?;
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&db_path)
            .with_context(|| format!("Failed to open session DB: {}", db_path.display()))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;

        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Open an in-memory store (for testing)
    #[cfg(test)]
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let version: i32 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap_or(0);

        if version < SCHEMA_VERSION {
            self.conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS sessions (
                    session_key TEXT PRIMARY KEY,
                    agent_name  TEXT NOT NULL,
                    model       TEXT NOT NULL DEFAULT '',
                    created_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL,
                    total_tokens  INTEGER NOT NULL DEFAULT 0
                );

                CREATE TABLE IF NOT EXISTS messages (
                    id              INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_key     TEXT NOT NULL,
                    role            TEXT NOT NULL,
                    content         TEXT,
                    reasoning_content TEXT,
                    tool_calls_json TEXT,
                    tool_call_id    TEXT,
                    timestamp_ms    INTEGER NOT NULL,
                    FOREIGN KEY (session_key) REFERENCES sessions(session_key)
                );

                CREATE INDEX IF NOT EXISTS idx_messages_session
                    ON messages(session_key, id);
                ",
            )?;
            self.conn
                .execute_batch(&format!("PRAGMA user_version = {};", SCHEMA_VERSION))?;
        }

        Ok(())
    }

    /// Create a new session
    pub fn create_session(
        &self,
        session_key: &str,
        agent_name: &str,
        model: &str,
    ) -> Result<()> {
        let now = now_ms();
        self.conn.execute(
            "INSERT OR IGNORE INTO sessions (session_key, agent_name, model, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session_key, agent_name, model, now, now],
        )?;
        Ok(())
    }

    /// Append a message to a session
    pub fn append_message(
        &self,
        session_key: &str,
        msg: &crate::llm::Message,
    ) -> Result<()> {
        let role = match msg.role {
            crate::llm::Role::System => "system",
            crate::llm::Role::User => "user",
            crate::llm::Role::Assistant => "assistant",
            crate::llm::Role::Tool => "tool",
        };

        let tool_calls_json = msg
            .tool_calls
            .as_ref()
            .map(|tc| serde_json::to_string(tc).unwrap_or_default());

        let now = now_ms();
        self.conn.execute(
            "INSERT INTO messages (session_key, role, content, reasoning_content, tool_calls_json, tool_call_id, timestamp_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                session_key,
                role,
                msg.content,
                msg.reasoning_content,
                tool_calls_json,
                msg.tool_call_id,
                now,
            ],
        )?;

        self.conn.execute(
            "UPDATE sessions SET updated_at_ms = ?1 WHERE session_key = ?2",
            params![now, session_key],
        )?;

        Ok(())
    }

    /// Update token count for a session
    pub fn add_tokens(&self, session_key: &str, tokens: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET total_tokens = total_tokens + ?1 WHERE session_key = ?2",
            params![tokens, session_key],
        )?;
        Ok(())
    }

    /// Load all messages for a session (for context replay)
    pub fn load_messages(&self, session_key: &str) -> Result<Vec<SessionMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT role, content, reasoning_content, tool_calls_json, tool_call_id, timestamp_ms
             FROM messages WHERE session_key = ?1 ORDER BY id ASC",
        )?;

        let messages = stmt
            .query_map(params![session_key], |row| {
                Ok(SessionMessage {
                    role: row.get(0)?,
                    content: row.get(1)?,
                    reasoning_content: row.get(2)?,
                    tool_calls_json: row.get(3)?,
                    tool_call_id: row.get(4)?,
                    timestamp_ms: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(messages)
    }

    /// Convert stored messages back to LLM Message format
    pub fn load_llm_messages(&self, session_key: &str) -> Result<Vec<crate::llm::Message>> {
        let stored = self.load_messages(session_key)?;
        let mut messages = Vec::with_capacity(stored.len());

        for msg in stored {
            let role = match msg.role.as_str() {
                "system" => crate::llm::Role::System,
                "user" => crate::llm::Role::User,
                "assistant" => crate::llm::Role::Assistant,
                "tool" => crate::llm::Role::Tool,
                _ => continue,
            };

            let tool_calls = msg
                .tool_calls_json
                .as_deref()
                .and_then(|json| serde_json::from_str(json).ok());

            messages.push(crate::llm::Message {
                role,
                content: msg.content,
                reasoning_content: msg.reasoning_content,
                tool_call_id: msg.tool_call_id,
                tool_calls,
                image_urls: Vec::new(),
            });
        }

        Ok(messages)
    }

    /// List recent sessions for an agent
    pub fn list_sessions(&self, agent_name: &str, limit: usize) -> Result<Vec<SessionInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.session_key, s.agent_name, s.model, s.created_at_ms, s.updated_at_ms,
                    s.total_tokens, COUNT(m.id) as msg_count
             FROM sessions s
             LEFT JOIN messages m ON m.session_key = s.session_key
             WHERE s.agent_name = ?1
             GROUP BY s.session_key
             ORDER BY s.updated_at_ms DESC
             LIMIT ?2",
        )?;

        let sessions = stmt
            .query_map(params![agent_name, limit as i64], |row| {
                Ok(SessionInfo {
                    session_key: row.get(0)?,
                    agent_name: row.get(1)?,
                    model: row.get(2)?,
                    created_at_ms: row.get(3)?,
                    updated_at_ms: row.get(4)?,
                    total_tokens: row.get(5)?,
                    message_count: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(sessions)
    }

    /// Get the most recent session key for an agent (for --continue flag)
    pub fn latest_session_key(&self, agent_name: &str) -> Result<Option<String>> {
        let result = self.conn.query_row(
            "SELECT session_key FROM sessions WHERE agent_name = ?1 ORDER BY updated_at_ms DESC LIMIT 1",
            params![agent_name],
            |row| row.get(0),
        );

        match result {
            Ok(key) => Ok(Some(key)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete sessions older than `max_age_days` days. Returns the number of sessions pruned.
    pub fn prune_old_sessions(&self, max_age_days: u32) -> Result<usize> {
        let cutoff_ms = now_ms() - (max_age_days as i64 * 86_400_000);

        // Find sessions to prune
        let mut stmt = self.conn.prepare(
            "SELECT session_key FROM sessions WHERE updated_at_ms < ?1"
        )?;
        let keys: Vec<String> = stmt
            .query_map(params![cutoff_ms], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        drop(stmt);

        if keys.is_empty() {
            return Ok(0);
        }

        self.conn.execute_batch("PRAGMA foreign_keys = OFF;")?;

        for key in &keys {
            self.conn.execute("DELETE FROM messages WHERE session_key = ?1", params![key])?;
            self.conn.execute("DELETE FROM sessions WHERE session_key = ?1", params![key])?;
        }

        self.conn.execute_batch("PRAGMA foreign_keys = ON;")?;

        Ok(keys.len())
    }

    /// Migrate old-format session keys (without user_id) to new format.
    /// Old format: `{prefix}:{agent}:{channel}` → New format: `{prefix}:{agent}:0:{channel}`
    /// Returns the number of sessions migrated.
    pub fn migrate_old_session_keys(&self) -> Result<usize> {
        // Temporarily disable foreign key checks for the migration
        self.conn.execute_batch("PRAGMA foreign_keys = OFF;")?;

        let mut stmt = self.conn.prepare(
            "SELECT session_key FROM sessions WHERE session_key LIKE 'tg:%' OR session_key LIKE 'dc:%'"
        )?;

        let keys: Vec<String> = stmt
            .query_map([], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        drop(stmt);

        let mut migrated = 0;

        for key in &keys {
            let parts: Vec<&str> = key.split(':').collect();
            // Old format has 3 parts: prefix:agent:channel
            // New format has 4+ parts: prefix:agent:user:channel[:uuid]
            if parts.len() == 3 {
                // Insert user_id=0 as placeholder
                let new_key = format!("{}:{}:0:{}", parts[0], parts[1], parts[2]);

                // Check if new key already exists
                let exists: bool = self.conn.query_row(
                    "SELECT COUNT(*) > 0 FROM sessions WHERE session_key = ?1",
                    params![new_key],
                    |row| row.get(0),
                ).unwrap_or(false);

                if !exists {
                    // Update messages first, then sessions
                    self.conn.execute(
                        "UPDATE messages SET session_key = ?1 WHERE session_key = ?2",
                        params![new_key, key],
                    )?;
                    self.conn.execute(
                        "UPDATE sessions SET session_key = ?1 WHERE session_key = ?2",
                        params![new_key, key],
                    )?;
                    migrated += 1;
                }
            }
        }

        // Re-enable foreign key checks
        self.conn.execute_batch("PRAGMA foreign_keys = ON;")?;

        Ok(migrated)
    }
}

fn resolve_db_path(agent_name: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home
        .join(".openclaw")
        .join("agents")
        .join(agent_name)
        .join("sessions.db"))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{Message, Role};

    #[test]
    fn test_create_and_load_session() {
        let store = SessionStore::open_memory().unwrap();
        store
            .create_session("test-session-1", "main", "kimi-k2.5")
            .unwrap();

        let msg = Message::user("Hello, world!");
        store.append_message("test-session-1", &msg).unwrap();

        let msg2 = Message::assistant("Hi there!");
        store.append_message("test-session-1", &msg2).unwrap();

        let messages = store.load_messages("test-session-1").unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content.as_deref(), Some("Hello, world!"));
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].content.as_deref(), Some("Hi there!"));
    }

    #[test]
    fn test_load_llm_messages() {
        let store = SessionStore::open_memory().unwrap();
        store
            .create_session("test-session-2", "main", "test-model")
            .unwrap();

        store
            .append_message("test-session-2", &Message::system("You are helpful."))
            .unwrap();
        store
            .append_message("test-session-2", &Message::user("What is 2+2?"))
            .unwrap();
        store
            .append_message("test-session-2", &Message::assistant("4"))
            .unwrap();

        let llm_msgs = store.load_llm_messages("test-session-2").unwrap();
        assert_eq!(llm_msgs.len(), 3);
        assert!(matches!(llm_msgs[0].role, Role::System));
        assert!(matches!(llm_msgs[1].role, Role::User));
        assert!(matches!(llm_msgs[2].role, Role::Assistant));
    }

    #[test]
    fn test_list_sessions() {
        let store = SessionStore::open_memory().unwrap();
        store
            .create_session("session-a", "main", "model-a")
            .unwrap();
        store
            .create_session("session-b", "main", "model-b")
            .unwrap();
        store
            .create_session("session-c", "other", "model-c")
            .unwrap();

        store
            .append_message("session-a", &Message::user("msg1"))
            .unwrap();
        store
            .append_message("session-a", &Message::assistant("reply1"))
            .unwrap();
        store
            .append_message("session-b", &Message::user("msg2"))
            .unwrap();

        let main_sessions = store.list_sessions("main", 10).unwrap();
        assert_eq!(main_sessions.len(), 2);

        let other_sessions = store.list_sessions("other", 10).unwrap();
        assert_eq!(other_sessions.len(), 1);
    }

    #[test]
    fn test_latest_session_key() {
        let store = SessionStore::open_memory().unwrap();
        assert!(store.latest_session_key("main").unwrap().is_none());

        store
            .create_session("old-session", "main", "model")
            .unwrap();
        store
            .append_message("old-session", &Message::user("old"))
            .unwrap();

        // Small delay to ensure different updated_at_ms
        std::thread::sleep(std::time::Duration::from_millis(10));

        store
            .create_session("new-session", "main", "model")
            .unwrap();
        store
            .append_message("new-session", &Message::user("new"))
            .unwrap();

        let latest = store.latest_session_key("main").unwrap();
        assert_eq!(latest.as_deref(), Some("new-session"));
    }

    #[test]
    fn test_token_tracking() {
        let store = SessionStore::open_memory().unwrap();
        store
            .create_session("token-test", "main", "model")
            .unwrap();
        store.add_tokens("token-test", 100).unwrap();
        store.add_tokens("token-test", 250).unwrap();

        let sessions = store.list_sessions("main", 10).unwrap();
        assert_eq!(sessions[0].total_tokens, 350);
    }

    #[test]
    fn test_tool_call_persistence() {
        let store = SessionStore::open_memory().unwrap();
        store
            .create_session("tool-test", "main", "model")
            .unwrap();

        let tool_calls = vec![crate::llm::ToolCall {
            id: "call_123".to_string(),
            call_type: "function".to_string(),
            function: crate::llm::FunctionCall {
                name: "exec".to_string(),
                arguments: r#"{"command":"ls"}"#.to_string(),
            },
        }];

        let msg = Message::assistant_tool_calls(tool_calls, None);
        store.append_message("tool-test", &msg).unwrap();

        let result_msg = Message::tool_result("call_123", "file1.txt\nfile2.txt");
        store.append_message("tool-test", &result_msg).unwrap();

        let loaded = store.load_llm_messages("tool-test").unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(loaded[0].tool_calls.is_some());
        assert_eq!(loaded[0].tool_calls.as_ref().unwrap().len(), 1);
        assert_eq!(loaded[1].tool_call_id.as_deref(), Some("call_123"));
    }

    #[test]
    fn test_migrate_old_session_keys() {
        let store = SessionStore::open_memory().unwrap();

        // Create old-format sessions (3 parts: prefix:agent:channel)
        store.create_session("tg:main:12345", "main", "model").unwrap();
        store.append_message("tg:main:12345", &Message::user("hello")).unwrap();

        store.create_session("dc:main:67890", "main", "model").unwrap();
        store.append_message("dc:main:67890", &Message::user("hi discord")).unwrap();

        // Create new-format session (4 parts: prefix:agent:user:channel) — should NOT be migrated
        store.create_session("tg:main:999:12345", "main", "model").unwrap();

        let migrated = store.migrate_old_session_keys().unwrap();
        assert_eq!(migrated, 2);

        // Old keys should no longer exist
        let old_msgs = store.load_messages("tg:main:12345").unwrap();
        assert!(old_msgs.is_empty());

        // New keys should have the messages
        let new_msgs = store.load_messages("tg:main:0:12345").unwrap();
        assert_eq!(new_msgs.len(), 1);
        assert_eq!(new_msgs[0].content.as_deref(), Some("hello"));

        let dc_msgs = store.load_messages("dc:main:0:67890").unwrap();
        assert_eq!(dc_msgs.len(), 1);
        assert_eq!(dc_msgs[0].content.as_deref(), Some("hi discord"));

        // Running again should migrate 0
        let migrated2 = store.migrate_old_session_keys().unwrap();
        assert_eq!(migrated2, 0);
    }

    #[test]
    fn test_prune_old_sessions() {
        let store = SessionStore::open_memory().unwrap();

        // Create sessions
        store.create_session("old-session", "main", "model").unwrap();
        store.append_message("old-session", &Message::user("old msg")).unwrap();
        store.create_session("new-session", "main", "model").unwrap();
        store.append_message("new-session", &Message::user("new msg")).unwrap();

        // Manually backdate the "old" session by updating its updated_at_ms
        store.conn.execute(
            "UPDATE sessions SET updated_at_ms = ?1 WHERE session_key = 'old-session'",
            params![1_000_000], // very old timestamp
        ).unwrap();

        // Prune sessions older than 1 day
        let pruned = store.prune_old_sessions(1).unwrap();
        assert_eq!(pruned, 1);

        // Old session should be gone
        let old_msgs = store.load_messages("old-session").unwrap();
        assert!(old_msgs.is_empty());

        // New session should still exist
        let new_msgs = store.load_messages("new-session").unwrap();
        assert_eq!(new_msgs.len(), 1);

        // Pruning again should find nothing
        let pruned2 = store.prune_old_sessions(1).unwrap();
        assert_eq!(pruned2, 0);
    }
}
