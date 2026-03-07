use crate::agent::tool::{Tool, ToolContext, ToolResult};
use crate::skills::loader::SkillLoader;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct SkillSearchTool {
    pub loader: Arc<Mutex<SkillLoader>>,
}

#[async_trait]
impl Tool for SkillSearchTool {
    fn name(&self) -> &str {
        "skill_search"
    }

    fn description(&self) -> &str {
        "Search available skills and load their full instructions. \
         When you receive a task that may require specialized capabilities, \
         call this tool first to find relevant skills. \
         Supports Chinese and English queries."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keywords describing the task, e.g. 'make a PPT', '制作幻灯片', 'excel spreadsheet', '数据分析'"
                }
            },
            "required": ["query"]
        })
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let query = match input["query"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(q) => q,
            None => return Ok(ToolResult::err("'query' is required")),
        };

        let loader = self.loader.lock().await;
        let matches = loader.search_skills(query);

        if matches.is_empty() {
            return Ok(ToolResult::ok(format!(
                "No skill found matching '{}'. Proceed with built-in capabilities.",
                query
            )));
        }

        let mut parts = Vec::new();
        for skill in matches {
            parts.push(format!(
                "## Skill: {}\n{}\n\nPermissions: {}\n\n{}",
                skill.name,
                skill.description,
                if skill.permissions.is_empty() {
                    "none".to_string()
                } else {
                    skill.permissions.join(", ")
                },
                skill.instructions
            ));
        }

        Ok(ToolResult::ok(format!(
            "Found {} skill(s) matching '{}':\n\n{}",
            parts.len(),
            query,
            parts.join("\n\n---\n\n")
        )))
    }
}
