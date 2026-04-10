use crate::agent::messages::AgentEvent;
use crate::agent::plan::{merge_todos, summarize_todos, validate_todos, PlanTodoItem};
use crate::agent::tool::{Tool, ToolContext, ToolResult};
use crate::store::AppState;
use async_trait::async_trait;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

pub struct PlanTodoTool {
    pub app: AppHandle,
}

#[async_trait]
impl Tool for PlanTodoTool {
    fn name(&self) -> &str {
        "plan_todo"
    }

    fn description(&self) -> &str {
        "Maintain a short visible execution plan for the current task. \
         Use this for non-trivial tasks with multiple steps so the user can see what you plan to do and what is currently in progress. \
         Keep plans concise (usually 2-7 items), update statuses as you work, and make sure at most one item is 'in_progress' at a time. \
         Prefer replacing the whole list when your plan changes significantly; use merge=true for incremental updates. \
         This tool only updates your visible plan. It does NOT execute work, does NOT create deliverables, does NOT send pool messages, and does NOT replace using the real tools needed to move the task forward."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["todos"],
            "properties": {
                "merge": {
                    "type": "boolean",
                    "description": "If true, update existing todo items by id and append new ones. If false or omitted, replace the whole plan."
                },
                "todos": {
                    "type": "array",
                    "description": "The plan items to set or update.",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "required": ["id", "content", "status"],
                        "properties": {
                            "id": { "type": "string", "description": "Stable todo id, e.g. 'scan-files'" },
                            "content": { "type": "string", "description": "Short user-facing task description" },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed", "cancelled"]
                            }
                        }
                    }
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let merge = input["merge"].as_bool().unwrap_or(false);
        let todos: Vec<PlanTodoItem> = match serde_json::from_value(input["todos"].clone()) {
            Ok(items) => items,
            Err(e) => return Ok(ToolResult::err(format!("'todos' 格式无效: {}", e))),
        };
        if let Err(e) = validate_todos(&todos) {
            return Ok(ToolResult::err(e));
        }

        let state = self.app.state::<AppState>();
        let updated = {
            let mut plan_state = state.plan_state.lock().await;
            let current = plan_state.get(&ctx.session_id).cloned().unwrap_or_default();
            let next = if merge {
                merge_todos(&current, &todos)
            } else {
                todos.clone()
            };
            if let Err(e) = validate_todos(&next) {
                return Ok(ToolResult::err(e));
            }
            plan_state.insert(ctx.session_id.clone(), next.clone());
            next
        };

        let payload = serde_json::to_value(AgentEvent::PlanUpdate {
            items: updated.clone(),
        })?;
        let _ = self
            .app
            .emit(&format!("agent_event_{}", ctx.session_id), payload);

        let scope_note = if ctx.pool_session_id.is_some() {
            "\n\n注意：这只更新你的内部计划板，不会把结果发送到 `pool_chat`，也不会创建、认领或完成 `pool_org` 的协作 todo。\
             如果你在协作池中工作，更新计划后仍需要用实际执行工具推进交付，并在需要时显式 handoff 或汇报状态。"
        } else {
            "\n\n注意：这只更新计划板，不会直接完成任务。更新计划后仍需要调用实际工具或直接产出结果。"
        };

        Ok(ToolResult::ok(format!(
            "计划已更新（{} 项）:\n{}{}",
            updated.len(),
            summarize_todos(&updated),
            scope_note
        )))
    }
}
