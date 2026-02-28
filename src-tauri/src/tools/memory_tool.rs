use crate::agent::tool::{Tool, ToolContext, ToolResult};
use crate::store::Database;
use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;
use std::sync::Arc;

pub struct MemoryStoreTool {
    pub db: Arc<Mutex<Database>>,
}

#[async_trait]
impl Tool for MemoryStoreTool {
    fn name(&self) -> &str {
        "memory_store"
    }

    fn description(&self) -> &str {
        "Save an important piece of information to long-term memory. Use this when you learn something significant about the user's preferences, facts, goals, or task outcomes that should be remembered in future conversations."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "content": {
                    "type": "string",
                    "description": "The information to remember. Be concise and specific (1-2 sentences)."
                },
                "category": {
                    "type": "string",
                    "description": "Category for this memory: 'preference', 'fact', 'task', 'person', 'project', or 'general'.",
                    "enum": ["preference", "fact", "task", "person", "project", "general"]
                }
            },
            "required": ["content"]
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let content = input["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing required field: content"))?;

        if content.trim().is_empty() {
            return Ok(ToolResult::err("Memory content cannot be empty".to_string()));
        }

        let category = input["category"].as_str().unwrap_or("general");
        let session_id = ctx.session_id.as_str();

        let db = self.db.lock().await;
        match db.save_memory(content, category, 0.9, Some(session_id)) {
            Ok(mem) => Ok(ToolResult::ok(format!(
                "Memory saved (id: {}, category: {}): {}",
                &mem.id[..8],
                category,
                content
            ))),
            Err(e) => Ok(ToolResult::err(format!("Failed to save memory: {}", e))),
        }
    }
}
