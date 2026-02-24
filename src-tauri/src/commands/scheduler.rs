use crate::store::{db::ScheduledTask, AppState};
use serde::Serialize;
use tauri::State;

#[derive(Debug, Serialize)]
pub struct TaskList {
    pub tasks: Vec<ScheduledTask>,
    pub total: usize,
}

#[tauri::command]
pub async fn list_tasks(state: State<'_, AppState>) -> Result<TaskList, String> {
    let db = state.db.lock().await;
    let tasks = db.list_tasks().map_err(|e| e.to_string())?;
    let total = tasks.len();
    Ok(TaskList { tasks, total })
}

#[tauri::command]
pub async fn create_task(
    state: State<'_, AppState>,
    name: String,
    description: Option<String>,
    cron_expression: String,
    task_prompt: String,
) -> Result<ScheduledTask, String> {
    // Validate cron expression (5 parts)
    let parts: Vec<&str> = cron_expression.trim().split_whitespace().collect();
    if parts.len() != 5 {
        return Err(format!(
            "Invalid cron expression '{}': must have 5 parts (minute hour day month weekday)",
            cron_expression
        ));
    }

    let db = state.db.lock().await;
    db.create_task(&name, description.as_deref(), &cron_expression, &task_prompt)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn update_task(
    state: State<'_, AppState>,
    task_id: String,
    name: Option<String>,
    cron_expression: Option<String>,
    task_prompt: Option<String>,
    status: Option<String>,
) -> Result<(), String> {
    let db = state.db.lock().await;
    db.update_task(
        &task_id,
        name.as_deref(),
        cron_expression.as_deref(),
        task_prompt.as_deref(),
        status.as_deref(),
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_task(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<(), String> {
    let db = state.db.lock().await;
    db.delete_task(&task_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn run_task_now(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<String, String> {
    let task = {
        let db = state.db.lock().await;
        db.get_task(&task_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Task {} not found", task_id))?
    };

    // Record the run
    {
        let db = state.db.lock().await;
        db.record_task_run(&task_id).map_err(|e| e.to_string())?;
    }

    Ok(format!("Task '{}' triggered manually", task.name))
}
