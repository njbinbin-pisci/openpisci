/// Collaboration Trial — spawn real Koi agents with LLM to test multi-agent cooperation.
///
/// Unlike `test_runner` (which uses mock execution), this module:
/// - Creates real Koi agents in the production DB
/// - Creates a real Pool session visible in the UI
/// - Uses KoiRuntime with TauriEventBus to drive real LLM agent loops
/// - All events stream to the Chat Pool and Board in real-time
///
/// The user can observe the full collaboration in the Pond UI.
use crate::koi::runtime::KoiRuntime;
use crate::pisci::project_state::{
    assess_project_state, ProjectAssessment as TrialAssessment, ProjectDecision as TrialDecision,
};
use crate::store::AppState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use tauri::{Emitter, State};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrialStatus {
    pub phase: String,
    pub pool_id: String,
    pub koi_ids: Vec<String>,
    pub steps: Vec<TrialStep>,
    pub completed: bool,
    pub error: Option<String>,
    pub error_key: Option<String>,
    pub error_params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrialStep {
    pub name: String,
    pub koi_name: String,
    pub task: String,
    pub success: bool,
    pub reply_preview: String,
    pub reply_preview_key: Option<String>,
    pub reply_preview_params: Option<Value>,
    pub duration_ms: u64,
}

struct TrialKoiSpec {
    name: &'static str,
    role: &'static str,
    icon: &'static str,
    color: &'static str,
    system_prompt: &'static str,
    description: &'static str,
}

fn normalize_trial_text(value: &str) -> String {
    value.trim().to_lowercase()
}

fn ensure_trial_koi(
    db: &crate::store::db::Database,
    all_kois: &mut Vec<crate::koi::KoiDefinition>,
    spec: &TrialKoiSpec,
) -> Result<crate::koi::KoiDefinition, String> {
    let role_key = normalize_trial_text(spec.role);
    if let Some(existing) = all_kois
        .iter()
        .find(|k| normalize_trial_text(&k.role) == role_key)
        .cloned()
    {
        return Ok(existing);
    }

    if let Some(existing) = all_kois.iter().find(|k| k.name == spec.name).cloned() {
        if existing.role != spec.role {
            db.update_koi(
                &existing.id,
                None,
                Some(spec.role),
                None,
                None,
                None,
                None,
                None,
            )
            .map_err(|e| e.to_string())?;

            let mut updated = existing.clone();
            updated.role = spec.role.to_string();
            if let Some(idx) = all_kois.iter().position(|k| k.id == updated.id) {
                all_kois[idx] = updated.clone();
            }
            return Ok(updated);
        }
        return Ok(existing);
    }

    let created = db
        .create_koi(
            spec.name,
            spec.role,
            spec.icon,
            spec.color,
            spec.system_prompt,
            spec.description,
            None,
        )
        .map_err(|e| e.to_string())?;
    all_kois.push(created.clone());
    Ok(created)
}

fn set_trial_error(status: &mut TrialStatus, key: &str, params: Value, fallback: String) {
    status.error = Some(fallback);
    status.error_key = Some(key.to_string());
    status.error_params = Some(params);
}

pub(crate) fn assess_trial_project_state(
    messages: &[crate::koi::PoolMessage],
    todos: &[crate::koi::KoiTodo],
    koi_ids: &[String],
) -> TrialAssessment {
    assess_project_state(messages, todos, koi_ids)
}

/// Launch a multi-agent collaboration trial.
///
/// Creates 3 Koi agents (Architect, Coder, Reviewer), a project pool,
/// and orchestrates a realistic task flow with @mention handoffs.
/// All results are observable in the Pond UI.
#[tauri::command]
pub async fn run_collaboration_trial(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<TrialStatus, String> {
    run_collaboration_trial_with_state(app, &state).await
}

pub async fn run_collaboration_trial_with_state(
    app: tauri::AppHandle,
    state: &AppState,
) -> Result<TrialStatus, String> {
    tracing::info!("=== Collaboration Trial: starting ===");

    let app_handle = app.clone();
    let runtime = KoiRuntime::from_tauri(app.clone(), state.db.clone());
    let mut status = TrialStatus {
        phase: "setup".into(),
        pool_id: String::new(),
        koi_ids: vec![],
        steps: vec![],
        completed: false,
        error: None,
        error_key: None,
        error_params: None,
    };

    let pool_id_cell: std::sync::Arc<std::sync::Mutex<String>> =
        std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let pool_id_for_emit = pool_id_cell.clone();
    let emit = move |phase: &str, detail: &str| {
        let pid = pool_id_for_emit.lock().unwrap().clone();
        let mut payload = json!({ "phase": phase, "detail": detail });
        if !pid.is_empty() {
            payload["pool_id"] = json!(pid);
        }
        let _ = app.emit("collab_trial_progress", payload);
    };

    // ─── Phase 1: Find or create Koi agents ─────────────────────
    emit(
        "setup",
        "Checking required Koi roles and creating missing ones...",
    );

    let (architect, coder, reviewer, pool) = {
        let db = state.db.lock().await;
        let mut all_kois = db.list_kois().map_err(|e| e.to_string())?;

        let architect = ensure_trial_koi(
            &db,
            &mut all_kois,
            &TrialKoiSpec {
                name: "Architect",
                role: "架构师",
                icon: "🏗️",
                color: "#7c6af7",
                system_prompt:
                    "You are a software architect. Your job is to design clear, practical technical specifications. \
                     Be concise and structured. Output your design as a numbered specification with clear sections. \
                     When you finish a design, signal `[ProjectStatus] follow_up_needed` in pool_chat and @mention whoever should implement next. \
                     Do not decide that the project is finished yourself.",
                description: "Architecture, system design, technical specification",
            },
        )?;

        let coder = ensure_trial_koi(
            &db,
            &mut all_kois,
            &TrialKoiSpec {
                name: "Coder",
                role: "程序员",
                icon: "💻",
                color: "#45b7d1",
                system_prompt:
                    "You are a software developer. Given a specification, write clean, working code. \
                     Be practical and focus on correctness. If review or further work is needed, signal `[ProjectStatus] follow_up_needed` and @mention the next actor. \
                     If you address requested changes and believe the work may be ready, signal `[ProjectStatus] ready_for_pisci_review` or @mention the next reviewer as appropriate.",
                description: "Implementation, coding, development",
            },
        )?;

        let reviewer = ensure_trial_koi(
            &db,
            &mut all_kois,
            &TrialKoiSpec {
                name: "Reviewer",
                role: "代码审查员",
                icon: "🔍",
                color: "#26de81",
                system_prompt:
                    "You are a code reviewer. Given code or a design, provide constructive feedback. \
                     Point out issues, suggest improvements, and give an overall assessment. \
                     Be specific and actionable. If more work is needed, signal `[ProjectStatus] follow_up_needed` and @mention the responsible Koi. \
                     If the work looks acceptable, signal `[ProjectStatus] ready_for_pisci_review` and @mention Pisci rather than declaring the project finished yourself.",
                description: "Code review, quality assurance, feedback",
            },
        )?;

        let pool = db
            .create_pool_session("Collaboration Trial")
            .map_err(|e| e.to_string())?;

        let org_spec = format!(
            "## Project: Collaboration Trial\n\n\
             ### Goal\n\
             Test multi-agent collaboration by designing and reviewing a simple utility module.\n\n\
             ### Team\n\
             - **Architect** ({}): Design the specification\n\
             - **Coder** ({}): Implement based on the spec\n\
             - **Reviewer** ({}): Review the implementation\n\n\
             ### Workflow\n\
             1. Pisci assigns the design task to Architect\n\
             2. Architect produces a spec, then @Coder implements it\n\
             3. Coder produces code, then @Reviewer reviews it\n\
             4. If review requests changes, the work loops until Pisci judges it is ready to wrap up\n\n\
             ### Success Criteria\n\
             - Each task builds on the previous agent's output\n\
             - Communication flows through the pool chat\n\
             - If more work is needed, agents clearly signal `[ProjectStatus] follow_up_needed`\n\
             - When the project may be ready to conclude, an agent signals `[ProjectStatus] ready_for_pisci_review` and Pisci decides whether the trial can end",
            architect.id, coder.id, reviewer.id
        );
        db.update_pool_org_spec(&pool.id, &org_spec)
            .map_err(|e| e.to_string())?;

        // Post the project kickoff to the pool
        db.insert_pool_message(
            &pool.id,
            "pisci",
            &format!(
                "🚀 **Collaboration Trial started**\n\n\
                 Team: {} Architect, {} Coder, {} Reviewer\n\
                 Goal: Design and review a simple \"string utility\" module.\n\n\
                 Workflow: Architect → Coder → Reviewer",
                architect.icon, coder.icon, reviewer.icon
            ),
            "text",
            "{}",
        )
        .map_err(|e| e.to_string())?;

        (architect, coder, reviewer, pool)
    };

    status.pool_id = pool.id.clone();
    status.koi_ids = vec![architect.id.clone(), coder.id.clone(), reviewer.id.clone()];
    *pool_id_cell.lock().unwrap() = pool.id.clone();
    emit("pool_ready", "Pool session created, agents ready");

    tracing::info!(
        "Trial setup: pool={}, architect={}, coder={}, reviewer={}",
        pool.id,
        architect.id,
        coder.id,
        reviewer.id
    );

    // ─── Phase 2: Pisci posts @Architect in pool chat (natural communication) ──
    // The entire workflow is driven by @mention cascading:
    //   Pisci @Architect → Architect designs, @Coder → Coder implements, @Reviewer → Reviewer reviews
    // No direct assign_koi calls — everything flows through pool_chat @mentions.
    status.phase = "architect".into();
    emit("architect", "Pisci @Architect in pool chat...");

    let task_message = "@Architect Design a small \"string utility\" module with 3 functions: \
         1) reverse_words(s) - reverses word order in a sentence \
         2) count_vowels(s) - counts vowels in a string \
         3) to_title_case(s) - converts a string to title case. \
         Write a clear, concise specification with function signatures, \
         parameter descriptions, expected behavior, and edge cases. \
         Keep it practical. When you finish, share the spec in pool_chat, include `[ProjectStatus] follow_up_needed`, and @Coder to hand off implementation."
        .to_string();

    // Post the message to pool chat (just like Pisci would via pool_chat tool)
    {
        let db = state.db.lock().await;
        let msg = db
            .insert_pool_message(&pool.id, "pisci", &task_message, "mention", "{}")
            .map_err(|e| e.to_string())?;
        let _ = app_handle.emit(
            &format!("pool_message_{}", pool.id),
            serde_json::to_value(&msg).unwrap_or_default(),
        );
    }

    // The @mention dispatch activates Architect (assigns task + blocks until done)
    let chain_start = std::time::Instant::now();
    let arch_results = runtime
        .handle_mention("pisci", &pool.id, &task_message)
        .await;

    let arch_step = match &arch_results {
        Ok(results) if !results.is_empty() && results[0].success => TrialStep {
            name: "design_spec".into(),
            koi_name: "Architect".into(),
            task: "Design string utility module spec".into(),
            success: true,
            reply_preview: results[0].reply.chars().take(200).collect(),
            reply_preview_key: None,
            reply_preview_params: None,
            duration_ms: chain_start.elapsed().as_millis() as u64,
        },
        Ok(_) | Err(_) => TrialStep {
            name: "design_spec".into(),
            koi_name: "Architect".into(),
            task: "Design string utility module spec".into(),
            success: false,
            reply_preview: arch_results
                .as_ref()
                .map(|r| r.first().map(|x| x.reply.clone()).unwrap_or_default())
                .unwrap_or_else(|e| format!("Error: {}", e)),
            reply_preview_key: Some("debug.multiAgentErrWithDetail".into()),
            reply_preview_params: Some(json!({ "detail": "Architect dispatch failed" })),
            duration_ms: chain_start.elapsed().as_millis() as u64,
        },
    };
    tracing::info!(
        "[Trial] Architect: {} ({}ms)",
        if arch_step.success { "PASS" } else { "FAIL" },
        arch_step.duration_ms
    );
    status.steps.push(arch_step.clone());

    if !arch_step.success {
        set_trial_error(
            &mut status,
            "debug.multiAgentTrialTaskFailed",
            json!({ "subject": "Architect" }),
            "Architect task failed".into(),
        );
        emit("error", "Architect task failed");
        return Ok(status);
    }

    // ─── Phase 3 & 4: Wait for the collaboration chain to settle, then let Pisci judge readiness ───
    // The trial no longer ends just because a fixed role completed. Instead, it watches the pool until
    // work is either clearly still in progress or looks ready for Pisci review.
    status.phase = "chain".into();
    emit(
        "chain",
        "Waiting for collaboration to settle so Pisci can assess project state...",
    );

    let chain_timeout = std::time::Duration::from_secs(900);
    let poll_interval = std::time::Duration::from_secs(5);
    let quiet_polls_needed = 2u32;
    let mut quiet_polls = 0u32;
    let mut last_phase_detail = String::new();
    let mut last_message_count = 0usize;
    let mut seen_completion_event_ids: HashSet<i64> = HashSet::new();
    let mut completion_counts: HashMap<String, usize> = HashMap::new();
    let mut final_assessment = TrialAssessment {
        decision: TrialDecision::Continue,
        active_todo_count: 0,
        blocked_todo_count: 0,
        follow_up_signal_count: 0,
        ready_signal_count: 0,
        explicit_pisci_handoff_count: 0,
        summary: "No assessment yet.".into(),
    };

    loop {
        tokio::time::sleep(poll_interval).await;

        if chain_start.elapsed() > chain_timeout {
            tracing::warn!("[Trial] Chain timed out after {}s", chain_timeout.as_secs());
            emit(
                "timeout",
                "Collaboration timed out before Pisci could conclude",
            );
            break;
        }

        let db = state.db.lock().await;
        let msgs = db.get_pool_messages(&pool.id, 500, 0).unwrap_or_default();
        let all_todos = db.list_koi_todos(None).unwrap_or_default();
        let pool_todos: Vec<_> = all_todos
            .into_iter()
            .filter(|t| t.pool_session_id.as_deref() == Some(&pool.id))
            .collect();
        let architect_koi = db.get_koi(&architect.id).ok().flatten();
        let coder_koi = db.get_koi(&coder.id).ok().flatten();
        let reviewer_koi = db.get_koi(&reviewer.id).ok().flatten();
        drop(db);

        let architect_status = architect_koi
            .as_ref()
            .map(|k| k.status.as_str())
            .unwrap_or("unknown");
        let coder_status = coder_koi
            .as_ref()
            .map(|k| k.status.as_str())
            .unwrap_or("unknown");
        let reviewer_status = reviewer_koi
            .as_ref()
            .map(|k| k.status.as_str())
            .unwrap_or("unknown");

        final_assessment = assess_trial_project_state(&msgs, &pool_todos, &status.koi_ids);
        let phase_detail = format!(
            "Architect: {} | Coder: {} | Reviewer: {} | active_todos: {} | blocked: {} | follow_up: {} | ready: {} | handoff_to_pisci: {}",
            architect_status,
            coder_status,
            reviewer_status,
            final_assessment.active_todo_count,
            final_assessment.blocked_todo_count,
            final_assessment.follow_up_signal_count,
            final_assessment.ready_signal_count,
            final_assessment.explicit_pisci_handoff_count,
        );
        if phase_detail != last_phase_detail {
            emit("chain", &phase_detail);
            last_phase_detail = phase_detail;
        }

        if msgs.len() == last_message_count {
            quiet_polls += 1;
        } else {
            last_message_count = msgs.len();
            quiet_polls = 0;
        }

        for msg in msgs.iter().filter(|m| {
            m.event_type.as_deref() == Some("task_completed")
                || m.event_type.as_deref() == Some("task_failed")
        }) {
            if !status.koi_ids.iter().any(|id| id == &msg.sender_id)
                || !seen_completion_event_ids.insert(msg.id)
            {
                continue;
            }

            let count = completion_counts.entry(msg.sender_id.clone()).or_insert(0);
            *count += 1;

            let (base_name, koi_name, task_label) = if msg.sender_id == architect.id {
                (
                    "design_spec",
                    "Architect",
                    "Design string utility module spec",
                )
            } else if msg.sender_id == coder.id {
                ("implement", "Coder", "Implement string utility module")
            } else if msg.sender_id == reviewer.id {
                ("review", "Reviewer", "Review the implementation")
            } else {
                ("task", "Koi", "Trial task")
            };

            // The architect's first pass is already represented by arch_step above.
            if msg.sender_id == architect.id
                && *count == 1
                && status.steps.iter().any(|s| s.name == "design_spec")
            {
                continue;
            }

            let step_name = if *count == 1 {
                base_name.to_string()
            } else {
                format!("{}_round_{}", base_name, count)
            };
            let success = msg.event_type.as_deref() == Some("task_completed");
            status.steps.push(TrialStep {
                name: step_name,
                koi_name: koi_name.into(),
                task: task_label.into(),
                success,
                reply_preview: msg.content.chars().take(200).collect(),
                reply_preview_key: None,
                reply_preview_params: None,
                duration_ms: chain_start.elapsed().as_millis() as u64,
            });
        }

        let all_idle =
            architect_status == "idle" && coder_status == "idle" && reviewer_status == "idle";
        if all_idle && quiet_polls >= quiet_polls_needed {
            match final_assessment.decision {
                TrialDecision::ReadyForPisciReview => {
                    status.phase = "pisci_review".into();
                    emit("pisci_review", &final_assessment.summary);
                    break;
                }
                TrialDecision::Continue => {
                    if chain_start.elapsed().as_secs() > 30 {
                        emit(
                            "chain",
                            &format!(
                                "Project is not ready to conclude yet: {}",
                                final_assessment.summary
                            ),
                        );
                        break;
                    }
                }
            }
        }
    }

    // Fill in any missing primary steps as failures so the trial remains debuggable.
    if !status
        .steps
        .iter()
        .any(|s| s.name == "implement" || s.name.starts_with("implement_round_"))
    {
        status.steps.push(TrialStep {
            name: "implement".into(),
            koi_name: "Coder".into(),
            task: "Implement string utility module".into(),
            success: false,
            reply_preview: "Coder never produced a completed implementation step".into(),
            reply_preview_key: Some("debug.multiAgentErrWithDetail".into()),
            reply_preview_params: Some(json!({ "detail": "implement_missing" })),
            duration_ms: chain_start.elapsed().as_millis() as u64,
        });
    }
    if !status
        .steps
        .iter()
        .any(|s| s.name == "review" || s.name.starts_with("review_round_"))
    {
        status.steps.push(TrialStep {
            name: "review".into(),
            koi_name: "Reviewer".into(),
            task: "Review the implementation".into(),
            success: false,
            reply_preview: "Reviewer never produced a completed review step".into(),
            reply_preview_key: Some("debug.multiAgentErrWithDetail".into()),
            reply_preview_params: Some(json!({ "detail": "review_missing" })),
            duration_ms: chain_start.elapsed().as_millis() as u64,
        });
    }

    status.steps.push(TrialStep {
        name: "pisci_assess".into(),
        koi_name: "Pisci".into(),
        task: "Assess whether the project is ready to conclude".into(),
        success: final_assessment.decision == TrialDecision::ReadyForPisciReview,
        reply_preview: final_assessment.summary.clone(),
        reply_preview_key: None,
        reply_preview_params: None,
        duration_ms: chain_start.elapsed().as_millis() as u64,
    });

    // ─── Phase 5: Summary ───────────────────────────────────────
    status.phase = "completed".into();
    status.completed = status.steps.iter().all(|s| s.success);

    // Post summary to pool
    {
        let db = state.db.lock().await;
        let emoji = if status.completed { "✅" } else { "⚠️" };
        let step_lines: Vec<String> = status
            .steps
            .iter()
            .map(|s| {
                format!(
                    "- {} **{}** ({}): {} ({}ms)",
                    if s.success { "✅" } else { "❌" },
                    s.koi_name,
                    s.name,
                    if s.success { "completed" } else { "failed" },
                    s.duration_ms,
                )
            })
            .collect();
        let total_ms: u64 = status.steps.iter().map(|s| s.duration_ms).sum();
        let summary = format!(
            "{} **Collaboration Trial {}**\n\n{}\n\nPisci assessment: {}\n\nTotal time: {}ms",
            emoji,
            if status.completed {
                "PASSED"
            } else {
                "INCOMPLETE"
            },
            step_lines.join("\n"),
            final_assessment.summary,
            total_ms,
        );
        let _ = db.insert_pool_message(&pool.id, "pisci", &summary, "text", "{}");
    }

    emit(
        "done",
        if status.completed {
            "All agents completed successfully!"
        } else {
            "Trial incomplete"
        },
    );

    tracing::info!(
        "=== Collaboration Trial {} ({}/{} steps) ===",
        if status.completed {
            "PASSED"
        } else {
            "INCOMPLETE"
        },
        status.steps.iter().filter(|s| s.success).count(),
        status.steps.len(),
    );

    // Clean up trial artifacts: delete todos and pool session
    {
        let db = state.db.lock().await;
        let deleted = db.delete_todos_by_pool(&pool.id).unwrap_or(0);
        if deleted > 0 {
            tracing::info!("[Trial] Cleaned up {} trial todos", deleted);
        }
        let _ = db.delete_pool_session(&pool.id);
        tracing::info!("[Trial] Deleted trial pool {}", pool.id);
    }

    // Reset trial Koi statuses back to idle
    for koi_id in &status.koi_ids {
        let db = state.db.lock().await;
        let _ = db.update_koi_status(koi_id, "idle");
    }

    Ok(status)
}
