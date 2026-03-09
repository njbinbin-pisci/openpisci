use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Data models
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: Option<String>,
    pub status: String,
    /// Origin of this session: "chat" (UI), "im_telegram", "im_feishu", etc.
    pub source: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub message_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
    /// JSON array of ToolUse blocks for assistant messages that made tool calls.
    /// Serialized form of Vec<ContentBlock::ToolUse>.
    #[serde(default)]
    pub tool_calls_json: Option<String>,
    /// JSON array of ToolResult blocks for user messages that carry tool results.
    /// Serialized form of Vec<ContentBlock::ToolResult>.
    #[serde(default)]
    pub tool_results_json: Option<String>,
    /// 1-based index of the conversation turn this message belongs to.
    /// A "turn" starts with each user message.
    #[serde(default)]
    pub turn_index: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub content: String,
    pub category: String,
    pub confidence: f64,
    pub source_session_id: Option<String>,
    pub memory_type: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledTask {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub cron_expression: String,
    pub task_prompt: String,
    pub status: String,
    pub last_run_status: Option<String>,
    pub run_count: i64,
    pub last_run_at: Option<DateTime<Utc>>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub icon: String,
    pub config: String, // JSON string
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: String,
    pub session_id: String,
    pub timestamp: DateTime<Utc>,
    pub tool_name: String,
    pub action: String,
    pub input_summary: Option<String>,
    pub result_summary: Option<String>,
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskState {
    pub id: String,
    pub scope_type: String,
    pub scope_id: String,
    pub goal: String,
    pub state_json: String,
    pub summary: String,
    pub status: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Database
// ---------------------------------------------------------------------------

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("Failed to open database at {:?}", path))?;

        // Enable WAL mode for better concurrent read performance
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                title TEXT,
                status TEXT NOT NULL DEFAULT 'idle',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                message_count INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS messages (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at TEXT NOT NULL,
                FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, created_at);

            CREATE TABLE IF NOT EXISTS memories (
                id TEXT PRIMARY KEY,
                content TEXT NOT NULL,
                category TEXT NOT NULL DEFAULT 'general',
                confidence REAL NOT NULL DEFAULT 0.7,
                source_session_id TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS scheduled_tasks (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                cron_expression TEXT NOT NULL,
                task_prompt TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'active',
                run_count INTEGER NOT NULL DEFAULT 0,
                last_run_at TEXT,
                next_run_at TEXT,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS skills (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                icon TEXT NOT NULL DEFAULT '',
                config TEXT NOT NULL DEFAULT '{}'
            );

            CREATE TABLE IF NOT EXISTS audit_log (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                tool_name TEXT NOT NULL,
                action TEXT NOT NULL,
                input_summary TEXT,
                result_summary TEXT,
                is_error INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_audit_session ON audit_log(session_id, timestamp);
            CREATE INDEX IF NOT EXISTS idx_audit_tool ON audit_log(tool_name, timestamp);
        ")?;

        // Add last_run_status to scheduled_tasks (ignore if already exists)
        let _ = self.conn.execute(
            "ALTER TABLE scheduled_tasks ADD COLUMN last_run_status TEXT",
            [],
        );

        // Add source column to sessions for IM origin tracking (ignore if already exists)
        let _ = self.conn.execute(
            "ALTER TABLE sessions ADD COLUMN source TEXT NOT NULL DEFAULT 'chat'",
            [],
        );

        // Memory enhancement: add embedding and memory_type columns (ignore if already exist)
        let _ = self.conn.execute("ALTER TABLE memories ADD COLUMN embedding BLOB", []);
        let _ = self.conn.execute("ALTER TABLE memories ADD COLUMN memory_type TEXT NOT NULL DEFAULT 'personal'", []);

        // Context management: add tool call persistence columns to messages (ignore if already exist)
        let _ = self.conn.execute("ALTER TABLE messages ADD COLUMN tool_calls_json TEXT", []);
        let _ = self.conn.execute("ALTER TABLE messages ADD COLUMN tool_results_json TEXT", []);
        let _ = self.conn.execute("ALTER TABLE messages ADD COLUMN turn_index INTEGER", []);

        // Agent checkpoints for crash recovery
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS agent_checkpoints (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                iteration INTEGER NOT NULL,
                messages_json TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'running',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_checkpoint_session ON agent_checkpoints(session_id, updated_at);
        ")?;

        self.conn.execute_batch("
            -- FTS5 full-text search for memories
            CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                content,
                content=memories,
                content_rowid=rowid
            );

            -- Triggers to keep FTS5 in sync
            CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
                INSERT INTO memories_fts(rowid, content) VALUES (new.rowid, new.content);
            END;
            CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, content) VALUES('delete', old.rowid, old.content);
            END;
            CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, content) VALUES('delete', old.rowid, old.content);
                INSERT INTO memories_fts(rowid, content) VALUES (new.rowid, new.content);
            END;

            -- Embedding cache to avoid redundant API calls
            CREATE TABLE IF NOT EXISTS embedding_cache (
                content_hash TEXT PRIMARY KEY,
                embedding BLOB NOT NULL,
                created_at TEXT NOT NULL
            );
        ")?;

        // Fish instances table (user-activated sub-Agents)
        let _ = self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS fish_instances (
                fish_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'active',
                user_config TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
        ");

        // Task state table for structured task progress tracking.
        // scope_type: 'session' (chat) or 'scheduled_task' (scheduler).
        // state_json stores structured progress: goal, done_items, pending_items, etc.
        let _ = self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS task_states (
                id TEXT PRIMARY KEY,
                scope_type TEXT NOT NULL DEFAULT 'session',
                scope_id TEXT NOT NULL,
                goal TEXT NOT NULL DEFAULT '',
                state_json TEXT NOT NULL DEFAULT '{}',
                summary TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'active',
                version INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_task_states_scope ON task_states(scope_type, scope_id);
        ");

        // ---------- Koi system tables (v2) ----------

        // Koi: persistent independent Agents
        let _ = self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS kois (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                icon TEXT NOT NULL DEFAULT '🐡',
                color TEXT NOT NULL DEFAULT '#7c6af7',
                system_prompt TEXT NOT NULL DEFAULT '',
                description TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'idle',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
        ");

        // Koi todo items (shared board)
        let _ = self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS koi_todos (
                id TEXT PRIMARY KEY,
                owner_id TEXT NOT NULL,
                title TEXT NOT NULL,
                description TEXT DEFAULT '',
                status TEXT NOT NULL DEFAULT 'todo',
                priority TEXT DEFAULT 'medium',
                assigned_by TEXT DEFAULT '',
                pool_session_id TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_koi_todos_owner ON koi_todos(owner_id);
            CREATE INDEX IF NOT EXISTS idx_koi_todos_status ON koi_todos(status);
        ");

        // Chat Pool sessions and messages
        let _ = self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS pool_sessions (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS pool_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                pool_session_id TEXT NOT NULL,
                sender_id TEXT NOT NULL,
                content TEXT NOT NULL,
                msg_type TEXT DEFAULT 'text',
                metadata TEXT DEFAULT '{}',
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_pool_messages_session ON pool_messages(pool_session_id, created_at);
        ");

        // Memory isolation: add owner_id to memories (default 'pisci' for existing records)
        let _ = self.conn.execute(
            "ALTER TABLE memories ADD COLUMN owner_id TEXT NOT NULL DEFAULT 'pisci'",
            [],
        );

        // One-time deduplication: remove duplicate messages caused by a previous bug where
        // persist_agent_turn saved the full message history (including already-stored messages).
        // Keep the earliest row (lowest rowid) for each (session_id, role, content) group.
        let _ = self.conn.execute_batch("
            DELETE FROM messages
            WHERE rowid NOT IN (
                SELECT MIN(rowid)
                FROM messages
                GROUP BY session_id, role, content, COALESCE(tool_calls_json,''), COALESCE(tool_results_json,'')
            );
        ");

        // Seed default skills if empty
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM skills",
            [],
            |r| r.get(0),
        )?;
        if count == 0 {
            self.seed_skills()?;
        }

        Ok(())
    }

    fn seed_skills(&self) -> Result<()> {
        let skills = vec![
            ("web-search", "Web Search", "Search the web for information", true, "🔍"),
            ("shell", "Shell / PowerShell", "Execute shell commands via PowerShell", true, "💻"),
            ("file-ops", "File Operations", "Read, write and edit files", true, "📁"),
            ("uia", "Windows UI Automation", "Control Windows desktop apps via UIA", true, "🖥️"),
            ("screen-vision", "Screen Vision", "Screenshot + Vision AI fallback", true, "👁️"),
            ("scheduled-tasks", "Scheduled Tasks", "Recurring automated tasks", true, "⏰"),
            ("docx", "Word Document", "Generate .docx documents", true, "📄"),
            ("xlsx", "Excel Spreadsheet", "Generate .xlsx spreadsheets", true, "📊"),
        ];
        for (id, name, desc, enabled, icon) in skills {
            self.conn.execute(
                "INSERT OR IGNORE INTO skills (id, name, description, enabled, icon, config) VALUES (?1, ?2, ?3, ?4, ?5, '{}')",
                params![id, name, desc, enabled as i64, icon],
            )?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Sessions
    // ------------------------------------------------------------------

    pub fn create_session(&self, title: Option<&str>) -> Result<Session> {
        self.create_session_with_source(title, "chat")
    }

    pub fn create_session_with_source(&self, title: Option<&str>, source: &str) -> Result<Session> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        self.conn.execute(
            "INSERT INTO sessions (id, title, status, source, created_at, updated_at, message_count) VALUES (?1, ?2, 'idle', ?3, ?4, ?4, 0)",
            params![id, title, source, now_str],
        )?;
        Ok(Session {
            id,
            title: title.map(String::from),
            status: "idle".into(),
            source: source.to_string(),
            created_at: now,
            updated_at: now,
            message_count: 0,
        })
    }

    /// Idempotent: create a session with a fixed `id` for IM routing.
    /// If it already exists, return it as-is (updating `updated_at` is skipped
    /// to preserve chronological ordering in the session list).
    pub fn ensure_im_session(&self, session_id: &str, title: &str, source: &str) -> Result<Session> {
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        // INSERT OR IGNORE — no-op if the session already exists
        self.conn.execute(
            "INSERT OR IGNORE INTO sessions (id, title, status, source, created_at, updated_at, message_count) VALUES (?1, ?2, 'idle', ?3, ?4, ?4, 0)",
            params![session_id, title, source, now_str],
        )?;
        // Fetch current record (may have been created just now or earlier)
        let session = self.conn.query_row(
            "SELECT id, title, status, source, created_at, updated_at, message_count FROM sessions WHERE id = ?1",
            params![session_id],
            |r| Ok(Session {
                id: r.get(0)?,
                title: r.get(1)?,
                status: r.get(2)?,
                source: r.get(3)?,
                created_at: r.get::<_, String>(4)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
                updated_at: r.get::<_, String>(5)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
                message_count: r.get(6)?,
            }),
        )?;
        Ok(session)
    }

    pub fn list_sessions(&self, limit: i64, offset: i64) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, status, COALESCE(source, 'chat'), created_at, updated_at, message_count FROM sessions ORDER BY updated_at DESC LIMIT ?1 OFFSET ?2"
        )?;
        let rows = stmt.query_map(params![limit, offset], |r| {
            Ok(Session {
                id: r.get(0)?,
                title: r.get(1)?,
                status: r.get(2)?,
                source: r.get(3)?,
                created_at: r.get::<_, String>(4)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
                updated_at: r.get::<_, String>(5)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
                message_count: r.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    pub fn delete_session(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn rename_session(&self, id: &str, title: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![title, now, id],
        )?;
        Ok(())
    }

    pub fn update_session_status(&self, id: &str, status: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE sessions SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status, now, id],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Messages
    // ------------------------------------------------------------------

    pub fn append_message(&self, session_id: &str, role: &str, content: &str) -> Result<ChatMessage> {
        self.append_message_full(session_id, role, content, None, None, None)
    }

    /// Persist a message with optional tool call data and turn index.
    /// `tool_calls_json`: JSON array of ToolUse blocks (for assistant messages).
    /// `tool_results_json`: JSON array of ToolResult blocks (for user/tool messages).
    /// `turn_index`: 1-based conversation turn counter.
    pub fn append_message_full(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        tool_calls_json: Option<&str>,
        tool_results_json: Option<&str>,
        turn_index: Option<i64>,
    ) -> Result<ChatMessage> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        self.conn.execute(
            "INSERT INTO messages (id, session_id, role, content, created_at, tool_calls_json, tool_results_json, turn_index) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![id, session_id, role, content, now_str, tool_calls_json, tool_results_json, turn_index],
        )?;
        // Update session message count and updated_at
        self.conn.execute(
            "UPDATE sessions SET message_count = message_count + 1, updated_at = ?1 WHERE id = ?2",
            params![now_str, session_id],
        )?;
        Ok(ChatMessage {
            id,
            session_id: session_id.to_string(),
            role: role.to_string(),
            content: content.to_string(),
            created_at: now,
            tool_calls_json: tool_calls_json.map(|s| s.to_string()),
            tool_results_json: tool_results_json.map(|s| s.to_string()),
            turn_index,
        })
    }

    pub fn get_messages(&self, session_id: &str, limit: i64, offset: i64) -> Result<Vec<ChatMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, role, content, created_at, tool_calls_json, tool_results_json, turn_index \
             FROM messages WHERE session_id = ?1 ORDER BY created_at ASC, rowid ASC LIMIT ?2 OFFSET ?3"
        )?;
        let rows = stmt.query_map(params![session_id, limit, offset], |r| {
            Ok(ChatMessage {
                id: r.get(0)?,
                session_id: r.get(1)?,
                role: r.get(2)?,
                content: r.get(3)?,
                created_at: r.get::<_, String>(4)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
                tool_calls_json: r.get(5)?,
                tool_results_json: r.get(6)?,
                turn_index: r.get(7)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    /// Fetch the latest `limit` messages for a session, ordered chronologically (oldest first).
    /// Unlike `get_messages`, this always includes the most recent messages rather than the oldest,
    /// which is critical for building LLM context when a session has many messages.
    pub fn get_messages_latest(&self, session_id: &str, limit: i64) -> Result<Vec<ChatMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, role, content, created_at, tool_calls_json, tool_results_json, turn_index \
             FROM messages WHERE session_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT ?2"
        )?;
        let rows = stmt.query_map(params![session_id, limit], |r| {
            Ok(ChatMessage {
                id: r.get(0)?,
                session_id: r.get(1)?,
                role: r.get(2)?,
                content: r.get(3)?,
                created_at: r.get::<_, String>(4)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
                tool_calls_json: r.get(5)?,
                tool_results_json: r.get(6)?,
                turn_index: r.get(7)?,
            })
        })?;
        let mut msgs: Vec<ChatMessage> = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        msgs.reverse(); // Return in chronological order (oldest first)
        Ok(msgs)
    }

    // ------------------------------------------------------------------
    // Memories
    // ------------------------------------------------------------------

    pub fn list_memories(&self) -> Result<Vec<Memory>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, category, confidence, source_session_id, created_at, updated_at FROM memories ORDER BY confidence DESC, updated_at DESC"
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Memory {
                id: r.get(0)?,
                content: r.get(1)?,
                category: r.get(2)?,
                confidence: r.get(3)?,
                source_session_id: r.get(4)?,
                memory_type: "personal".to_string(),
                created_at: r.get::<_, String>(5)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
                updated_at: r.get::<_, String>(6)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    /// Save a memory with dedup: if a very similar memory already exists (same category,
    /// high content overlap), update it instead of creating a duplicate.
    pub fn save_memory(&self, content: &str, category: &str, confidence: f64, source_session_id: Option<&str>) -> Result<Memory> {
        // Dedup: check for existing memories in the same category with high content overlap
        if let Some(existing) = self.find_similar_memory(content, category)? {
            let now_str = Utc::now().to_rfc3339();
            let new_confidence = confidence.max(existing.confidence);
            self.conn.execute(
                "UPDATE memories SET content = ?1, confidence = ?2, updated_at = ?3 WHERE id = ?4",
                params![content, new_confidence, now_str, existing.id],
            )?;
            tracing::info!("Memory dedup: updated existing memory {} instead of creating duplicate", existing.id);
            return Ok(Memory {
                id: existing.id,
                content: content.to_string(),
                category: category.to_string(),
                confidence: new_confidence,
                source_session_id: existing.source_session_id,
                memory_type: existing.memory_type,
                created_at: existing.created_at,
                updated_at: Utc::now(),
            });
        }

        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        self.conn.execute(
            "INSERT INTO memories (id, content, category, confidence, source_session_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![id, content, category, confidence, source_session_id, now_str],
        )?;
        Ok(Memory {
            id,
            content: content.to_string(),
            category: category.to_string(),
            confidence,
            source_session_id: source_session_id.map(String::from),
            memory_type: "personal".to_string(),
            created_at: now,
            updated_at: now,
        })
    }

    /// Find a memory in the same category that has high content overlap with the given text.
    /// Uses word-level Jaccard similarity (threshold: 0.6).
    fn find_similar_memory(&self, content: &str, category: &str) -> Result<Option<Memory>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, category, confidence, source_session_id, memory_type, created_at, updated_at \
             FROM memories WHERE category = ?1 ORDER BY updated_at DESC LIMIT 50"
        )?;
        let rows = stmt.query_map(params![category], |r| {
            Ok(Memory {
                id: r.get(0)?,
                content: r.get(1)?,
                category: r.get(2)?,
                confidence: r.get(3)?,
                source_session_id: r.get(4)?,
                memory_type: r.get::<_, String>(5).unwrap_or_else(|_| "personal".to_string()),
                created_at: r.get::<_, String>(6)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
                updated_at: r.get::<_, String>(7)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
            })
        })?;

        let new_words: std::collections::HashSet<&str> = content.split_whitespace().collect();
        if new_words.is_empty() { return Ok(None); }

        for mem in rows.flatten() {
            let existing_words: std::collections::HashSet<&str> = mem.content.split_whitespace().collect();
            if existing_words.is_empty() { continue; }
            let intersection = new_words.intersection(&existing_words).count();
            let union = new_words.union(&existing_words).count();
            let jaccard = intersection as f64 / union as f64;
            if jaccard >= 0.6 {
                return Ok(Some(mem));
            }
        }
        Ok(None)
    }

    pub fn delete_memory(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn clear_memories(&self) -> Result<()> {
        self.conn.execute("DELETE FROM memories", [])?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Vector / Embedding support
    // ------------------------------------------------------------------

    /// Store a floating-point embedding for an existing memory row.
    pub fn store_embedding(&self, memory_id: &str, embedding: &[f32]) -> Result<()> {
        let bytes = crate::memory::vector::embedding_to_bytes(embedding);
        self.conn.execute(
            "UPDATE memories SET embedding = ?1 WHERE id = ?2",
            params![bytes, memory_id],
        )?;
        Ok(())
    }

    /// Retrieve all memories that have an embedding stored.
    pub fn list_memories_with_embeddings(&self) -> Result<Vec<(Memory, Vec<f32>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, category, confidence, source_session_id, created_at, updated_at, embedding \
             FROM memories WHERE embedding IS NOT NULL"
        )?;
        let rows = stmt.query_map([], |r| {
            let embedding_bytes: Vec<u8> = r.get(7)?;
            Ok((
                Memory {
                    id: r.get(0)?,
                    content: r.get(1)?,
                    category: r.get(2)?,
                    confidence: r.get(3)?,
                    source_session_id: r.get(4)?,
                    memory_type: "personal".to_string(),
                    created_at: r.get::<_, String>(5)?
                        .parse::<DateTime<Utc>>()
                        .unwrap_or_else(|_| Utc::now()),
                    updated_at: r.get::<_, String>(6)?
                        .parse::<DateTime<Utc>>()
                        .unwrap_or_else(|_| Utc::now()),
                },
                embedding_bytes,
            ))
        })?;
        let pairs: rusqlite::Result<Vec<(Memory, Vec<u8>)>> = rows.collect();
        let pairs = pairs?;
        Ok(pairs
            .into_iter()
            .map(|(m, bytes)| {
                let embedding = crate::memory::vector::bytes_to_embedding(&bytes);
                (m, embedding)
            })
            .collect())
    }

    /// Scan memories with vector similarity against a query embedding.
    /// Returns (Memory, cosine_score) pairs sorted by descending score.
    pub fn search_by_embedding(&self, query_vec: &[f32], top_k: usize) -> Result<Vec<(Memory, f32)>> {
        let all = self.list_memories_with_embeddings()?;
        let mut scored: Vec<(Memory, f32)> = all
            .into_iter()
            .map(|(m, emb)| {
                let score = crate::memory::vector::cosine_similarity(query_vec, &emb);
                (m, score)
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);
        Ok(scored)
    }

    /// Full-text search using FTS5. Returns (memory_id, bm25_score) pairs.
    pub fn fts_search(&self, query: &str, top_k: usize) -> Result<Vec<(String, f32)>> {
        // Sanitise query for FTS5: escape special chars
        let safe_query = query
            .replace('"', "\"\"")
            .replace(['*', '^'], "");
        let fts_query = format!("\"{}\"", safe_query);

        let mut stmt = self.conn.prepare(
            "SELECT m.id, bm25(memories_fts) AS score \
             FROM memories_fts \
             JOIN memories m ON m.rowid = memories_fts.rowid \
             WHERE memories_fts MATCH ?1 \
             ORDER BY score \
             LIMIT ?2"
        )?;
        let rows = stmt.query_map(params![fts_query, top_k as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)? as f32))
        })?;
        let results: rusqlite::Result<Vec<_>> = rows.collect();
        // bm25 returns negative scores; negate to make higher = better
        Ok(results?.into_iter().map(|(id, s)| (id, -s)).collect())
    }

    // ------------------------------------------------------------------
    // Skills
    // ------------------------------------------------------------------

    pub fn list_skills(&self) -> Result<Vec<Skill>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, enabled, icon, config FROM skills ORDER BY name"
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Skill {
                id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
                enabled: r.get::<_, i64>(3)? != 0,
                icon: r.get(4)?,
                config: r.get(5)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    pub fn set_skill_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE skills SET enabled = ?1 WHERE id = ?2",
            params![enabled as i64, id],
        )?;
        Ok(())
    }

    /// Remove a skill record from the DB by ID.
    pub fn delete_skill(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM skills WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Insert or update a skill record in the DB.
    /// Uses the skill name (lowercased, sanitised) as the ID.
    /// If a record with the same ID already exists it is updated in-place.
    pub fn upsert_skill(&self, id: &str, name: &str, description: &str, icon: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO skills (id, name, description, enabled, icon, config) \
             VALUES (?1, ?2, ?3, 1, ?4, '{}') \
             ON CONFLICT(id) DO UPDATE SET name=excluded.name, description=excluded.description, icon=excluded.icon",
            params![id, name, description, icon],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Scheduled tasks
    // ------------------------------------------------------------------

    pub fn list_tasks(&self) -> Result<Vec<ScheduledTask>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, cron_expression, task_prompt, status, last_run_status, run_count, last_run_at, next_run_at, created_at FROM scheduled_tasks ORDER BY created_at DESC"
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ScheduledTask {
                id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
                cron_expression: r.get(3)?,
                task_prompt: r.get(4)?,
                status: r.get(5)?,
                last_run_status: r.get(6)?,
                run_count: r.get(7)?,
                last_run_at: r.get::<_, Option<String>>(8)?.and_then(|s| s.parse().ok()),
                next_run_at: r.get::<_, Option<String>>(9)?.and_then(|s| s.parse().ok()),
                created_at: r.get::<_, String>(10)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    pub fn get_task(&self, id: &str) -> Result<Option<ScheduledTask>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, cron_expression, task_prompt, status, last_run_status, run_count, last_run_at, next_run_at, created_at FROM scheduled_tasks WHERE id = ?1"
        )?;
        let mut rows = stmt.query_map(params![id], |r| {
            Ok(ScheduledTask {
                id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
                cron_expression: r.get(3)?,
                task_prompt: r.get(4)?,
                status: r.get(5)?,
                last_run_status: r.get(6)?,
                run_count: r.get(7)?,
                last_run_at: r.get::<_, Option<String>>(8)?.and_then(|s| s.parse().ok()),
                next_run_at: r.get::<_, Option<String>>(9)?.and_then(|s| s.parse().ok()),
                created_at: r.get::<_, String>(10)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn create_task(&self, name: &str, description: Option<&str>, cron_expression: &str, task_prompt: &str) -> Result<ScheduledTask> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        self.conn.execute(
            "INSERT INTO scheduled_tasks (id, name, description, cron_expression, task_prompt, status, run_count, created_at) VALUES (?1, ?2, ?3, ?4, ?5, 'active', 0, ?6)",
            params![id, name, description, cron_expression, task_prompt, now_str],
        )?;
        Ok(ScheduledTask {
            id,
            name: name.to_string(),
            description: description.map(String::from),
            cron_expression: cron_expression.to_string(),
            task_prompt: task_prompt.to_string(),
            status: "active".into(),
            last_run_status: None,
            run_count: 0,
            last_run_at: None,
            next_run_at: None,
            created_at: now,
        })
    }

    pub fn update_task(&self, id: &str, name: Option<&str>, cron_expression: Option<&str>, task_prompt: Option<&str>, status: Option<&str>) -> Result<()> {
        if let Some(n) = name {
            self.conn.execute("UPDATE scheduled_tasks SET name = ?1 WHERE id = ?2", params![n, id])?;
        }
        if let Some(c) = cron_expression {
            self.conn.execute("UPDATE scheduled_tasks SET cron_expression = ?1 WHERE id = ?2", params![c, id])?;
        }
        if let Some(p) = task_prompt {
            self.conn.execute("UPDATE scheduled_tasks SET task_prompt = ?1 WHERE id = ?2", params![p, id])?;
        }
        if let Some(s) = status {
            self.conn.execute("UPDATE scheduled_tasks SET status = ?1 WHERE id = ?2", params![s, id])?;
        }
        Ok(())
    }

    pub fn delete_task(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM scheduled_tasks WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn record_task_run(&self, id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE scheduled_tasks SET run_count = run_count + 1, last_run_at = ?1, last_run_status = 'running' WHERE id = ?2",
            params![now, id],
        )?;
        Ok(())
    }

    /// Update the last_run_status for a task: "success", "failed", or "running".
    pub fn update_task_run_status(&self, id: &str, run_status: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE scheduled_tasks SET last_run_status = ?1 WHERE id = ?2",
            params![run_status, id],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Audit Log
    // ------------------------------------------------------------------

    pub fn append_audit(
        &self,
        session_id: &str,
        tool_name: &str,
        action: &str,
        input_summary: Option<&str>,
        result_summary: Option<&str>,
        is_error: bool,
    ) -> Result<()> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO audit_log (id, session_id, timestamp, tool_name, action, input_summary, result_summary, is_error) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![id, session_id, now, tool_name, action, input_summary, result_summary, is_error as i64],
        )?;
        Ok(())
    }

    pub fn get_audit_log(
        &self,
        session_id: Option<&str>,
        tool_name: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<AuditEntry>> {
        let mut query = String::from(
            "SELECT id, session_id, timestamp, tool_name, action, input_summary, result_summary, is_error \
             FROM audit_log WHERE 1=1"
        );
        let mut bind_values: Vec<String> = Vec::new();

        if let Some(sid) = session_id {
            query.push_str(&format!(" AND session_id = ?{}", bind_values.len() + 1));
            bind_values.push(sid.to_string());
        }
        if let Some(tool) = tool_name {
            query.push_str(&format!(" AND tool_name = ?{}", bind_values.len() + 1));
            bind_values.push(tool.to_string());
        }
        query.push_str(&format!(
            " ORDER BY timestamp DESC LIMIT {} OFFSET {}",
            limit, offset
        ));

        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(bind_values.iter()), |r| {
            Ok(AuditEntry {
                id: r.get(0)?,
                session_id: r.get(1)?,
                timestamp: r.get::<_, String>(2)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
                tool_name: r.get(3)?,
                action: r.get(4)?,
                input_summary: r.get(5)?,
                result_summary: r.get(6)?,
                is_error: r.get::<_, i64>(7)? != 0,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    pub fn clear_audit_log(&self, session_id: Option<&str>) -> Result<()> {
        if let Some(sid) = session_id {
            self.conn.execute("DELETE FROM audit_log WHERE session_id = ?1", params![sid])?;
        } else {
            self.conn.execute("DELETE FROM audit_log", [])?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Memory FTS & Embeddings
    // ------------------------------------------------------------------

    pub fn search_memories_fts(&self, query: &str, limit: i64) -> Result<Vec<Memory>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.id, m.content, m.category, m.confidence, m.source_session_id, m.created_at, m.updated_at \
             FROM memories m \
             JOIN memories_fts f ON m.rowid = f.rowid \
             WHERE memories_fts MATCH ?1 \
             ORDER BY rank \
             LIMIT ?2"
        )?;
        let rows = stmt.query_map(params![query, limit], |r| {
            Ok(Memory {
                id: r.get(0)?,
                content: r.get(1)?,
                category: r.get(2)?,
                confidence: r.get(3)?,
                source_session_id: r.get(4)?,
                memory_type: "personal".to_string(),
                created_at: r.get::<_, String>(5)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
                updated_at: r.get::<_, String>(6)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    pub fn save_embedding_cache(&self, content_hash: &str, embedding: &[u8]) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT OR REPLACE INTO embedding_cache (content_hash, embedding, created_at) VALUES (?1, ?2, ?3)",
            params![content_hash, embedding, now],
        )?;
        Ok(())
    }

    pub fn get_embedding_cache(&self, content_hash: &str) -> Result<Option<Vec<u8>>> {
        let mut stmt = self.conn.prepare(
            "SELECT embedding FROM embedding_cache WHERE content_hash = ?1"
        )?;
        let mut rows = stmt.query_map(params![content_hash], |r| r.get::<_, Vec<u8>>(0))?;
        Ok(rows.next().transpose()?)
    }

    pub fn update_memory_embedding(&self, id: &str, embedding: &[u8]) -> Result<()> {
        self.conn.execute(
            "UPDATE memories SET embedding = ?1 WHERE id = ?2",
            params![embedding, id],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Agent Checkpoints
    // ------------------------------------------------------------------

    /// Upsert a checkpoint for the given session. Replaces any existing running checkpoint.
    pub fn upsert_checkpoint(&self, session_id: &str, iteration: usize, messages_json: &str) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        // Delete old running checkpoint for this session first
        self.conn.execute(
            "DELETE FROM agent_checkpoints WHERE session_id = ?1 AND status = 'running'",
            params![session_id],
        )?;
        self.conn.execute(
            "INSERT INTO agent_checkpoints (id, session_id, iteration, messages_json, status, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?5)",
            params![id, session_id, iteration as i64, messages_json, now],
        )?;
        Ok(id)
    }

    /// Mark a checkpoint as completed (success) or failed.
    pub fn finish_checkpoint(&self, session_id: &str, status: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE agent_checkpoints SET status = ?1, updated_at = ?2 \
             WHERE session_id = ?3 AND status = 'running'",
            params![status, now, session_id],
        )?;
        Ok(())
    }

    /// Load a pending (running) checkpoint for a session, if any.
    pub fn load_checkpoint(&self, session_id: &str) -> Result<Option<(usize, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT iteration, messages_json FROM agent_checkpoints \
             WHERE session_id = ?1 AND status = 'running' \
             ORDER BY updated_at DESC LIMIT 1"
        )?;
        let mut rows = stmt.query_map(params![session_id], |r| {
            Ok((r.get::<_, i64>(0)? as usize, r.get::<_, String>(1)?))
        })?;
        Ok(rows.next().transpose()?)
    }

    /// Prune stale checkpoints older than the given number of hours to keep the table small.
    pub fn prune_checkpoints(&self, older_than_hours: i64) -> Result<usize> {
        let cutoff = (Utc::now() - chrono::Duration::hours(older_than_hours)).to_rfc3339();
        let n = self.conn.execute(
            "DELETE FROM agent_checkpoints WHERE created_at < ?1",
            params![cutoff],
        )?;
        Ok(n)
    }

    // ------------------------------------------------------------------
    // Fish instances
    // ------------------------------------------------------------------

    // ------------------------------------------------------------------
    // Task States
    // ------------------------------------------------------------------

    /// Get or create a task state for the given scope (session or scheduled_task).
    pub fn get_or_create_task_state(&self, scope_type: &str, scope_id: &str) -> Result<TaskState> {
        let mut stmt = self.conn.prepare(
            "SELECT id, scope_type, scope_id, goal, state_json, summary, status, version, created_at, updated_at \
             FROM task_states WHERE scope_type = ?1 AND scope_id = ?2 \
             ORDER BY updated_at DESC LIMIT 1"
        )?;
        let mut rows = stmt.query_map(params![scope_type, scope_id], |r| {
            Ok(TaskState {
                id: r.get(0)?,
                scope_type: r.get(1)?,
                scope_id: r.get(2)?,
                goal: r.get(3)?,
                state_json: r.get(4)?,
                summary: r.get(5)?,
                status: r.get(6)?,
                version: r.get(7)?,
                created_at: r.get::<_, String>(8)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
                updated_at: r.get::<_, String>(9)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
            })
        })?;

        if let Some(existing) = rows.next().transpose()? {
            return Ok(existing);
        }

        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO task_states (id, scope_type, scope_id, goal, state_json, summary, status, version, created_at, updated_at) \
             VALUES (?1, ?2, ?3, '', '{}', '', 'active', 1, ?4, ?4)",
            params![id, scope_type, scope_id, now],
        )?;

        Ok(TaskState {
            id,
            scope_type: scope_type.to_string(),
            scope_id: scope_id.to_string(),
            goal: String::new(),
            state_json: "{}".to_string(),
            summary: String::new(),
            status: "active".to_string(),
            version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        })
    }

    /// Update a task state with new goal, state_json, summary, and status.
    pub fn update_task_state(
        &self,
        id: &str,
        goal: Option<&str>,
        state_json: Option<&str>,
        summary: Option<&str>,
        status: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE task_states SET \
             goal = COALESCE(?2, goal), \
             state_json = COALESCE(?3, state_json), \
             summary = COALESCE(?4, summary), \
             status = COALESCE(?5, status), \
             version = version + 1, \
             updated_at = ?6 \
             WHERE id = ?1",
            params![id, goal, state_json, summary, status, now],
        )?;
        Ok(())
    }

    /// Load task state for a scope, if it exists.
    pub fn load_task_state(&self, scope_type: &str, scope_id: &str) -> Result<Option<TaskState>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, scope_type, scope_id, goal, state_json, summary, status, version, created_at, updated_at \
             FROM task_states WHERE scope_type = ?1 AND scope_id = ?2 \
             ORDER BY updated_at DESC LIMIT 1"
        )?;
        let mut rows = stmt.query_map(params![scope_type, scope_id], |r| {
            Ok(TaskState {
                id: r.get(0)?,
                scope_type: r.get(1)?,
                scope_id: r.get(2)?,
                goal: r.get(3)?,
                state_json: r.get(4)?,
                summary: r.get(5)?,
                status: r.get(6)?,
                version: r.get(7)?,
                created_at: r.get::<_, String>(8)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
                updated_at: r.get::<_, String>(9)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    // ------------------------------------------------------------------
    // Koi (persistent Agents)
    // ------------------------------------------------------------------

    pub fn create_koi(
        &self,
        name: &str,
        icon: &str,
        color: &str,
        system_prompt: &str,
        description: &str,
    ) -> Result<crate::koi::KoiDefinition> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        self.conn.execute(
            "INSERT INTO kois (id, name, icon, color, system_prompt, description, status, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'idle', ?7, ?7)",
            params![id, name, icon, color, system_prompt, description, now_str],
        )?;
        Ok(crate::koi::KoiDefinition {
            id,
            name: name.to_string(),
            icon: icon.to_string(),
            color: color.to_string(),
            system_prompt: system_prompt.to_string(),
            description: description.to_string(),
            status: "idle".to_string(),
            created_at: now,
            updated_at: now,
        })
    }

    pub fn list_kois(&self) -> Result<Vec<crate::koi::KoiDefinition>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, icon, color, system_prompt, description, status, created_at, updated_at \
             FROM kois ORDER BY created_at ASC"
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(crate::koi::KoiDefinition {
                id: r.get(0)?,
                name: r.get(1)?,
                icon: r.get(2)?,
                color: r.get(3)?,
                system_prompt: r.get(4)?,
                description: r.get(5)?,
                status: r.get(6)?,
                created_at: r.get::<_, String>(7)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
                updated_at: r.get::<_, String>(8)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    pub fn get_koi(&self, id: &str) -> Result<Option<crate::koi::KoiDefinition>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, icon, color, system_prompt, description, status, created_at, updated_at \
             FROM kois WHERE id = ?1"
        )?;
        let mut rows = stmt.query_map(params![id], |r| {
            Ok(crate::koi::KoiDefinition {
                id: r.get(0)?,
                name: r.get(1)?,
                icon: r.get(2)?,
                color: r.get(3)?,
                system_prompt: r.get(4)?,
                description: r.get(5)?,
                status: r.get(6)?,
                created_at: r.get::<_, String>(7)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
                updated_at: r.get::<_, String>(8)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn update_koi(
        &self,
        id: &str,
        name: Option<&str>,
        icon: Option<&str>,
        color: Option<&str>,
        system_prompt: Option<&str>,
        description: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE kois SET \
             name = COALESCE(?2, name), \
             icon = COALESCE(?3, icon), \
             color = COALESCE(?4, color), \
             system_prompt = COALESCE(?5, system_prompt), \
             description = COALESCE(?6, description), \
             updated_at = ?7 \
             WHERE id = ?1",
            params![id, name, icon, color, system_prompt, description, now],
        )?;
        Ok(())
    }

    pub fn update_koi_status(&self, id: &str, status: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE kois SET status = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, status, now],
        )?;
        Ok(())
    }

    pub fn delete_koi(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM kois WHERE id = ?1", params![id])?;
        self.conn.execute("DELETE FROM koi_todos WHERE owner_id = ?1", params![id])?;
        self.conn.execute("DELETE FROM memories WHERE owner_id = ?1", params![id])?;
        Ok(())
    }

    /// Count memories belonging to a specific owner.
    pub fn count_memories_for_owner(&self, owner_id: &str) -> Result<i64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE owner_id = ?1",
            params![owner_id],
            |r| r.get(0),
        )?;
        Ok(count)
    }

    // ------------------------------------------------------------------
    // Koi Todos (Board)
    // ------------------------------------------------------------------

    pub fn create_koi_todo(
        &self,
        owner_id: &str,
        title: &str,
        description: &str,
        priority: &str,
        assigned_by: &str,
        pool_session_id: Option<&str>,
    ) -> Result<crate::koi::KoiTodo> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        self.conn.execute(
            "INSERT INTO koi_todos (id, owner_id, title, description, status, priority, assigned_by, pool_session_id, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, 'todo', ?5, ?6, ?7, ?8, ?8)",
            params![id, owner_id, title, description, priority, assigned_by, pool_session_id, now_str],
        )?;
        Ok(crate::koi::KoiTodo {
            id,
            owner_id: owner_id.to_string(),
            title: title.to_string(),
            description: description.to_string(),
            status: "todo".to_string(),
            priority: priority.to_string(),
            assigned_by: assigned_by.to_string(),
            pool_session_id: pool_session_id.map(String::from),
            created_at: now,
            updated_at: now,
        })
    }

    pub fn list_koi_todos(&self, owner_id: Option<&str>) -> Result<Vec<crate::koi::KoiTodo>> {
        let sql = if owner_id.is_some() {
            "SELECT id, owner_id, title, description, status, priority, assigned_by, pool_session_id, created_at, updated_at \
             FROM koi_todos WHERE owner_id = ?1 ORDER BY created_at DESC"
        } else {
            "SELECT id, owner_id, title, description, status, priority, assigned_by, pool_session_id, created_at, updated_at \
             FROM koi_todos ORDER BY created_at DESC"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = if let Some(oid) = owner_id {
            stmt.query_map(params![oid], Self::map_koi_todo)?
        } else {
            stmt.query_map([], Self::map_koi_todo)?
        };
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    fn map_koi_todo(r: &rusqlite::Row) -> rusqlite::Result<crate::koi::KoiTodo> {
        Ok(crate::koi::KoiTodo {
            id: r.get(0)?,
            owner_id: r.get(1)?,
            title: r.get(2)?,
            description: r.get(3)?,
            status: r.get(4)?,
            priority: r.get(5)?,
            assigned_by: r.get(6)?,
            pool_session_id: r.get(7)?,
            created_at: r.get::<_, String>(8)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
            updated_at: r.get::<_, String>(9)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
        })
    }

    pub fn update_koi_todo(
        &self,
        id: &str,
        title: Option<&str>,
        description: Option<&str>,
        status: Option<&str>,
        priority: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE koi_todos SET \
             title = COALESCE(?2, title), \
             description = COALESCE(?3, description), \
             status = COALESCE(?4, status), \
             priority = COALESCE(?5, priority), \
             updated_at = ?6 \
             WHERE id = ?1",
            params![id, title, description, status, priority, now],
        )?;
        Ok(())
    }

    pub fn delete_koi_todo(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM koi_todos WHERE id = ?1", params![id])?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Pool Sessions & Messages (Chat Pool)
    // ------------------------------------------------------------------

    pub fn create_pool_session(&self, name: &str) -> Result<crate::koi::PoolSession> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        self.conn.execute(
            "INSERT INTO pool_sessions (id, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
            params![id, name, now_str],
        )?;
        Ok(crate::koi::PoolSession {
            id,
            name: name.to_string(),
            created_at: now,
            updated_at: now,
        })
    }

    pub fn list_pool_sessions(&self) -> Result<Vec<crate::koi::PoolSession>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, created_at, updated_at FROM pool_sessions ORDER BY updated_at DESC"
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(crate::koi::PoolSession {
                id: r.get(0)?,
                name: r.get(1)?,
                created_at: r.get::<_, String>(2)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
                updated_at: r.get::<_, String>(3)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    pub fn delete_pool_session(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM pool_messages WHERE pool_session_id = ?1", params![id])?;
        self.conn.execute("DELETE FROM pool_sessions WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn insert_pool_message(
        &self,
        pool_session_id: &str,
        sender_id: &str,
        content: &str,
        msg_type: &str,
        metadata: &str,
    ) -> Result<crate::koi::PoolMessage> {
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        self.conn.execute(
            "INSERT INTO pool_messages (pool_session_id, sender_id, content, msg_type, metadata, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![pool_session_id, sender_id, content, msg_type, metadata, now_str],
        )?;
        let id = self.conn.last_insert_rowid();
        // Touch pool_session updated_at
        self.conn.execute(
            "UPDATE pool_sessions SET updated_at = ?1 WHERE id = ?2",
            params![now_str, pool_session_id],
        )?;
        Ok(crate::koi::PoolMessage {
            id,
            pool_session_id: pool_session_id.to_string(),
            sender_id: sender_id.to_string(),
            content: content.to_string(),
            msg_type: msg_type.to_string(),
            metadata: metadata.to_string(),
            created_at: now,
        })
    }

    pub fn get_pool_messages(
        &self,
        pool_session_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<crate::koi::PoolMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, pool_session_id, sender_id, content, msg_type, metadata, created_at \
             FROM pool_messages WHERE pool_session_id = ?1 \
             ORDER BY created_at ASC LIMIT ?2 OFFSET ?3"
        )?;
        let rows = stmt.query_map(params![pool_session_id, limit, offset], |r| {
            Ok(crate::koi::PoolMessage {
                id: r.get(0)?,
                pool_session_id: r.get(1)?,
                sender_id: r.get(2)?,
                content: r.get(3)?,
                msg_type: r.get(4)?,
                metadata: r.get(5)?,
                created_at: r.get::<_, String>(6)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }
}
