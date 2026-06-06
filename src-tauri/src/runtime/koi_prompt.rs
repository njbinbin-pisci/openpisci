//! Shared Koi task system-prompt assembly for every desktop execution path
//! (`call_koi`, in-process coordinator turns, soft-fence retries).

use crate::commands::config::scene::{MemorySliceMode, SceneKind, ScenePolicy};
use crate::store::db::TaskState;
use crate::store::AppState;
use piscis_core::koi_prompt::{build_koi_persona_system_prompt, build_koi_task_system_prompt};
use piscis_core::models::KoiDefinition;
use piscis_core::project_state::build_coordination_event_digest;

fn truncate_chars(content: &str, max_chars: usize) -> String {
    if max_chars == 0 || content.chars().count() <= max_chars {
        return content.to_string();
    }
    format!("{}...", content.chars().take(max_chars).collect::<String>())
}

fn koi_continuity_scope_id(koi_id: &str, pool_session_id: Option<&str>) -> String {
    format!("{}::{}", koi_id, pool_session_id.unwrap_or("default"))
}

fn koi_continuity_context(task_state: &TaskState) -> String {
    crate::commands::chat::render_task_state_section(
        "Your Recent Working Context",
        "Most Recent Outcome",
        task_state,
    )
}

/// Build the full 6-layer Koi protocol system prompt (Run Shape, Stop Gate, …)
/// with pool/board/memory context slices — used by both `call_koi` and the
/// in-process coordinator subagent path.
pub async fn assemble_koi_task_system_prompt(
    state: &AppState,
    koi_def: &KoiDefinition,
    pool_session_id: Option<&str>,
    assignment_text: &str,
) -> String {
    let scene_policy = ScenePolicy::for_kind(SceneKind::KoiTask);
    let koi_id = koi_def.id.clone();

    let org_spec_ctx = if let Some(psid) = pool_session_id {
        let db = state.db.lock().await;
        if let Ok(Some(session)) = db.resolve_pool_session_identifier(psid) {
            if session.org_spec.is_empty() {
                String::new()
            } else {
                format!(
                    "\n\n## Project Organization\n{}",
                    truncate_chars(&session.org_spec, scene_policy.org_spec_preview_chars())
                )
            }
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let continuity_context = {
        let scope_id = koi_continuity_scope_id(&koi_id, pool_session_id);
        let db = state.db.lock().await;
        db.load_task_state("koi_session", &scope_id)
            .ok()
            .flatten()
            .map(|s| koi_continuity_context(&s))
            .unwrap_or_default()
    };

    let memory_context = {
        let task = assignment_text.trim();
        let db = state.db.lock().await;
        let mut sections = Vec::new();
        let koi_memories = db
            .search_memories_scoped(task, &koi_id, pool_session_id, 5)
            .unwrap_or_default();
        if !koi_memories.is_empty() {
            let items: Vec<String> = koi_memories
                .iter()
                .map(|m| {
                    let scope_tag = if m.scope_type != "private" {
                        format!(" [{}]", m.scope_type)
                    } else {
                        String::new()
                    };
                    format!("- [{}]{} {}", m.category, scope_tag, m.content)
                })
                .collect();
            sections.push(format!("\n\n## Your Memories\n{}", items.join("\n")));
        }
        if matches!(
            scene_policy.memory_slice_mode(),
            MemorySliceMode::ScopedPlusRecent
        ) {
            let recent_items = db
                .list_memories_for_owner(&koi_id)
                .unwrap_or_default()
                .into_iter()
                .take(3)
                .map(|m| format!("- [{}] {}", m.category, truncate_chars(&m.content, 180)))
                .collect::<Vec<_>>();
            if !recent_items.is_empty() {
                sections.push(format!(
                    "\n\n## Recently Saved Memory\n{}",
                    recent_items.join("\n")
                ));
            }
        }
        sections.join("")
    };

    let pool_chat_ctx = if let Some(psid) = pool_session_id {
        let db = state.db.lock().await;
        let messages = db
            .get_pool_messages(psid, scene_policy.recent_pool_message_limit() as i64 * 2, 0)
            .unwrap_or_default();
        if messages.is_empty() {
            String::new()
        } else {
            let kois = db.list_kois().unwrap_or_default();
            let koi_names: std::collections::HashMap<String, String> = kois
                .iter()
                .map(|k| (k.id.clone(), format!("{} {}", k.icon, k.name)))
                .collect();
            let digest = build_coordination_event_digest(
                &messages,
                scene_policy.event_digest_mode(),
                &[koi_def.name.as_str()],
                scene_policy.recent_pool_message_limit(),
                scene_policy.recent_pool_message_chars(),
            );
            let raw_lines: Vec<String> = messages
                .iter()
                .rev()
                .take(3)
                .rev()
                .map(|m| {
                    let sender = koi_names
                        .get(&m.sender_id)
                        .cloned()
                        .unwrap_or_else(|| m.sender_id.clone());
                    let time = m.created_at.format("%m-%d %H:%M").to_string();
                    let content =
                        truncate_chars(&m.content, scene_policy.recent_pool_message_chars());
                    format!("[{}] {} ({}): {}", time, sender, m.msg_type, content)
                })
                .collect();
            let mut section = String::new();
            if !digest.lines.is_empty() {
                section.push_str("\n\n## Coordination Event Digest\n");
                section.push_str(&digest.lines.join("\n"));
            }
            if !raw_lines.is_empty() {
                section.push_str("\n\n## Latest Raw Pool Messages\n");
                section.push_str(&raw_lines.join("\n"));
            }
            section
        }
    } else {
        String::new()
    };

    let board_state_ctx = if let Some(psid) = pool_session_id {
        let db = state.db.lock().await;
        let all_todos = db.list_koi_todos(None).unwrap_or_default();
        let pool_todos: Vec<_> = all_todos
            .iter()
            .filter(|t| t.pool_session_id.as_deref() == Some(psid))
            .collect();
        if pool_todos.is_empty() {
            String::new()
        } else {
            let lines: Vec<String> = pool_todos
                .iter()
                .map(|t| {
                    let marker = match t.status.as_str() {
                        "in_progress" => "🔄",
                        "todo" => "📋",
                        "done" => "✅",
                        "blocked" => "🚫",
                        _ => "❓",
                    };
                    format!(
                        "{} [{}] {} — \"{}\"",
                        marker,
                        t.status,
                        t.owner_id,
                        t.title.chars().take(80).collect::<String>()
                    )
                })
                .collect();
            format!("\n\n## Current Board State\n{}", lines.join("\n"))
        }
    } else {
        String::new()
    };

    let kb_ctx = {
        let ws = {
            let settings = state.settings.lock().await;
            settings.workspace_root.clone()
        };
        let kb_path = std::path::Path::new(&ws).join("kb");
        if kb_path.exists() {
            let entries: Vec<String> = std::fs::read_dir(&kb_path)
                .unwrap_or_else(|_| std::fs::read_dir(std::path::Path::new(&ws)).unwrap())
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .take(30)
                .collect();
            if entries.is_empty() {
                String::new()
            } else {
                format!(
                    "\n\n## Project Knowledge Base (kb/)\nAvailable directories: {}\nRead relevant files before starting work.",
                    entries.join(", ")
                )
            }
        } else {
            String::new()
        }
    };

    let assignment_ctx = {
        let trimmed = assignment_text.trim();
        if trimmed.is_empty() {
            String::new()
        } else {
            let clipped = if trimmed.chars().count() > 2400 {
                format!("{}...", trimmed.chars().take(2400).collect::<String>())
            } else {
                trimmed.to_string()
            };
            format!(
                "\n\n## Current Assignment\n{}\n\
                 - This assignment remains your active contract for the entire run.\n\
                 - Keep it aligned with the latest relevant pool_chat evidence.\n\
                 - Do not let exploratory tool use, repeated planning, or repeated notifications replace the actual deliverable, handoff target, or completion condition stated here.",
                clipped
            )
        }
    };

    let combined_env_ctx = format!("{}{}{}", board_state_ctx, kb_ctx, pool_chat_ctx);

    build_koi_task_system_prompt(
        &koi_def.system_prompt,
        &koi_def.name,
        &koi_def.icon,
        &continuity_context,
        &memory_context,
        &org_spec_ctx,
        &combined_env_ctx,
        &assignment_ctx,
    )
}

/// Build the Koi persona system prompt for main-chat direct conversation.
pub async fn assemble_koi_persona_system_prompt(
    state: &AppState,
    koi_def: &KoiDefinition,
    query_text: &str,
) -> String {
    let koi_id = koi_def.id.clone();
    let scene_policy = ScenePolicy::for_kind(SceneKind::KoiPersona);

    let memory_context = {
        let task = query_text.trim();
        let db = state.db.lock().await;
        let mut sections = Vec::new();
        let koi_memories = db
            .search_memories_scoped(task, &koi_id, None, 5)
            .unwrap_or_default();
        if !koi_memories.is_empty() {
            let items: Vec<String> = koi_memories
                .iter()
                .map(|m| {
                    let scope_tag = if m.scope_type != "private" {
                        format!(" [{}]", m.scope_type)
                    } else {
                        String::new()
                    };
                    format!("- [{}]{} {}", m.category, scope_tag, m.content)
                })
                .collect();
            sections.push(format!("\n\n## Your Memories\n{}", items.join("\n")));
        }
        if matches!(
            scene_policy.memory_slice_mode(),
            MemorySliceMode::ScopedPlusRecent
        ) {
            let recent_items = db
                .list_memories_for_owner(&koi_id)
                .unwrap_or_default()
                .into_iter()
                .take(3)
                .map(|m| format!("- [{}] {}", m.category, truncate_chars(&m.content, 180)))
                .collect::<Vec<_>>();
            if !recent_items.is_empty() {
                sections.push(format!(
                    "\n\n## Recently Saved Memory\n{}",
                    recent_items.join("\n")
                ));
            }
        }
        sections.join("")
    };

    build_koi_persona_system_prompt(
        &koi_def.system_prompt,
        &koi_def.name,
        &koi_def.icon,
        &memory_context,
    )
}
