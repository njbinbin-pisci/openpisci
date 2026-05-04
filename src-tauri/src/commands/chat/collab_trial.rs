/// Collaboration Trial — spawn real Koi agents with LLM to test multi-agent cooperation.
///
/// - Creates real Koi agents in the production DB
/// - Creates a real Pool session visible in the UI
/// - Fans Koi turns through `pool::bridge::handle_mention` (kernel coordinator)
/// - All events stream to the Chat Pool and Board in real-time
///
/// The user can observe the full collaboration in the Pond UI.
use crate::pool::bridge;
use crate::store::AppState;
use pisci_core::project_state::{
    assess_project_state, ProjectAssessment as TrialAssessment, ProjectDecision as TrialDecision,
};
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
    /// 0 means inherit the user-configurable system default from settings.
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
        kickoff_message: "@!Architect Design a small \"string utility\" module with 3 functions: \
             1) reverse_words(s) - reverses word order in a sentence \
             2) count_vowels(s) - counts vowels in a string \
             3) to_title_case(s) - converts a string to title case. \
             Keep the spec practical: function signatures, parameter descriptions, expected behavior, and edge cases. \
             Follow your coordination protocol to hand off to Coder when the spec is ready."
            .into(),
        workflow: vec![
            "Pisci assigns the initial design task to Architect.".into(),
            "Architect produces a specification, then hands off implementation to Coder.".into(),
            "Coder implements based on the specification, then hands off to @!Reviewer.".into(),
            "Reviewer requests follow-up work or signals `[ProjectStatus] ready_for_pisci_review` for Pisci to assess."
                .into(),
        ],
        success_criteria: vec![
            "Each task builds on the previous agent's output.".into(),
            "Communication flows through the pool chat.".into(),
            "If more work is needed, agents clearly signal `[ProjectStatus] follow_up_needed`."
                .into(),
            "When the project may be ready for Pisci review, an agent signals `[ProjectStatus] ready_for_pisci_review` and the trial records that snapshot."
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
                 Publish your design in pool_chat, then hand off clearly with `[ProjectStatus] follow_up_needed` and @!Coder if implementation should continue. \
                 Do not decide that the project is finished yourself."
                    .into(),
            description: "Architecture, system design, technical specification".into(),
            max_iterations: 0,
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
                 When implementation is ready for review, hand off to Reviewer with `[ProjectStatus] follow_up_needed` and an explicit @!mention. \
                 If more work is needed first, signal `[ProjectStatus] follow_up_needed` and @!mention the next actor. \
                 Only use `[ProjectStatus] ready_for_pisci_review` after reviewer-level verification is truly complete."
                    .into(),
            description: "Implementation, coding, development".into(),
            max_iterations: 0,
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
                 If more work is needed, signal `[ProjectStatus] follow_up_needed` and @!mention the responsible specialist. \
                 If the work looks acceptable, you MUST post a pool_chat message containing the exact text `[ProjectStatus] ready_for_pisci_review @pisci` before completing your review todo. Do not merely say \"ready for review\" in prose, and do not declare the project finished yourself."
                    .into(),
            description: "Review, quality assurance, feedback".into(),
            max_iterations: 0,
            step_name: "review".into(),
            task_label: "Review the implementation".into(),
        },
        chain_timeout_secs: 900,
        poll_interval_secs: 5,
        quiet_polls_needed: 2,
    }
}

pub use pisci_core::trial::effective_trial_koi_status;

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
    all_kois: &mut Vec<crate::pool::KoiDefinition>,
    spec: &TrialKoiSpec,
) -> Result<crate::pool::KoiDefinition, String> {
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
            Some(0),
        )
        .map_err(|e| e.to_string())?;
        let _ = db.update_koi_status(&existing.id, "idle");
        let mut updated = existing.clone();
        updated.name = spec.name.clone();
        updated.role = spec.role.clone();
        updated.icon = spec.icon.clone();
        updated.color = spec.color.clone();
        updated.system_prompt = spec.system_prompt.clone();
        updated.description = spec.description.clone();
        updated.max_iterations = spec.max_iterations;
        updated.status = "idle".to_string();
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
            Some(0),
        )
        .map_err(|e| e.to_string())?;
        let _ = db.update_koi_status(&existing.id, "idle");
        let mut updated = existing.clone();
        updated.name = spec.name.clone();
        updated.role = spec.role.clone();
        updated.icon = spec.icon.clone();
        updated.color = spec.color.clone();
        updated.system_prompt = spec.system_prompt.clone();
        updated.description = spec.description.clone();
        updated.max_iterations = spec.max_iterations;
        updated.status = "idle".to_string();
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
            0,
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
    lead: &'a crate::pool::KoiDefinition,
    second: &'a crate::pool::KoiDefinition,
    third: &'a crate::pool::KoiDefinition,
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

fn trial_koi_session_id(koi_id: &str, pool_id: &str) -> String {
    format!("koi_{}_{}", koi_id, pool_id)
}

fn trial_koi_runtime_active(run_slot_active: bool, checkpoint_running: bool) -> bool {
    run_slot_active || checkpoint_running
}

pub(crate) fn assess_trial_project_state(
    messages: &[crate::pool::PoolMessage],
    todos: &[crate::pool::KoiTodo],
    koi_ids: &[String],
) -> TrialAssessment {
    assess_project_state(messages, todos, koi_ids)
}

/// Launch a multi-agent collaboration trial.
///
/// Creates 3 Koi agents for a scenario-defined workflow, a project pool,
/// and orchestrates a realistic task flow with delegated @!mention handoffs.
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
            .create_pool_session(&scenario.pool_name, 0)
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

    // ─── Phase 2: Pisci posts the initial @!mention in pool chat (natural communication) ──
    // The entire workflow is driven by delegated @!mention cascading:
    //   Pisci @!lead → lead hands off to @!second → second hands off to @!third
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

    // Wake the lead specialist via @!mention — the agent reads the pool and decides autonomously.
    let chain_start = std::time::Instant::now();
    let lead_results =
        bridge::handle_mention(&app_handle, state, "pisci", &pool.id, &task_message).await;

    let kickoff_preview = match &lead_results {
        Ok(()) => format!(
            "Initial @!mention dispatched to {}. Koi turn is running — \
             results will stream via pool events.",
            lead.name
        ),
        Err(e) => format!("Initial @mention dispatch failed: {}", e),
    };
    push_trial_observation(
        &mut status,
        "kickoff_dispatch",
        "Pisci",
        format!("Kick off collaboration with @!{}", lead.name),
        lead_results.is_ok(),
        kickoff_preview.clone(),
        chain_start.elapsed().as_millis() as u64,
    );

    if lead_results.is_err() {
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
    let mut last_phase_detail = String::new();
    let mut seen_observation_event_ids: HashSet<i64> = HashSet::new();
    let mut last_message_id = 0i64;
    let mut quiet_polls = 0u32;
    let mut final_assessment = TrialAssessment {
        decision: TrialDecision::Continue,
        active_todo_count: 0,
        blocked_todo_count: 0,
        needs_review_count: 0,
        task_failed_count: 0,
        follow_up_signal_count: 0,
        ready_signal_count: 0,
        explicit_pisci_handoff_count: 0,
        attention_reasons: vec![],
        summary: "No assessment yet.".into(),
    };

    let (stop_reason, stop_detail) = loop {
        tokio::time::sleep(poll_interval).await;

        if chain_start.elapsed() > chain_timeout {
            tracing::warn!("[Trial] Chain timed out after {}s", chain_timeout.as_secs());
            emit(
                "timeout",
                "Trial reached the configured timeout boundary. Review the pool snapshot for current project state.",
            );
            break (
                "timeout".to_string(),
                "Collaboration reached the configured timeout boundary before a final snapshot."
                    .to_string(),
            );
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
        let lead_checkpoint_running = db
            .load_checkpoint(&trial_koi_session_id(&lead.id, &pool.id))
            .ok()
            .flatten()
            .is_some();
        let second_checkpoint_running = db
            .load_checkpoint(&trial_koi_session_id(&second.id, &pool.id))
            .ok()
            .flatten()
            .is_some();
        let third_checkpoint_running = db
            .load_checkpoint(&trial_koi_session_id(&third.id, &pool.id))
            .ok()
            .flatten()
            .is_some();
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
        let lead_run_active =
            crate::tools::call_koi::runtime::is_koi_run_slot_active(&lead.id, Some(&pool.id)).await;
        let second_run_active =
            crate::tools::call_koi::runtime::is_koi_run_slot_active(&second.id, Some(&pool.id))
                .await;
        let third_run_active =
            crate::tools::call_koi::runtime::is_koi_run_slot_active(&third.id, Some(&pool.id))
                .await;
        let lead_effective_status = effective_trial_koi_status(
            lead_status,
            trial_koi_runtime_active(lead_run_active, lead_checkpoint_running),
        );
        let second_effective_status = effective_trial_koi_status(
            second_status,
            trial_koi_runtime_active(second_run_active, second_checkpoint_running),
        );
        let third_effective_status = effective_trial_koi_status(
            third_status,
            trial_koi_runtime_active(third_run_active, third_checkpoint_running),
        );

        final_assessment = assess_trial_project_state(&msgs, &pool_todos, &status.koi_ids);
        let coordination_signal_count = msgs
            .iter()
            .filter(|msg| msg.event_type.as_deref() == Some("coordination_signal"))
            .count();
        let phase_detail = format!(
            "{}: {} | {}: {} | {}: {} | checkpoints: [{}, {}, {}] | pool_messages: {} | coordination_signals: {} | active_todos: {} | blocked: {} | follow_up: {} | ready: {} | handoff_to_pisci: {}",
            lead.name,
            lead_effective_status,
            second.name,
            second_effective_status,
            third.name,
            third_effective_status,
            lead_checkpoint_running,
            second_checkpoint_running,
            third_checkpoint_running,
            msgs.len(),
            coordination_signal_count,
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
        let latest_message_id = msgs.last().map(|msg| msg.id).unwrap_or_default();
        if latest_message_id > last_message_id {
            last_message_id = latest_message_id;
            quiet_polls = 0;
        } else {
            quiet_polls = quiet_polls.saturating_add(1);
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
            if (!sender_is_koi && !is_protocol_warning)
                || !seen_observation_event_ids.insert(msg.id)
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

        let trial_quiet = quiet_polls >= scenario.quiet_polls_needed;
        if trial_quiet && final_assessment.decision == TrialDecision::ReadyForPisciReview {
            status.phase = "pisci_review".into();
            emit("pisci_review", &final_assessment.summary);
            break (
                "ready_for_pisci_review".to_string(),
                final_assessment.summary.clone(),
            );
        }
        if trial_quiet && final_assessment.decision == TrialDecision::EscalateToHuman {
            push_trial_observation(
                &mut status,
                "escalate_to_human",
                "system",
                "Record human-escalation snapshot",
                false,
                if final_assessment.attention_reasons.is_empty() {
                    final_assessment.summary.clone()
                } else {
                    format!(
                        "{} | attention: {}",
                        final_assessment.summary,
                        final_assessment.attention_reasons.join("; ")
                    )
                },
                chain_start.elapsed().as_millis() as u64,
            );
            break (
                "escalate_to_human".to_string(),
                format!(
                    "The trial reached a state that should be stopped and handed to the user for a human decision. {}",
                    final_assessment.summary
                ),
            );
        }
        let unfinished_work_remaining =
            final_assessment.active_todo_count > 0 || final_assessment.blocked_todo_count > 0;
        let all_idle = lead_effective_status == "idle"
            && second_effective_status == "idle"
            && third_effective_status == "idle";
        if all_idle
            && trial_quiet
            && !unfinished_work_remaining
            && final_assessment.decision == TrialDecision::SupervisorDecisionRequired
        {
            push_trial_observation(
                &mut status,
                "supervisor_decision_required",
                "system",
                "Record supervisor-decision snapshot",
                false,
                if final_assessment.attention_reasons.is_empty() {
                    final_assessment.summary.clone()
                } else {
                    format!(
                        "{} | attention: {}",
                        final_assessment.summary,
                        final_assessment.attention_reasons.join("; ")
                    )
                },
                chain_start.elapsed().as_millis() as u64,
            );
            break (
                "supervisor_decision_required".to_string(),
                format!(
                    "Worker-visible work reached a locally terminal snapshot, but the next global decision belongs to Pisci. {}",
                    final_assessment.summary
                ),
            );
        }
        if all_idle && trial_quiet && !unfinished_work_remaining {
            push_trial_observation(
                &mut status,
                "idle_snapshot",
                "system",
                "Record idle trial snapshot",
                false,
                if final_assessment.attention_reasons.is_empty() {
                    final_assessment.summary.clone()
                } else {
                    format!(
                        "{} | attention: {}",
                        final_assessment.summary,
                        final_assessment.attention_reasons.join("; ")
                    )
                },
                chain_start.elapsed().as_millis() as u64,
            );
            break (
                "idle_quiet_snapshot".to_string(),
                format!(
                    "All trial agents became idle without reaching a final Pisci-review handoff. {}",
                    final_assessment.summary
                ),
            );
        }
    };

    push_trial_observation(
        &mut status,
        "pisci_assess",
        "Pisci",
        "Observe whether the project reached a ready-for-review snapshot",
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
        let emoji = if status.completed { "✅" } else { "⏸️" };
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
            "{} **Collaboration Trial Snapshot**\n\nStop reason: `{}`\n{}\n\nObserved events:\n{}\n\nCurrent assessment: {}\nAttention reasons: {}\n\nRecoverable todos: active={}, blocked={}, needs_review={}\nContext-quality metrics: observations={}, coordination_events={}, ready_signals={}, follow_up_signals={}, task_failed_events={}\n\nTotal time: {}ms",
            emoji,
            stop_reason,
            stop_detail,
            observation_lines.join("\n"),
            final_assessment.summary,
            if final_assessment.attention_reasons.is_empty() {
                "(none)".to_string()
            } else {
                final_assessment.attention_reasons.join("; ")
            },
            final_assessment.active_todo_count,
            final_assessment.blocked_todo_count,
            final_assessment.needs_review_count,
            status.steps.len(),
            status.steps
                .iter()
                .filter(|s| s.name == "coordination_signal")
                .count(),
            final_assessment.ready_signal_count,
            final_assessment.follow_up_signal_count,
            final_assessment.task_failed_count,
            total_ms,
        );
        let _ = db.insert_pool_message(&pool.id, "pisci", &summary, "text", "{}");
    }

    emit(
        "done",
        if status.completed {
            "Trial observed a ready-for-review snapshot."
        } else {
            "Trial stopped at a recoverable snapshot. Review the pool for current state."
        },
    );

    tracing::info!(
        "=== Collaboration Trial snapshot stop_reason={} ({}/{} observations marked ok) ===",
        stop_reason,
        status.steps.iter().filter(|s| s.success).count(),
        status.steps.len(),
    );

    // Clean up trial artifacts unless the developer asked to keep them for inspection.
    let keep_artifacts = keep_trial_artifacts();
    if keep_artifacts {
        tracing::info!(
            "[Trial] Keeping artifacts for inspection: pool={} remains active; todos remain available",
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

    // Only force-reset trial Koi statuses when the trial artifacts are torn down.
    // If artifacts are kept, background Koi runs may still be progressing naturally.
    if !keep_artifacts {
        for koi_id in &status.koi_ids {
            let db = state.db.lock().await;
            let _ = db.update_koi_status(koi_id, "idle");
        }
    }

    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::{default_trial_scenario, effective_trial_koi_status, trial_koi_runtime_active};

    #[test]
    fn active_run_slot_overrides_idle_db_status() {
        assert_eq!(effective_trial_koi_status("idle", true), "busy");
    }

    #[test]
    fn db_status_is_preserved_without_active_run_slot() {
        assert_eq!(effective_trial_koi_status("idle", false), "idle");
        assert_eq!(effective_trial_koi_status("busy", false), "busy");
    }

    #[test]
    fn checkpoint_running_counts_as_trial_runtime_activity() {
        assert!(trial_koi_runtime_active(false, true));
        assert!(trial_koi_runtime_active(true, false));
        assert!(trial_koi_runtime_active(true, true));
        assert!(!trial_koi_runtime_active(false, false));
    }

    #[test]
    fn default_trial_scenario_uses_delegated_mentions_for_live_handoffs() {
        let scenario = default_trial_scenario();

        assert!(
            scenario.kickoff_message.contains("@!Architect"),
            "kickoff must use delegated @!mention so the coordinator dispatches the first Koi turn"
        );
        assert_eq!(
            scenario.kickoff_message.matches("@!").count(),
            1,
            "kickoff must delegate only to the lead Koi; mentioning downstream agents with @! here causes premature dispatch"
        );
        assert!(
            scenario.lead.system_prompt.contains("@!Coder"),
            "Architect's system prompt must preserve the downstream delegated handoff target without putting it in the kickoff message"
        );
        assert!(
            scenario.second.system_prompt.contains("@!mention")
                || scenario.second.system_prompt.contains("@!Reviewer"),
            "Coder prompt must preserve delegated handoff guidance for Reviewer"
        );
        assert!(
            scenario.third.system_prompt.contains("@pisci"),
            "Reviewer should return readiness to Pisci without waking a peer Koi"
        );
        assert!(
            scenario
                .third
                .system_prompt
                .contains("[ProjectStatus] ready_for_pisci_review @pisci"),
            "Reviewer prompt must require the exact terminal handoff signal used by project-state assessment"
        );
    }
}
