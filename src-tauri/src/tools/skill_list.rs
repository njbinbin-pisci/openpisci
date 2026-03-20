use crate::agent::tool::{Tool, ToolContext, ToolResult};
use crate::skills::loader::SkillLoader;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;

const PAGE_SIZE: usize = 20;

pub struct SkillListTool {
    pub loader: Arc<Mutex<SkillLoader>>,
}

#[async_trait]
impl Tool for SkillListTool {
    fn name(&self) -> &str {
        "skill_list"
    }

    fn description(&self) -> &str {
        "List all installed skills with their name and description. \
         Use this to browse available skills when you are unsure which one applies. \
         Supports optional pagination via the `page` parameter (1-based, default 1)."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "page": {
                    "type": "integer",
                    "description": "Page number (1-based). Omit or set to 1 to get the first page.",
                    "minimum": 1
                }
            },
            "required": []
        })
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let page = input["page"].as_u64().unwrap_or(1).max(1) as usize;

        let loader = self.loader.lock().await;
        let mut skills = loader.list_skills();

        // Sort alphabetically for stable pagination
        skills.sort_by(|a, b| a.name.cmp(&b.name));

        let total = skills.len();
        if total == 0 {
            return Ok(ToolResult::ok("No skills installed.".to_string()));
        }

        let total_pages = total.div_ceil(PAGE_SIZE);
        let page = page.min(total_pages);
        let start = (page - 1) * PAGE_SIZE;
        let end = (start + PAGE_SIZE).min(total);
        let page_skills = &skills[start..end];

        let lines: Vec<String> = page_skills
            .iter()
            .map(|s| {
                let skill_md = s.source_path.join("SKILL.md");
                format!(
                    "- **{}** (`{}`): {}",
                    s.name,
                    skill_md.display(),
                    s.description
                )
            })
            .collect();

        Ok(ToolResult::ok(format!(
            "Skills (page {}/{}, total {}):\n\n{}",
            page,
            total_pages,
            total,
            lines.join("\n")
        )))
    }
}
