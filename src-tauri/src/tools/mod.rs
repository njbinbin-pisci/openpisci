// ─── Platform-specific / host-coupled tools (still live in the desktop crate)
pub mod app_control;
pub mod browser;
pub mod call_fish;
pub mod call_koi;
pub mod chat_ui;
pub mod dpi;
pub mod office;
pub mod plan_todo;
pub mod pool_chat;
pub mod pool_org;
pub mod powershell;
pub mod skill_list;
pub mod wmi_tool;

// ─── Platform-neutral tools re-exported from the kernel.
//
// Only modules that are still referenced by their full `crate::tools::<name>::…`
// path from outside this module need a re-export; everything else is
// reachable through `pisci_kernel::tools` directly and the `HostTools`
// trait handles all registration internally.
pub use pisci_kernel::tools::{mcp, user_tool};

#[cfg(target_os = "windows")]
pub mod com_invoke;
#[cfg(target_os = "windows")]
pub mod com_tool;
#[cfg(target_os = "windows")]
pub mod screen;
#[cfg(target_os = "windows")]
pub mod uia;

use std::collections::HashMap;

/// Runtime tool profile. The interactive desktop host never calls
/// [`apply_runtime_tool_profile`] / [`runtime_disabled_tools`] — it uses
/// the full builtin set — so we only enumerate the headless variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeToolProfile {
    HeadlessPisci,
    HeadlessPool,
}

#[derive(Debug, Clone)]
pub struct ToolAvailability {
    pub name: &'static str,
    pub reason: Option<&'static str>,
}

const WINDOWS_ORIENTED_TOOLS: &[(&str, &str)] = &[
    (
        "powershell_query",
        "Disabled outside Windows: relies on Windows PowerShell semantics.",
    ),
    ("wmi", "Disabled outside Windows: WMI is Windows-only."),
    (
        "office",
        "Disabled outside Windows: current implementation depends on Windows Office automation.",
    ),
    (
        "uia",
        "Disabled outside Windows: UI Automation is Windows-only.",
    ),
    (
        "screen_capture",
        "Disabled outside Windows: current implementation uses Windows APIs.",
    ),
    ("com", "Disabled outside Windows: COM/OLE is Windows-only."),
    (
        "com_invoke",
        "Disabled outside Windows: COM/OLE is Windows-only.",
    ),
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
