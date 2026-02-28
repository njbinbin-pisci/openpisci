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
        Self { client, model, max_tokens }
    }

    pub async fn decompose_task(&self, user_request: &str) -> Result<Vec<SubTask>> {
        let system = "You are a task decomposition agent. Given a user request, break it down into \
            a sequence of concrete sub-tasks. Each sub-task should be independently executable. \
            Return a JSON array of objects with fields: id, description, app_hint (optional application name), \
            dependencies (array of prerequisite task ids). \
            Keep it concise - most requests need 1-3 sub-tasks.".to_string();

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
        };

        let response = self.client.complete(req).await?;
        let text = response.content.trim();

        let json_start = text.find('[').unwrap_or(0);
        let json_end = text.rfind(']').map(|i| i + 1).unwrap_or(text.len());
        let json_str = &text[json_start..json_end];

        match serde_json::from_str::<Vec<SubTaskRaw>>(json_str) {
            Ok(raw_tasks) => {
                Ok(raw_tasks.into_iter().map(|t| SubTask {
                    id: t.id,
                    description: t.description,
                    app_hint: t.app_hint,
                    dependencies: t.dependencies.unwrap_or_default(),
                    status: SubTaskStatus::Pending,
                }).collect())
            }
            Err(_) => {
                Ok(vec![SubTask {
                    id: "task_1".to_string(),
                    description: user_request.to_string(),
                    app_hint: None,
                    dependencies: vec![],
                    status: SubTaskStatus::Pending,
                }])
            }
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
}

#[derive(Debug, Deserialize)]
struct SubTaskRaw {
    id: String,
    description: String,
    app_hint: Option<String>,
    dependencies: Option<Vec<String>>,
}
