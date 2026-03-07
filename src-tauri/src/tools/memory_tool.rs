use crate::agent::tool::{Tool, ToolContext, ToolResult};
use crate::store::Database;
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use std::sync::Arc;

pub struct MemoryStoreTool {
    pub db: Arc<Mutex<Database>>,
}

#[async_trait]
impl Tool for MemoryStoreTool {
    fn name(&self) -> &str { "memory_store" }

    fn description(&self) -> &str {
        "Manage long-term memory across conversations. \
         Actions: \
         - 'save': Save an important piece of information (default action when no action specified). \
         - 'search': Search memories by keyword. Use to check what you already know before saving duplicates. \
         - 'list': List all stored memories (optionally filtered by category). \
         - 'delete': Delete a memory by its ID (get IDs from list or search). \
         \
         Memory is automatically injected into the system prompt at conversation start. \
         Use 'search' before 'save' to avoid duplicate memories."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["save", "search", "list", "delete"],
                    "description": "Action to perform (default: 'save')"
                },
                "content": {
                    "type": "string",
                    "description": "For 'save': the information to remember (1-2 sentences). For 'search': the search query."
                },
                "category": {
                    "type": "string",
                    "enum": ["preference", "fact", "task", "person", "project", "general"],
                    "description": "For 'save': memory category. For 'list': filter by category (optional)."
                },
                "id": {
                    "type": "string",
                    "description": "For 'delete': the memory ID to delete (get from list or search results)"
                }
            }
        })
    }

    fn is_read_only(&self) -> bool { false }

    async fn call(&self, input: Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let action = input["action"].as_str().unwrap_or("save");

        match action {
            "save" => self.save(&input, ctx).await,
            "search" => self.search(&input).await,
            "list" => self.list(&input).await,
            "delete" => self.delete(&input).await,
            _ => Ok(ToolResult::err(format!("Unknown action '{}'. Use: save, search, list, delete", action))),
        }
    }
}

impl MemoryStoreTool {
    async fn save(&self, input: &Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let content = match input["content"].as_str() {
            Some(c) if !c.trim().is_empty() => c,
            _ => return Ok(ToolResult::err("save requires non-empty 'content'")),
        };
        let category = input["category"].as_str().unwrap_or("general");

        let db = self.db.lock().await;
        match db.save_memory(content, category, 0.9, Some(&ctx.session_id)) {
            Ok(mem) => Ok(ToolResult::ok(format!(
                "Memory saved.\nID: {}\nCategory: {}\nContent: {}",
                &mem.id[..8.min(mem.id.len())],
                category,
                content
            ))),
            Err(e) => Ok(ToolResult::err(format!("Failed to save memory: {}", e))),
        }
    }

    async fn search(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let query = match input["content"].as_str() {
            Some(q) if !q.trim().is_empty() => q,
            _ => return Ok(ToolResult::err("search requires 'content' as the search query")),
        };

        let db = self.db.lock().await;
        match db.search_memories_fts(query, 20) {
            Ok(mems) if mems.is_empty() => Ok(ToolResult::ok(format!(
                "No memories found matching '{}'", query
            ))),
            Ok(mems) => {
                let items: Vec<String> = mems.iter().map(|m| {
                    format!("[{}] [{}] {}", &m.id[..8.min(m.id.len())], m.category, m.content)
                }).collect();
                Ok(ToolResult::ok(format!(
                    "Found {} memory/memories matching '{}':\n{}",
                    items.len(), query, items.join("\n")
                )))
            }
            Err(e) => Ok(ToolResult::err(format!("Search failed: {}", e))),
        }
    }

    async fn list(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let category_filter = input["category"].as_str();

        let db = self.db.lock().await;
        match db.list_memories() {
            Ok(mems) => {
                let filtered: Vec<_> = mems.iter()
                    .filter(|m| category_filter.map(|c| m.category == c).unwrap_or(true))
                    .collect();

                if filtered.is_empty() {
                    return Ok(ToolResult::ok(match category_filter {
                        Some(c) => format!("No memories in category '{}'", c),
                        None => "No memories stored yet".to_string(),
                    }));
                }

                let items: Vec<String> = filtered.iter().map(|m| {
                    format!("[{}] [{}] {}", &m.id[..8.min(m.id.len())], m.category, m.content)
                }).collect();

                Ok(ToolResult::ok(format!(
                    "{} memory/memories{}:\n{}",
                    items.len(),
                    category_filter.map(|c| format!(" in category '{}'", c)).unwrap_or_default(),
                    items.join("\n")
                )))
            }
            Err(e) => Ok(ToolResult::err(format!("Failed to list memories: {}", e))),
        }
    }

    async fn delete(&self, input: &Value) -> anyhow::Result<ToolResult> {
        let id = match input["id"].as_str() {
            Some(i) if !i.trim().is_empty() => i,
            _ => return Ok(ToolResult::err("delete requires 'id' (get IDs from list or search)")),
        };

        // Allow partial ID match — find the full ID first
        let db = self.db.lock().await;
        let all = db.list_memories().unwrap_or_default();
        let matched: Vec<_> = all.iter().filter(|m| m.id.starts_with(id)).collect();

        match matched.len() {
            0 => Ok(ToolResult::err(format!("No memory found with ID starting with '{}'", id))),
            1 => {
                let full_id = &matched[0].id;
                let content_preview = &matched[0].content;
                match db.delete_memory(full_id) {
                    Ok(_) => Ok(ToolResult::ok(format!(
                        "Deleted memory [{}]: {}",
                        &full_id[..8.min(full_id.len())],
                        content_preview
                    ))),
                    Err(e) => Ok(ToolResult::err(format!("Failed to delete memory: {}", e))),
                }
            }
            n => Ok(ToolResult::err(format!(
                "Ambiguous ID '{}' matches {} memories. Use more characters.",
                id, n
            ))),
        }
    }
}
