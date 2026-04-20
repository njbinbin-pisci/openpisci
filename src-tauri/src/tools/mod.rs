pub mod app_control;
pub mod browser;
pub mod call_fish;
pub mod call_koi;
pub mod chat_ui;
pub mod code_run;
pub mod dpi;
pub mod email;
pub mod file_diff;
pub mod file_list;
pub mod file_read;
pub mod file_search;
pub mod file_write;
pub mod mcp;
pub mod memory_tool;
pub mod office;
pub mod pdf;
pub mod plan_todo;
pub mod pool_chat;
pub mod pool_org;
pub mod powershell;
pub mod process_control;
pub mod recall_tool;
pub mod shell;
pub mod skill_list;
pub mod ssh;
pub mod user_tool;
pub mod vision_context;
pub mod web_search;
pub mod wmi_tool;

#[cfg(target_os = "windows")]
pub mod elevate;

#[cfg(target_os = "windows")]
pub mod com_invoke;
#[cfg(target_os = "windows")]
pub mod com_tool;
#[cfg(target_os = "windows")]
pub mod screen;
#[cfg(target_os = "windows")]
pub mod uia;

use crate::agent::tool::ToolRegistry;
use crate::browser::SharedBrowserManager;
use crate::skills::loader::SkillLoader;
use crate::store::{Database, Settings};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::AppHandle;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeToolProfile {
    Desktop,
    HeadlessPisci,
    HeadlessPool,
}

#[derive(Debug, Clone)]
pub struct ToolAvailability {
    pub name: &'static str,
    pub reason: Option<&'static str>,
}

const WINDOWS_ORIENTED_TOOLS: &[(&str, &str)] = &[
    ("powershell_query", "Disabled outside Windows: relies on Windows PowerShell semantics."),
    ("wmi", "Disabled outside Windows: WMI is Windows-only."),
    ("office", "Disabled outside Windows: current implementation depends on Windows Office automation."),
    ("uia", "Disabled outside Windows: UI Automation is Windows-only."),
    ("screen_capture", "Disabled outside Windows: current implementation uses Windows APIs."),
    ("com", "Disabled outside Windows: COM/OLE is Windows-only."),
    ("com_invoke", "Disabled outside Windows: COM/OLE is Windows-only."),
];

const HEADLESS_PISCI_DISABLED_TOOLS: &[(&str, &str)] = &[
    (
        "call_koi",
        "Disabled in headless pisci mode: single-agent baseline should not delegate to Koi.",
    ),
    (
        "pool_org",
        "Disabled in headless pisci mode: project-pool orchestration belongs to pool mode.",
    ),
    (
        "pool_chat",
        "Disabled in headless pisci mode: project-pool coordination belongs to pool mode.",
    ),
    (
        "chat_ui",
        "Disabled in headless modes: no interactive desktop chat UI is available.",
    ),
];

const HEADLESS_COMMON_DISABLED_TOOLS: &[(&str, &str)] = &[(
    "chat_ui",
    "Disabled in headless modes: no interactive desktop chat UI is available.",
)];

fn disable_tools(
    effective: &mut HashMap<String, bool>,
    disabled: &[(&'static str, &'static str)],
    output: &mut Vec<ToolAvailability>,
) {
    for (name, reason) in disabled {
        effective.insert((*name).to_string(), false);
        output.push(ToolAvailability {
            name,
            reason: Some(reason),
        });
    }
}

pub fn apply_runtime_tool_profile(
    base: &HashMap<String, bool>,
    profile: RuntimeToolProfile,
) -> HashMap<String, bool> {
    let mut effective = base.clone();
    let mut ignored = Vec::new();
    if !cfg!(target_os = "windows") {
        disable_tools(&mut effective, WINDOWS_ORIENTED_TOOLS, &mut ignored);
    }
    match profile {
        RuntimeToolProfile::Desktop => {}
        RuntimeToolProfile::HeadlessPisci => {
            disable_tools(&mut effective, HEADLESS_COMMON_DISABLED_TOOLS, &mut ignored);
            disable_tools(&mut effective, HEADLESS_PISCI_DISABLED_TOOLS, &mut ignored);
        }
        RuntimeToolProfile::HeadlessPool => {
            disable_tools(&mut effective, HEADLESS_COMMON_DISABLED_TOOLS, &mut ignored);
        }
    }
    effective
}

pub fn runtime_disabled_tools(profile: RuntimeToolProfile) -> Vec<ToolAvailability> {
    let mut out = Vec::new();
    let mut effective = HashMap::new();
    let mut seen = std::collections::HashSet::new();
    let mut push_unique = |disabled: &[(&'static str, &'static str)]| {
        let unique: Vec<_> = disabled
            .iter()
            .copied()
            .filter(|(name, _)| seen.insert(*name))
            .collect();
        disable_tools(&mut effective, &unique, &mut out);
    };
    if !cfg!(target_os = "windows") {
        push_unique(WINDOWS_ORIENTED_TOOLS);
    }
    match profile {
        RuntimeToolProfile::Desktop => {}
        RuntimeToolProfile::HeadlessPisci => {
            push_unique(HEADLESS_COMMON_DISABLED_TOOLS);
            push_unique(HEADLESS_PISCI_DISABLED_TOOLS);
        }
        RuntimeToolProfile::HeadlessPool => {
            push_unique(HEADLESS_COMMON_DISABLED_TOOLS);
        }
    }
    out
}

/// Build the default tool registry with all enabled tools.
/// Pass the shared browser manager so BrowserTool can reuse the same Chrome instance.
/// Also loads any user-installed tools from `user_tools_dir`.
#[allow(clippy::too_many_arguments)]
pub fn build_registry(
    browser: SharedBrowserManager,
    user_tools_dir: Option<&std::path::Path>,
    db: Option<Arc<Mutex<Database>>>,
    builtin_tool_enabled: Option<&HashMap<String, bool>>,
    app_handle: Option<AppHandle>,
    settings: Option<Arc<Mutex<Settings>>>,
    app_data_dir: Option<PathBuf>,
    skill_loader: Option<Arc<Mutex<SkillLoader>>>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    let is_enabled = |name: &str| -> bool {
        builtin_tool_enabled
            .and_then(|m| m.get(name).copied())
            .unwrap_or(true)
    };

    // Cross-platform tools
    if is_enabled("file_read") {
        registry.register(Box::new(file_read::FileReadTool));
    }
    if is_enabled("file_write") {
        registry.register(Box::new(file_write::FileWriteTool));
    }
    if is_enabled("file_edit") {
        registry.register(Box::new(file_write::FileEditTool));
    }
    if is_enabled("file_diff") {
        registry.register(Box::new(file_diff::FileDiffTool));
    }
    if is_enabled("code_run") {
        registry.register(Box::new(code_run::CodeRunTool));
    }
    if is_enabled("file_search") {
        registry.register(Box::new(file_search::FileSearchTool));
    }
    if is_enabled("file_list") {
        registry.register(Box::new(file_list::FileListTool));
    }
    if is_enabled("process_control") {
        registry.register(Box::new(process_control::ProcessControlTool));
    }
    if is_enabled("shell") {
        registry.register(Box::new(shell::ShellTool));
    }
    if is_enabled("web_search") {
        registry.register(Box::new(web_search::WebSearchTool));
    }
    if is_enabled("email") {
        registry.register(Box::new(email::EmailTool));
    }
    if is_enabled("browser") {
        registry.register(Box::new(browser::BrowserTool::new(browser)));
    }

    // Memory tool — requires DB access
    if is_enabled("memory_store") {
        if let Some(ref db_arc) = db {
            registry.register(Box::new(memory_tool::MemoryStoreTool {
                db: db_arc.clone(),
            }));
        }
    }

    // recall_tool_result — re-fetch the original full output of a demoted
    // tool result (p11). Requires DB access; enabled by default.
    if is_enabled("recall_tool_result") {
        if let Some(ref db_arc) = db {
            registry.register(Box::new(recall_tool::RecallToolResultTool {
                db: db_arc.clone(),
            }));
        }
    }

    if is_enabled("plan_todo") {
        if let Some(ref app) = app_handle {
            registry.register(Box::new(plan_todo::PlanTodoTool { app: app.clone() }));
        }
    }

    if is_enabled("vision_context") {
        registry.register(Box::new(vision_context::VisionContextTool));
    }

    // call_fish tool — lets the main agent delegate sub-tasks to Fish agents
    if is_enabled("call_fish") {
        if let Some(ref app) = app_handle {
            registry.register(Box::new(call_fish::CallFishTool { app: app.clone() }));
        }
    }

    // call_koi tool — lets Pisci or a Koi delegate tasks to persistent Koi agents
    if is_enabled("call_koi") {
        if let Some(ref app) = app_handle {
            registry.register(Box::new(call_koi::CallKoiTool {
                app: app.clone(),
                caller_koi_id: None,
                depth: 0,
                managed_externally: false,
                notification_rx: std::sync::Mutex::new(None),
                await_completion: false,
            }));
        }
    }

    // chat_ui tool — lets Pisci show interactive UI cards in the chat
    if is_enabled("chat_ui") {
        if let Some(ref app) = app_handle {
            registry.register(Box::new(chat_ui::ChatUiTool { app: app.clone() }));
        }
    }

    // pool_org tool — lets Pisci create/manage project pools and org specs
    if is_enabled("pool_org") {
        if let (Some(ref app), Some(ref db_arc)) = (&app_handle, &db) {
            registry.register(Box::new(pool_org::PoolOrgTool {
                app: app.clone(),
                db: db_arc.clone(),
            }));
        }
    }

    // pool_chat tool — lets Pisci participate in pool conversations directly
    if is_enabled("pool_chat") {
        if let (Some(ref app), Some(ref db_arc)) = (&app_handle, &db) {
            registry.register(Box::new(pool_chat::PoolChatTool {
                app: app.clone(),
                db: db_arc.clone(),
                sender_id: "pisci".to_string(),
            }));
        }
    }

    // app_control tool — manage scheduled tasks, settings, and skills via Agent
    if is_enabled("app_control") {
        if let (Some(ref db_arc), Some(ref settings_arc), Some(ref data_dir)) =
            (&db, &settings, &app_data_dir)
        {
            registry.register(Box::new(app_control::AppControlTool {
                db: db_arc.clone(),
                settings: settings_arc.clone(),
                app_data_dir: data_dir.clone(),
                app_handle: app_handle.clone(),
            }));
        }
    }

    // skill_list tool — browse installed skills
    if is_enabled("skill_list") {
        if let Some(loader) = skill_loader {
            registry.register(Box::new(skill_list::SkillListTool { loader }));
        }
    }

    // SSH tool — connect to remote servers and execute commands
    if is_enabled("ssh") {
        let ssh_settings = settings.clone();
        registry.register(Box::new(ssh::SshTool::new(ssh_settings)));
    }

    // PDF read/write
    if is_enabled("pdf") {
        registry.register(Box::new(pdf::PdfTool));
    }

    // Windows-only tools
    #[cfg(target_os = "windows")]
    {
        if is_enabled("powershell_query") {
            registry.register(Box::new(powershell::PowerShellTool));
        }
        if is_enabled("wmi") {
            registry.register(Box::new(wmi_tool::WmiTool));
        }
        if is_enabled("office") {
            registry.register(Box::new(office::OfficeTool));
        }
        if is_enabled("uia") {
            registry.register(Box::new(uia::UiaTool));
        }
        if is_enabled("screen_capture") {
            registry.register(Box::new(screen::ScreenTool));
        }
        if is_enabled("com") {
            registry.register(Box::new(com_tool::ComTool));
        }
        if is_enabled("com_invoke") {
            registry.register(Box::new(com_invoke::ComInvokeTool));
        }
    }

    // Dynamically loaded user tools
    if let Some(dir) = user_tools_dir {
        let user_tools = user_tool::load_user_tools(dir);
        tracing::info!(
            "Loaded {} user tool(s) from {}",
            user_tools.len(),
            dir.display()
        );
        for tool in user_tools {
            registry.register(Box::new(tool));
        }
    }

    registry
}

/// Load MCP tools from configured servers and register them into an existing registry.
/// This is async because MCP connections require network/process I/O.
#[allow(dead_code)]
pub async fn register_mcp_tools(registry: &mut ToolRegistry, mcp_servers: &[mcp::McpServerConfig]) {
    for server in mcp_servers {
        if !server.enabled {
            continue;
        }
        let tools = mcp::build_mcp_tools(server).await;
        for tool in tools {
            registry.register(Box::new(tool));
        }
    }
}
