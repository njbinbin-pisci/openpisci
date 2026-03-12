use crate::llm::{LlmClient, LlmMessage, LlmRequest, MessageContent};
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubTask {
    pub id: String,
    pub description: String,
    pub app_hint: Option<String>,
    pub dependencies: Vec<String>,
    pub status: SubTaskStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SubTaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed(String),
}

pub struct HostAgent {
    client: Box<dyn LlmClient>,
    model: String,
    max_tokens: u32,
}

impl HostAgent {
    pub fn new(client: Box<dyn LlmClient>, model: String, max_tokens: u32) -> Self {
        Self {
            client,
            model,
            max_tokens,
        }
    }

    pub async fn decompose_task(&self, user_request: &str) -> Result<Vec<SubTask>> {
        let system = "You are a task decomposition agent. Given a user request, break it down into \
            a sequence of concrete sub-tasks. Each sub-task should be independently executable. \
            Return a JSON array of objects with fields: id, description, app_hint (optional application name), \
            dependencies (array of prerequisite task ids). \
            Keep it concise - most requests need 1-3 sub-tasks. \
            IMPORTANT: If tasks have data or file dependencies, set the `dependencies` array correctly. \
            Tasks that edit the same file MUST be sequential (set dependency). \
            Tasks on different files/modules CAN run in parallel (empty dependencies).".to_string();

        let messages = vec![LlmMessage {
            role: "user".into(),
            content: MessageContent::text(user_request),
        }];

        let req = LlmRequest {
            messages,
            system: Some(system),
            tools: vec![],
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            stream: false,
            vision_override: None,
        };

        let response = self.client.complete(req).await?;
        let text = response.content.trim();

        let json_start = text.find('[').unwrap_or(0);
        let json_end = text.rfind(']').map(|i| i + 1).unwrap_or(text.len());
        let json_str = &text[json_start..json_end];

        match serde_json::from_str::<Vec<SubTaskRaw>>(json_str) {
            Ok(raw_tasks) => Ok(raw_tasks
                .into_iter()
                .map(|t| SubTask {
                    id: t.id,
                    description: t.description,
                    app_hint: t.app_hint,
                    dependencies: t.dependencies.unwrap_or_default(),
                    status: SubTaskStatus::Pending,
                })
                .collect()),
            Err(_) => Ok(vec![SubTask {
                id: "task_1".to_string(),
                description: user_request.to_string(),
                app_hint: None,
                dependencies: vec![],
                status: SubTaskStatus::Pending,
            }]),
        }
    }

    pub fn should_decompose(request: &str) -> bool {
        let word_count = request.split_whitespace().count();
        let has_multiple_actions = request.contains(" and ")
            || request.contains(" then ")
            || request.contains("，然后")
            || request.contains("，接着")
            || request.contains("并且");
        word_count > 20 || has_multiple_actions
    }

    /// Attempt to route a sub-task to a persistent Koi agent.
    ///
    /// Returns `Some(koi_id)` if the task description or hint suggests a role
    /// that matches an available Koi agent. Looks at the Koi's `role`,
    /// `description`, `name`, and `system_prompt`.
    pub fn route_to_koi(
        task_description: &str,
        kois: &[crate::koi::KoiDefinition],
    ) -> Option<String> {
        if kois.is_empty() {
            return None;
        }
        let desc_lower = task_description.to_lowercase();

        for koi in kois {
            if koi.status == "offline" {
                continue;
            }
            let koi_role = koi.role.to_lowercase();
            let koi_desc = koi.description.to_lowercase();
            let koi_name = koi.name.to_lowercase();
            let koi_prompt = koi.system_prompt.to_lowercase();
            let role_keywords: Vec<&str> = koi_role
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .filter(|w| w.len() > 1)
                .collect();
            let role_match = role_keywords
                .iter()
                .filter(|kw| desc_lower.contains(*kw))
                .count();
            if role_match >= 1 || (!koi_role.is_empty() && desc_lower.contains(&koi_role)) {
                return Some(koi.id.clone());
            }

            let desc_keywords: Vec<&str> = koi_desc
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .filter(|w| w.len() > 2)
                .collect();

            let match_count = desc_keywords
                .iter()
                .filter(|kw| desc_lower.contains(*kw))
                .count();

            if match_count >= 2 {
                return Some(koi.id.clone());
            }
            if desc_lower.contains(&koi_name) {
                return Some(koi.id.clone());
            }
            let prompt_keywords: Vec<&str> = koi_prompt
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .filter(|w| w.len() > 3)
                .collect();
            let prompt_match = prompt_keywords
                .iter()
                .filter(|kw| desc_lower.contains(*kw))
                .count();
            if prompt_match >= 3 {
                return Some(koi.id.clone());
            }
        }
        None
    }

    /// Map an `app_hint` from a decomposed SubTask to a Fish ID.
    ///
    /// Returns `Some(fish_id)` if the hint matches a known skill-based Fish,
    /// or `None` if the main Agent should handle the sub-task directly.
    ///
    /// The fish IDs here correspond to the auto-generated skill Fish IDs
    /// produced by `fish::skill_fish_id()` from the built-in skill names.
    pub fn route_to_fish(hint: &str) -> Option<&'static str> {
        let h = hint.to_lowercase();
        if h.contains("file") || h.contains("文件") || h.contains("folder") || h.contains("目录")
        {
            return Some("skill-file-management");
        }
        if h.contains("office")
            || h.contains("excel")
            || h.contains("word")
            || h.contains("ppt")
            || h.contains("spreadsheet")
            || h.contains("表格")
            || h.contains("文档")
            || h.contains("报告")
        {
            return Some("skill-office-automation");
        }
        if h.contains("web")
            || h.contains("browser")
            || h.contains("网页")
            || h.contains("crawl")
            || h.contains("scrape")
            || h.contains("url")
            || h.contains("浏览器")
            || h.contains("抓取")
        {
            return Some("skill-web-automation");
        }
        if h.contains("system")
            || h.contains("windows")
            || h.contains("powershell")
            || h.contains("process")
            || h.contains("service")
            || h.contains("系统")
            || h.contains("进程")
            || h.contains("服务")
        {
            return Some("skill-system-admin");
        }
        if h.contains("desktop")
            || h.contains("uia")
            || h.contains("桌面")
            || h.contains("click")
            || h.contains("window")
            || h.contains("窗口")
            || h.contains("界面")
            || h.contains("自动化操作")
        {
            return Some("skill-desktop-control");
        }
        None
    }
}

#[derive(Debug, Deserialize)]
struct SubTaskRaw {
    id: String,
    description: String,
    app_hint: Option<String>,
    dependencies: Option<Vec<String>>,
}
