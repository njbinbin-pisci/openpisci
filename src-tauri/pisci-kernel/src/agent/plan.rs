use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanTodoItem {
    pub id: String,
    pub content: String,
    pub status: String,
}

/// Shared per-session plan state used by the kernel `plan_todo` tool and
/// by host code that renders the plan back into the system prompt (task
/// spine) or forwards it into scheduler turns.
pub type PlanState = HashMap<String, Vec<PlanTodoItem>>;

/// Thread-safe handle to [`PlanState`]. Hosts construct one at startup
/// and hand the same `Arc` to (a) the neutral `plan_todo` tool, (b) any
/// call-Koi / call-Fish tool that forwards plan context, and (c) chat
/// command handlers that rehydrate the task spine.
pub type PlanStore = Arc<Mutex<PlanState>>;

/// Build an empty [`PlanStore`]. Equivalent to `Arc::new(Mutex::new(..))`,
/// exposed so hosts avoid importing `tokio::sync` directly.
pub fn new_plan_store() -> PlanStore {
    Arc::new(Mutex::new(HashMap::new()))
}

impl PlanTodoItem {
    pub fn normalized(&self) -> Self {
        Self {
            id: self.id.trim().to_string(),
            content: self.content.trim().to_string(),
            status: self.status.trim().to_string(),
        }
    }
}

pub fn validate_todos(items: &[PlanTodoItem]) -> Result<(), String> {
    if items.is_empty() {
        return Err("至少需要 1 个 todo 项".into());
    }

    let mut seen = std::collections::HashSet::new();
    let mut in_progress_count = 0usize;

    for item in items {
        let item = item.normalized();
        if item.id.is_empty() {
            return Err("todo.id 不能为空".into());
        }
        if item.content.is_empty() {
            return Err(format!("todo '{}' 的 content 不能为空", item.id));
        }
        match item.status.as_str() {
            "pending" | "in_progress" | "completed" | "cancelled" => {}
            other => {
                return Err(format!(
                    "todo '{}' 的 status '{}' 无效，必须是 pending / in_progress / completed / cancelled",
                    item.id, other
                ));
            }
        }
        if !seen.insert(item.id.clone()) {
            return Err(format!("todo id '{}' 重复", item.id));
        }
        if item.status == "in_progress" {
            in_progress_count += 1;
        }
    }

    if in_progress_count > 1 {
        return Err("同一时间最多只能有 1 个 todo 处于 in_progress".into());
    }

    Ok(())
}

pub fn merge_todos(existing: &[PlanTodoItem], updates: &[PlanTodoItem]) -> Vec<PlanTodoItem> {
    let mut merged = existing.to_vec();
    for update in updates {
        if let Some(idx) = merged.iter().position(|item| item.id == update.id) {
            merged[idx] = update.clone();
        } else {
            merged.push(update.clone());
        }
    }
    merged
}

pub fn summarize_todos(items: &[PlanTodoItem]) -> String {
    items
        .iter()
        .map(|item| format!("- [{}] {} ({})", item.status, item.content, item.id))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::{merge_todos, validate_todos, PlanTodoItem};

    fn todo(id: &str, content: &str, status: &str) -> PlanTodoItem {
        PlanTodoItem {
            id: id.into(),
            content: content.into(),
            status: status.into(),
        }
    }

    #[test]
    fn validate_rejects_multiple_in_progress() {
        let err = validate_todos(&[
            todo("a", "first", "in_progress"),
            todo("b", "second", "in_progress"),
        ])
        .unwrap_err();
        assert!(err.contains("最多"));
    }

    #[test]
    fn merge_updates_existing_and_appends_new() {
        let merged = merge_todos(
            &[
                todo("a", "first", "pending"),
                todo("b", "second", "in_progress"),
            ],
            &[
                todo("b", "second updated", "completed"),
                todo("c", "third", "pending"),
            ],
        );

        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].id, "a");
        assert_eq!(merged[1].content, "second updated");
        assert_eq!(merged[1].status, "completed");
        assert_eq!(merged[2].id, "c");
    }
}
