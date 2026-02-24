/// Policy Gate — host-side security layer.
/// Validates file paths and shell commands before execution.
use anyhow::Result;
use regex::Regex;
use std::path::{Path, PathBuf};
use once_cell::sync::Lazy;

#[derive(Debug, Clone, PartialEq)]
pub enum PolicyDecision {
    Allow,
    Deny(String),
    Warn(String),
}

// ---------------------------------------------------------------------------
// Blocked shell command patterns (Windows + Unix)
// ---------------------------------------------------------------------------

static BLOCKED_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        Regex::new(r"(?i)^\s*format\s+[a-z]:").unwrap(),
        Regex::new(r"(?i)del\s+/[fsq]+\s+[a-z]:\\").unwrap(),
        Regex::new(r"(?i)rd\s+/[sq]+\s+[a-z]:\\").unwrap(),
        Regex::new(r"(?i)reg\s+(delete|add)\s+HKLM\\SYSTEM").unwrap(),
        Regex::new(r"(?i)shutdown\s+(/s|/r|-h|-r)").unwrap(),
        Regex::new(r"(?i)rm\s+-rf\s+/").unwrap(),
        Regex::new(r"(?i)dd\s+if=.*of=/dev/(sd|hd|nvme)").unwrap(),
        Regex::new(r"(?i)mkfs\s+").unwrap(),
        Regex::new(r"(?i):\(\)\s*\{.*\};\s*:").unwrap(), // fork bomb
    ]
});

static WARN_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        Regex::new(r"(?i)pip\s+install").unwrap(),
        Regex::new(r"(?i)npm\s+install\s+-g").unwrap(),
        Regex::new(r"(?i)curl\s+.*\|\s*(bash|sh|powershell)").unwrap(),
        Regex::new(r"(?i)powershell\s+-EncodedCommand").unwrap(),
        Regex::new(r"(?i)Invoke-Expression").unwrap(),
        Regex::new(r"(?i)iex\s+").unwrap(),
        Regex::new(r"(?i)Set-ExecutionPolicy\s+Unrestricted").unwrap(),
    ]
});

// ---------------------------------------------------------------------------
// PolicyGate
// ---------------------------------------------------------------------------

pub struct PolicyGate {
    pub workspace_root: PathBuf,
}

impl PolicyGate {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
        }
    }

    /// Check a file path — must be within workspace_root
    pub fn check_path(&self, path: &str) -> PolicyDecision {
        let p = Path::new(path);

        // Resolve to absolute path
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.workspace_root.join(p)
        };

        // Canonicalize to resolve symlinks / ".."
        let canonical = match abs.canonicalize() {
            Ok(c) => c,
            Err(_) => {
                // File doesn't exist yet — check the parent
                let parent = abs.parent().unwrap_or(&abs);
                match parent.canonicalize() {
                    Ok(c) => c.join(abs.file_name().unwrap_or_default()),
                    Err(_) => abs.clone(),
                }
            }
        };

        let ws = match self.workspace_root.canonicalize() {
            Ok(c) => c,
            Err(_) => self.workspace_root.clone(),
        };

        if canonical.starts_with(&ws) {
            PolicyDecision::Allow
        } else {
            PolicyDecision::Deny(format!(
                "Path '{}' is outside the workspace root '{}'",
                path,
                ws.display()
            ))
        }
    }

    /// Check a shell command string
    pub fn check_command(&self, command: &str) -> PolicyDecision {
        for pattern in BLOCKED_PATTERNS.iter() {
            if pattern.is_match(command) {
                return PolicyDecision::Deny(format!(
                    "Command matches blocked pattern: {}",
                    pattern.as_str()
                ));
            }
        }
        for pattern in WARN_PATTERNS.iter() {
            if pattern.is_match(command) {
                return PolicyDecision::Warn(format!(
                    "Command matches warning pattern: {}",
                    pattern.as_str()
                ));
            }
        }
        PolicyDecision::Allow
    }

    /// Check both path and command for a tool call
    pub fn check_tool_call(
        &self,
        tool_name: &str,
        input: &serde_json::Value,
    ) -> PolicyDecision {
        match tool_name {
            "file_read" | "file_write" | "file_edit" => {
                if let Some(path) = input["path"].as_str().or(input["file_path"].as_str()) {
                    return self.check_path(path);
                }
            }
            "shell" | "bash" | "powershell" => {
                if let Some(cmd) = input["command"].as_str().or(input["cmd"].as_str()) {
                    return self.check_command(cmd);
                }
            }
            _ => {}
        }
        PolicyDecision::Allow
    }
}
