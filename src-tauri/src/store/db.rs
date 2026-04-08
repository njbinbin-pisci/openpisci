use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, Row};
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
    #[serde(default)]
    pub rolling_summary: String,
    #[serde(default)]
    pub rolling_summary_version: i64,
    #[serde(default)]
    pub total_input_tokens: i64,
    #[serde(default)]
    pub total_output_tokens: i64,
    #[serde(default)]
    pub last_compacted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionContextState {
    pub session_id: String,
    pub rolling_summary: String,
    pub rolling_summary_version: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub last_compacted_at: Option<DateTime<Utc>>,
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
    /// Who owns this memory: "pisci" or a koi_id
    pub owner_id: String,
    /// "private" | "project" | "global"
    pub scope_type: String,
    /// For project scope: pool_session_id; for private: same as owner_id; for global: "global"
    pub scope_id: String,
    /// For private memories: the pool_session_id where this memory was created (NULL = cross-project skill/preference)
    pub project_scope_id: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskSpine {
    #[serde(default)]
    pub goal: String,
    #[serde(default)]
    pub current_step: String,
    #[serde(default)]
    pub done: Vec<String>,
    #[serde(default)]
    pub pending: Vec<String>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub facts: Vec<String>,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub next_questions: Vec<String>,
}

impl TaskState {
    pub fn to_task_spine(&self) -> TaskSpine {
        let mut spine = serde_json::from_str::<TaskSpine>(&self.state_json).unwrap_or_default();
        if spine.goal.trim().is_empty() {
            spine.goal = self.goal.clone();
        }
        if spine.current_step.trim().is_empty() && !self.summary.trim().is_empty() {
            spine.current_step = self.summary.clone();
        }
        spine
    }
}

// ---------------------------------------------------------------------------
// Database
// ---------------------------------------------------------------------------

pub struct Database {
    pub(crate) conn: Connection,
}

fn parse_datetime(value: String) -> DateTime<Utc> {
    value.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now())
}

fn parse_optional_datetime(value: Option<String>) -> Option<DateTime<Utc>> {
    value.and_then(|raw| raw.parse::<DateTime<Utc>>().ok())
}

fn map_session_row(r: &Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: r.get(0)?,
        title: r.get(1)?,
        status: r.get(2)?,
        source: r.get(3)?,
        created_at: parse_datetime(r.get::<_, String>(4)?),
        updated_at: parse_datetime(r.get::<_, String>(5)?),
        message_count: r.get(6)?,
        rolling_summary: r.get(7)?,
        rolling_summary_version: r.get(8)?,
        total_input_tokens: r.get(9)?,
        total_output_tokens: r.get(10)?,
        last_compacted_at: parse_optional_datetime(r.get::<_, Option<String>>(11)?),
    })
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

    /// Open an in-memory database for testing.
    /// Uses the same schema migration as production.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("Failed to open in-memory database")?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                title TEXT,
                status TEXT NOT NULL DEFAULT 'idle',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                message_count INTEGER NOT NULL DEFAULT 0,
                rolling_summary TEXT NOT NULL DEFAULT '',
                rolling_summary_version INTEGER NOT NULL DEFAULT 0,
                total_input_tokens INTEGER NOT NULL DEFAULT 0,
                total_output_tokens INTEGER NOT NULL DEFAULT 0,
                last_compacted_at TEXT
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
        ",
        )?;

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
        let _ = self.conn.execute(
            "ALTER TABLE sessions ADD COLUMN rolling_summary TEXT NOT NULL DEFAULT ''",
            [],
        );
        let _ = self.conn.execute(
            "ALTER TABLE sessions ADD COLUMN rolling_summary_version INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = self.conn.execute(
            "ALTER TABLE sessions ADD COLUMN total_input_tokens INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = self.conn.execute(
            "ALTER TABLE sessions ADD COLUMN total_output_tokens INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = self
            .conn
            .execute("ALTER TABLE sessions ADD COLUMN last_compacted_at TEXT", []);

        // Memory enhancement: add embedding and memory_type columns (ignore if already exist)
        let _ = self
            .conn
            .execute("ALTER TABLE memories ADD COLUMN embedding BLOB", []);
        let _ = self.conn.execute(
            "ALTER TABLE memories ADD COLUMN memory_type TEXT NOT NULL DEFAULT 'personal'",
            [],
        );

        // Context management: add tool call persistence columns to messages (ignore if already exist)
        let _ = self
            .conn
            .execute("ALTER TABLE messages ADD COLUMN tool_calls_json TEXT", []);
        let _ = self
            .conn
            .execute("ALTER TABLE messages ADD COLUMN tool_results_json TEXT", []);
        let _ = self
            .conn
            .execute("ALTER TABLE messages ADD COLUMN turn_index INTEGER", []);

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
        let _ = self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS fish_instances (
                fish_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'active',
                user_config TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
        ",
        );

        // Task state table for structured task progress tracking.
        // scope_type: 'session' (chat) or 'scheduled_task' (scheduler).
        // state_json stores structured progress: goal, done_items, pending_items, etc.
        let _ = self.conn.execute_batch(
            "
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
        ",
        );

        // ---------- Koi system tables (v2) ----------

        // Koi: persistent independent Agents
        let _ = self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS kois (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                role TEXT NOT NULL DEFAULT '',
                icon TEXT NOT NULL DEFAULT '🐡',
                color TEXT NOT NULL DEFAULT '#7c6af7',
                system_prompt TEXT NOT NULL DEFAULT '',
                description TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'idle',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                llm_provider_id TEXT,
                max_iterations INTEGER NOT NULL DEFAULT 0
            );
        ",
        );
        // Migrations: add columns to existing kois tables
        let _ = self
            .conn
            .execute("ALTER TABLE kois ADD COLUMN llm_provider_id TEXT", []);
        let _ = self.conn.execute(
            "ALTER TABLE kois ADD COLUMN max_iterations INTEGER NOT NULL DEFAULT 0",
            [],
        );

        // Koi todo items (shared board)
        let _ = self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS koi_todos (
                id TEXT PRIMARY KEY,
                owner_id TEXT NOT NULL,
                title TEXT NOT NULL,
                description TEXT DEFAULT '',
                status TEXT NOT NULL DEFAULT 'todo',
                priority TEXT DEFAULT 'medium',
                assigned_by TEXT DEFAULT '',
                pool_session_id TEXT,
                claimed_by TEXT,
                claimed_at TEXT,
                depends_on TEXT,
                blocked_reason TEXT,
                result_message_id INTEGER,
                source_type TEXT NOT NULL DEFAULT 'user',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                FOREIGN KEY(owner_id) REFERENCES kois(id) ON DELETE CASCADE,
                FOREIGN KEY(pool_session_id) REFERENCES pool_sessions(id) ON DELETE CASCADE,
                FOREIGN KEY(claimed_by) REFERENCES kois(id) ON DELETE SET NULL
            );
            CREATE INDEX IF NOT EXISTS idx_koi_todos_owner ON koi_todos(owner_id);
            CREATE INDEX IF NOT EXISTS idx_koi_todos_status ON koi_todos(status);
        ",
        );
        // Migrate existing koi_todos table with new columns
        for col in &[
            "ALTER TABLE koi_todos ADD COLUMN claimed_by TEXT",
            "ALTER TABLE koi_todos ADD COLUMN claimed_at TEXT",
            "ALTER TABLE koi_todos ADD COLUMN depends_on TEXT",
            "ALTER TABLE koi_todos ADD COLUMN blocked_reason TEXT",
            "ALTER TABLE koi_todos ADD COLUMN result_message_id INTEGER",
            "ALTER TABLE koi_todos ADD COLUMN source_type TEXT NOT NULL DEFAULT 'user'",
        ] {
            let _ = self.conn.execute(col, []);
        }

        // Chat Pool sessions and messages
        let _ = self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS pool_sessions (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                org_spec TEXT NOT NULL DEFAULT '',
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
                todo_id TEXT,
                reply_to_message_id INTEGER,
                event_type TEXT,
                created_at TEXT NOT NULL,
                FOREIGN KEY(pool_session_id) REFERENCES pool_sessions(id) ON DELETE CASCADE,
                FOREIGN KEY(todo_id) REFERENCES koi_todos(id) ON DELETE SET NULL,
                FOREIGN KEY(reply_to_message_id) REFERENCES pool_messages(id) ON DELETE SET NULL
            );
            CREATE INDEX IF NOT EXISTS idx_pool_messages_session ON pool_messages(pool_session_id, created_at);
            CREATE INDEX IF NOT EXISTS idx_pool_messages_todo ON pool_messages(todo_id);
        ");
        // Migrate existing pool_messages table with new columns
        for col in &[
            "ALTER TABLE pool_messages ADD COLUMN todo_id TEXT",
            "ALTER TABLE pool_messages ADD COLUMN reply_to_message_id INTEGER",
            "ALTER TABLE pool_messages ADD COLUMN event_type TEXT",
        ] {
            let _ = self.conn.execute(col, []);
        }
        // Migrate pool_sessions with org_spec
        let _ = self.conn.execute(
            "ALTER TABLE pool_sessions ADD COLUMN org_spec TEXT NOT NULL DEFAULT ''",
            [],
        );
        // Migrate pool_sessions with status, last_active_at, project_dir
        for col in &[
            "ALTER TABLE pool_sessions ADD COLUMN status TEXT NOT NULL DEFAULT 'active'",
            "ALTER TABLE pool_sessions ADD COLUMN last_active_at TEXT",
            "ALTER TABLE pool_sessions ADD COLUMN project_dir TEXT DEFAULT NULL",
        ] {
            let _ = self.conn.execute(col, []);
        }
        let _ = self.conn.execute(
            "ALTER TABLE kois ADD COLUMN role TEXT NOT NULL DEFAULT ''",
            [],
        );

        if let Ok(normalized) = self.normalize_identifier_references() {
            if normalized > 0 {
                tracing::info!(
                    "Database startup: normalized {} legacy identifier references",
                    normalized
                );
            }
        }

        // Clean up orphaned Koi / pool records from older schemas that lacked FK constraints.
        let _ = self.conn.execute_batch(
            "
            DELETE FROM koi_todos
            WHERE owner_id NOT IN (SELECT id FROM kois);

            DELETE FROM koi_todos
            WHERE pool_session_id IS NOT NULL
              AND pool_session_id NOT IN (SELECT id FROM pool_sessions);

            UPDATE koi_todos
            SET claimed_by = NULL, claimed_at = NULL
            WHERE claimed_by IS NOT NULL
              AND claimed_by NOT IN (SELECT id FROM kois);

            DELETE FROM pool_messages
            WHERE pool_session_id NOT IN (SELECT id FROM pool_sessions);

            UPDATE pool_messages
            SET todo_id = NULL
            WHERE todo_id IS NOT NULL
              AND todo_id NOT IN (SELECT id FROM koi_todos);

            UPDATE pool_messages
            SET reply_to_message_id = NULL
            WHERE reply_to_message_id IS NOT NULL
              AND reply_to_message_id NOT IN (SELECT id FROM pool_messages);
        ",
        );

        // Triggers provide cascade-like cleanup for existing databases created before FK support.
        let _ = self.conn.execute_batch(
            "
            CREATE TRIGGER IF NOT EXISTS trg_pool_sessions_delete_cleanup
            AFTER DELETE ON pool_sessions
            BEGIN
                DELETE FROM pool_messages WHERE pool_session_id = OLD.id;
                DELETE FROM koi_todos WHERE pool_session_id = OLD.id;
            END;

            CREATE TRIGGER IF NOT EXISTS trg_kois_delete_cleanup
            AFTER DELETE ON kois
            BEGIN
                DELETE FROM koi_todos WHERE owner_id = OLD.id;
                UPDATE koi_todos SET claimed_by = NULL, claimed_at = NULL WHERE claimed_by = OLD.id;
                DELETE FROM memories WHERE owner_id = OLD.id;
            END;

            CREATE TRIGGER IF NOT EXISTS trg_koi_todos_delete_cleanup
            AFTER DELETE ON koi_todos
            BEGIN
                UPDATE pool_messages SET todo_id = NULL WHERE todo_id = OLD.id;
            END;
        ",
        );

        // Memory isolation: add owner_id, scope_type, scope_id to memories
        let _ = self.conn.execute(
            "ALTER TABLE memories ADD COLUMN owner_id TEXT NOT NULL DEFAULT 'pisci'",
            [],
        );
        let _ = self.conn.execute(
            "ALTER TABLE memories ADD COLUMN scope_type TEXT NOT NULL DEFAULT 'private'",
            [],
        );
        let _ = self.conn.execute(
            "ALTER TABLE memories ADD COLUMN scope_id TEXT NOT NULL DEFAULT 'pisci'",
            [],
        );
        let _ = self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_memories_owner ON memories(owner_id);
             CREATE INDEX IF NOT EXISTS idx_memories_scope ON memories(scope_type, scope_id);",
        );

        // Memory project tagging: allows private memories to be tagged with the project they were
        // created in, enabling project-priority search while still falling back to cross-project skills.
        let _ = self.conn.execute(
            "ALTER TABLE memories ADD COLUMN project_scope_id TEXT", // NULL = cross-project skill/preference
            [],
        );
        let _ = self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_memories_project_scope ON memories(owner_id, scope_type, project_scope_id);"
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
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM skills", [], |r| r.get(0))?;
        if count == 0 {
            self.seed_skills()?;
        }

        Ok(())
    }

    fn seed_skills(&self) -> Result<()> {
        let skills = vec![
            (
                "web-search",
                "Web Search",
                "Search the web for information",
                true,
                "🔍",
            ),
            (
                "shell",
                "Shell / PowerShell",
                "Execute shell commands via PowerShell",
                true,
                "💻",
            ),
            (
                "file-ops",
                "File Operations",
                "Read, write and edit files",
                true,
                "📁",
            ),
            (
                "uia",
                "Windows UI Automation",
                "Control Windows desktop apps via UIA",
                true,
                "🖥️",
            ),
            (
                "screen-vision",
                "Screen Vision",
                "Screenshot + Vision AI fallback",
                true,
                "👁️",
            ),
            (
                "scheduled-tasks",
                "Scheduled Tasks",
                "Recurring automated tasks",
                true,
                "⏰",
            ),
            (
                "docx",
                "Word Document",
                "Generate .docx documents",
                true,
                "📄",
            ),
            (
                "xlsx",
                "Excel Spreadsheet",
                "Generate .xlsx spreadsheets",
                true,
                "📊",
            ),
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
            rolling_summary: String::new(),
            rolling_summary_version: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            last_compacted_at: None,
        })
    }

    pub fn get_session(&self, id: &str) -> Result<Option<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, status, COALESCE(source, 'chat'), created_at, updated_at, message_count, \
                    COALESCE(rolling_summary, ''), COALESCE(rolling_summary_version, 0), \
                    COALESCE(total_input_tokens, 0), COALESCE(total_output_tokens, 0), last_compacted_at \
             FROM sessions WHERE id = ?1"
        )?;
        let mut rows = stmt.query_map(params![id], map_session_row)?;
        Ok(rows.next().transpose()?)
    }

    pub fn ensure_fixed_session(
        &self,
        session_id: &str,
        title: &str,
        source: &str,
    ) -> Result<Session> {
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        self.conn.execute(
            "INSERT OR IGNORE INTO sessions (id, title, status, source, created_at, updated_at, message_count) VALUES (?1, ?2, 'idle', ?3, ?4, ?4, 0)",
            params![session_id, title, source, now_str],
        )?;
        self.get_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session '{}' missing after ensure", session_id))
    }

    /// Idempotent: create a session with a fixed `id` for IM routing.
    /// If it already exists, return it as-is (updating `updated_at` is skipped
    /// to preserve chronological ordering in the session list).
    pub fn ensure_im_session(
        &self,
        session_id: &str,
        title: &str,
        source: &str,
    ) -> Result<Session> {
        self.ensure_fixed_session(session_id, title, source)
    }

    pub fn list_sessions(&self, limit: i64, offset: i64) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, status, COALESCE(source, 'chat'), created_at, updated_at, message_count, \
                    COALESCE(rolling_summary, ''), COALESCE(rolling_summary_version, 0), \
                    COALESCE(total_input_tokens, 0), COALESCE(total_output_tokens, 0), last_compacted_at \
             FROM sessions ORDER BY updated_at DESC LIMIT ?1 OFFSET ?2"
        )?;
        let rows = stmt.query_map(params![limit, offset], map_session_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn delete_session(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
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

    pub fn get_session_context_state(&self, id: &str) -> Result<Option<SessionContextState>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, COALESCE(rolling_summary, ''), COALESCE(rolling_summary_version, 0), \
                    COALESCE(total_input_tokens, 0), COALESCE(total_output_tokens, 0), last_compacted_at \
             FROM sessions WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], |r| {
            Ok(SessionContextState {
                session_id: r.get(0)?,
                rolling_summary: r.get(1)?,
                rolling_summary_version: r.get(2)?,
                total_input_tokens: r.get(3)?,
                total_output_tokens: r.get(4)?,
                last_compacted_at: parse_optional_datetime(r.get::<_, Option<String>>(5)?),
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn update_session_usage_totals(
        &self,
        session_id: &str,
        input_delta: u32,
        output_delta: u32,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions
             SET total_input_tokens = total_input_tokens + ?1,
                 total_output_tokens = total_output_tokens + ?2
             WHERE id = ?3",
            params![i64::from(input_delta), i64::from(output_delta), session_id],
        )?;
        Ok(())
    }

    pub fn update_session_rolling_summary(
        &self,
        session_id: &str,
        summary: &str,
        version: i64,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE sessions
             SET rolling_summary = ?1,
                 rolling_summary_version = ?2,
                 last_compacted_at = ?3,
                 updated_at = ?3
             WHERE id = ?4",
            params![summary, version, now, session_id],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Messages
    // ------------------------------------------------------------------

    pub fn append_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
    ) -> Result<ChatMessage> {
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

    pub fn get_messages(
        &self,
        session_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ChatMessage>> {
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
                created_at: r
                    .get::<_, String>(4)?
                    .parse::<DateTime<Utc>>()
                    .unwrap_or_else(|_| Utc::now()),
                tool_calls_json: r.get(5)?,
                tool_results_json: r.get(6)?,
                turn_index: r.get(7)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Fetch `limit` messages older than the newest `offset` messages, in chronological order.
    /// Used for scroll-up pagination: skip the newest `offset` rows, return the next `limit` older rows.
    pub fn get_messages_older(
        &self,
        session_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ChatMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, role, content, created_at, tool_calls_json, tool_results_json, turn_index \
             FROM ( \
               SELECT id, session_id, role, content, created_at, tool_calls_json, tool_results_json, turn_index \
               FROM messages WHERE session_id = ?1 \
               ORDER BY created_at DESC, rowid DESC LIMIT ?2 OFFSET ?3 \
             ) ORDER BY created_at ASC, rowid ASC",
        )?;
        let rows = stmt.query_map(params![session_id, limit, offset], |r| {
            Ok(ChatMessage {
                id: r.get(0)?,
                session_id: r.get(1)?,
                role: r.get(2)?,
                content: r.get(3)?,
                created_at: r
                    .get::<_, String>(4)?
                    .parse::<DateTime<Utc>>()
                    .unwrap_or_else(|_| Utc::now()),
                tool_calls_json: r.get(5)?,
                tool_results_json: r.get(6)?,
                turn_index: r.get(7)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
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
                created_at: r
                    .get::<_, String>(4)?
                    .parse::<DateTime<Utc>>()
                    .unwrap_or_else(|_| Utc::now()),
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
            "SELECT id, content, category, confidence, source_session_id, owner_id, scope_type, scope_id, project_scope_id, created_at, updated_at \
             FROM memories ORDER BY confidence DESC, updated_at DESC"
        )?;
        let rows = stmt.query_map([], Self::map_memory)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// List memories filtered by owner_id.
    pub fn list_memories_for_owner(&self, owner_id: &str) -> Result<Vec<Memory>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, category, confidence, source_session_id, owner_id, scope_type, scope_id, project_scope_id, created_at, updated_at \
             FROM memories WHERE owner_id = ?1 ORDER BY confidence DESC, updated_at DESC"
        )?;
        let rows = stmt.query_map(params![owner_id], Self::map_memory)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn map_memory(r: &rusqlite::Row) -> rusqlite::Result<Memory> {
        Ok(Memory {
            id: r.get(0)?,
            content: r.get(1)?,
            category: r.get(2)?,
            confidence: r.get(3)?,
            source_session_id: r.get(4)?,
            memory_type: "personal".to_string(),
            owner_id: r
                .get::<_, String>(5)
                .unwrap_or_else(|_| "pisci".to_string()),
            scope_type: r
                .get::<_, String>(6)
                .unwrap_or_else(|_| "private".to_string()),
            scope_id: r
                .get::<_, String>(7)
                .unwrap_or_else(|_| "pisci".to_string()),
            project_scope_id: r.get::<_, Option<String>>(8).unwrap_or(None),
            created_at: r
                .get::<_, String>(9)?
                .parse::<DateTime<Utc>>()
                .unwrap_or_else(|_| Utc::now()),
            updated_at: r
                .get::<_, String>(10)?
                .parse::<DateTime<Utc>>()
                .unwrap_or_else(|_| Utc::now()),
        })
    }

    /// Save a memory with dedup: if a very similar memory already exists (same category + owner,
    /// high content overlap), update it instead of creating a duplicate.
    ///
    /// `project_scope_id`: for private memories, the pool_session_id where this memory was created.
    /// NULL means it is a cross-project skill or preference (visible in all projects as a fallback).
    #[allow(clippy::too_many_arguments)]
    pub fn save_memory(
        &self,
        content: &str,
        category: &str,
        confidence: f64,
        source_session_id: Option<&str>,
        owner_id: &str,
        scope_type: &str,
        scope_id: &str,
        project_scope_id: Option<&str>,
    ) -> Result<Memory> {
        if let Some(existing) = self.find_similar_memory(content, category, owner_id)? {
            let now_str = Utc::now().to_rfc3339();
            let new_confidence = confidence.max(existing.confidence);
            self.conn.execute(
                "UPDATE memories SET content = ?1, confidence = ?2, updated_at = ?3 WHERE id = ?4",
                params![content, new_confidence, now_str, existing.id],
            )?;
            tracing::info!(
                "Memory dedup: updated existing memory {} instead of creating duplicate",
                existing.id
            );
            return Ok(Memory {
                id: existing.id,
                content: content.to_string(),
                category: category.to_string(),
                confidence: new_confidence,
                source_session_id: existing.source_session_id,
                memory_type: existing.memory_type,
                owner_id: existing.owner_id,
                scope_type: existing.scope_type,
                scope_id: existing.scope_id,
                project_scope_id: existing.project_scope_id,
                created_at: existing.created_at,
                updated_at: Utc::now(),
            });
        }

        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        self.conn.execute(
            "INSERT INTO memories (id, content, category, confidence, source_session_id, owner_id, scope_type, scope_id, project_scope_id, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
            params![id, content, category, confidence, source_session_id, owner_id, scope_type, scope_id, project_scope_id, now_str],
        )?;
        Ok(Memory {
            id,
            content: content.to_string(),
            category: category.to_string(),
            confidence,
            source_session_id: source_session_id.map(String::from),
            memory_type: "personal".to_string(),
            owner_id: owner_id.to_string(),
            scope_type: scope_type.to_string(),
            scope_id: scope_id.to_string(),
            project_scope_id: project_scope_id.map(String::from),
            created_at: now,
            updated_at: now,
        })
    }

    /// Find a memory in the same category+owner that has high content overlap with the given text.
    /// Uses word-level Jaccard similarity (threshold: 0.6).
    fn find_similar_memory(
        &self,
        content: &str,
        category: &str,
        owner_id: &str,
    ) -> Result<Option<Memory>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, category, confidence, source_session_id, owner_id, scope_type, scope_id, project_scope_id, created_at, updated_at \
             FROM memories WHERE category = ?1 AND owner_id = ?2 ORDER BY updated_at DESC LIMIT 50"
        )?;
        let rows = stmt.query_map(params![category, owner_id], Self::map_memory)?;

        let new_words: std::collections::HashSet<&str> = content.split_whitespace().collect();
        if new_words.is_empty() {
            return Ok(None);
        }

        for mem in rows.flatten() {
            let existing_words: std::collections::HashSet<&str> =
                mem.content.split_whitespace().collect();
            if existing_words.is_empty() {
                continue;
            }
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
        self.conn
            .execute("DELETE FROM memories WHERE id = ?1", params![id])?;
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
            "SELECT id, content, category, confidence, source_session_id, \
             owner_id, scope_type, scope_id, project_scope_id, created_at, updated_at, embedding \
             FROM memories WHERE embedding IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            let embedding_bytes: Vec<u8> = r.get(11)?;
            Ok((
                Memory {
                    id: r.get(0)?,
                    content: r.get(1)?,
                    category: r.get(2)?,
                    confidence: r.get(3)?,
                    source_session_id: r.get(4)?,
                    memory_type: "personal".to_string(),
                    owner_id: r
                        .get::<_, String>(5)
                        .unwrap_or_else(|_| "pisci".to_string()),
                    scope_type: r
                        .get::<_, String>(6)
                        .unwrap_or_else(|_| "private".to_string()),
                    scope_id: r
                        .get::<_, String>(7)
                        .unwrap_or_else(|_| "pisci".to_string()),
                    project_scope_id: r.get::<_, Option<String>>(8).unwrap_or(None),
                    created_at: r
                        .get::<_, String>(9)?
                        .parse::<DateTime<Utc>>()
                        .unwrap_or_else(|_| Utc::now()),
                    updated_at: r
                        .get::<_, String>(10)?
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
    pub fn search_by_embedding(
        &self,
        query_vec: &[f32],
        top_k: usize,
    ) -> Result<Vec<(Memory, f32)>> {
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
        let safe_query = query.replace('"', "\"\"").replace(['*', '^'], "");
        let fts_query = format!("\"{}\"", safe_query);

        let mut stmt = self.conn.prepare(
            "SELECT m.id, bm25(memories_fts) AS score \
             FROM memories_fts \
             JOIN memories m ON m.rowid = memories_fts.rowid \
             WHERE memories_fts MATCH ?1 \
             ORDER BY score \
             LIMIT ?2",
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
            "SELECT id, name, description, enabled, icon, config FROM skills ORDER BY name",
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
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
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
        self.conn
            .execute("DELETE FROM skills WHERE id = ?1", params![id])?;
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
                created_at: r
                    .get::<_, String>(10)?
                    .parse::<DateTime<Utc>>()
                    .unwrap_or_else(|_| Utc::now()),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
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
                created_at: r
                    .get::<_, String>(10)?
                    .parse::<DateTime<Utc>>()
                    .unwrap_or_else(|_| Utc::now()),
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn create_task(
        &self,
        name: &str,
        description: Option<&str>,
        cron_expression: &str,
        task_prompt: &str,
    ) -> Result<ScheduledTask> {
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

    pub fn update_task(
        &self,
        id: &str,
        name: Option<&str>,
        cron_expression: Option<&str>,
        task_prompt: Option<&str>,
        status: Option<&str>,
    ) -> Result<()> {
        if let Some(n) = name {
            self.conn.execute(
                "UPDATE scheduled_tasks SET name = ?1 WHERE id = ?2",
                params![n, id],
            )?;
        }
        if let Some(c) = cron_expression {
            self.conn.execute(
                "UPDATE scheduled_tasks SET cron_expression = ?1 WHERE id = ?2",
                params![c, id],
            )?;
        }
        if let Some(p) = task_prompt {
            self.conn.execute(
                "UPDATE scheduled_tasks SET task_prompt = ?1 WHERE id = ?2",
                params![p, id],
            )?;
        }
        if let Some(s) = status {
            self.conn.execute(
                "UPDATE scheduled_tasks SET status = ?1 WHERE id = ?2",
                params![s, id],
            )?;
        }
        Ok(())
    }

    pub fn delete_task(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM scheduled_tasks WHERE id = ?1", params![id])?;
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
                timestamp: r
                    .get::<_, String>(2)?
                    .parse::<DateTime<Utc>>()
                    .unwrap_or_else(|_| Utc::now()),
                tool_name: r.get(3)?,
                action: r.get(4)?,
                input_summary: r.get(5)?,
                result_summary: r.get(6)?,
                is_error: r.get::<_, i64>(7)? != 0,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn clear_audit_log(&self, session_id: Option<&str>) -> Result<()> {
        if let Some(sid) = session_id {
            self.conn
                .execute("DELETE FROM audit_log WHERE session_id = ?1", params![sid])?;
        } else {
            self.conn.execute("DELETE FROM audit_log", [])?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Memory FTS & Embeddings
    // ------------------------------------------------------------------

    /// Full-text search across all memories (global, no owner filter).
    pub fn search_memories_fts(&self, query: &str, limit: i64) -> Result<Vec<Memory>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.id, m.content, m.category, m.confidence, m.source_session_id, \
             m.owner_id, m.scope_type, m.scope_id, m.project_scope_id, m.created_at, m.updated_at \
             FROM memories m \
             JOIN memories_fts f ON m.rowid = f.rowid \
             WHERE memories_fts MATCH ?1 \
             ORDER BY rank \
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![query, limit], Self::map_memory)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Scoped memory search: retrieves memories in priority order (4 layers) for the given owner.
    ///
    /// Layer 1: private memories tagged to the current project (project_scope_id = pool_session_id)
    /// Layer 2: private memories with no project tag (cross-project skills/preferences)
    /// Layer 3: project-shared memories (scope_type = 'project', scope_id = pool_session_id)
    /// Layer 4: global memories
    pub fn search_memories_scoped(
        &self,
        query: &str,
        owner_id: &str,
        pool_session_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Memory>> {
        let mut results = Vec::new();

        let dedupe = |results: &Vec<Memory>| -> std::collections::HashSet<String> {
            results.iter().map(|m| m.id.clone()).collect()
        };

        // Layer 1: project-specific private memories (highest priority)
        if let Some(psid) = pool_session_id {
            let mut stmt = self.conn.prepare(
                "SELECT m.id, m.content, m.category, m.confidence, m.source_session_id, \
                 m.owner_id, m.scope_type, m.scope_id, m.project_scope_id, m.created_at, m.updated_at \
                 FROM memories m \
                 JOIN memories_fts f ON m.rowid = f.rowid \
                 WHERE memories_fts MATCH ?1 AND m.owner_id = ?2 AND m.scope_type = 'private' \
                 AND m.project_scope_id = ?3 \
                 ORDER BY rank \
                 LIMIT ?4"
            )?;
            let rows = stmt.query_map(params![query, owner_id, psid, limit], Self::map_memory)?;
            results.extend(rows.flatten());
        }

        // Layer 2: cross-project private memories (skills, preferences — project_scope_id IS NULL)
        {
            let remaining = (limit - results.len() as i64).max(0);
            if remaining > 0 {
                let mut stmt = self.conn.prepare(
                    "SELECT m.id, m.content, m.category, m.confidence, m.source_session_id, \
                     m.owner_id, m.scope_type, m.scope_id, m.project_scope_id, m.created_at, m.updated_at \
                     FROM memories m \
                     JOIN memories_fts f ON m.rowid = f.rowid \
                     WHERE memories_fts MATCH ?1 AND m.owner_id = ?2 AND m.scope_type = 'private' \
                     AND m.project_scope_id IS NULL \
                     ORDER BY rank \
                     LIMIT ?3"
                )?;
                let rows = stmt.query_map(params![query, owner_id, remaining], Self::map_memory)?;
                let seen = dedupe(&results);
                for mem in rows.flatten() {
                    if !seen.contains(&mem.id) {
                        results.push(mem);
                    }
                }
            }
        }

        // Layer 3: project-shared memories
        if let Some(psid) = pool_session_id {
            let remaining = (limit - results.len() as i64).max(0);
            if remaining > 0 {
                let mut stmt = self.conn.prepare(
                    "SELECT m.id, m.content, m.category, m.confidence, m.source_session_id, \
                     m.owner_id, m.scope_type, m.scope_id, m.project_scope_id, m.created_at, m.updated_at \
                     FROM memories m \
                     JOIN memories_fts f ON m.rowid = f.rowid \
                     WHERE memories_fts MATCH ?1 AND m.scope_type = 'project' AND m.scope_id = ?2 \
                     ORDER BY rank \
                     LIMIT ?3"
                )?;
                let rows = stmt.query_map(params![query, psid, remaining], Self::map_memory)?;
                let seen = dedupe(&results);
                for mem in rows.flatten() {
                    if !seen.contains(&mem.id) {
                        results.push(mem);
                    }
                }
            }
        }

        // Layer 4: global memories
        {
            let remaining = (limit - results.len() as i64).max(0);
            if remaining > 0 {
                let mut stmt = self.conn.prepare(
                    "SELECT m.id, m.content, m.category, m.confidence, m.source_session_id, \
                     m.owner_id, m.scope_type, m.scope_id, m.project_scope_id, m.created_at, m.updated_at \
                     FROM memories m \
                     JOIN memories_fts f ON m.rowid = f.rowid \
                     WHERE memories_fts MATCH ?1 AND m.scope_type = 'global' \
                     ORDER BY rank \
                     LIMIT ?2"
                )?;
                let rows = stmt.query_map(params![query, remaining], Self::map_memory)?;
                let seen = dedupe(&results);
                for mem in rows.flatten() {
                    if !seen.contains(&mem.id) {
                        results.push(mem);
                    }
                }
            }
        }

        results.truncate(limit as usize);
        Ok(results)
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
        let mut stmt = self
            .conn
            .prepare("SELECT embedding FROM embedding_cache WHERE content_hash = ?1")?;
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
    pub fn upsert_checkpoint(
        &self,
        session_id: &str,
        iteration: usize,
        messages_json: &str,
    ) -> Result<String> {
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
             ORDER BY updated_at DESC LIMIT 1",
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
                created_at: r
                    .get::<_, String>(8)?
                    .parse::<DateTime<Utc>>()
                    .unwrap_or_else(|_| Utc::now()),
                updated_at: r
                    .get::<_, String>(9)?
                    .parse::<DateTime<Utc>>()
                    .unwrap_or_else(|_| Utc::now()),
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
                created_at: r
                    .get::<_, String>(8)?
                    .parse::<DateTime<Utc>>()
                    .unwrap_or_else(|_| Utc::now()),
                updated_at: r
                    .get::<_, String>(9)?
                    .parse::<DateTime<Utc>>()
                    .unwrap_or_else(|_| Utc::now()),
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    // ------------------------------------------------------------------
    // Koi (persistent Agents)
    // ------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn create_koi(
        &self,
        name: &str,
        role: &str,
        icon: &str,
        color: &str,
        system_prompt: &str,
        description: &str,
        llm_provider_id: Option<&str>,
        max_iterations: u32,
    ) -> Result<crate::koi::KoiDefinition> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        self.conn.execute(
            "INSERT INTO kois (id, name, role, icon, color, system_prompt, description, status, created_at, updated_at, llm_provider_id, max_iterations) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'idle', ?8, ?8, ?9, ?10)",
            params![id, name, role, icon, color, system_prompt, description, now_str, llm_provider_id, max_iterations],
        )?;
        Ok(crate::koi::KoiDefinition {
            id,
            name: name.to_string(),
            role: role.to_string(),
            icon: icon.to_string(),
            color: color.to_string(),
            system_prompt: system_prompt.to_string(),
            description: description.to_string(),
            status: "idle".to_string(),
            created_at: now,
            updated_at: now,
            llm_provider_id: llm_provider_id.map(String::from),
            max_iterations,
        })
    }

    pub fn ensure_starter_kois(&self) -> Result<Vec<crate::koi::KoiDefinition>> {
        if !self.list_kois()?.is_empty() {
            return Ok(Vec::new());
        }

        let mut created = Vec::new();
        for spec in crate::koi::STARTER_KOI_SPECS {
            created.push(self.create_koi(
                spec.name,
                spec.role,
                spec.icon,
                spec.color,
                spec.system_prompt,
                spec.description,
                None,
                0,
            )?);
        }
        Ok(created)
    }

    pub fn list_kois(&self) -> Result<Vec<crate::koi::KoiDefinition>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, role, icon, color, system_prompt, description, status, created_at, updated_at, llm_provider_id, max_iterations \
             FROM kois ORDER BY created_at ASC"
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(crate::koi::KoiDefinition {
                id: r.get(0)?,
                name: r.get(1)?,
                role: r.get::<_, String>(2).unwrap_or_default(),
                icon: r.get(3)?,
                color: r.get(4)?,
                system_prompt: r.get(5)?,
                description: r.get(6)?,
                status: r.get(7)?,
                created_at: r
                    .get::<_, String>(8)?
                    .parse::<DateTime<Utc>>()
                    .unwrap_or_else(|_| Utc::now()),
                updated_at: r
                    .get::<_, String>(9)?
                    .parse::<DateTime<Utc>>()
                    .unwrap_or_else(|_| Utc::now()),
                llm_provider_id: r.get(10)?,
                max_iterations: r.get::<_, u32>(11).unwrap_or(0),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn get_koi(&self, id: &str) -> Result<Option<crate::koi::KoiDefinition>> {
        let koi_row = |r: &rusqlite::Row| -> rusqlite::Result<crate::koi::KoiDefinition> {
            Ok(crate::koi::KoiDefinition {
                id: r.get(0)?,
                name: r.get(1)?,
                role: r.get::<_, String>(2).unwrap_or_default(),
                icon: r.get(3)?,
                color: r.get(4)?,
                system_prompt: r.get(5)?,
                description: r.get(6)?,
                status: r.get(7)?,
                created_at: r
                    .get::<_, String>(8)?
                    .parse::<DateTime<Utc>>()
                    .unwrap_or_else(|_| Utc::now()),
                updated_at: r
                    .get::<_, String>(9)?
                    .parse::<DateTime<Utc>>()
                    .unwrap_or_else(|_| Utc::now()),
                llm_provider_id: r.get(10)?,
                max_iterations: r.get::<_, u32>(11).unwrap_or(0),
            })
        };

        // Exact match first
        let mut stmt = self.conn.prepare(
            "SELECT id, name, role, icon, color, system_prompt, description, status, created_at, updated_at, llm_provider_id, max_iterations \
             FROM kois WHERE id = ?1"
        )?;
        let mut rows = stmt.query_map(params![id], koi_row)?;
        if let Some(row) = rows.next() {
            return Ok(Some(row?));
        }

        // Prefix match fallback (for short IDs like "4b80c3e1")
        if id.len() >= 6 {
            let pattern = format!("{}%", id);
            let mut stmt2 = self.conn.prepare(
                "SELECT id, name, role, icon, color, system_prompt, description, status, created_at, updated_at, llm_provider_id, max_iterations \
                 FROM kois WHERE id LIKE ?1"
            )?;
            let matches: Vec<crate::koi::KoiDefinition> = stmt2
                .query_map(params![pattern], koi_row)?
                .filter_map(|r| r.ok())
                .collect();
            if matches.len() == 1 {
                return Ok(Some(matches.into_iter().next().unwrap()));
            }
        }

        Ok(None)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_koi(
        &self,
        id: &str,
        name: Option<&str>,
        role: Option<&str>,
        icon: Option<&str>,
        color: Option<&str>,
        system_prompt: Option<&str>,
        description: Option<&str>,
        llm_provider_id: Option<Option<&str>>,
        max_iterations: Option<u32>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        // llm_provider_id: None = don't change, Some(None) = clear, Some(Some(v)) = set
        match llm_provider_id {
            None => {
                self.conn.execute(
                    "UPDATE kois SET \
                     name = COALESCE(?2, name), \
                     role = COALESCE(?3, role), \
                     icon = COALESCE(?4, icon), \
                     color = COALESCE(?5, color), \
                     system_prompt = COALESCE(?6, system_prompt), \
                     description = COALESCE(?7, description), \
                     max_iterations = COALESCE(?9, max_iterations), \
                     updated_at = ?8 \
                     WHERE id = ?1",
                    params![
                        id,
                        name,
                        role,
                        icon,
                        color,
                        system_prompt,
                        description,
                        now,
                        max_iterations
                    ],
                )?;
            }
            Some(provider_id) => {
                self.conn.execute(
                    "UPDATE kois SET \
                     name = COALESCE(?2, name), \
                     role = COALESCE(?3, role), \
                     icon = COALESCE(?4, icon), \
                     color = COALESCE(?5, color), \
                     system_prompt = COALESCE(?6, system_prompt), \
                     description = COALESCE(?7, description), \
                     llm_provider_id = ?9, \
                     max_iterations = COALESCE(?10, max_iterations), \
                     updated_at = ?8 \
                     WHERE id = ?1",
                    params![
                        id,
                        name,
                        role,
                        icon,
                        color,
                        system_prompt,
                        description,
                        now,
                        provider_id,
                        max_iterations
                    ],
                )?;
            }
        }
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

    pub fn find_koi_by_name(&self, name: &str) -> Result<Option<crate::koi::KoiDefinition>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, role, icon, color, system_prompt, description, status, created_at, updated_at, llm_provider_id, max_iterations \
             FROM kois WHERE name = ?1 ORDER BY created_at ASC LIMIT 1"
        )?;
        let mut rows = stmt.query_map(params![name], |r| {
            Ok(crate::koi::KoiDefinition {
                id: r.get(0)?,
                name: r.get(1)?,
                role: r.get::<_, String>(2).unwrap_or_default(),
                icon: r.get(3)?,
                color: r.get(4)?,
                system_prompt: r.get(5)?,
                description: r.get(6)?,
                status: r.get(7)?,
                created_at: r
                    .get::<_, String>(8)?
                    .parse::<DateTime<Utc>>()
                    .unwrap_or_else(|_| Utc::now()),
                updated_at: r
                    .get::<_, String>(9)?
                    .parse::<DateTime<Utc>>()
                    .unwrap_or_else(|_| Utc::now()),
                llm_provider_id: r.get(10)?,
                max_iterations: r.get::<_, u32>(11).unwrap_or(0),
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn resolve_koi_identifier(&self, value: &str) -> Result<Option<crate::koi::KoiDefinition>> {
        let value = value.trim();
        if value.is_empty() {
            return Ok(None);
        }
        if let Some(koi) = self.get_koi(value)? {
            return Ok(Some(koi));
        }
        let matches: Vec<crate::koi::KoiDefinition> = self
            .list_kois()?
            .into_iter()
            .filter(|k| k.name == value)
            .collect();
        match matches.len() {
            0 => Ok(None),
            1 => Ok(matches.into_iter().next()),
            _ => Err(anyhow::anyhow!("Koi identifier '{}' is ambiguous", value)),
        }
    }

    /// Remove duplicate Koi entries, keeping the oldest one per name.
    /// Returns the number of duplicates removed.
    pub fn dedup_kois(&self) -> Result<usize> {
        let all = self.list_kois()?;
        let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let mut removed = 0usize;
        for koi in &all {
            if let Some(_kept_id) = seen.get(&koi.name) {
                self.delete_koi(&koi.id)?;
                removed += 1;
            } else {
                seen.insert(koi.name.clone(), koi.id.clone());
            }
        }
        Ok(removed)
    }

    pub fn delete_koi(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM kois WHERE id = ?1", params![id])?;
        self.conn
            .execute("DELETE FROM koi_todos WHERE owner_id = ?1", params![id])?;
        self.conn
            .execute("DELETE FROM memories WHERE owner_id = ?1", params![id])?;
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

    #[allow(clippy::too_many_arguments)]
    pub fn create_koi_todo(
        &self,
        owner_id: &str,
        title: &str,
        description: &str,
        priority: &str,
        assigned_by: &str,
        pool_session_id: Option<&str>,
        source_type: &str,
        depends_on: Option<&str>,
    ) -> Result<crate::koi::KoiTodo> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        self.conn.execute(
            "INSERT INTO koi_todos (id, owner_id, title, description, status, priority, assigned_by, pool_session_id, source_type, depends_on, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, 'todo', ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
            params![id, owner_id, title, description, priority, assigned_by, pool_session_id, source_type, depends_on, now_str],
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
            claimed_by: None,
            claimed_at: None,
            depends_on: depends_on.map(String::from),
            blocked_reason: None,
            result_message_id: None,
            source_type: source_type.to_string(),
            created_at: now,
            updated_at: now,
        })
    }

    const KOI_TODO_COLS: &'static str = "id, owner_id, title, description, status, priority, assigned_by, \
        pool_session_id, claimed_by, claimed_at, depends_on, blocked_reason, result_message_id, source_type, \
        created_at, updated_at";

    pub fn list_koi_todos(&self, owner_id: Option<&str>) -> Result<Vec<crate::koi::KoiTodo>> {
        let sql = if owner_id.is_some() {
            format!(
                "SELECT {} FROM koi_todos WHERE owner_id = ?1 ORDER BY created_at DESC",
                Self::KOI_TODO_COLS
            )
        } else {
            format!(
                "SELECT {} FROM koi_todos ORDER BY created_at DESC",
                Self::KOI_TODO_COLS
            )
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = if let Some(oid) = owner_id {
            stmt.query_map(params![oid], Self::map_koi_todo)?
        } else {
            stmt.query_map([], Self::map_koi_todo)?
        };
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn get_koi_todo(&self, id: &str) -> Result<Option<crate::koi::KoiTodo>> {
        let sql = format!(
            "SELECT {} FROM koi_todos WHERE id = ?1",
            Self::KOI_TODO_COLS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(params![id], Self::map_koi_todo)?;
        Ok(rows.next().transpose()?)
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
            claimed_by: r.get(8)?,
            claimed_at: r
                .get::<_, Option<String>>(9)?
                .and_then(|s| s.parse::<DateTime<Utc>>().ok()),
            depends_on: r.get(10)?,
            blocked_reason: r.get(11)?,
            result_message_id: r.get(12)?,
            source_type: r
                .get::<_, String>(13)
                .unwrap_or_else(|_| "user".to_string()),
            created_at: r
                .get::<_, String>(14)?
                .parse::<DateTime<Utc>>()
                .unwrap_or_else(|_| Utc::now()),
            updated_at: r
                .get::<_, String>(15)?
                .parse::<DateTime<Utc>>()
                .unwrap_or_else(|_| Utc::now()),
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

    /// Claim a todo (a Koi starts working on it)
    pub fn claim_koi_todo(&self, id: &str, claimed_by: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE koi_todos SET claimed_by = ?2, claimed_at = ?3, status = 'in_progress', updated_at = ?3 WHERE id = ?1",
            params![id, claimed_by, now],
        )?;
        Ok(())
    }

    /// Block a todo with a reason
    pub fn block_koi_todo(&self, id: &str, reason: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE koi_todos SET status = 'blocked', blocked_reason = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, reason, now],
        )?;
        Ok(())
    }

    /// Complete a todo with a link to the result message
    pub fn complete_koi_todo(&self, id: &str, result_message_id: Option<i64>) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE koi_todos SET status = 'done', result_message_id = COALESCE(?2, result_message_id), updated_at = ?3 WHERE id = ?1",
            params![id, result_message_id, now],
        )?;
        Ok(())
    }

    pub fn delete_koi_todo(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM koi_todos WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn delete_todos_by_pool(&self, pool_session_id: &str) -> Result<u32> {
        let count = self.conn.execute(
            "DELETE FROM koi_todos WHERE pool_session_id = ?1",
            params![pool_session_id],
        )?;
        Ok(count as u32)
    }

    // ------------------------------------------------------------------
    // Pool Sessions & Messages (Chat Pool)
    // ------------------------------------------------------------------

    pub fn create_pool_session(&self, name: &str) -> Result<crate::koi::PoolSession> {
        self.create_pool_session_with_dir(name, None)
    }

    pub fn create_pool_session_with_dir(
        &self,
        name: &str,
        project_dir: Option<&str>,
    ) -> Result<crate::koi::PoolSession> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        self.conn.execute(
            "INSERT INTO pool_sessions (id, name, org_spec, status, project_dir, last_active_at, created_at, updated_at) \
             VALUES (?1, ?2, '', 'active', ?3, ?4, ?4, ?4)",
            params![id, name, project_dir, now_str],
        )?;
        Ok(crate::koi::PoolSession {
            id,
            name: name.to_string(),
            org_spec: String::new(),
            status: "active".to_string(),
            project_dir: project_dir.map(String::from),
            last_active_at: Some(now),
            created_at: now,
            updated_at: now,
        })
    }

    fn map_pool_session(r: &rusqlite::Row) -> rusqlite::Result<crate::koi::PoolSession> {
        Ok(crate::koi::PoolSession {
            id: r.get(0)?,
            name: r.get(1)?,
            org_spec: r.get::<_, String>(2).unwrap_or_default(),
            status: r
                .get::<_, String>(3)
                .unwrap_or_else(|_| "active".to_string()),
            project_dir: r.get::<_, Option<String>>(4)?,
            last_active_at: r
                .get::<_, Option<String>>(5)?
                .and_then(|s| s.parse::<DateTime<Utc>>().ok()),
            created_at: r
                .get::<_, String>(6)?
                .parse::<DateTime<Utc>>()
                .unwrap_or_else(|_| Utc::now()),
            updated_at: r
                .get::<_, String>(7)?
                .parse::<DateTime<Utc>>()
                .unwrap_or_else(|_| Utc::now()),
        })
    }

    pub fn list_pool_sessions(&self) -> Result<Vec<crate::koi::PoolSession>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, org_spec, status, project_dir, last_active_at, created_at, updated_at \
             FROM pool_sessions ORDER BY updated_at DESC"
        )?;
        let rows = stmt.query_map([], Self::map_pool_session)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn get_pool_session(&self, id: &str) -> Result<Option<crate::koi::PoolSession>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, org_spec, status, project_dir, last_active_at, created_at, updated_at \
             FROM pool_sessions WHERE id = ?1"
        )?;
        let mut rows = stmt.query_map(params![id], Self::map_pool_session)?;
        Ok(rows.next().transpose()?)
    }

    pub fn get_pool_session_by_prefix(
        &self,
        prefix: &str,
    ) -> Result<Option<crate::koi::PoolSession>> {
        if prefix.trim().is_empty() {
            return Ok(None);
        }
        if let Some(session) = self.get_pool_session(prefix)? {
            return Ok(Some(session));
        }
        let like = format!("{}%", prefix);
        let mut stmt = self.conn.prepare(
            "SELECT id, name, org_spec, status, project_dir, last_active_at, created_at, updated_at \
             FROM pool_sessions WHERE id LIKE ?1 ORDER BY updated_at DESC"
        )?;
        let rows = stmt.query_map(params![like], Self::map_pool_session)?;
        let matches = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        match matches.len() {
            0 => Ok(None),
            1 => Ok(matches.into_iter().next()),
            _ => Err(anyhow::anyhow!("Pool id prefix '{}' is ambiguous", prefix)),
        }
    }

    pub fn resolve_pool_session_identifier(
        &self,
        value: &str,
    ) -> Result<Option<crate::koi::PoolSession>> {
        let value = value.trim();
        if value.is_empty() {
            return Ok(None);
        }
        if let Some(session) = self.get_pool_session_by_prefix(value)? {
            return Ok(Some(session));
        }
        let matches: Vec<crate::koi::PoolSession> = self
            .list_pool_sessions()?
            .into_iter()
            .filter(|session| session.name == value)
            .collect();
        match matches.len() {
            0 => Ok(None),
            1 => Ok(matches.into_iter().next()),
            _ => Err(anyhow::anyhow!("Pool identifier '{}' is ambiguous", value)),
        }
    }

    pub fn normalize_identifier_references(&self) -> Result<u32> {
        let mut updated = 0u32;

        {
            let mut stmt = self
                .conn
                .prepare("SELECT id, owner_id, claimed_by, pool_session_id FROM koi_todos")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })?;
            let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;

            for row in rows {
                let (todo_id, owner_id, claimed_by, pool_session_id) = row;

                if let Some(koi) = self.resolve_koi_identifier(&owner_id)? {
                    if koi.id != owner_id {
                        self.conn.execute(
                            "UPDATE koi_todos SET owner_id = ?2 WHERE id = ?1",
                            params![todo_id, koi.id],
                        )?;
                        updated += 1;
                    }
                }

                if let Some(claimed_by) = claimed_by {
                    if let Some(koi) = self.resolve_koi_identifier(&claimed_by)? {
                        if koi.id != claimed_by {
                            self.conn.execute(
                                "UPDATE koi_todos SET claimed_by = ?2 WHERE id = ?1",
                                params![todo_id, koi.id],
                            )?;
                            updated += 1;
                        }
                    }
                }

                if let Some(pool_session_id) = pool_session_id {
                    if let Some(session) = self.resolve_pool_session_identifier(&pool_session_id)? {
                        if session.id != pool_session_id {
                            self.conn.execute(
                                "UPDATE koi_todos SET pool_session_id = ?2 WHERE id = ?1",
                                params![todo_id, session.id],
                            )?;
                            updated += 1;
                        }
                    }
                }
            }
        }

        {
            let mut stmt = self.conn.prepare(
                "SELECT id, owner_id, scope_type, scope_id, project_scope_id FROM memories",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })?;
            let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;

            for row in rows {
                let (memory_id, owner_id, scope_type, scope_id, project_scope_id) = row;

                if let Some(koi) = self.resolve_koi_identifier(&owner_id)? {
                    if koi.id != owner_id {
                        self.conn.execute(
                            "UPDATE memories SET owner_id = ?2 WHERE id = ?1",
                            params![memory_id, koi.id],
                        )?;
                        updated += 1;
                    }

                    if scope_type == "private" && scope_id != koi.id {
                        self.conn.execute(
                            "UPDATE memories SET scope_id = ?2 WHERE id = ?1",
                            params![memory_id, koi.id],
                        )?;
                        updated += 1;
                    }
                }

                if scope_type == "project" {
                    if let Some(session) = self.resolve_pool_session_identifier(&scope_id)? {
                        if session.id != scope_id {
                            self.conn.execute(
                                "UPDATE memories SET scope_id = ?2 WHERE id = ?1",
                                params![memory_id, session.id],
                            )?;
                            updated += 1;
                        }
                    }
                }

                if let Some(project_scope_id) = project_scope_id {
                    if let Some(session) =
                        self.resolve_pool_session_identifier(&project_scope_id)?
                    {
                        if session.id != project_scope_id {
                            self.conn.execute(
                                "UPDATE memories SET project_scope_id = ?2 WHERE id = ?1",
                                params![memory_id, session.id],
                            )?;
                            updated += 1;
                        }
                    }
                }
            }
        }

        Ok(updated)
    }

    /// Return all todos for a pool that are in active states (todo / in_progress / blocked).
    pub fn list_active_todos_by_pool(
        &self,
        pool_session_id: &str,
    ) -> Result<Vec<crate::koi::KoiTodo>> {
        let sql = format!(
            "SELECT {} FROM koi_todos WHERE pool_session_id = ?1 AND status IN ('todo','in_progress','blocked') ORDER BY created_at DESC",
            Self::KOI_TODO_COLS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![pool_session_id], Self::map_koi_todo)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn update_pool_session_status(&self, id: &str, status: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE pool_sessions SET status = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, status, now],
        )?;
        Ok(())
    }

    pub fn touch_pool_session_active(&self, id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE pool_sessions SET last_active_at = ?2, updated_at = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        Ok(())
    }

    pub fn find_related_pool_sessions(
        &self,
        keywords: &str,
    ) -> Result<Vec<crate::koi::PoolSession>> {
        let kw_lower = keywords.to_lowercase();
        let terms: Vec<&str> = kw_lower.split_whitespace().collect();
        if terms.is_empty() {
            return self.list_pool_sessions();
        }
        let all = self.list_pool_sessions()?;
        let mut results: Vec<(usize, crate::koi::PoolSession)> = Vec::new();
        for session in all {
            let haystack = format!(
                "{} {} {}",
                session.name.to_lowercase(),
                session.org_spec.to_lowercase(),
                session.status,
            );
            let score = terms.iter().filter(|t| haystack.contains(*t)).count();
            if score > 0 {
                results.push((score, session));
            }
        }
        results.sort_by(|a, b| b.0.cmp(&a.0));
        Ok(results.into_iter().map(|(_, s)| s).collect())
    }

    pub fn update_pool_org_spec(&self, id: &str, org_spec: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE pool_sessions SET org_spec = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, org_spec, now],
        )?;
        Ok(())
    }

    pub fn delete_pool_session(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM koi_todos WHERE pool_session_id = ?1",
            params![id],
        )?;
        self.conn.execute(
            "DELETE FROM pool_messages WHERE pool_session_id = ?1",
            params![id],
        )?;
        self.conn
            .execute("DELETE FROM pool_sessions WHERE id = ?1", params![id])?;
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
        self.insert_pool_message_ext(
            pool_session_id,
            sender_id,
            content,
            msg_type,
            metadata,
            None,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_pool_message_ext(
        &self,
        pool_session_id: &str,
        sender_id: &str,
        content: &str,
        msg_type: &str,
        metadata: &str,
        todo_id: Option<&str>,
        reply_to_message_id: Option<i64>,
        event_type: Option<&str>,
    ) -> Result<crate::koi::PoolMessage> {
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        self.conn.execute(
            "INSERT INTO pool_messages (pool_session_id, sender_id, content, msg_type, metadata, todo_id, reply_to_message_id, event_type, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![pool_session_id, sender_id, content, msg_type, metadata, todo_id, reply_to_message_id, event_type, now_str],
        )?;
        let id = self.conn.last_insert_rowid();
        self.conn.execute(
            "UPDATE pool_sessions SET updated_at = ?1, last_active_at = ?1 WHERE id = ?2",
            params![now_str, pool_session_id],
        )?;
        Ok(crate::koi::PoolMessage {
            id,
            pool_session_id: pool_session_id.to_string(),
            sender_id: sender_id.to_string(),
            content: content.to_string(),
            msg_type: msg_type.to_string(),
            metadata: metadata.to_string(),
            todo_id: todo_id.map(String::from),
            reply_to_message_id,
            event_type: event_type.map(String::from),
            created_at: now,
        })
    }

    pub fn get_pool_messages(
        &self,
        pool_session_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<crate::koi::PoolMessage>> {
        // Fetch the newest `limit` rows starting at `offset` from the end,
        // then re-sort ascending so callers always receive chronological order.
        let mut stmt = self.conn.prepare(
            "SELECT id, pool_session_id, sender_id, content, msg_type, metadata, \
             todo_id, reply_to_message_id, event_type, created_at \
             FROM ( \
               SELECT id, pool_session_id, sender_id, content, msg_type, metadata, \
                      todo_id, reply_to_message_id, event_type, created_at \
               FROM pool_messages WHERE pool_session_id = ?1 \
               ORDER BY created_at DESC LIMIT ?2 OFFSET ?3 \
             ) ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(
            params![pool_session_id, limit, offset],
            Self::map_pool_message,
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Get the content of the most recent "result" type pool message sent by a specific Koi.
    /// Used to recover the complete_todo summary when the agent loop ends with a tool call.
    pub fn get_latest_result_message(
        &self,
        pool_session_id: &str,
        sender_id: &str,
    ) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT content FROM pool_messages \
             WHERE pool_session_id = ?1 AND sender_id = ?2 AND msg_type = 'result' \
             ORDER BY created_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![pool_session_id, sender_id], |row| {
            row.get::<_, String>(0)
        })?;
        Ok(rows.next().transpose()?)
    }

    /// Get the id of the most recent "result" message from a Koi that is not yet linked to a todo.
    /// Used by runtime to link a complete_todo-written message to the current todo.
    pub fn get_latest_unlinked_result_message_id(
        &self,
        pool_session_id: &str,
        sender_id: &str,
    ) -> Result<Option<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM pool_messages \
             WHERE pool_session_id = ?1 AND sender_id = ?2 AND msg_type = 'result' \
             AND (todo_id IS NULL OR todo_id = '') \
             ORDER BY created_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![pool_session_id, sender_id], |row| {
            row.get::<_, i64>(0)
        })?;
        Ok(rows.next().transpose()?)
    }

    /// Link a pool message to a todo (set its todo_id field).
    pub fn link_pool_message_to_todo(&self, message_id: i64, todo_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE pool_messages SET todo_id = ?2 WHERE id = ?1",
            params![message_id, todo_id],
        )?;
        Ok(())
    }

    pub fn get_pool_message_by_id(&self, id: i64) -> Result<Option<crate::koi::PoolMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, pool_session_id, sender_id, content, msg_type, metadata, \
             todo_id, reply_to_message_id, event_type, created_at \
             FROM pool_messages WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], Self::map_pool_message)?;
        Ok(rows.next().transpose()?)
    }

    /// Get pool messages linked to a specific todo
    pub fn get_pool_messages_for_todo(
        &self,
        todo_id: &str,
    ) -> Result<Vec<crate::koi::PoolMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, pool_session_id, sender_id, content, msg_type, metadata, \
             todo_id, reply_to_message_id, event_type, created_at \
             FROM pool_messages WHERE todo_id = ?1 \
             ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![todo_id], Self::map_pool_message)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn map_pool_message(r: &rusqlite::Row) -> rusqlite::Result<crate::koi::PoolMessage> {
        Ok(crate::koi::PoolMessage {
            id: r.get(0)?,
            pool_session_id: r.get(1)?,
            sender_id: r.get(2)?,
            content: r.get(3)?,
            msg_type: r.get(4)?,
            metadata: r.get(5)?,
            todo_id: r.get(6)?,
            reply_to_message_id: r.get(7)?,
            event_type: r.get(8)?,
            created_at: r
                .get::<_, String>(9)?
                .parse::<DateTime<Utc>>()
                .unwrap_or_else(|_| Utc::now()),
        })
    }

    /// Reset all "busy" Koi back to "idle" — called on startup to fix stale state from crashes.
    pub fn recover_stale_koi_status(&self) -> Result<u32> {
        let count = self.conn.execute(
            "UPDATE kois SET status = 'idle', updated_at = datetime('now') WHERE status = 'busy'",
            [],
        )?;
        if count > 0 {
            tracing::info!("Startup recovery: reset {} stale busy Koi to idle", count);
        }
        Ok(count as u32)
    }

    /// Reset all "in_progress" todos back to "todo" — called on startup.
    /// These were being worked on when the app crashed.
    pub fn recover_stale_todos(&self) -> Result<u32> {
        let count = self.conn.execute(
            "UPDATE koi_todos SET status = 'todo', claimed_by = NULL, claimed_at = NULL, updated_at = datetime('now') WHERE status = 'in_progress'",
            [],
        )?;
        if count > 0 {
            tracing::info!(
                "Startup recovery: reset {} stale in_progress todos to todo",
                count
            );
        }
        Ok(count as u32)
    }

    /// Reset Koi that have been "busy" longer than max_age_secs — for runtime watchdog.
    pub fn recover_stale_busy_kois(&self, max_age_secs: i64) -> Result<u32> {
        let count = if max_age_secs <= 0 {
            self.conn.execute(
                "UPDATE kois SET status = 'idle', updated_at = datetime('now') WHERE status = 'busy'",
                [],
            )?
        } else {
            self.conn.execute(
                "UPDATE kois SET status = 'idle', updated_at = datetime('now') WHERE status = 'busy' AND updated_at < datetime('now', ?)",
                rusqlite::params![format!("-{} seconds", max_age_secs)],
            )?
        };
        Ok(count as u32)
    }

    /// Reset "in_progress" todos older than max_age_secs — for runtime watchdog.
    pub fn recover_stale_in_progress_todos(&self, max_age_secs: i64) -> Result<u32> {
        let count = if max_age_secs <= 0 {
            self.conn.execute(
                "UPDATE koi_todos SET status = 'todo', claimed_by = NULL, claimed_at = NULL, updated_at = datetime('now') WHERE status = 'in_progress'",
                [],
            )?
        } else {
            self.conn.execute(
                "UPDATE koi_todos SET status = 'todo', claimed_by = NULL, claimed_at = NULL, updated_at = datetime('now') WHERE status = 'in_progress' AND updated_at < datetime('now', ?)",
                rusqlite::params![format!("-{} seconds", max_age_secs)],
            )?
        };
        Ok(count as u32)
    }

    pub fn recover_stale_running_sessions(&self, max_age_secs: i64) -> Result<u32> {
        let count = if max_age_secs <= 0 {
            self.conn.execute(
                "UPDATE sessions SET status = 'idle', updated_at = datetime('now') WHERE status = 'running'",
                [],
            )?
        } else {
            self.conn.execute(
                "UPDATE sessions SET status = 'idle', updated_at = datetime('now') WHERE status = 'running' AND updated_at < datetime('now', ?)",
                rusqlite::params![format!("-{} seconds", max_age_secs)],
            )?
        };
        Ok(count as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::Database;

    #[test]
    fn session_context_state_defaults_and_updates_roundtrip() {
        let db = Database::open_in_memory().expect("in-memory db");
        let session = db.create_session(Some("Context")).expect("session");

        let initial = db
            .get_session_context_state(&session.id)
            .expect("state query")
            .expect("state exists");
        assert_eq!(initial.rolling_summary, "");
        assert_eq!(initial.rolling_summary_version, 0);
        assert_eq!(initial.total_input_tokens, 0);
        assert_eq!(initial.total_output_tokens, 0);
        assert!(initial.last_compacted_at.is_none());

        db.update_session_usage_totals(&session.id, 123, 45)
            .expect("usage update");
        db.update_session_rolling_summary(&session.id, "summary body", 2)
            .expect("summary update");

        let updated = db
            .get_session(&session.id)
            .expect("session query")
            .expect("session exists");
        assert_eq!(updated.total_input_tokens, 123);
        assert_eq!(updated.total_output_tokens, 45);
        assert_eq!(updated.rolling_summary, "summary body");
        assert_eq!(updated.rolling_summary_version, 2);
        assert!(updated.last_compacted_at.is_some());
    }
}
