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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub content: String,
    pub category: String,
    pub confidence: f64,
    pub source_session_id: Option<String>,
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
        ")?;

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
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        self.conn.execute(
            "INSERT INTO sessions (id, title, status, created_at, updated_at, message_count) VALUES (?1, ?2, 'idle', ?3, ?3, 0)",
            params![id, title, now_str],
        )?;
        Ok(Session {
            id,
            title: title.map(String::from),
            status: "idle".into(),
            created_at: now,
            updated_at: now,
            message_count: 0,
        })
    }

    pub fn list_sessions(&self, limit: i64, offset: i64) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, status, created_at, updated_at, message_count FROM sessions ORDER BY updated_at DESC LIMIT ?1 OFFSET ?2"
        )?;
        let rows = stmt.query_map(params![limit, offset], |r| {
            Ok(Session {
                id: r.get(0)?,
                title: r.get(1)?,
                status: r.get(2)?,
                created_at: r.get::<_, String>(3)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
                updated_at: r.get::<_, String>(4)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
                message_count: r.get(5)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    pub fn delete_session(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
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
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        self.conn.execute(
            "INSERT INTO messages (id, session_id, role, content, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, session_id, role, content, now_str],
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
        })
    }

    pub fn get_messages(&self, session_id: &str, limit: i64, offset: i64) -> Result<Vec<ChatMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, role, content, created_at FROM messages WHERE session_id = ?1 ORDER BY created_at ASC LIMIT ?2 OFFSET ?3"
        )?;
        let rows = stmt.query_map(params![session_id, limit, offset], |r| {
            Ok(ChatMessage {
                id: r.get(0)?,
                session_id: r.get(1)?,
                role: r.get(2)?,
                content: r.get(3)?,
                created_at: r.get::<_, String>(4)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
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
                created_at: r.get::<_, String>(5)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
                updated_at: r.get::<_, String>(6)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    pub fn save_memory(&self, content: &str, category: &str, confidence: f64, source_session_id: Option<&str>) -> Result<Memory> {
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
            created_at: now,
            updated_at: now,
        })
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

    // ------------------------------------------------------------------
    // Scheduled tasks
    // ------------------------------------------------------------------

    pub fn list_tasks(&self) -> Result<Vec<ScheduledTask>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, cron_expression, task_prompt, status, run_count, last_run_at, next_run_at, created_at FROM scheduled_tasks ORDER BY created_at DESC"
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ScheduledTask {
                id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
                cron_expression: r.get(3)?,
                task_prompt: r.get(4)?,
                status: r.get(5)?,
                run_count: r.get(6)?,
                last_run_at: r.get::<_, Option<String>>(7)?.and_then(|s| s.parse().ok()),
                next_run_at: r.get::<_, Option<String>>(8)?.and_then(|s| s.parse().ok()),
                created_at: r.get::<_, String>(9)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    pub fn get_task(&self, id: &str) -> Result<Option<ScheduledTask>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, cron_expression, task_prompt, status, run_count, last_run_at, next_run_at, created_at FROM scheduled_tasks WHERE id = ?1"
        )?;
        let mut rows = stmt.query_map(params![id], |r| {
            Ok(ScheduledTask {
                id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
                cron_expression: r.get(3)?,
                task_prompt: r.get(4)?,
                status: r.get(5)?,
                run_count: r.get(6)?,
                last_run_at: r.get::<_, Option<String>>(7)?.and_then(|s| s.parse().ok()),
                next_run_at: r.get::<_, Option<String>>(8)?.and_then(|s| s.parse().ok()),
                created_at: r.get::<_, String>(9)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
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
            "UPDATE scheduled_tasks SET run_count = run_count + 1, last_run_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(())
    }
}
