//! Desktop-side helpers for headless execution.
//!
//! Canonical schemas live in [`pisci_core::host`]. This module only owns
//! the pieces that require desktop-specific knowledge:
//!
//!   * [`disabled_tools_for_mode`] — computed from the desktop tool profile.

use crate::tools::{self, RuntimeToolProfile};

pub use pisci_core::host::{
    DisabledToolInfo, HeadlessCliMode, HeadlessCliRequest, HeadlessCliResponse,
    HeadlessContextToggles, PoolWaitSummary,
};

/// Map a canonical `HeadlessCliMode` onto the desktop's runtime tool
/// profile so we can answer "which tools are disabled?" without duplicating
/// the enum.
pub(crate) fn tool_profile(mode: HeadlessCliMode) -> RuntimeToolProfile {
    match mode {
        HeadlessCliMode::Pisci => RuntimeToolProfile::HeadlessPisci,
        HeadlessCliMode::Pool => RuntimeToolProfile::HeadlessPool,
    }
}

pub fn disabled_tools_for_mode(mode: HeadlessCliMode) -> Vec<DisabledToolInfo> {
    tools::runtime_disabled_tools(tool_profile(mode))
        .into_iter()
        .map(|tool| DisabledToolInfo {
            name: tool.name.to_string(),
            reason: tool
                .reason
                .unwrap_or("Disabled by runtime profile.")
                .to_string(),
        })
        .collect()
}
