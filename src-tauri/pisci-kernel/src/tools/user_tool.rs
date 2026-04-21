//! User-defined tool plugin system.
//!
//! Each user tool lives in `<app_data>/user-tools/<name>/` and consists of:
//!   - `manifest.json` - metadata, input_schema, config_schema, runtime declaration
//!   - an entry-point script (e.g. `index.ts`, `index.js`, `tool.ps1`)
//!
//! Execution model: Rust spawns a child process and passes two JSON arguments:
//!   argv[1] = tool input (from LLM)
//!   argv[2] = tool config (from user settings, passwords not in audit log)
//!
//! The child must write a JSON object to stdout:
//!   { "ok": true,  "content": "..." }
//!   { "ok": false, "error":   "..." }

use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{info, warn};

// ─── Manifest ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigFieldSchema {
    /// "string" | "number" | "boolean" | "password"
    #[serde(rename = "type")]
    pub field_type: String,
    pub label: Option<String>,
    pub default: Option<Value>,
    pub description: Option<String>,
    /// For "string" fields: optional placeholder text
    pub placeholder: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserToolManifest {
    pub name: String,
    pub description: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub author: String,
    /// "deno" | "node" | "powershell" | "python" | "bun"
    pub runtime: String,
    /// Path relative to the tool directory (e.g. "index.ts")
    pub entrypoint: String,
    pub input_schema: Value,
    /// Map of config key → field schema (drives the auto-generated Settings form)
    #[serde(default)]
    pub config_schema: HashMap<String, ConfigFieldSchema>,
    /// Fields listed here are treated as read-only (built-in tools exposed as user tools)
    #[serde(default)]
    pub readonly: bool,
    /// Timeout in seconds for subprocess execution (default 60)
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_version() -> String {
    "1.0".into()
}
fn default_timeout() -> u64 {
    60
}

impl UserToolManifest {
    pub fn load(dir: &Path) -> Result<Self> {
        let manifest_path = dir.join("manifest.json");
        let content = std::fs::read_to_string(&manifest_path)?;
        let manifest: UserToolManifest = serde_json::from_str(&content)?;
        Ok(manifest)
    }

    /// Returns `true` if the config_schema contains at least one password field.
    #[allow(dead_code)]
    pub fn has_secret_fields(&self) -> bool {
        self.config_schema
            .values()
            .any(|f| f.field_type == "password")
    }
}

// ─── Runtime command builder ─────────────────────────────────────────────────

/// Returns (program, base_args) for the given runtime.
fn runtime_command(runtime: &str, entrypoint: &Path) -> (String, Vec<String>) {
    let ep = entrypoint.to_string_lossy().to_string();
    match runtime {
        "deno" => ("deno".into(), vec!["run".into(), "--allow-all".into(), ep]),
        "node" => {
            // Try tsx (TypeScript runner) first, fall back to plain node
            (
                "node".into(),
                vec![
                    "-e".into(),
                    format!("require('tsx/cjs'); require('{}')", ep.replace('\\', "/")),
                ],
            )
        }
        "powershell" | "ps1" => (
            "powershell".into(),
            vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-File".into(),
                ep,
            ],
        ),
        "python" | "python3" => ("python".into(), vec![ep]),
        "bun" => ("bun".into(), vec!["run".into(), ep]),
        _ => ("node".into(), vec![ep]),
    }
}

// ─── UserTool ─────────────────────────────────────────────────────────────────

pub struct UserTool {
    pub manifest: UserToolManifest,
    pub tool_dir: PathBuf,
}

impl UserTool {
    pub fn new(manifest: UserToolManifest, tool_dir: PathBuf) -> Self {
        Self { manifest, tool_dir }
    }
}

#[async_trait]
impl Tool for UserTool {
    fn name(&self) -> &str {
        &self.manifest.name
    }
    fn description(&self) -> &str {
        &self.manifest.description
    }
    fn input_schema(&self) -> Value {
        self.manifest.input_schema.clone()
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let entrypoint = self.tool_dir.join(&self.manifest.entrypoint);
        if !entrypoint.exists() {
            return Ok(ToolResult::err(format!(
                "User tool '{}': entrypoint '{}' not found",
                self.manifest.name,
                entrypoint.display()
            )));
        }

        // Build config JSON from context (no secrets in audit log — handled upstream)
        let config = ctx
            .settings
            .user_tool_configs
            .get(&self.manifest.name)
            .cloned()
            .unwrap_or_else(|| json!({}));

        let input_str = serde_json::to_string(&input).unwrap_or_default();
        let config_str = serde_json::to_string(&config).unwrap_or_default();

        let (program, mut args) = runtime_command(&self.manifest.runtime, &entrypoint);
        args.push(input_str);
        args.push(config_str);

        info!(
            tool = %self.manifest.name,
            runtime = %self.manifest.runtime,
            "Executing user tool"
        );

        let timeout = Duration::from_secs(self.manifest.timeout_secs);

        let output = tokio::time::timeout(
            timeout,
            tokio::task::spawn_blocking({
                let program = program.clone();
                let args = args.clone();
                let dir = self.tool_dir.clone();
                move || {
                    let mut cmd = std::process::Command::new(&program);
                    cmd.args(&args).current_dir(&dir);
                    // CREATE_NO_WINDOW: prevents a console window from flashing on screen
                    #[cfg(target_os = "windows")]
                    {
                        use std::os::windows::process::CommandExt;
                        cmd.creation_flags(0x0800_0000);
                    }
                    cmd.output()
                }
            }),
        )
        .await;

        let raw = match output {
            Ok(Ok(Ok(out))) => out,
            Ok(Ok(Err(e))) => {
                return Ok(ToolResult::err(format!(
                    "User tool '{}' failed to start: {}.\n\
                     Make sure '{}' runtime is installed and on PATH.",
                    self.manifest.name, e, program
                )))
            }
            Ok(Err(e)) => return Ok(ToolResult::err(format!("User tool spawn error: {}", e))),
            Err(_) => {
                return Ok(ToolResult::err(format!(
                    "User tool '{}' timed out after {}s",
                    self.manifest.name, self.manifest.timeout_secs
                )))
            }
        };

        let stdout = String::from_utf8_lossy(&raw.stdout).to_string();
        let stderr = String::from_utf8_lossy(&raw.stderr).to_string();

        if !raw.status.success() && stdout.trim().is_empty() {
            warn!(
                tool = %self.manifest.name,
                stderr = %stderr,
                "User tool exited with non-zero status"
            );
            return Ok(ToolResult::err(format!(
                "User tool '{}' failed (exit {}): {}",
                self.manifest.name,
                raw.status.code().unwrap_or(-1),
                if stderr.is_empty() {
                    "no stderr output".into()
                } else {
                    stderr
                }
            )));
        }

        // Parse stdout as JSON response
        let stdout_trimmed = stdout.trim();
        match serde_json::from_str::<Value>(stdout_trimmed) {
            Ok(resp) => {
                if resp["ok"].as_bool() == Some(false) {
                    let err_msg = resp["error"]
                        .as_str()
                        .unwrap_or("Unknown error")
                        .to_string();
                    Ok(ToolResult::err(err_msg))
                } else {
                    let content = resp["content"]
                        .as_str()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| stdout_trimmed.to_string());
                    Ok(ToolResult::ok(content))
                }
            }
            Err(_) => {
                // Not JSON — return raw stdout (useful for simple scripts)
                if stdout_trimmed.is_empty() {
                    Ok(ToolResult::ok("(no output)"))
                } else {
                    Ok(ToolResult::ok(stdout_trimmed))
                }
            }
        }
    }
}

// ─── Loader ───────────────────────────────────────────────────────────────────

/// Scan `user_tools_dir` and return all valid UserTool instances.
/// Skips directories with invalid/missing manifests (logs a warning).
pub fn load_user_tools(user_tools_dir: &Path) -> Vec<UserTool> {
    if !user_tools_dir.exists() {
        return Vec::new();
    }

    let mut tools = Vec::new();

    let entries = match std::fs::read_dir(user_tools_dir) {
        Ok(e) => e,
        Err(e) => {
            warn!("Failed to read user-tools directory: {}", e);
            return tools;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        match UserToolManifest::load(&path) {
            Ok(manifest) => {
                info!(
                    "Loaded user tool: {} (runtime={})",
                    manifest.name, manifest.runtime
                );
                tools.push(UserTool::new(manifest, path));
            }
            Err(e) => {
                warn!("Skipping user tool at '{}': {}", path.display(), e);
            }
        }
    }

    tools
}
