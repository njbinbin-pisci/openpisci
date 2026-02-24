/// Policy Gate — host-side security layer.
/// Validates file paths, shell commands, browser URLs, UIA actions, and COM operations.
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
// Browser URL blocked patterns
// ---------------------------------------------------------------------------

static BLOCKED_URL_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        // Internal browser pages
        Regex::new(r"(?i)^chrome://").unwrap(),
        Regex::new(r"(?i)^chrome-extension://").unwrap(),
        Regex::new(r"(?i)^edge://").unwrap(),
        Regex::new(r"(?i)^about:").unwrap(),
        // Local file system
        Regex::new(r"(?i)^file://").unwrap(),
        // Localhost and loopback (could expose local services)
        Regex::new(r"(?i)^https?://(localhost|127\.0\.0\.1|0\.0\.0\.0)(:|/)").unwrap(),
        // Private IP ranges (RFC 1918)
        Regex::new(r"(?i)^https?://10\.\d+\.\d+\.\d+").unwrap(),
        Regex::new(r"(?i)^https?://192\.168\.\d+\.\d+").unwrap(),
        Regex::new(r"(?i)^https?://172\.(1[6-9]|2\d|3[01])\.\d+\.\d+").unwrap(),
    ]
});

static WARN_URL_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        // Banking / financial sites
        Regex::new(r"(?i)(bank|paypal|stripe|payment|checkout)").unwrap(),
        // Authentication pages
        Regex::new(r"(?i)(login|signin|auth|oauth|password)").unwrap(),
    ]
});

// ---------------------------------------------------------------------------
// UIA sensitive control patterns
// ---------------------------------------------------------------------------

static SENSITIVE_UIA_CLASSES: Lazy<Vec<&'static str>> = Lazy::new(|| {
    vec![
        "PasswordBox",
        "PasswordEdit",
        // Windows UAC dialog
        "Credential Dialog Xaml Host",
        // Task Manager
        "TaskManagerWindow",
    ]
});

// ---------------------------------------------------------------------------
// Blocked process names for UIA (AI should not control these)
// ---------------------------------------------------------------------------

static BLOCKED_PROCESS_TITLES: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        Regex::new(r"(?i)Registry Editor").unwrap(),
        Regex::new(r"(?i)User Account Control").unwrap(),
        Regex::new(r"(?i)Windows Security").unwrap(),
        Regex::new(r"(?i)BitLocker").unwrap(),
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

        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.workspace_root.join(p)
        };

        let canonical = match abs.canonicalize() {
            Ok(c) => c,
            Err(_) => {
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

    /// Check a browser URL
    pub fn check_url(&self, url: &str) -> PolicyDecision {
        for pattern in BLOCKED_URL_PATTERNS.iter() {
            if pattern.is_match(url) {
                return PolicyDecision::Deny(format!(
                    "URL blocked by policy: '{}' matches pattern '{}'",
                    url,
                    pattern.as_str()
                ));
            }
        }
        for pattern in WARN_URL_PATTERNS.iter() {
            if pattern.is_match(url) {
                return PolicyDecision::Warn(format!(
                    "Navigating to potentially sensitive URL: {}",
                    url
                ));
            }
        }
        PolicyDecision::Allow
    }

    /// Check a UIA action
    pub fn check_uia_action(&self, action: &str, input: &serde_json::Value) -> PolicyDecision {
        // Block operations on sensitive windows
        if let Some(title) = input["window_title"].as_str().or(input["name"].as_str()) {
            for pattern in BLOCKED_PROCESS_TITLES.iter() {
                if pattern.is_match(title) {
                    return PolicyDecision::Deny(format!(
                        "UIA action '{}' blocked on sensitive window: '{}'",
                        action, title
                    ));
                }
            }
        }

        // Warn when typing into password boxes
        if action == "type" {
            if let Some(class) = input["class_name"].as_str() {
                for &sensitive_class in SENSITIVE_UIA_CLASSES.iter() {
                    if class.eq_ignore_ascii_case(sensitive_class) {
                        return PolicyDecision::Warn(format!(
                            "Typing into potentially sensitive control class: '{}'",
                            class
                        ));
                    }
                }
            }
        }

        PolicyDecision::Allow
    }

    /// Check a COM/clipboard action
    pub fn check_com_action(&self, action: &str, input: &serde_json::Value) -> PolicyDecision {
        match action {
            "clipboard_write" => PolicyDecision::Warn(
                "Writing to clipboard — this will replace current clipboard content".into()
            ),
            "shell_run" => {
                if let Some(cmd) = input["command"].as_str() {
                    return self.check_command(cmd);
                }
                PolicyDecision::Allow
            }
            _ => PolicyDecision::Allow,
        }
    }

    /// Check a browser eval_js action
    pub fn check_browser_js(&self, _js: &str) -> PolicyDecision {
        PolicyDecision::Warn(
            "Executing JavaScript in browser — ensure the code is safe".into()
        )
    }

    /// Unified tool call check — dispatches to appropriate checker
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
            "shell" | "bash" | "powershell" | "powershell_query" => {
                if let Some(cmd) = input["command"].as_str()
                    .or(input["cmd"].as_str())
                    .or(input["ps_command"].as_str())
                {
                    return self.check_command(cmd);
                }
            }
            "browser" => {
                let action = input["action"].as_str().unwrap_or("");
                match action {
                    "navigate" => {
                        if let Some(url) = input["url"].as_str() {
                            return self.check_url(url);
                        }
                    }
                    "eval_js" => {
                        if let Some(js) = input["js"].as_str() {
                            return self.check_browser_js(js);
                        }
                    }
                    "get_cookies" | "set_cookie" | "clear_cookies" => {
                        return PolicyDecision::Warn(
                            "Cookie operation in browser — may affect authentication/session state".into()
                        );
                    }
                    _ => {}
                }
            }
            "uia" => {
                let action = input["action"].as_str().unwrap_or("");
                return self.check_uia_action(action, input);
            }
            "com" => {
                let action = input["action"].as_str().unwrap_or("");
                return self.check_com_action(action, input);
            }
            _ => {}
        }
        PolicyDecision::Allow
    }
}
