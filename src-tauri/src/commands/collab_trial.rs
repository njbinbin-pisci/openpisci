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
use std::collections::HashSet;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TrialKoiSpec {
    name: String,
    role: String,
    icon: String,
    color: String,
    system_prompt: String,
    description: String,
    max_iterations: u32,
    step_name: String,
    task_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TrialScenario {
    pool_name: String,
    project_title: String,
    goal: String,
    kickoff_phase: String,
    kickoff_detail: String,
    kickoff_message: String,
    workflow: Vec<String>,
    success_criteria: Vec<String>,
    lead: TrialKoiSpec,
    second: TrialKoiSpec,
    third: TrialKoiSpec,
    chain_timeout_secs: u64,
    poll_interval_secs: u64,
    quiet_polls_needed: u32,
}

fn default_trial_scenario() -> TrialScenario {
    TrialScenario {
        pool_name: "Collaboration Trial".into(),
        project_title: "Collaboration Trial".into(),
        goal: "Test multi-agent collaboration by designing and reviewing a simple utility module."
            .into(),
        kickoff_phase: "lead".into(),
        kickoff_detail: "Pisci starts the collaboration by assigning the first specialist."
            .into(),
        kickoff_message: "@Architect Design a small \"string utility\" module with 3 functions: \
             1) reverse_words(s) - reverses word order in a sentence \
             2) count_vowels(s) - counts vowels in a string \
             3) to_title_case(s) - converts a string to title case. \
             Write a clear, concise specification with function signatures, \
             parameter descriptions, expected behavior, and edge cases. \
             Keep it practical. When you finish, share the spec in pool_chat, include `[ProjectStatus] follow_up_needed`, and @Coder to hand off implementation."
            .into(),
        workflow: vec![
            "Pisci assigns the initial design task to Architect.".into(),
            "Architect produces a specification, then hands off to @Coder.".into(),
            "Coder implements based on the specification, then hands off to @Reviewer.".into(),
            "Reviewer requests follow-up work or signals `[ProjectStatus] ready_for_pisci_review` for Pisci to assess."
                .into(),
        ],
        success_criteria: vec![
            "Each task builds on the previous agent's output.".into(),
            "Communication flows through the pool chat.".into(),
            "If more work is needed, agents clearly signal `[ProjectStatus] follow_up_needed`."
                .into(),
            "When the project may be ready to conclude, an agent signals `[ProjectStatus] ready_for_pisci_review` and Pisci decides whether the trial can end."
                .into(),
        ],
        lead: TrialKoiSpec {
            name: "Architect".into(),
            role: "架构师".into(),
            icon: "🏗️".into(),
            color: "#7c6af7".into(),
            system_prompt:
                "You are a software architect collaborating inside a multi-agent project. Your job is to produce clear, practical technical specifications that help the next specialist move the work forward. \
                 Be concise, structured, and explicit about assumptions, interfaces, and edge cases. \
                 Publish your design in pool_chat, then hand off clearly if another specialist should continue. \
                 Do not decide that the project is finished yourself."
                    .into(),
            description: "Architecture, system design, technical specification".into(),
            max_iterations: 8,
            step_name: "design_spec".into(),
            task_label: "Design string utility module spec".into(),
        },
        second: TrialKoiSpec {
            name: "Coder".into(),
            role: "程序员".into(),
            icon: "💻".into(),
            color: "#45b7d1".into(),
            system_prompt:
                "You are a software developer collaborating inside a multi-agent project. Given a specification or concrete handoff, produce a practical implementation summary or implementation-ready output that helps the project advance. \
                 Focus on correctness, actionable detail, and clear handoff notes. \
                 If review or further work is needed, signal `[ProjectStatus] follow_up_needed` and @mention the next actor. \
                 If the work may be ready, signal `[ProjectStatus] ready_for_pisci_review` or hand off to the reviewer as appropriate."
                    .into(),
            description: "Implementation, coding, development".into(),
            max_iterations: 8,
            step_name: "implement".into(),
            task_label: "Implement string utility module".into(),
        },
        third: TrialKoiSpec {
            name: "Reviewer".into(),
            role: "代码审查员".into(),
            icon: "🔍".into(),
            color: "#26de81".into(),
            system_prompt:
                "You are a reviewer collaborating inside a multi-agent project. Given prior work, provide constructive feedback, identify risks, and state clearly whether follow-up is needed. \
                 Be specific and actionable. \
                 If more work is needed, signal `[ProjectStatus] follow_up_needed` and @mention the responsible specialist. \
                 If the work looks acceptable, signal `[ProjectStatus] ready_for_pisci_review` and @mention Pisci rather than declaring the project finished yourself."
                    .into(),
            description: "Review, quality assurance, feedback".into(),
            max_iterations: 8,
            step_name: "review".into(),
            task_label: "Review the implementation".into(),
        },
        chain_timeout_secs: 900,
        poll_interval_secs: 5,
        quiet_polls_needed: 2,
    }
}

fn load_trial_scenario() -> Result<TrialScenario, String> {
    match std::env::var("PISCI_COLLAB_TRIAL_SPEC_JSON") {
        Ok(raw) if !raw.trim().is_empty() => serde_json::from_str(&raw).map_err(|e| {
            format!(
                "Failed to parse PISCI_COLLAB_TRIAL_SPEC_JSON as TrialScenario JSON: {}",
                e
            )
        }),
        _ => Ok(default_trial_scenario()),
    }
}

fn keep_trial_artifacts() -> bool {
    std::env::var("PISCI_COLLAB_TRIAL_KEEP_ARTIFACTS")
        .ok()
        .as_deref()
        == Some("1")
}

fn normalize_trial_text(value: &str) -> String {
    value.trim().to_lowercase()
}

fn ensure_trial_koi(
    db: &crate::store::db::Database,
    all_kois: &mut Vec<crate::koi::KoiDefinition>,
    spec: &TrialKoiSpec,
) -> Result<crate::koi::KoiDefinition, String> {
    let role_key = normalize_trial_text(spec.role.as_str());
    if let Some(existing) = all_kois
        .iter()
        .find(|k| normalize_trial_text(&k.role) == role_key)
        .cloned()
    {
        db.update_koi(
            &existing.id,
            Some(spec.name.as_str()),
            Some(spec.role.as_str()),
            Some(spec.icon.as_str()),
            Some(spec.color.as_str()),
            Some(spec.system_prompt.as_str()),
            Some(spec.description.as_str()),
            None,
            Some(spec.max_iterations),
        )
        .map_err(|e| e.to_string())?;
        let mut updated = existing.clone();
        updated.name = spec.name.clone();
        updated.role = spec.role.clone();
        updated.icon = spec.icon.clone();
        updated.color = spec.color.clone();
        updated.system_prompt = spec.system_prompt.clone();
        updated.description = spec.description.clone();
        updated.max_iterations = spec.max_iterations;
        if let Some(idx) = all_kois.iter().position(|k| k.id == updated.id) {
            all_kois[idx] = updated.clone();
        }
        return Ok(updated);
    }

    if let Some(existing) = all_kois.iter().find(|k| k.name == spec.name).cloned() {
        db.update_koi(
            &existing.id,
            Some(spec.name.as_str()),
            Some(spec.role.as_str()),
            Some(spec.icon.as_str()),
            Some(spec.color.as_str()),
            Some(spec.system_prompt.as_str()),
            Some(spec.description.as_str()),
            None,
            Some(spec.max_iterations),
        )
        .map_err(|e| e.to_string())?;
        let mut updated = existing.clone();
        updated.name = spec.name.clone();
        updated.role = spec.role.clone();
        updated.icon = spec.icon.clone();
        updated.color = spec.color.clone();
        updated.system_prompt = spec.system_prompt.clone();
        updated.description = spec.description.clone();
        updated.max_iterations = spec.max_iterations;
        if let Some(idx) = all_kois.iter().position(|k| k.id == updated.id) {
            all_kois[idx] = updated.clone();
        }
        return Ok(updated);
    }

    let created = db
        .create_koi(
            spec.name.as_str(),
            spec.role.as_str(),
            spec.icon.as_str(),
            spec.color.as_str(),
            spec.system_prompt.as_str(),
            spec.description.as_str(),
            None,
            spec.max_iterations,
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

fn push_trial_observation(
    status: &mut TrialStatus,
    name: impl Into<String>,
    koi_name: impl Into<String>,
    task: impl Into<String>,
    success: bool,
    reply_preview: impl Into<String>,
    duration_ms: u64,
) {
    status.steps.push(TrialStep {
        name: name.into(),
        koi_name: koi_name.into(),
        task: task.into(),
        success,
        reply_preview: reply_preview.into(),
        reply_preview_key: None,
        reply_preview_params: None,
        duration_ms,
    });
}

fn trial_koi_name<'a>(
    sender_id: &str,
    lead: &'a crate::koi::KoiDefinition,
    second: &'a crate::koi::KoiDefinition,
    third: &'a crate::koi::KoiDefinition,
) -> &'a str {
    if sender_id == lead.id {
        lead.name.as_str()
    } else if sender_id == second.id {
        second.name.as_str()
    } else if sender_id == third.id {
        third.name.as_str()
    } else {
        "system"
    }
}

fn event_task_label(event_type: Option<&str>) -> &'static str {
    match event_type {
        Some("task_claimed") => "Claimed a pool todo",
        Some("task_completed") => "Completed a pool todo",
        Some("task_failed") => "A pool todo failed",
        Some("task_assigned") => "A pool todo was assigned",
        Some("protocol_warning") => "Protocol anomaly observed",
        Some("task_progress") => "Reported task progress",
        _ => "Pool event observed",
    }
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
/// Creates 3 Koi agents for a scenario-defined workflow, a project pool,
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
    let scenario = load_trial_scenario()?;
    tracing::info!(
        "=== Collaboration Trial: starting title={} ===",
        scenario.project_title
    );

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

    let (lead, second, third, pool) = {
        let db = state.db.lock().await;
        let mut all_kois = db.list_kois().map_err(|e| e.to_string())?;

        let lead = ensure_trial_koi(&db, &mut all_kois, &scenario.lead)?;
        let second = ensure_trial_koi(&db, &mut all_kois, &scenario.second)?;
        let third = ensure_trial_koi(&db, &mut all_kois, &scenario.third)?;

        let pool = db
            .create_pool_session(&scenario.pool_name)
            .map_err(|e| e.to_string())?;

        let workflow = scenario
            .workflow
            .iter()
            .enumerate()
            .map(|(idx, step)| format!("{}. {}", idx + 1, step))
            .collect::<Vec<_>>()
            .join("\n");
        let success_criteria = scenario
            .success_criteria
            .iter()
            .map(|item| format!("- {}", item))
            .collect::<Vec<_>>()
            .join("\n");
        let org_spec = format!(
            "## Project: {}\n\n\
             ### Goal\n\
             {}\n\n\
             ### Team\n\
             - **{}** ({}): {}\n\
             - **{}** ({}): {}\n\
             - **{}** ({}): {}\n\n\
             ### Workflow\n\
             {}\n\n\
             ### Success Criteria\n\
             {}",
            scenario.project_title,
            scenario.goal,
            lead.name,
            lead.role,
            scenario.lead.description,
            second.name,
            second.role,
            scenario.second.description,
            third.name,
            third.role,
            scenario.third.description,
            workflow,
            success_criteria,
        );
        db.update_pool_org_spec(&pool.id, &org_spec)
            .map_err(|e| e.to_string())?;

        // Post the project kickoff to the pool
        db.insert_pool_message(
            &pool.id,
            "pisci",
            &format!(
                "🚀 **{} started**\n\n\
                 Team: {} {}, {} {}, {} {}\n\
                 Goal: {}\n\n\
                 Workflow: {} → {} → {}",
                scenario.project_title,
                lead.icon,
                lead.name,
                second.icon,
                second.name,
                third.icon,
                third.name,
                scenario.goal,
                lead.name,
                second.name,
                third.name,
            ),
            "text",
            "{}",
        )
        .map_err(|e| e.to_string())?;

        (lead, second, third, pool)
    };

    status.pool_id = pool.id.clone();
    status.koi_ids = vec![lead.id.clone(), second.id.clone(), third.id.clone()];
    *pool_id_cell.lock().unwrap() = pool.id.clone();
    emit("pool_ready", "Pool session created, agents ready");

    tracing::info!(
        "Trial setup: pool={}, lead={}, second={}, third={}",
        pool.id,
        lead.id,
        second.id,
        third.id
    );

    // ─── Phase 2: Pisci posts the initial @mention in pool chat (natural communication) ──
    // The entire workflow is driven by @mention cascading:
    //   Pisci @lead → lead hands off to second → second hands off to third
    // No direct assign_koi calls — everything flows through pool_chat @mentions.
    status.phase = scenario.kickoff_phase.clone();
    emit(&scenario.kickoff_phase, &scenario.kickoff_detail);

    let task_message = scenario.kickoff_message.clone();

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

    // The first @mention dispatch activates the lead specialist and blocks until done.
    let chain_start = std::time::Instant::now();
    let lead_results = runtime
        .handle_mention("pisci", &pool.id, &task_message)
        .await;

    let kickoff_preview = match &lead_results {
        Ok(results) if !results.is_empty() => {
            let first = &results[0];
            format!(
                "Initial @mention dispatched to {}. Runtime returned {} result(s). First result success={}, preview={}",
                lead.name,
                results.len(),
                first.success,
                first.reply.chars().take(160).collect::<String>()
            )
        }
        Ok(_) => {
            "Initial @mention returned no execution result. The trial could not confirm that the first specialist was activated."
                .to_string()
        }
        Err(e) => format!("Initial @mention dispatch failed: {}", e),
    };
    push_trial_observation(
        &mut status,
        "kickoff_dispatch",
        "Pisci",
        format!("Kick off collaboration with @{}", lead.name),
        matches!(&lead_results, Ok(results) if !results.is_empty()),
        kickoff_preview.clone(),
        chain_start.elapsed().as_millis() as u64,
    );

    if !matches!(&lead_results, Ok(results) if !results.is_empty()) {
        set_trial_error(
            &mut status,
            "debug.multiAgentTrialTaskFailed",
            json!({ "subject": lead.name }),
            format!("Initial dispatch to {} failed", lead.name),
        );
        emit("error", &kickoff_preview);
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

    let chain_timeout = std::time::Duration::from_secs(scenario.chain_timeout_secs);
    let poll_interval = std::time::Duration::from_secs(scenario.poll_interval_secs);
    let quiet_polls_needed = scenario.quiet_polls_needed;
    let mut quiet_polls = 0u32;
    let mut last_phase_detail = String::new();
    let mut last_message_count = 0usize;
    let mut seen_observation_event_ids: HashSet<i64> = HashSet::new();
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
        let lead_koi = db.get_koi(&lead.id).ok().flatten();
        let second_koi = db.get_koi(&second.id).ok().flatten();
        let third_koi = db.get_koi(&third.id).ok().flatten();
        drop(db);

        let lead_status = lead_koi
            .as_ref()
            .map(|k| k.status.as_str())
            .unwrap_or("unknown");
        let second_status = second_koi
            .as_ref()
            .map(|k| k.status.as_str())
            .unwrap_or("unknown");
        let third_status = third_koi
            .as_ref()
            .map(|k| k.status.as_str())
            .unwrap_or("unknown");

        final_assessment = assess_trial_project_state(&msgs, &pool_todos, &status.koi_ids);
        let phase_detail = format!(
            "{}: {} | {}: {} | {}: {} | active_todos: {} | blocked: {} | follow_up: {} | ready: {} | handoff_to_pisci: {}",
            lead.name,
            lead_status,
            second.name,
            second_status,
            third.name,
            third_status,
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
            matches!(
                m.event_type.as_deref(),
                Some(
                    "task_assigned"
                        | "task_claimed"
                        | "task_completed"
                        | "task_failed"
                        | "task_progress"
                        | "protocol_warning"
                )
            )
        }) {
            let sender_is_koi = status.koi_ids.iter().any(|id| id == &msg.sender_id);
            let is_protocol_warning = msg.event_type.as_deref() == Some("protocol_warning");
            if (!sender_is_koi && !is_protocol_warning) || !seen_observation_event_ids.insert(msg.id)
            {
                continue;
            }

            let koi_name = if is_protocol_warning {
                "system"
            } else {
                trial_koi_name(&msg.sender_id, &lead, &second, &third)
            };
            let event_name = msg
                .event_type
                .clone()
                .unwrap_or_else(|| "pool_event".to_string());
            let success = !matches!(
                msg.event_type.as_deref(),
                Some("task_failed" | "protocol_warning")
            );
            push_trial_observation(
                &mut status,
                event_name,
                koi_name,
                event_task_label(msg.event_type.as_deref()),
                success,
                msg.content.chars().take(200).collect::<String>(),
                chain_start.elapsed().as_millis() as u64,
            );
        }

        let all_idle = lead_status == "idle" && second_status == "idle" && third_status == "idle";
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

    push_trial_observation(
        &mut status,
        "pisci_assess",
        "Pisci",
        "Assess whether the project is ready to conclude",
        final_assessment.decision == TrialDecision::ReadyForPisciReview,
        final_assessment.summary.clone(),
        chain_start.elapsed().as_millis() as u64,
    );

    // ─── Phase 5: Summary ───────────────────────────────────────
    status.phase = "completed".into();
    status.completed =
        final_assessment.decision == TrialDecision::ReadyForPisciReview && status.error.is_none();

    // Post summary to pool
    {
        let db = state.db.lock().await;
        let emoji = if status.completed { "✅" } else { "⚠️" };
        let observation_lines: Vec<String> = status
            .steps
            .iter()
            .map(|s| {
                format!(
                    "- {} **{}** [{}]: {} ({}ms)",
                    if s.success { "✅" } else { "❌" },
                    s.koi_name,
                    s.name,
                    s.task,
                    s.duration_ms,
                )
            })
            .collect();
        let total_ms: u64 = status.steps.iter().map(|s| s.duration_ms).sum();
        let summary = format!(
            "{} **Collaboration Trial {}**\n\nObserved events:\n{}\n\nPisci assessment: {}\n\nTotal time: {}ms",
            emoji,
            if status.completed {
                "PASSED"
            } else {
                "INCOMPLETE"
            },
            observation_lines.join("\n"),
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
        "=== Collaboration Trial {} ({}/{} observations marked ok) ===",
        if status.completed {
            "PASSED"
        } else {
            "INCOMPLETE"
        },
        status.steps.iter().filter(|s| s.success).count(),
        status.steps.len(),
    );

    // Clean up trial artifacts unless the developer asked to keep them for inspection.
    if keep_trial_artifacts() {
        {
            let db = state.db.lock().await;
            let _ = db.update_pool_session_status(&pool.id, "paused");
        }
        tracing::info!(
            "[Trial] Keeping artifacts for inspection: pool={} paused; todos remain available",
            pool.id
        );
    } else {
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
