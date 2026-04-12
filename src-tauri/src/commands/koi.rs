/// Koi (锦鲤) commands — CRUD for persistent independent Agents.
use crate::commands::chat::{
    run_agent_headless, HeadlessRunOptions, SESSION_SOURCE_PISCI_INBOX_POOL,
};
use crate::koi::{KoiDefinition, KOI_COLORS, KOI_ICONS};
use crate::pisci::heartbeat::ensure_heartbeat_session;
use crate::store::AppState;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager, State};

#[derive(Serialize)]
pub struct KoiWithStats {
    #[serde(flatten)]
    pub koi: KoiDefinition,
    pub memory_count: i64,
    pub todo_count: i64,
    pub active_todo_count: i64,
}

/// Returned by delete_koi so the frontend can show a confirmation summary.
#[derive(Serialize)]
pub struct KoiDeleteInfo {
    pub name: String,
    pub icon: String,
    pub todo_count: usize,
    pub memory_count: i64,
    pub is_busy: bool,
}

#[tauri::command]
pub async fn list_kois(state: State<'_, AppState>) -> Result<Vec<KoiWithStats>, String> {
    let db = state.db.lock().await;
    let kois = db.list_kois().map_err(|e| e.to_string())?;
    let mut result = Vec::with_capacity(kois.len());
    for koi in kois {
        let memory_count = db.count_memories_for_owner(&koi.id).unwrap_or(0);
        let todos = db.list_koi_todos(Some(&koi.id)).unwrap_or_default();
        let todo_count = todos.len() as i64;
        let active_todo_count = todos
            .iter()
            .filter(|t| t.status == "todo" || t.status == "in_progress")
            .count() as i64;
        result.push(KoiWithStats {
            koi,
            memory_count,
            todo_count,
            active_todo_count,
        });
    }
    Ok(result)
}

#[tauri::command]
pub async fn get_koi(
    state: State<'_, AppState>,
    id: String,
) -> Result<Option<KoiDefinition>, String> {
    let db = state.db.lock().await;
    db.get_koi(&id).map_err(|e| e.to_string())
}

#[derive(Deserialize)]
pub struct CreateKoiInput {
    pub name: String,
    pub role: String,
    pub icon: String,
    pub color: String,
    pub system_prompt: String,
    pub description: String,
    /// Optional named LLM provider id (empty string = use global default)
    #[serde(default)]
    pub llm_provider_id: Option<String>,
    /// Maximum AgentLoop iterations. 0 = use system default (30).
    #[serde(default)]
    pub max_iterations: u32,
    /// Per-Koi default task timeout in seconds. 0 = inherit from project/system.
    #[serde(default)]
    pub task_timeout_secs: u32,
}

#[tauri::command]
pub async fn create_koi(
    state: State<'_, AppState>,
    input: CreateKoiInput,
) -> Result<KoiDefinition, String> {
    let db = state.db.lock().await;
    let existing = db.list_kois().map_err(|e| e.to_string())?;
    const MAX_KOIS: usize = 10;
    if existing.len() >= MAX_KOIS {
        return Err(format!(
            "已达到 Koi 数量上限 ({}/{}). 请删除或编辑现有 Koi.",
            existing.len(),
            MAX_KOIS
        ));
    }
    let provider_id = input.llm_provider_id.as_deref().filter(|s| !s.is_empty());
    db.create_koi(
        &input.name,
        &input.role,
        &input.icon,
        &input.color,
        &input.system_prompt,
        &input.description,
        provider_id,
        input.max_iterations,
        input.task_timeout_secs,
    )
    .map_err(|e| e.to_string())
}

#[derive(Deserialize)]
pub struct UpdateKoiInput {
    pub id: String,
    pub name: Option<String>,
    pub role: Option<String>,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub system_prompt: Option<String>,
    pub description: Option<String>,
    /// `None` = don't touch the field; `Some("")` = clear (use global); `Some("id")` = set
    #[serde(default)]
    pub llm_provider_id: Option<String>,
    /// `None` = don't touch; `Some(0)` = use system default; `Some(n)` = set to n
    #[serde(default)]
    pub max_iterations: Option<u32>,
    /// `None` = don't touch; `Some(0)` = inherit; `Some(n)` = set task timeout seconds
    #[serde(default)]
    pub task_timeout_secs: Option<u32>,
}

#[tauri::command]
pub async fn update_koi(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    input: UpdateKoiInput,
) -> Result<(), String> {
    // Read old koi before update so we can detect name/role changes
    let old_koi = {
        let db = state.db.lock().await;
        db.get_koi(&input.id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Koi '{}' not found", input.id))?
    };

    {
        let db = state.db.lock().await;
        let provider_update: Option<Option<&str>> =
            input
                .llm_provider_id
                .as_ref()
                .map(|s| if s.is_empty() { None } else { Some(s.as_str()) });
        db.update_koi(
            &input.id,
            input.name.as_deref(),
            input.role.as_deref(),
            input.icon.as_deref(),
            input.color.as_deref(),
            input.system_prompt.as_deref(),
            input.description.as_deref(),
            provider_update,
            input.max_iterations,
            input.task_timeout_secs,
        )
        .map_err(|e| e.to_string())?;
    }

    let name_changed = input
        .name
        .as_deref()
        .map(|n| n != old_koi.name)
        .unwrap_or(false);
    let new_name = input.name.as_deref().unwrap_or(&old_koi.name);
    let new_role = input.role.as_deref().unwrap_or(&old_koi.role);
    let role_changed = input
        .role
        .as_deref()
        .map(|r| r != old_koi.role)
        .unwrap_or(false);
    let prompt_changed = input
        .system_prompt
        .as_deref()
        .map(|p| p != old_koi.system_prompt)
        .unwrap_or(false);

    // Collect pools this Koi participates in
    let affected_pools: Vec<(String, String)> = {
        let db = state.db.lock().await;
        let todos = db.list_koi_todos(Some(&input.id)).unwrap_or_default();
        let mut pools: Vec<(String, String)> = todos
            .iter()
            .filter_map(|t| t.pool_session_id.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .filter_map(|psid| {
                db.get_pool_session(&psid)
                    .ok()
                    .flatten()
                    .filter(|p| p.status == "active")
                    .map(|p| (psid, p.name))
            })
            .collect();
        pools.dedup_by(|a, b| a.0 == b.0);
        pools
    };

    // Post system messages and build Pisci context
    let mut change_parts: Vec<String> = Vec::new();

    if name_changed {
        change_parts.push(format!(
            "更名：{} → {}（@mention 请改用 @{}）",
            old_koi.name, new_name, new_name
        ));
        for (psid, pool_name) in &affected_pools {
            let db = state.db.lock().await;
            let msg = format!(
                "📛 {} 已更名为 {}。请在后续对话中使用 @{} 与其沟通。（项目：{}）",
                old_koi.name, new_name, new_name, pool_name
            );
            let _ = db.insert_pool_message(
                psid,
                "system",
                &msg,
                "status_update",
                &serde_json::json!({
                    "event": "koi_renamed",
                    "koi_id": input.id,
                    "old_name": old_koi.name,
                    "new_name": new_name
                })
                .to_string(),
            );
            let _ = app.emit(
                &format!("pool_message_{}", psid),
                serde_json::json!({ "event": "koi_renamed", "koi_id": input.id }),
            );
        }
    }

    if role_changed || prompt_changed {
        let mut desc = format!("调岗：{}", new_name);
        if role_changed {
            desc.push_str(&format!("，角色：{} → {}", old_koi.role, new_role));
        }
        if prompt_changed {
            desc.push_str("，提示词已更新（对正在执行的任务无影响，下次接任务时生效）");
        }
        change_parts.push(desc.clone());
        for (psid, pool_name) in &affected_pools {
            let db = state.db.lock().await;
            let msg = format!("🔄 {}。（项目：{}）", desc, pool_name);
            let _ = db.insert_pool_message(
                psid,
                "system",
                &msg,
                "status_update",
                &serde_json::json!({
                    "event": "koi_updated",
                    "koi_id": input.id
                })
                .to_string(),
            );
            let _ = app.emit(
                &format!("pool_message_{}", psid),
                serde_json::json!({ "event": "koi_updated", "koi_id": input.id }),
            );
        }
    }

    // Trigger Pisci to reassess if there are meaningful changes
    if !change_parts.is_empty() && !affected_pools.is_empty() {
        let pool_names: Vec<&str> = affected_pools.iter().map(|(_, n)| n.as_str()).collect();
        let pisci_prompt = format!(
            "用户对团队成员 {} ({}) 进行了调整：{}\n\n\
             受影响的项目：{}\n\n\
             请评估是否需要：\n\
             1. 在项目 pool_chat 中发布公告说明变化\n\
             2. 重新分配受影响的任务\n\
             3. 询问用户是否需要进一步调整\n\
             如果项目进展正常无需干预，直接回复 HEARTBEAT_OK。",
            new_name,
            new_role,
            change_parts.join("；"),
            pool_names.join("、")
        );

        // Trigger Pisci in the first affected pool's inbox session
        let (first_pool_id, first_pool_name) = &affected_pools[0];
        let session_id = format!("pisci_pool_{}", first_pool_id);
        let app_clone = app.clone();
        let session_id_clone = session_id.clone();
        let pool_name_clone = first_pool_name.clone();
        let pool_id_clone = first_pool_id.clone();
        let pisci_prompt_clone = pisci_prompt.clone();
        tokio::spawn(async move {
            let st = app_clone.state::<AppState>();
            let _ = ensure_heartbeat_session(
                &st,
                &session_id_clone,
                &format!("Pisci · {}", pool_name_clone),
                SESSION_SOURCE_PISCI_INBOX_POOL,
            )
            .await;
            let _ = run_agent_headless(
                &st,
                &session_id_clone,
                &pisci_prompt_clone,
                None,
                "heartbeat",
                Some(HeadlessRunOptions {
                    pool_session_id: Some(pool_id_clone),
                    extra_system_context: Some(
                        "用户手动调整了团队成员配置，请根据当前项目状态决定是否需要重新协调工作。"
                            .to_string(),
                    ),
                    session_title: Some(format!("Pisci · {}", pool_name_clone)),
                    session_source: Some(SESSION_SOURCE_PISCI_INBOX_POOL.to_string()),
                }),
            )
            .await;
        });
    }

    Ok(())
}

/// Returns info about the Koi before deletion so the frontend can show a confirmation dialog.
#[tauri::command]
pub async fn get_koi_delete_info(
    state: State<'_, AppState>,
    id: String,
) -> Result<KoiDeleteInfo, String> {
    let db = state.db.lock().await;
    let koi = db
        .get_koi(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Koi '{}' not found", id))?;
    let todos = db.list_koi_todos(Some(&id)).unwrap_or_default();
    let active_todos: Vec<_> = todos
        .iter()
        .filter(|t| t.status == "todo" || t.status == "in_progress")
        .collect();
    let memory_count = db.count_memories_for_owner(&id).unwrap_or(0);
    Ok(KoiDeleteInfo {
        name: koi.name,
        icon: koi.icon,
        todo_count: active_todos.len(),
        memory_count,
        is_busy: koi.status == "busy",
    })
}

#[tauri::command]
pub async fn delete_koi(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    // Read koi info and affected pools before deletion
    let (koi_name, koi_icon, koi_role, affected_pools) = {
        let db = state.db.lock().await;
        let koi = db
            .get_koi(&id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Koi '{}' not found", id))?;
        if koi.status == "busy" {
            return Err(format!("BUSY:{}:{}", koi.name, koi.role));
        }
        let todos = db.list_koi_todos(Some(&id)).unwrap_or_default();
        let pools: Vec<(String, String)> = todos
            .iter()
            .filter_map(|t| t.pool_session_id.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .filter_map(|psid| {
                db.get_pool_session(&psid)
                    .ok()
                    .flatten()
                    .filter(|p| p.status == "active")
                    .map(|p| (psid, p.name))
            })
            .collect();
        (koi.name, koi.icon, koi.role, pools)
    };

    // Perform deletion
    {
        let db = state.db.lock().await;
        db.delete_koi(&id).map_err(|e| e.to_string())?;
    }

    // Post system messages to affected pools
    for (psid, pool_name) in &affected_pools {
        let db = state.db.lock().await;
        let msg = format!(
            "🚪 {} {} 已离开团队，其所有任务已取消。（项目：{}）",
            koi_icon, koi_name, pool_name
        );
        let _ = db.insert_pool_message(
            psid,
            "system",
            &msg,
            "status_update",
            &serde_json::json!({ "event": "koi_deleted", "koi_id": id }).to_string(),
        );
        let _ = app.emit(
            &format!("pool_message_{}", psid),
            serde_json::json!({ "event": "koi_deleted", "koi_id": id }),
        );
    }

    // Trigger Pisci to reassess affected projects
    if !affected_pools.is_empty() {
        let pool_names: Vec<&str> = affected_pools.iter().map(|(_, n)| n.as_str()).collect();
        let pisci_prompt = format!(
            "用户解雇了团队成员 {} {}（角色：{}），其所有任务已取消。\n\n\
             受影响的项目：{}\n\n\
             请评估是否需要：\n\
             1. 将已取消的任务重新分配给其他合适的 Koi\n\
             2. 在项目 pool_chat 中发布公告\n\
             3. 询问用户是否需要招募替代者\n\
             如果项目无需干预，直接回复 HEARTBEAT_OK。",
            koi_icon,
            koi_name,
            koi_role,
            pool_names.join("、")
        );

        let (first_pool_id, first_pool_name) = &affected_pools[0];
        let session_id = format!("pisci_pool_{}", first_pool_id);
        let app_clone = app.clone();
        let session_id_clone = session_id.clone();
        let pool_name_clone = first_pool_name.clone();
        let pool_id_clone = first_pool_id.clone();
        tokio::spawn(async move {
            let st = app_clone.state::<AppState>();
            let _ = ensure_heartbeat_session(
                &st,
                &session_id_clone,
                &format!("Pisci · {}", pool_name_clone),
                SESSION_SOURCE_PISCI_INBOX_POOL,
            )
            .await;
            let _ = run_agent_headless(
                &st,
                &session_id_clone,
                &pisci_prompt,
                None,
                "heartbeat",
                Some(HeadlessRunOptions {
                    pool_session_id: Some(pool_id_clone),
                    extra_system_context: Some(
                        "用户解雇了一名团队成员，请根据当前项目状态决定是否需要重新分配工作。"
                            .to_string(),
                    ),
                    session_title: Some(format!("Pisci · {}", pool_name_clone)),
                    session_source: Some(SESSION_SOURCE_PISCI_INBOX_POOL.to_string()),
                }),
            )
            .await;
        });
    }

    Ok(())
}

#[derive(Serialize)]
pub struct KoiPalette {
    pub colors: Vec<(String, String)>,
    pub icons: Vec<String>,
}

#[tauri::command]
pub async fn dedup_kois(state: State<'_, AppState>) -> Result<usize, String> {
    let db = state.db.lock().await;
    db.dedup_kois().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_koi_palette() -> Result<KoiPalette, String> {
    Ok(KoiPalette {
        colors: KOI_COLORS
            .iter()
            .map(|(c, n)| (c.to_string(), n.to_string()))
            .collect(),
        icons: KOI_ICONS.iter().map(|s| s.to_string()).collect(),
    })
}

/// Activate or deactivate (vacation) a Koi.
/// When deactivating a busy Koi, returns error "BUSY:<name>:<role>" so the
/// frontend can show a confirmation dialog before force-deactivating.
/// When deactivated: status → offline, uncompleted todos cancelled, pool notified,
/// and Pisci is triggered to reassess affected projects.
#[tauri::command]
pub async fn set_koi_active(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
    active: bool,
    force: Option<bool>,
) -> Result<(), String> {
    let db = state.db.lock().await;
    let koi = db
        .get_koi(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Koi '{}' not found", id))?;

    if active {
        if koi.status != "offline" {
            return Ok(());
        }
        db.update_koi_status(&id, "idle")
            .map_err(|e| e.to_string())?;
        let _ = app.emit(
            "koi_status_changed",
            serde_json::json!({ "id": id, "status": "idle" }),
        );

        // Trigger Pisci to check if there's pending work for this Koi
        let koi_name = koi.name.clone();
        let koi_role = koi.role.clone();
        let koi_icon = koi.icon.clone();
        let todos = db.list_koi_todos(Some(&id)).unwrap_or_default();
        let affected_pools: Vec<(String, String)> = todos
            .iter()
            .filter_map(|t| t.pool_session_id.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .filter_map(|psid| {
                db.get_pool_session(&psid)
                    .ok()
                    .flatten()
                    .filter(|p| p.status == "active")
                    .map(|p| (psid, p.name))
            })
            .collect();
        drop(db);

        if !affected_pools.is_empty() {
            let pool_names: Vec<&str> = affected_pools.iter().map(|(_, n)| n.as_str()).collect();
            let pisci_prompt = format!(
                "{} {}（{}）已回归上班。\n\n\
                 受影响的项目：{}\n\n\
                 请检查是否有待分配或被取消的任务可以重新交给 {}，\
                 或在 pool_chat 欢迎其回归并说明下一步工作安排。\n\
                 如果无需干预，直接回复 HEARTBEAT_OK。",
                koi_icon,
                koi_name,
                koi_role,
                pool_names.join("、"),
                koi_name
            );
            let (first_pool_id, first_pool_name) = &affected_pools[0];
            let session_id = format!("pisci_pool_{}", first_pool_id);
            let app_clone = app.clone();
            let session_id_clone = session_id.clone();
            let pool_name_clone = first_pool_name.clone();
            let pool_id_clone = first_pool_id.clone();
            tokio::spawn(async move {
                let st = app_clone.state::<AppState>();
                let _ = ensure_heartbeat_session(
                    &st,
                    &session_id_clone,
                    &format!("Pisci · {}", pool_name_clone),
                    SESSION_SOURCE_PISCI_INBOX_POOL,
                )
                .await;
                let _ = run_agent_headless(
                    &st,
                    &session_id_clone,
                    &pisci_prompt,
                    None,
                    "heartbeat",
                    Some(HeadlessRunOptions {
                        pool_session_id: Some(pool_id_clone),
                        extra_system_context: Some(
                            "团队成员回归上班，请检查是否有工作需要重新安排。".to_string(),
                        ),
                        session_title: Some(format!("Pisci · {}", pool_name_clone)),
                        session_source: Some(SESSION_SOURCE_PISCI_INBOX_POOL.to_string()),
                    }),
                )
                .await;
            });
        }
    } else {
        if koi.status == "offline" {
            return Ok(());
        }
        // Guard: if busy and not forced, return a sentinel error for the frontend
        if koi.status == "busy" && !force.unwrap_or(false) {
            return Err(format!("BUSY:{}:{}", koi.name, koi.role));
        }

        db.update_koi_status(&id, "offline")
            .map_err(|e| e.to_string())?;
        let _ = app.emit(
            "koi_status_changed",
            serde_json::json!({ "id": id, "status": "offline" }),
        );

        // Cancel all uncompleted todos and collect affected pools
        let todos = db.list_koi_todos(Some(&id)).unwrap_or_default();
        let mut affected_pool_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for todo in &todos {
            if todo.status == "todo" || todo.status == "in_progress" {
                let _ = db.update_koi_todo(&todo.id, None, None, Some("cancelled"), None);
                if let Some(ref psid) = todo.pool_session_id {
                    let _ = db.insert_pool_message(
                        psid,
                        "system",
                        &format!(
                            "{} {} 已进入休假状态，任务「{}」已自动取消。",
                            koi.icon, koi.name, todo.title
                        ),
                        "status_update",
                        &serde_json::json!({
                            "event": "koi_vacation",
                            "koi_id": id,
                            "todo_id": todo.id
                        })
                        .to_string(),
                    );
                    let _ = app.emit(
                        &format!("pool_message_{}", psid),
                        serde_json::json!({ "event": "koi_vacation", "koi_id": id }),
                    );
                    affected_pool_ids.insert(psid.clone());
                }
            }
        }

        // Trigger Pisci to reassess affected projects
        let affected_pools: Vec<(String, String)> = affected_pool_ids
            .into_iter()
            .filter_map(|psid| {
                db.get_pool_session(&psid)
                    .ok()
                    .flatten()
                    .filter(|p| p.status == "active")
                    .map(|p| (psid, p.name))
            })
            .collect();

        if !affected_pools.is_empty() {
            let pool_names: Vec<&str> = affected_pools.iter().map(|(_, n)| n.as_str()).collect();
            let koi_name = koi.name.clone();
            let koi_role = koi.role.clone();
            let koi_icon = koi.icon.clone();
            let pisci_prompt = format!(
                "用户让 {} {}（{}）进入休假，其进行中的任务已自动取消。\n\n\
                 受影响的项目：{}\n\n\
                 请评估是否需要：\n\
                 1. 将已取消的任务重新分配给其他合适的 Koi\n\
                 2. 在项目 pool_chat 中发布公告说明情况\n\
                 如果项目无需干预，直接回复 HEARTBEAT_OK。",
                koi_icon,
                koi_name,
                koi_role,
                pool_names.join("、")
            );
            let (first_pool_id, first_pool_name) = &affected_pools[0];
            let session_id = format!("pisci_pool_{}", first_pool_id);
            let app_clone = app.clone();
            let session_id_clone = session_id.clone();
            let pool_name_clone = first_pool_name.clone();
            let pool_id_clone = first_pool_id.clone();
            drop(db);
            tokio::spawn(async move {
                let st = app_clone.state::<AppState>();
                let _ = ensure_heartbeat_session(
                    &st,
                    &session_id_clone,
                    &format!("Pisci · {}", pool_name_clone),
                    SESSION_SOURCE_PISCI_INBOX_POOL,
                )
                .await;
                let _ = run_agent_headless(
                    &st,
                    &session_id_clone,
                    &pisci_prompt,
                    None,
                    "heartbeat",
                    Some(HeadlessRunOptions {
                        pool_session_id: Some(pool_id_clone),
                        extra_system_context: Some(
                            "团队成员进入休假，请根据当前项目状态决定是否需要重新分配工作。"
                                .to_string(),
                        ),
                        session_title: Some(format!("Pisci · {}", pool_name_clone)),
                        session_source: Some(SESSION_SOURCE_PISCI_INBOX_POOL.to_string()),
                    }),
                )
                .await;
            });
        }
    }
    Ok(())
}
